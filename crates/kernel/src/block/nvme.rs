//! NVMe: диск, подключённый напрямую к шине PCI Express.
//!
//! Второй драйвер, закрывающий «система не находит свой корень на чужой
//! машине». AHCI (фаза 26a) — это то, чем диск подключён в гипервизоре и в
//! компьютере постарше; NVMe — то, чем он подключён в любом ноутбуке,
//! купленном за последние лет восемь. Между ними нет ничего общего, кроме
//! результата: сектор, прочитанный в буфер.
//!
//! # Устройство в трёх абзацах
//!
//! Регистров у контроллера мало, и почти все — про запуск. Работа идёт через
//! **пары очередей в памяти**: очередь отправки (команды по 64 байта) и очередь
//! завершения (ответы по 16). Пар две: административная — ей создают остальные и
//! спрашивают, что это за диск, — и очередь ввода-вывода, через которую ходят
//! чтения и записи. Разделение не формальность: административные команды
//! медленные и редкие, и держать их в одной очереди с чтением значило бы
//! останавливать диск ради вопроса о нём.
//!
//! Команда кладётся в очередь отправки, после чего в **звонок** (doorbell —
//! регистр с номером свободного места) пишется новый хвост. Контроллер читает
//! команду сам, выполняет и кладёт ответ в очередь завершения. Как понять, что
//! ответ новый, а не прошлого круга: у каждой записи есть бит фазы, и он
//! меняется на противоположный каждый раз, когда очередь идёт по кругу заново.
//! Обнулять очередь не нужно, сравнивать с прошлым содержимым — тоже.
//!
//! Куда положить данные, описывают **PRP** — физические адреса страниц. Первое
//! поле указывает на начало, второе — либо на вторую страницу, либо на список
//! адресов, если страниц больше двух. Не «указатель и длина»: у NVMe нет
//! понятия непрерывного буфера, есть набор страниц, и это как раз то, чем
//! обычная память и является.
//!
//! # Чего здесь нет
//!
//! Одной очереди ввода-вывода хватает, потому что выше по стеку стоит одно
//! чтение за другим — очередь на тысячу команд имеет смысл там, где есть кому их
//! выдавать. Прерываний нет по той же причине, что в AHCI: завершение
//! опрашивается, ограничение по времени настоящее, по монотонному счётчику.
//! Пространств имён (namespace) поддержано одно, первое: диск с несколькими —
//! это диск, разделённый на части средствами контроллера, и выбирать между ними
//! сегодня некому.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::pci::{self, Device};
use crate::{kprintln, time};

/// Класс устройства PCI: контроллер запоминающих устройств.
const CLASS_STORAGE: u8 = 0x01;
/// Подкласс: контроллер энергонезависимой памяти.
const SUBCLASS_NVM: u8 = 0x08;
/// Интерфейс: NVM Express.
const PROG_IF_NVME: u8 = 0x02;

// --- регистры контроллера ----------------------------------------------------

/// Возможности контроллера (64 бита).
const REG_CAP: usize = 0x00;
/// Версия.
const REG_VS: usize = 0x08;
/// Настройка контроллера.
const REG_CC: usize = 0x14;
/// Состояние контроллера.
const REG_CSTS: usize = 0x1C;
/// Размеры административных очередей.
const REG_AQA: usize = 0x24;
/// Адрес административной очереди отправки.
const REG_ASQ: usize = 0x28;
/// Адрес административной очереди завершения.
const REG_ACQ: usize = 0x30;
/// Начало области звонков.
const DOORBELL_BASE: usize = 0x1000;

/// `CC.EN` — контроллер включён.
const CC_ENABLE: u32 = 1 << 0;
/// `CSTS.RDY` — контроллер готов принимать команды.
const CSTS_RDY: u32 = 1 << 0;
/// `CSTS.CFS` — контроллер объявил о собственном отказе.
const CSTS_CFS: u32 = 1 << 1;

