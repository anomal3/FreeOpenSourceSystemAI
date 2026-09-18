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

use alloc::vec::Vec;

use crate::kprintln;
use core::sync::atomic::{Ordering, fence};

use super::{
    DESC_F_NEXT, DESC_F_WRITE, FEATURE_VERSION_1, Queue, Transport, VirtioError, map_bar,
};
use crate::mm::dma::{self, DmaBuffer};
use crate::pci::{self, Device, MSIX_ENTRY_SIZE};

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
    /// Ярлык прерывания очереди, если устройство согласилось его присылать.
    ///
    /// `None` — ждём завершения холостым циклом, как ядро ждало всегда. Это не
    /// редкий случай: на GICv3 MSI у нас нет вовсе, и диск там работает ровно
    /// как до этой фазы.
    irq: Option<u32>,
}

/// Ярлык источника прерывания для планировщика.
///
/// Число ничего не значит снаружи (см. `sched::Wait::Irq`); важно лишь, что оно
/// не совпадает с ярлыками xHCI (1), питания (2) и сети (3).
const IRQ_SOURCE: u32 = crate::irq::source::VIRTIO_BLK;

/// Обработчик прерывания диска.
///
/// Будит того, кто ждёт завершения, и больше ничего: само завершение лежит в
/// кольце `used`, и проверять его будет проснувшийся — под своим локом, а не
/// здесь, где взять его нельзя.
///
/// Признака «пришло» тут нет намеренно, в отличие от сети: у диска источник
/// правды — само кольцо, и ждущий смотрит **в него** (`Queue::has_used`).
/// Отдельный признак был бы вторым описанием того же события, и разошлись бы
/// они на первом же потерянном прерывании.
pub fn on_interrupt() {
    crate::sched::wake_irq(IRQ_SOURCE);
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

    /// Поднять **все** контроллеры virtio-blk, какие есть на шине.
    ///
    /// # Почему это не то же самое, что [`VirtioBlk::probe`]
    ///
    /// Потому что дисков бывает больше одного, и это не редкий случай, а
    /// обычный: сразу после установки в машине стоят два — тот, на который
    /// поставили, и тот, с которого ставили. Ядро, поднимавшее только первый,
    /// находило на нём один лишь ESP установочного носителя, не находило
    /// корневого раздела и оставалось на initrd. Со стороны это выглядит как
    /// «установка не сработала», хотя сработала она полностью: система на
    /// диске есть, просто ядро на неё не посмотрело.
    ///
    /// Отказ отдельного устройства пропускается, а не прекращает перебор:
    /// неисправный диск не должен отменять исправный.
    ///
    /// # Safety
    ///
    /// См. [`VirtioBlk::probe`].
    pub unsafe fn probe_all(root: &pci::Root) -> Vec<Self> {
        let mut found = Vec::new();
        let mut devices = Vec::new();

        // Устройства сначала собираются, и только потом поднимаются: `attach`
        // трогает конфигурационное пространство и BAR, а перебор шины идёт по
        // тем же окнам. Смешивать одно с другим — значит менять то, по чему
        // идёшь.
        //
        // SAFETY: контракт функции.
        unsafe {
            pci::for_each(root, |device| {
                if device.vendor == pci::VENDOR_VIRTIO
                    && (device.device == pci::DEVICE_VIRTIO_BLK_LEGACY
                        || device.device == pci::DEVICE_VIRTIO_BLK_MODERN)
                {
                    devices.push(*device);
                }
                true
            });
        }

        for device in devices {
            // SAFETY: контракт функции.
            match unsafe { Self::attach(&device) } {
                Ok(disk) => {
                    crate::devices::claim(device.address, "virtio-blk");
                    found.push(disk);
                }
                Err(err) => {
                    kprintln!("  disk        : virtio-blk at {} unusable: {err}", device.address);
                }
            }
        }
        found
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

        let mut disk = Self {
            transport,
            queue,
            control,
            data,
            sectors,
            irq: None,
        };
        // Прерывания просятся последними: до `DRIVER_OK` устройство не обязано
        // смотреть в очередь, а первый запрос пойдёт уже после возврата отсюда.
        //
        // SAFETY: контракт функции — ядро на своих таблицах страниц.
        if unsafe { disk.enable_interrupts(device) } {
            disk.irq = Some(IRQ_SOURCE);
        }
        Ok(disk)
    }

    /// Перевести ожидание завершений с холостого цикла на прерывания.
    ///
    /// Возвращает `false`, когда не вышло, и это не ошибка: диск продолжает
    /// работать ровно как раньше. Каждая причина называется вслух — «диск
    /// занимает процессор, пока отвечает» человек должен видеть, а не угадывать.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц.
    unsafe fn enable_interrupts(&mut self, device: &Device) -> bool {
        let Some(msix) = device.msix() else {
            kprintln!("  virtio-blk  : no MSI-X capability; completions will be spun on");
            return false;
        };
        let Some((address, data)) = crate::arch::interrupts::alloc_msi(on_interrupt) else {
            kprintln!("  virtio-blk  : no MSI target on this machine; completions will be spun on");
            return false;
        };
        let span =
            u64::from(msix.table_offset) + (MSIX_ENTRY_SIZE * usize::from(msix.vectors)) as u64;
        // SAFETY: контракт функции; окно — регистры устройства.
        let table = match unsafe { map_bar(device, msix.bir as u8, span) } {
            Ok(base) => base.as_usize() + msix.table_offset as usize,
            Err(err) => {
                kprintln!("  virtio-blk  : cannot map the MSI-X table ({err:?}); completions will be spun on");
                return false;
            }
        };
        // SAFETY: таблица отображена, строка 0 существует всегда, обработчик
        // поставлен `alloc_msi` — прерывание может прийти сразу после записи.
        unsafe { device.set_msix_vector(&msix, table, 0, address, data) };
        // SAFETY: MSI-X включён записью выше.
        unsafe { Queue::silence_config_changes(&self.transport) };
        // SAFETY: см. выше; вектор ноль — та самая строка, что заполнена.
        if !unsafe { self.queue.want_interrupts(&self.transport, 0) } {
            kprintln!("  virtio-blk  : the device refused the queue vector; completions will be spun on");
            return false;
        }
        kprintln!(
            "  virtio-blk  : MSI-X vector 0 -> {address:#018x} data {data:#x}, completions arrive by interrupt"
        );
        true
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
        self.queue.submit_and_wait(POLL_LIMIT, self.irq)?;
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
