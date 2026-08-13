//! virtio-blk: диск.
//!
//! Запрос состоит из трёх буферов, и разделение между ними задано
//! спецификацией, а не удобством: заголовок устройство **читает**, данные при
//! чтении **пишет**, а однобайтовое состояние пишет всегда. Три дескриптора в
//! цепочке, флаг `WRITE` ровно там, где надо, — перепутать направление значит
//! получить устройство, которое молча ничего не делает.
//!
//! # Промежуточный буфер
//!
//! Данные ходят через выделенный под DMA буфер, а не напрямую в срез
//! вызывающего. Причина не в удобстве: срез приходит из кучи, а куча отображена
//! как обычная кешируемая память и не обязана быть физически непрерывной.
//! Устройство же адресует память физически и ничего не знает ни про таблицы
//! страниц, ни про кеш. Копирование через [`crate::mm::dma`] — единственный
//! способ дать ему адрес, по которому лежит именно то, что мы имели в виду.

use core::sync::atomic::{Ordering, fence};

use super::{
    DESC_F_NEXT, DESC_F_WRITE, FEATURE_VERSION_1, Queue, Transport, VirtioError,
};
use crate::mm::dma::{self, DmaBuffer};
use crate::pci::{self, Device};

/// Тип запроса: чтение.
const REQUEST_IN: u32 = 0;
/// Тип запроса: запись.
const REQUEST_OUT: u32 = 1;

/// Размер заголовка запроса: тип, зарезервированное поле, номер сектора.
const HEADER_SIZE: usize = 16;

/// Состояние, которым устройство отвечает: ноль — успех.
const STATUS_OK: u8 = 0;

/// Смещение поля «ёмкость» в конфигурации блочного устройства. Считается в
/// секторах по 512 байт независимо от того, чем считает сам носитель.
const CONFIG_CAPACITY: usize = 0;

/// Сектор virtio-blk — всегда 512 байт, что бы ни было у носителя под ним.
pub const SECTOR_SIZE: usize = 512;

/// Наибольшая передача за один запрос.
///
/// 64 КиБ с запасом покрывают всё, что просят вышележащие: блок ext2 — не
/// больше 4 КиБ, таблица разделов GPT — 16 КиБ. Буфер выделяется один раз при
/// подключении: выделять его на каждое чтение значило бы исчерпать окно DMA,
/// в котором нет освобождения.
const MAX_TRANSFER: usize = 64 * 1024;

/// Сколько раз опрашивать кольцо завершений, прежде чем признать устройство
/// зависшим.
///
/// Предел нужен не ради изящества: без него отказавшее устройство остановило бы
/// ядро навсегда, причём беззвучно. Значение подобрано с большим запасом —
/// эмулируемый диск отвечает за единицы тысяч оборотов цикла.
const POLL_LIMIT: u32 = 200_000_000;

/// Диск virtio.
pub struct VirtioBlk {
    /// Окно регистров. Читается только при подключении, но храниться обязано:
    /// в нём живёт отображение, по которому устройство уведомляют, и уронить
    /// его значит уронить очередь.
    #[allow(dead_code)]
    transport: Transport,
    queue: Queue,
    /// Заголовок запроса и байт состояния — в общей памяти с устройством.
    control: DmaBuffer,
    /// Буфер данных.
    data: DmaBuffer,
    sectors: u64,
}

impl VirtioBlk {
    /// Найти диск на шине и подготовить его к работе.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц.
    pub unsafe fn probe(root: &pci::Root) -> Result<Self, VirtioError> {
        // SAFETY: контракт функции.
        let device = unsafe {
            pci::find_by_id(
                root,
                pci::VENDOR_VIRTIO,
                &[pci::DEVICE_VIRTIO_BLK_LEGACY, pci::DEVICE_VIRTIO_BLK_MODERN],
            )
        }
        .ok_or(VirtioError::NoCapabilities)?;

        // SAFETY: контракт функции.
        unsafe { Self::attach(&device) }
    }

    /// Подготовить найденное устройство.
    ///
    /// # Safety
    ///
    /// См. [`VirtioBlk::probe`].
    unsafe fn attach(device: &Device) -> Result<Self, VirtioError> {
        // Ответы на обращения к памяти разрешаются **до** первого чтения
        // регистров, и это не порядок ради порядка. При сброшенном бите Memory
        // Space устройство не отвечает на обращения к своим BAR вовсе: чтения
        // возвращают все единицы, записи пропадают. Отказа при этом нет, и
        // выглядит всё как исправно работающий драйвер, у которого просто
        // «неправильное» железо.
        //
        // Ровно на это ушёл день отладки: прошивка `ArmVirtQemu` оставляет бит
        // сброшенным после `ExitBootServices`, а OVMF на x86-64 — нет. Тот же
        // драйвер работал на одной машине и молча не работал на другой, а
        // первым видимым признаком был отказ по таймауту в чтении диска —
        // максимально далеко от причины.
        //
        // SAFETY: bus master здесь ещё безопасен: устройство начнёт обращаться
        // к памяти только после того, как ему сообщат адреса колец и выставят
        // DRIVER_OK, а до этого оно даже не выведено из сброса.
        unsafe { device.enable_bus_master() };

        // SAFETY: контракт функции.
        let transport = unsafe { Transport::open(device) }?;

        // Возможностей просим ровно одну — соответствие virtio 1.0. Всё
        // остальное, что предлагает virtio-blk (барьеры, обрезка, многоочередность),
        // требует кода, которого здесь нет, а согласовать возможность и не
        // реализовать её — верный способ получить порчу данных.
        transport.negotiate(FEATURE_VERSION_1)?;

        let queue = Queue::new(&transport, 0)?;

        let control = dma::alloc(HEADER_SIZE + 1).map_err(VirtioError::NoMemory)?;
        let data = dma::alloc(MAX_TRANSFER).map_err(VirtioError::NoMemory)?;

        // Кольца построены и обнулены, адреса сообщены — только теперь
        // устройству разрешается ими пользоваться.
        transport.set_driver_ok();

        // SAFETY: окно конфигурации устройства отображено в `Transport::open`;
        // смещение поля ёмкости задано спецификацией virtio-blk.
        let sectors = unsafe { transport.device_config64(CONFIG_CAPACITY) };
        if sectors == 0 {
            transport.set_failed();
            return Err(VirtioError::NoMedium);
        }

        Ok(Self {
            transport,
            queue,
            control,
            data,
            sectors,
        })
    }