/// `CAP.TO` — сколько контроллеру дано на запуск, в единицах по 500 мс.
const CAP_TO_SHIFT: u64 = 24;
const CAP_TO_MASK: u64 = 0xFF;
/// `CAP.DSTRD` — шаг между звонками: `4 << DSTRD` байт.
const CAP_DSTRD_SHIFT: u64 = 32;
const CAP_DSTRD_MASK: u64 = 0x0F;
/// `CAP.MQES` — наибольшая глубина очереди минус один.
const CAP_MQES_MASK: u64 = 0xFFFF;

/// Глубина очередей.
///
/// Тридцать две команды — с большим запасом: в очереди одновременно бывает
/// ровно одна. Меньше делать незачем (место всё равно занимает страницу),
/// больше — тоже: глубина нужна тому, кто умеет держать несколько запросов в
/// полёте, а этого выше по стеку сегодня нет.
const QUEUE_DEPTH: u16 = 32;

/// Размер записи в очереди отправки и завершения.
const SQ_ENTRY: usize = 64;
const CQ_ENTRY: usize = 16;

// --- команды -----------------------------------------------------------------

/// Административная: создать очередь завершения.
const ADMIN_CREATE_CQ: u8 = 0x05;
/// Административная: создать очередь отправки.
const ADMIN_CREATE_SQ: u8 = 0x01;
/// Административная: рассказать о себе.
const ADMIN_IDENTIFY: u8 = 0x06;
/// Ввод-вывод: записать.
const IO_WRITE: u8 = 0x01;
/// Ввод-вывод: прочитать.
const IO_READ: u8 = 0x02;
/// Ввод-вывод: довести записанное до носителя.
const IO_FLUSH: u8 = 0x00;

/// `Identify`: сведения о пространстве имён.
const IDENTIFY_NAMESPACE: u32 = 0x00;
/// `Identify`: сведения о контроллере.
const IDENTIFY_CONTROLLER: u32 = 0x01;

/// Пространство имён, с которым работаем. Первое и единственное.
const NSID: u32 = 1;

/// Размер сектора, который поддерживается.
///
/// NVMe свободно бывает с блоком 4096 — и это не редкость, а обычный формат
/// корпоративных дисков. Молча посчитать такой диск 512-байтным значит писать
/// каждый блок не туда, куда собирались, поэтому он отвергается с сообщением, а
/// не подгоняется.
pub const SECTOR_SIZE: usize = 512;

/// Наибольшая передача за одну команду — столько же, сколько у остальных
/// драйверов, чтобы буфер выделялся один раз и на всё время работы.
const MAX_TRANSFER: usize = 64 * 1024;

/// Сколько ждать выполнения команды.
const COMMAND_TIMEOUT_MS: u64 = 10_000;

/// Что могло пойти не так.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvmeError {
    /// BAR0 пуст или описывает пространство ввода-вывода.
    BadBar,
    /// Не удалось отобразить окно регистров.
    MapFailed,
    /// Нет памяти под очереди и буферы.
    NoMemory(DmaError),
    /// Контроллер не выключился или не включился за отведённое время.
    NotReady,
    /// Контроллер объявил о собственном отказе (`CSTS.CFS`).
    ControllerFailure,
    /// Команда не завершилась за отведённое время.
    Timeout,
    /// Команда завершилась с кодом ошибки.
    Failed(u16),
    /// Контроллер не поддерживает страницу в 4 КиБ либо очередь нужной глубины.
    Unsupported,
    /// Пространство имён пусто или отвечает несуразицей.
    BadNamespace,
    /// Блок не 512 байт.
    UnsupportedSectorSize(u32),
    /// Запрос не кратен сектору или пуст.
    BadTransfer,
}

impl core::fmt::Display for NvmeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadBar => write!(f, "BAR0 is not a memory window"),
            Self::MapFailed => write!(f, "cannot map the register window"),
            Self::NoMemory(err) => write!(f, "no DMA memory: {err}"),
            Self::NotReady => write!(f, "the controller did not become ready in time"),
            Self::ControllerFailure => write!(f, "the controller reported a fatal status"),
            Self::Timeout => write!(f, "the command did not finish in time"),
            Self::Failed(status) => write!(f, "the command failed, status {status:#06x}"),
            Self::Unsupported => write!(f, "the controller lacks something this driver needs"),
            Self::BadNamespace => write!(f, "namespace 1 reported nothing usable"),
            Self::UnsupportedSectorSize(size) => write!(f, "block size {size} is not supported"),
            Self::BadTransfer => write!(f, "transfer is empty or not a whole number of blocks"),
        }
    }
}

/// Пара очередей: отправка и завершение.
struct QueuePair {
    /// Номер пары. Ноль — административная, звонки считаются от него.
    id: u16,
    sq: DmaBuffer,
    cq: DmaBuffer,
    /// Куда класть следующую команду.
    sq_tail: u16,
    /// Откуда читать следующий ответ.
    cq_head: u16,
    /// Какой бит фазы означает «эта запись новая».
    ///
    /// Меняется каждый раз, когда чтение доходит до конца очереди. Без него
    /// отличить свежий ответ от прошлогоднего можно было бы только обнуляя
    /// очередь после каждой команды — то есть записью в память на каждый запрос
    /// вместо одного бита.
    phase: bool,
}

impl QueuePair {
    fn new(id: u16) -> Result<Self, NvmeError> {
        let sq = dma::alloc(QUEUE_DEPTH as usize * SQ_ENTRY).map_err(NvmeError::NoMemory)?;
        let cq = dma::alloc(QUEUE_DEPTH as usize * CQ_ENTRY).map_err(NvmeError::NoMemory)?;
        Ok(Self { id, sq, cq, sq_tail: 0, cq_head: 0, phase: true })
    }
}

/// Диск NVMe.
pub struct Nvme {
    regs: VirtAddr,
    /// Шаг между звонками, уже в байтах.
    doorbell_stride: usize,
    admin: QueuePair,
    io: QueuePair,
    /// Буфер данных: через него проходят все чтения и записи.
    data: DmaBuffer,
    /// Список физических адресов страниц буфера данных.
    ///
    /// Считается один раз при подключении, а не на каждую команду: страницы
    /// буфера не переезжают, потому что окно DMA не размапливается никогда
    /// (фаза 25).
    prp_list: DmaBuffer,
    blocks: u64,
    /// Размер блока, объявленный самим пространством имён.
    block_size: usize,
    /// Номер команды. Растёт, чтобы ответ можно было сопоставить с запросом.
    next_id: u16,
}

/// Найти контроллеры NVMe и поднять диски за ними.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn probe(root: &pci::Root) -> Vec<Nvme> {
    let mut disks = Vec::new();

    // SAFETY: контракт функции.
    let Some(device) =
        (unsafe { pci::find_by_class(root, CLASS_STORAGE, SUBCLASS_NVM, PROG_IF_NVME) })
    else {
        return disks;
    };

    // SAFETY: контракт функции.
    match unsafe { Nvme::attach(&device) } {
        Ok(disk) => {
            kprintln!(
                "  nvme        : {} blocks of {} B ({} MiB)",
                disk.blocks,
                SECTOR_SIZE,
                disk.blocks * SECTOR_SIZE as u64 / (1024 * 1024),
            );
            disks.push(disk);
        }
        Err(err) => kprintln!("  nvme        : controller found but unusable: {err}"),
    }
    disks
}