    /// Выполнить один запрос к устройству.
    fn request(&mut self, kind: u32, sector: u64, len: usize) -> Result<(), VirtioError> {
        // Заголовок: тип, зарезервированное слово, номер сектора.
        // SAFETY: буфер выделен под заголовок и байт состояния.
        unsafe {
            let header = self.control.as_ptr::<u8>();
            header.cast::<u32>().write_volatile(kind);
            header.add(4).cast::<u32>().write_volatile(0);
            header.add(8).cast::<u64>().write_volatile(sector);
            // Байт состояния заполняется заведомо не нулём: иначе успех
            // невозможно отличить от «устройство его не тронуло».
            header.add(HEADER_SIZE).write_volatile(0xFF);
        }

        let status_phys = self.control.phys().as_u64() + HEADER_SIZE as u64;
        // Данные при чтении устройство пишет, при записи — читает. Флаг ровно
        // здесь, и он единственное, что отличает две операции на уровне колец.
        let data_flags = if kind == REQUEST_IN {
            DESC_F_NEXT | DESC_F_WRITE
        } else {
            DESC_F_NEXT
        };

        self.queue
            .set_descriptor(0, self.control.phys().as_u64(), HEADER_SIZE as u32, DESC_F_NEXT, 1);
        self.queue
            .set_descriptor(1, self.data.phys().as_u64(), len as u32, data_flags, 2);
        // Байт состояния устройство пишет всегда, и цепочка на нём кончается.
        self.queue.set_descriptor(2, status_phys, 1, DESC_F_WRITE, 0);

        fence(Ordering::SeqCst);
        self.queue.submit_and_wait(POLL_LIMIT)?;
        fence(Ordering::SeqCst);

        // SAFETY: буфер выделен под заголовок и байт состояния.
        let status = unsafe { self.control.as_ptr::<u8>().add(HEADER_SIZE).read_volatile() };
        if status != STATUS_OK {
            return Err(VirtioError::RequestFailed(status));
        }
        Ok(())
    }

    /// Прочитать сектора в буфер.
    pub fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), VirtioError> {
        if buf.is_empty() || buf.len() % SECTOR_SIZE != 0 {
            return Err(VirtioError::BadTransfer);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let chunk = (buf.len() - done).min(MAX_TRANSFER);
            self.request(REQUEST_IN, lba + (done / SECTOR_SIZE) as u64, chunk)?;
            // SAFETY: буфер выделен на MAX_TRANSFER байт, `chunk` не больше.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.data.as_ptr::<u8>(),
                    buf.as_mut_ptr().add(done),
                    chunk,
                );
            }
            done += chunk;
        }
        Ok(())
    }

    /// Записать сектора.
    ///
    /// Вызывающего сегодня нет: система только читает свой корень. Метод
    /// оставлен потому, что без него `BlockDevice` реализуется наполовину, а
    /// половинчатая реализация трейта — это отказ, который обнаружится в самый
    /// неподходящий момент. Путь проверен ровно настолько, насколько проверено
    /// чтение: разница между ними в одном флаге дескриптора.
    pub fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), VirtioError> {
        if buf.is_empty() || buf.len() % SECTOR_SIZE != 0 {
            return Err(VirtioError::BadTransfer);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let chunk = (buf.len() - done).min(MAX_TRANSFER);
            // SAFETY: буфер выделен на MAX_TRANSFER байт, `chunk` не больше.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    buf.as_ptr().add(done),
                    self.data.as_ptr::<u8>(),
                    chunk,
                );
            }
            self.request(REQUEST_OUT, lba + (done / SECTOR_SIZE) as u64, chunk)?;
            done += chunk;
        }
        Ok(())
    }
}

/// Мост к крейту `disk`: тот же трейт, что у образа в памяти на хосте и у
/// носителя прошивки в установщике.
///
/// Именно благодаря ему разбор GPT и чтение ext2 в ядре исполняются тем же
/// кодом, который покрыт тестами на хосте.
impl disk::BlockDevice for VirtioBlk {
    fn sector_size(&self) -> u32 {
        SECTOR_SIZE as u32
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        if lba + (buf.len() / SECTOR_SIZE) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        self.read_sectors(lba, buf).map_err(|err| {
            crate::kprintln!("virtio-blk: read at LBA {lba} failed: {err}");
            disk::Error::Io
        })
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        if lba + (buf.len() / SECTOR_SIZE) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        self.write_sectors(lba, buf).map_err(|err| {
            crate::kprintln!("virtio-blk: write at LBA {lba} failed: {err}");
            disk::Error::Io
        })
    }

    fn flush(&mut self) -> disk::Result<()> {
        // Сбрасывать нечего: возможность `VIRTIO_BLK_F_FLUSH` не согласована, а
        // значит устройство обязано выполнять записи немедленно. Сообщить об
        // этом честнее, чем послать запрос, которого оно не ждёт.
        Ok(())
    }
}