impl Nvme {
    /// Поднять контроллер и опознать его первое пространство имён.
    ///
    /// # Safety
    ///
    /// См. [`probe`].
    unsafe fn attach(device: &Device) -> Result<Self, NvmeError> {
        // Та же причина, что у virtio-blk и AHCI: при сброшенном бите Memory
        // Space чтения регистров возвращают все единицы, а записи пропадают.
        //
        // SAFETY: обращаться к памяти контроллер начнёт только после того, как
        // ему сообщат адреса очередей и включат его.
        unsafe { device.enable_bus_master() };

        let bar = device.memory_bar(0).ok_or(NvmeError::BadBar)?;
        // SAFETY: контракт функции.
        let regs = unsafe { map_window(bar) }?;

        // SAFETY: окно отображено, смещения — из спецификации NVMe.
        let (cap, version) = unsafe { (read64(regs, REG_CAP), read32(regs, REG_VS)) };
        let stride = 4usize << ((cap >> CAP_DSTRD_SHIFT) & CAP_DSTRD_MASK);
        let max_depth = ((cap & CAP_MQES_MASK) + 1) as u16;
        if max_depth < QUEUE_DEPTH {
            return Err(NvmeError::Unsupported);
        }
        // Сколько контроллеру дано на запуск, он говорит сам; удваивается,
        // потому что под эмуляцией отладочная сборка успевает не всё, а
        // ложный отказ здесь означает систему без корня.
        let ready_timeout = (((cap >> CAP_TO_SHIFT) & CAP_TO_MASK).max(1)) * 500 * 2;

        kprintln!(
            "  nvme        : version {}.{}.{}, doorbell stride {} B, queue depth up to {}",
            version >> 16,
            (version >> 8) & 0xFF,
            version & 0xFF,
            stride,
            max_depth,
        );

        // Выключение перед настройкой: состояние, в котором контроллер оставила
        // прошивка, — не наше дело. Она могла им пользоваться (грузилась же
        // как-то), могла не тронуть вовсе. Сброс приводит обе истории к одной.
        //
        // SAFETY: см. выше.
        unsafe {
            let cc = read32(regs, REG_CC);
            if cc & CC_ENABLE != 0 {
                write32(regs, REG_CC, cc & !CC_ENABLE);
            }
            wait_ready(regs, false, ready_timeout)?;
        }

        let admin = QueuePair::new(0)?;
        let io = QueuePair::new(1)?;
        let data = dma::alloc(MAX_TRANSFER).map_err(NvmeError::NoMemory)?;
        let prp_list = dma::alloc(MAX_TRANSFER / PAGE_SIZE * 8).map_err(NvmeError::NoMemory)?;

        // Список PRP: адреса страниц буфера данных, начиная со второй. Первая
        // передаётся отдельным полем, поэтому в списке её нет.
        //
        // SAFETY: список выделен под столько адресов, сколько страниц в буфере.
        unsafe {
            let entries = prp_list.as_ptr::<u64>();
            for page in 1..MAX_TRANSFER / PAGE_SIZE {
                entries
                    .add(page - 1)
                    .write_volatile(data.phys().as_u64() + (page * PAGE_SIZE) as u64);
            }
        }

        // SAFETY: окно отображено; очереди выделены и обнулены аллокатором.
        unsafe {
            let sizes = u32::from(QUEUE_DEPTH - 1) | (u32::from(QUEUE_DEPTH - 1) << 16);
            write32(regs, REG_AQA, sizes);
            write64(regs, REG_ASQ, admin.sq.phys().as_u64());
            write64(regs, REG_ACQ, admin.cq.phys().as_u64());

            // Страница 4 КиБ (MPS=0), набор команд NVM (CSS=0), записи очередей
            // штатного размера: 2^6 = 64 байта команда, 2^4 = 16 байт ответ.
            let cc = CC_ENABLE | (6 << 16) | (4 << 20);
            write32(regs, REG_CC, cc);
            wait_ready(regs, true, ready_timeout)?;
        }

        let mut disk = Self {
            regs,
            doorbell_stride: stride,
            admin,
            io,
            data,
            prp_list,
            blocks: 0,
            block_size: SECTOR_SIZE,
            next_id: 0,
        };

        // Порядок обязателен: очередь завершения создаётся до очереди отправки,
        // потому что вторая ссылается на первую по номеру. Контроллер откажет,
        // если сослаться на несуществующую.
        disk.create_io_completion_queue()?;
        disk.create_io_submission_queue()?;
        disk.identify_namespace()?;

        Ok(disk)
    }

    /// Ёмкость в блоках.
    #[must_use]
    pub const fn blocks(&self) -> u64 {
        self.blocks
    }

    /// Создать очередь завершения ввода-вывода.
    fn create_io_completion_queue(&mut self) -> Result<(), NvmeError> {
        let mut command = [0u32; 16];
        command[0] = u32::from(ADMIN_CREATE_CQ);
        command[6] = self.io.cq.phys().as_u64() as u32;
        command[7] = (self.io.cq.phys().as_u64() >> 32) as u32;
        command[10] = u32::from(self.io.id) | (u32::from(QUEUE_DEPTH - 1) << 16);
        // Бит 0 — очередь лежит в непрерывной памяти. Она и лежит: буфер взят
        // из окна DMA, которое непрерывно целиком. Прерывания не включаем.
        command[11] = 1;
        self.submit_admin(&command).map(|_| ())
    }

    /// Создать очередь отправки ввода-вывода.
    fn create_io_submission_queue(&mut self) -> Result<(), NvmeError> {
        let mut command = [0u32; 16];
        command[0] = u32::from(ADMIN_CREATE_SQ);
        command[6] = self.io.sq.phys().as_u64() as u32;
        command[7] = (self.io.sq.phys().as_u64() >> 32) as u32;
        command[10] = u32::from(self.io.id) | (u32::from(QUEUE_DEPTH - 1) << 16);
        // Бит 0 — непрерывная память; старшая половина — номер очереди
        // завершения, куда складывать ответы.
        command[11] = 1 | (u32::from(self.io.id) << 16);
        self.submit_admin(&command).map(|_| ())
    }

    /// Спросить контроллер про пространство имён 1.
    ///
    /// Нужны две вещи: сколько в нём блоков и какого они размера. Размер лежит
    /// не числом, а ссылкой: в `FLBAS` — номер используемого формата, в таблице
    /// форматов — двоичный логарифм размера блока. Прочитать первый формат
    /// вместо используемого — обычная ошибка, и она даёт правдоподобное, но
    /// неверное число на любом диске, отформатированном не по умолчанию.
    fn identify_namespace(&mut self) -> Result<(), NvmeError> {
        let mut command = [0u32; 16];
        command[0] = u32::from(ADMIN_IDENTIFY);
        command[1] = NSID;
        command[6] = self.data.phys().as_u64() as u32;
        command[7] = (self.data.phys().as_u64() >> 32) as u32;
        command[10] = IDENTIFY_NAMESPACE;
        self.submit_admin(&command)?;

        // SAFETY: буфер выделен на MAX_TRANSFER, читаются первые 128 байт.
        let (blocks, flbas, formats) = unsafe {
            let base = self.data.as_ptr::<u8>();
            let blocks = base.cast::<u64>().read_volatile();
            let flbas = base.add(26).read_volatile();
            let mut formats = [0u32; 16];
            for (index, format) in formats.iter_mut().enumerate() {
                *format = base.add(128 + index * 4).cast::<u32>().read_volatile();
            }
            (blocks, flbas, formats)
        };

        if blocks == 0 {
            return Err(NvmeError::BadNamespace);
        }
        let format = formats[usize::from(flbas & 0x0F)];
        // Биты 16–23 — двоичный логарифм размера блока.
        let block_size = 1u32 << ((format >> 16) & 0xFF);
        // Размер блока принимается таким, каким его назвал диск. До Phase 26c
        // здесь стоял отказ на всём, кроме 512, и он был честен: выше по стеку
        // размер сектора был константой, так что 4Kn-диск пришлось бы разметить
        // как 512-байтный — то есть потерять на нём данные. Теперь разметка
        // считается в секторах носителя, и отвергать остаётся только то, с чем
        // не работает арифметика выравнивания.
        if !disk::sector_size_supported(block_size) {
            return Err(NvmeError::UnsupportedSectorSize(block_size));
        }

        self.blocks = blocks;
        self.block_size = block_size as usize;
        Ok(())
    }

    /// Отправить административную команду и дождаться ответа.
    fn submit_admin(&mut self, command: &[u32; 16]) -> Result<u32, NvmeError> {
        // SAFETY: очередь и окно регистров живут столько же, сколько диск.
        unsafe { submit(self.regs, self.doorbell_stride, &mut self.admin, command, &mut self.next_id) }
    }

    /// Отправить команду ввода-вывода и дождаться ответа.
    fn submit_io(&mut self, command: &[u32; 16]) -> Result<u32, NvmeError> {
        // SAFETY: см. выше.
        unsafe { submit(self.regs, self.doorbell_stride, &mut self.io, command, &mut self.next_id) }
    }

    /// Заполнить поля PRP под передачу `bytes` байт из буфера данных.
    fn set_prp(&self, command: &mut [u32; 16], bytes: usize) {
        let first = self.data.phys().as_u64();
        command[6] = first as u32;
        command[7] = (first >> 32) as u32;

        let second = if bytes <= PAGE_SIZE {
            // Одна страница: второе поле не используется вовсе.
            0
        } else if bytes <= 2 * PAGE_SIZE {
            // Две страницы: второе поле — адрес второй, без всякого списка.
            first + PAGE_SIZE as u64
        } else {
            // Больше двух: второе поле указывает на список остальных адресов.
            self.prp_list.phys().as_u64()
        };
        command[8] = second as u32;
        command[9] = (second >> 32) as u32;
    }

    /// Прочитать блоки в буфер вызывающего.
    pub fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), NvmeError> {
        if buf.is_empty() || buf.len() % self.block_size != 0 {
            return Err(NvmeError::BadTransfer);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let chunk = (buf.len() - done).min(MAX_TRANSFER);
            self.transfer(IO_READ, lba + (done / self.block_size) as u64, chunk)?;
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

    /// Записать блоки.
    pub fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), NvmeError> {
        if buf.is_empty() || buf.len() % self.block_size != 0 {
            return Err(NvmeError::BadTransfer);
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
            self.transfer(IO_WRITE, lba + (done / self.block_size) as u64, chunk)?;
            done += chunk;
        }
        Ok(())
    }

    /// Одна команда чтения или записи.
    fn transfer(&mut self, opcode: u8, lba: u64, bytes: usize) -> Result<(), NvmeError> {
        let mut command = [0u32; 16];
        command[0] = u32::from(opcode);
        command[1] = NSID;
        self.set_prp(&mut command, bytes);
        command[10] = lba as u32;
        command[11] = (lba >> 32) as u32;
        // Число блоков хранится «на единицу меньше»: ноль означает один блок, а
        // не пустую передачу.
        command[12] = (bytes / self.block_size - 1) as u32;
        self.submit_io(&command).map(|_| ())
    }

    /// Довести записанное до носителя.
    pub fn flush_volatile_cache(&mut self) -> Result<(), NvmeError> {
        let mut command = [0u32; 16];
        command[0] = u32::from(IO_FLUSH);
        command[1] = NSID;
        self.submit_io(&command).map(|_| ())
    }
}

/// Положить команду в очередь, позвонить и дождаться ответа.
///
/// Вынесено из `impl`, потому что берёт очередь по `&mut` и одновременно читает
/// регистры: методу пришлось бы занимать `self` целиком, а очередей две.
///
/// # Safety
///
/// `regs` должен указывать на отображённое окно регистров, а очередь —
/// принадлежать этому же контроллеру.
unsafe fn submit(
    regs: VirtAddr,
    stride: usize,
    queue: &mut QueuePair,
    command: &[u32; 16],
    next_id: &mut u16,
) -> Result<u32, NvmeError> {
    let id = *next_id;
    *next_id = next_id.wrapping_add(1);

    let slot = usize::from(queue.sq_tail);
    // SAFETY: очередь выделена на QUEUE_DEPTH записей по 64 байта.
    unsafe {
        let entry = queue.sq.as_ptr::<u32>().add(slot * SQ_ENTRY / 4);
        // Номер команды живёт в старшей половине первого слова рядом с кодом
        // операции — по нему ответ сопоставляется с запросом.
        entry.write_volatile(command[0] | (u32::from(id) << 16));
        for (index, word) in command.iter().enumerate().skip(1) {
            entry.add(index).write_volatile(*word);
        }
    }

    // Команда записана целиком до того, как контроллеру о ней сообщат.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

    queue.sq_tail = (queue.sq_tail + 1) % QUEUE_DEPTH;
    // SAFETY: контракт функции; смещение звонка считается по номеру очереди.
    unsafe {
        write32(
            regs,
            DOORBELL_BASE + (2 * usize::from(queue.id)) * stride,
            u32::from(queue.sq_tail),
        );
    }

    // SAFETY: контракт функции.
    let status = unsafe { wait_completion(regs, stride, queue, id) }?;
    Ok(status)
}

/// Дождаться ответа с нужным номером команды.
///
/// # Safety
///
/// См. [`submit`].
unsafe fn wait_completion(
    regs: VirtAddr,
    stride: usize,
    queue: &mut QueuePair,
    id: u16,
) -> Result<u32, NvmeError> {
    let deadline = time::uptime_ms() + COMMAND_TIMEOUT_MS;
    loop {
        let slot = usize::from(queue.cq_head);
        // SAFETY: очередь выделена на QUEUE_DEPTH записей по 16 байт.
        let (dw0, dw3) = unsafe {
            let entry = queue.cq.as_ptr::<u32>().add(slot * CQ_ENTRY / 4);
            (entry.read_volatile(), entry.add(3).read_volatile())
        };

        let phase = dw3 & (1 << 16) != 0;
        if phase == queue.phase {
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

            queue.cq_head = (queue.cq_head + 1) % QUEUE_DEPTH;
            if queue.cq_head == 0 {
                // Круг пройден: то, что было «новым», станет старым.
                queue.phase = !queue.phase;
            }
            // SAFETY: контракт функции.
            unsafe {
                write32(
                    regs,
                    DOORBELL_BASE + (2 * usize::from(queue.id) + 1) * stride,
                    u32::from(queue.cq_head),
                );
            }

            let answered = (dw3 & 0xFFFF) as u16;
            let status = ((dw3 >> 17) & 0x7FFF) as u16;
            if status != 0 {
                return Err(NvmeError::Failed(status));
            }
            if answered != id {
                // Очередь одна и команда в ней одна, поэтому чужой номер здесь
                // означает не «ответ на другую команду», а рассогласование с
                // контроллером — то есть повод остановиться, а не продолжать.
                return Err(NvmeError::Failed(answered));
            }
            return Ok(dw0);
        }

        // SAFETY: контракт функции.
        if unsafe { read32(regs, REG_CSTS) } & CSTS_CFS != 0 {
            return Err(NvmeError::ControllerFailure);
        }
        if time::uptime_ms() >= deadline {
            return Err(NvmeError::Timeout);
        }
        core::hint::spin_loop();
    }
}

/// Дождаться, пока `CSTS.RDY` примет нужное значение.
///
/// # Safety
///
/// Окно регистров должно быть отображено.
unsafe fn wait_ready(regs: VirtAddr, want: bool, timeout_ms: u64) -> Result<(), NvmeError> {
    let deadline = time::uptime_ms() + timeout_ms;
    loop {
        // SAFETY: контракт функции.
        let csts = unsafe { read32(regs, REG_CSTS) };
        if csts & CSTS_CFS != 0 {
            return Err(NvmeError::ControllerFailure);
        }
        if (csts & CSTS_RDY != 0) == want {
            return Ok(());
        }
        if time::uptime_ms() >= deadline {
            return Err(NvmeError::NotReady);
        }
        core::hint::spin_loop();
    }
}

/// Отобразить окно регистров.
///
/// Отображается одна страница сверх области звонков: звонков столько, сколько
/// очередей, а очередей у нас две.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
unsafe fn map_window(phys: PhysAddr) -> Result<VirtAddr, NvmeError> {
    let span = DOORBELL_BASE + PAGE_SIZE;
    let virt = phys.to_direct_map();
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;
    // SAFETY: условия делегированы вызывающему. Регистры устройства требуют
    // семантики `DEVICE`: иначе запись в звонок случится когда-нибудь потом.
    unsafe { crate::arch::map_active(virt, phys, span, flags) }.map_err(|_| NvmeError::MapFailed)?;
    Ok(virt)
}

// --- обращения к регистрам ---------------------------------------------------

/// # Safety
///
/// Адрес должен указывать в отображённое окно регистров.
unsafe fn read32(base: VirtAddr, offset: usize) -> u32 {
    // SAFETY: контракт функции. `volatile` обязателен: это регистры.
    unsafe { (base.as_usize() as *const u8).add(offset).cast::<u32>().read_volatile() }
}

/// # Safety
///
/// См. [`read32`].
unsafe fn read64(base: VirtAddr, offset: usize) -> u64 {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *const u8).add(offset).cast::<u64>().read_volatile() }
}

/// # Safety
///
/// См. [`read32`]. Запись меняет состояние устройства.
unsafe fn write32(base: VirtAddr, offset: usize, value: u32) {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *mut u8).add(offset).cast::<u32>().write_volatile(value) };
}

/// # Safety
///
/// См. [`write32`].
unsafe fn write64(base: VirtAddr, offset: usize, value: u64) {
    // SAFETY: контракт функции. Спецификация допускает запись 64-битных
    // регистров двумя словами, но целиком — проще и разрешено везде, где
    // контроллер сидит на шине с 64-битным доступом; это все машины, на которых
    // существует NVMe.
    unsafe { (base.as_usize() as *mut u8).add(offset).cast::<u64>().write_volatile(value) };
}

/// Мост к крейту `disk`: тот же трейт, что у virtio-blk и AHCI.
impl disk::BlockDevice for Nvme {
    fn sector_size(&self) -> u32 {
        self.block_size as u32
    }

    fn sector_count(&self) -> u64 {
        self.blocks
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        if lba + (buf.len() / self.block_size) as u64 > self.blocks {
            return Err(disk::Error::OutOfRange);
        }
        self.read_blocks(lba, buf).map_err(|err| {
            kprintln!("nvme: read at LBA {lba} failed: {err}");
            disk::Error::Io
        })
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        if lba + (buf.len() / self.block_size) as u64 > self.blocks {
            return Err(disk::Error::OutOfRange);
        }
        self.write_blocks(lba, buf).map_err(|err| {
            kprintln!("nvme: write at LBA {lba} failed: {err}");
            disk::Error::Io
        })
    }

    fn flush(&mut self) -> disk::Result<()> {
        self.flush_volatile_cache().map_err(|err| {
            kprintln!("nvme: flush failed: {err}");
            disk::Error::Io
        })
    }
}

impl Nvme {
    pub fn into_block_device(self) -> Box<dyn disk::BlockDevice + Send> {
        Box::new(self)
    }
}

// SAFETY: структура владеет своими очередями и буферами; окно регистров
// отображено на всё время жизни ядра. Одновременный доступ исключён замком выше
// по стеку — очередь одна, и две команды в неё не положить.
unsafe impl Send for Nvme {}
