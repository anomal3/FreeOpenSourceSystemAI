//! AHCI: диск, подключённый по SATA.
//!
//! Зачем он нужен, если есть virtio-blk. Затем, что virtio-blk есть только там,
//! где кто-то согласился его предоставить: в QEMU он есть, в VirtualBox с
//! настройками по умолчанию — нет, а в ноутбуке нет и подавно. Пока в ядре был
//! один дисковый драйвер, установка в чужой гипервизор была дорогой в один
//! конец: установщик пишет диск через Block I/O прошивки и справляется, а
//! установленная система своего корня не находит. AHCI — первый из двух
//! драйверов, которые это закрывают (второй, NVMe, — фаза 26b).
//!
//! # Что такое AHCI в трёх абзацах
//!
//! Контроллер — обычное устройство PCI класса 0x01/0x06/0x01, у которого пятый
//! BAR указывает на окно регистров (ABAR). В окне сначала общие регистры
//! контроллера, а с адреса 0x100 — по 128 байт на каждый из портов; порт — это
//! разъём, в который воткнут (или не воткнут) диск. Какие порты вообще
//! существуют, сказано в маске `PI`; есть ли за портом диск — в `PxSSTS`.
//!
//! Команда передаётся не через регистры, а через память. У порта есть список
//! команд (32 заголовка по 32 байта) и область приёма ответов; заголовок
//! указывает на таблицу команды, в которой лежит FIS — двадцать байт, где
//! записаны код команды ATA, номер сектора и число секторов, — и таблица
//! PRDT, описывающая, куда положить данные. Запуск команды — установка бита её
//! слота в `PxCI`; завершение — сброс этого бита самим контроллером.
//!
//! Отсюда три требования, которые нельзя нарушить: все эти структуры должны
//! быть видны устройству (то есть жить в DMA-окне, а не в куче), выровнены
//! (список команд — на 1 КиБ, область приёма — на 256 байт, таблица — на 128),
//! и записаны до того, как будет взведён бит в `PxCI`. Выравнивание даётся
//! даром: [`crate::mm::dma::alloc`] отдаёт буферы, выровненные на страницу.
//!
//! # Чего здесь нет
//!
//! Опроса завершения по прерыванию: команда ожидается циклом с ограничением по
//! времени, ровно как в virtio-blk. Это сознательно оставлено на потом —
//! прерывание от контроллера не сделает чтение быстрее, а первым делом нужен
//! путь, по которому система вообще находит свой корень. Ограничение по времени
//! при этом настоящее, по монотонному счётчику из фазы 17: отказавший диск
//! обязан привести к сообщению, а не к молча остановившемуся ядру.
//!
//! Одновременных команд тоже нет: используется слот 0 и только он. Очередь на
//! 32 команды имеет смысл там, где есть кому их выдавать; сегодня выше по стеку
//! стоит одно чтение за другим.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{PageFlags, PhysAddr, VirtAddr};
use crate::pci::{self, Device};
use crate::{kprintln, time};

/// Класс устройства PCI: контроллер запоминающих устройств.
const CLASS_STORAGE: u8 = 0x01;
/// Подкласс: SATA.
const SUBCLASS_SATA: u8 = 0x06;
/// Интерфейс: AHCI 1.0. Тот же контроллер, переключённый в режим совместимости
/// с IDE, объявляет другой `prog_if` — и это другой драйвер, которого здесь нет.
const PROG_IF_AHCI: u8 = 0x01;

// --- общие регистры контроллера ---------------------------------------------

/// Возможности контроллера.
const HBA_CAP: usize = 0x00;
/// Общее управление.
const HBA_GHC: usize = 0x04;
/// Маска существующих портов.
const HBA_PI: usize = 0x0C;
/// Версия.
const HBA_VS: usize = 0x10;

/// `GHC.AE` — включить режим AHCI. Без него контроллер притворяется IDE.
const GHC_AE: u32 = 1 << 31;

/// `CAP.S64A` — контроллер умеет 64-битные адреса.
const CAP_S64A: u32 = 1 << 31;
/// `CAP.NP` — число портов минус один, младшие пять бит.
const CAP_NP_MASK: u32 = 0x1F;

/// Начало области портов и шаг между ними.
const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;

/// Сколько портов может быть у контроллера по спецификации.
const MAX_PORTS: usize = 32;

// --- регистры порта ----------------------------------------------------------

const PX_CLB: usize = 0x00;
const PX_CLBU: usize = 0x04;
const PX_FB: usize = 0x08;
const PX_FBU: usize = 0x0C;
const PX_IS: usize = 0x10;
const PX_IE: usize = 0x14;
const PX_CMD: usize = 0x18;
const PX_TFD: usize = 0x20;
const PX_SIG: usize = 0x24;
const PX_SSTS: usize = 0x28;
const PX_SCTL: usize = 0x2C;
const PX_SERR: usize = 0x30;
const PX_CI: usize = 0x38;

/// `PxCMD.ST` — обрабатывать команды.
const CMD_ST: u32 = 1 << 0;
/// `PxCMD.FRE` — принимать ответы устройства.
const CMD_FRE: u32 = 1 << 4;
/// `PxCMD.FR` — приём ответов действительно идёт.
const CMD_FR: u32 = 1 << 14;
/// `PxCMD.CR` — обработка команд действительно идёт.
const CMD_CR: u32 = 1 << 15;

/// `PxTFD.STS.BSY` — устройство занято.
const TFD_BSY: u32 = 1 << 7;
/// `PxTFD.STS.DRQ` — устройство ждёт передачи данных.
const TFD_DRQ: u32 = 1 << 3;
/// `PxTFD.STS.ERR` — последняя команда закончилась ошибкой.
const TFD_ERR: u32 = 1 << 0;

/// `PxIS.TFES` — ошибка в регистре состояния задачи.
const IS_TFES: u32 = 1 << 30;

/// `PxSSTS.DET` — что физически происходит на линии. Три означает «устройство
/// есть, связь установлена»; единица — «что-то есть, но связи нет», и это не то
/// же самое, что пустой разъём.
const SSTS_DET_MASK: u32 = 0x0F;
const SSTS_DET_PRESENT: u32 = 0x03;
/// `PxSSTS.IPM` — состояние управления питанием. Единица означает «активно».
const SSTS_IPM_SHIFT: u32 = 8;
const SSTS_IPM_MASK: u32 = 0x0F;
const SSTS_IPM_ACTIVE: u32 = 0x01;

/// Подпись обычного диска ATA в `PxSIG`. У ATAPI (привод CD) она другая, и
/// драйвера для него здесь нет: команды у него свои, через пакет SCSI.
const SIG_ATA: u32 = 0x0000_0101;

/// `PxSCTL.DET` — чем управляем физической линией. Единица означает «держать
/// сброс», ноль — «работать».
const SCTL_DET_MASK: u32 = 0x0F;
const SCTL_DET_RESET: u32 = 0x01;

// --- команды ATA -------------------------------------------------------------

/// Опознание устройства: 512 байт о том, что это за диск.
const ATA_IDENTIFY: u8 = 0xEC;
/// Чтение с 48-битным адресом.
const ATA_READ_DMA_EXT: u8 = 0x25;
/// Запись с 48-битным адресом.
const ATA_WRITE_DMA_EXT: u8 = 0x35;
/// Сбросить кеш записи на носитель.
const ATA_FLUSH_CACHE_EXT: u8 = 0xEA;

/// Тип FIS «регистры, от хоста к устройству».
const FIS_TYPE_H2D: u8 = 0x27;
/// Бит в байте 1 FIS: это команда, а не обновление регистров.
const FIS_H2D_COMMAND: u8 = 1 << 7;

/// Сколько байт возвращает команда IDENTIFY — всегда 512, независимо от того,
/// какие сектора у диска: это размер ответа, а не носителя.
const IDENTIFY_BYTES: usize = 512;

/// Размер сектора по умолчанию, пока диск не сказал иного. Диски с 4096 существуют, и молча
/// считать их 512-байтными — способ потерять данные; такой диск отвергается с
/// сообщением.
pub const SECTOR_SIZE: usize = 512;

/// Наибольшая передача за одну команду. Столько же, сколько у virtio-blk, и по
/// той же причине: буфер выделяется один раз на всё время работы.
const MAX_TRANSFER: usize = 64 * 1024;

/// Сколько ждать, пока порт остановится или запустится.
const PORT_TIMEOUT_MS: u64 = 1_000;
/// Сколько ждать выполнения команды. Секунды, а не миллисекунды: под TCG
/// эмулируемый диск бывает медленным, а ложный отказ здесь означает систему без
/// корня.
const COMMAND_TIMEOUT_MS: u64 = 10_000;

/// Что могло пойти не так.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AhciError {
    /// BAR5 пуст или описывает пространство ввода-вывода.
    BadBar,
    /// Не удалось отобразить окно регистров.
    MapFailed,
    /// Нет памяти под структуры команд.
    NoMemory(DmaError),
    /// Порт не остановился или не запустился за отведённое время.
    PortTimeout,
    /// Команда не завершилась за отведённое время.
    CommandTimeout,
    /// Устройство сообщило об ошибке: значение `PxTFD`.
    Failed(u32),
    /// За портом не обычный диск: значение `PxSIG`.
    NotAta(u32),
    /// Диск отвечает, но говорит о себе несуразицу.
    BadIdentify,
    /// Сектор не 512 байт.
    UnsupportedSectorSize(u32),
    /// Запрос не кратен сектору или пуст.
    BadTransfer,
}

impl core::fmt::Display for AhciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadBar => write!(f, "BAR5 is not a memory window"),
            Self::MapFailed => write!(f, "cannot map the register window"),
            Self::NoMemory(err) => write!(f, "no DMA memory: {err}"),
            Self::PortTimeout => write!(f, "the port did not settle in time"),
            Self::CommandTimeout => write!(f, "the command did not finish in time"),
            Self::Failed(tfd) => write!(f, "the device reported an error, PxTFD {tfd:#010x}"),
            Self::NotAta(sig) => write!(f, "not a plain ATA disk (signature {sig:#010x})"),
            Self::BadIdentify => write!(f, "IDENTIFY returned nothing usable"),
            Self::UnsupportedSectorSize(size) => write!(f, "sector size {size} is not supported"),
            Self::BadTransfer => write!(f, "transfer is empty or not a whole number of sectors"),
        }
    }
}

/// Диск за одним портом контроллера.
///
/// Портов у контроллера до 32, и каждый — самостоятельный носитель со своим
/// списком команд: общего состояния между ними нет, поэтому нет и общей
/// структуры контроллера, которую пришлось бы держать под замком. Окно
/// регистров отображено один раз на весь контроллер, а порт помнит адрес своей
/// части.
pub struct AhciDisk {
    /// Регистры порта.
    port: VirtAddr,
    /// Список команд: 32 заголовка по 32 байта. Используется слот 0.
    ///
    /// Буферы хранятся не потому, что нужны их адреса — те уже записаны в
    /// регистры порта, — а потому, что освобождение буфера, на который смотрит
    /// работающее устройство, отдало бы его следующему просителю. Пока живёт
    /// диск, живут и они.
    #[allow(dead_code)]
    command_list: DmaBuffer,
    /// Область, куда контроллер складывает ответы устройства.
    #[allow(dead_code)]
    received_fis: DmaBuffer,
    /// Таблица команды слота 0: FIS и один элемент PRDT.
    table: DmaBuffer,
    /// Буфер данных, через который проходят все чтения и записи.
    data: DmaBuffer,
    /// Номер порта — он же имя диска в журнале.
    port_index: usize,
    sectors: u64,
    /// Размер логического сектора, объявленный самим диском.
    sector_size: usize,
}

/// Найти контроллеры AHCI и поднять все диски, какие за ними есть.
///
/// Возвращает пустой вектор, если контроллера нет: машина без SATA — это
/// обычная машина, а не отказ.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn probe(root: &pci::Root) -> Vec<AhciDisk> {
    let mut disks = Vec::new();

    // SAFETY: контракт функции.
    let Some(device) = (unsafe {
        pci::find_by_class(root, CLASS_STORAGE, SUBCLASS_SATA, PROG_IF_AHCI)
    }) else {
        return disks;
    };

    // SAFETY: контракт функции.
    match unsafe { attach(&device, &mut disks) } {
        Ok(()) => {}
        Err(err) => kprintln!("  ahci        : controller found but unusable: {err}"),
    }
    disks
}

/// Поднять контроллер и все его порты с дисками.
///
/// # Safety
///
/// См. [`probe`].
unsafe fn attach(device: &Device, disks: &mut Vec<AhciDisk>) -> Result<(), AhciError> {
    // Тот же порядок, что у virtio-blk, и по той же причине: при сброшенном
    // бите Memory Space чтения регистров возвращают все единицы, а записи
    // пропадают — драйвер выглядит работающим ровно до момента, когда данные не
    // приходят. Прошивка ArmVirtQemu оставляет бит сброшенным, OVMF нет.
    //
    // SAFETY: bus master безопасен до того, как контроллеру сообщены адреса
    // структур и запущен порт; ниже это делается по порядку.
    unsafe { device.enable_bus_master() };

    let abar = device.memory_bar(5).ok_or(AhciError::BadBar)?;
    // SAFETY: контракт функции; окно регистров отображается как память
    // устройства, см. `map_window`.
    let base = unsafe { map_window(abar) }?;

    // SAFETY: окно отображено выше, смещения — из спецификации AHCI.
    let (cap, version, ports) = unsafe { (read32(base, HBA_CAP), read32(base, HBA_VS), read32(base, HBA_PI)) };

    // Включить режим AHCI обязательно **до** чтения чего-либо про порты: пока
    // `AE` сброшен, контроллер вправе показывать регистры совместимости с IDE,
    // и разбирать их как AHCI — читать мусор с уверенным видом.
    //
    // SAFETY: см. выше.
    unsafe {
        let ghc = read32(base, HBA_GHC);
        if ghc & GHC_AE == 0 {
            write32(base, HBA_GHC, ghc | GHC_AE);
        }
    }

    kprintln!(
        "  ahci        : version {}.{}, {} port(s), ports mask {:#010x}{}",
        version >> 16,
        (version >> 8) & 0xFF,
        (cap & CAP_NP_MASK) + 1,
        ports,
        if cap & CAP_S64A == 0 { ", 32-bit addressing only" } else { "" },
    );

    for index in 0..MAX_PORTS {
        if ports & (1 << index) == 0 {
            continue;
        }
        let port = VirtAddr::new(base.as_usize() + PORT_BASE + index * PORT_STRIDE);

        // SAFETY: окно отображено, смещение порта внутри него — маска `PI` не
        // может назвать порт за пределами 32.
        let ssts = unsafe { read32(port, PX_SSTS) };
        let det = ssts & SSTS_DET_MASK;
        let ipm = (ssts >> SSTS_IPM_SHIFT) & SSTS_IPM_MASK;
        if det != SSTS_DET_PRESENT || ipm != SSTS_IPM_ACTIVE {
            continue;
        }

        // SAFETY: контракт функции.
        match unsafe { AhciDisk::start(port, index) } {
            Ok(disk) => {
                kprintln!(
                    "  ahci        : port {index}: {} sectors of {} B ({} MiB)",
                    disk.sectors,
                    disk.sector_size,
                    disk.sectors * disk.sector_size as u64 / (1024 * 1024),
                );
                disks.push(disk);
            }
            Err(err) => kprintln!("  ahci        : port {index}: {err}"),
        }
    }

    Ok(())
}

/// Отобразить окно регистров контроллера.
///
/// Отображается фиксированный кусок: общие регистры плюс область всех 32
/// портов. Спрашивать у устройства размер BAR нельзя без записи в него (см.
/// [`pci::Device::memory_bar`]), а больше, чем описано спецификацией, там всё
/// равно ничего нет.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
unsafe fn map_window(phys: PhysAddr) -> Result<VirtAddr, AhciError> {
    let span = (PORT_BASE + MAX_PORTS * PORT_STRIDE).next_multiple_of(crate::mm::PAGE_SIZE);
    let virt = phys.to_direct_map();
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;
    // SAFETY: условия делегированы вызывающему. Это регистры устройства:
    // семантика `DEVICE` обязательна, иначе кеш и переупорядочивание превратят
    // запись в `PxCI` в запись, которая случится когда-нибудь потом.
    unsafe { crate::arch::map_active(virt, phys, span, flags) }
        .map_err(|_| AhciError::MapFailed)?;
    Ok(virt)
}

impl AhciDisk {
    /// Подготовить порт и опознать диск за ним.
    ///
    /// # Safety
    ///
    /// `port` должен указывать на отображённое окно регистров порта.
    unsafe fn start(port: VirtAddr, port_index: usize) -> Result<Self, AhciError> {
        // SAFETY: контракт функции.
        unsafe { stop(port) }?;

        let command_list = dma::alloc(32 * 32).map_err(AhciError::NoMemory)?;
        let received_fis = dma::alloc(256).map_err(AhciError::NoMemory)?;
        let table = dma::alloc(0x80 + 16).map_err(AhciError::NoMemory)?;
        let data = dma::alloc(MAX_TRANSFER).map_err(AhciError::NoMemory)?;

        // SAFETY: контракт функции; буферы выделены и обнулены аллокатором DMA.
        unsafe {
            write32(port, PX_CLB, command_list.phys().as_u64() as u32);
            write32(port, PX_CLBU, (command_list.phys().as_u64() >> 32) as u32);
            write32(port, PX_FB, received_fis.phys().as_u64() as u32);
            write32(port, PX_FBU, (received_fis.phys().as_u64() >> 32) as u32);
            // Ошибки, накопленные прошивкой, сбрасываются записью единиц в те же
            // биты: иначе первая же проверка после команды увидит чужую ошибку и
            // объявит отказом наше исправное чтение.
            write32(port, PX_SERR, u32::MAX);
            write32(port, PX_IS, u32::MAX);
            // Прерывания не разрешаем: завершение опрашивается. Разрешить их и
            // не иметь обработчика — верный способ получить бесконечный
            // уровневый прерыватель на общей линии.
            write32(port, PX_IE, 0);
        }

        // SAFETY: контракт функции.
        unsafe { start_port(port) }?;

        // Подпись читается **после** запуска порта, и это не порядок ради
        // порядка. `PxSIG` заполняется тем, что устройство прислало первым
        // кадром после установления связи, — то есть только если область приёма
        // уже включена. На x86-64 этого не было видно: OVMF умеет SATA и
        // поднимает порт сам, так что к нашему приходу подпись уже стояла. На
        // `virt` прошивка контроллер не трогает вовсе, и порт с исправным диском
        // отвечал `0xFFFFFFFF` — «устройства нет» там, где оно есть.
        //
        // SAFETY: контракт функции.
        let sig = unsafe { read32(port, PX_SIG) };
        if sig != SIG_ATA {
            // Привод CD (ATAPI) и порт-множитель отвечают другой подписью.
            // Пропустить их — не потеря: команды у них свои, и притвориться,
            // что это диск, значило бы читать с них мусор.
            return Err(AhciError::NotAta(sig));
        }

        let mut disk = Self {
            port,
            command_list,
            received_fis,
            table,
            data,
            port_index,
            sectors: 0,
            sector_size: SECTOR_SIZE,
        };
        disk.identify()?;
        Ok(disk)
    }

    /// Номер порта, за которым стоит этот диск.
    #[must_use]
    pub const fn port_index(&self) -> usize {
        self.port_index
    }

    /// Спросить диск, что он такое.
    ///
    /// Нужны две вещи: сколько у него секторов и какого они размера. Первое
    /// лежит в словах 100–103 (48-битная ёмкость), второе приходится собирать из
    /// слова 106 и пары 117–118 — поле, которого у старых дисков нет, и тогда
    /// сектор равен 512 по умолчанию.
    fn identify(&mut self) -> Result<(), AhciError> {
        self.run(ATA_IDENTIFY, 0, 0, IDENTIFY_BYTES, false)?;

        // SAFETY: буфер выделен на MAX_TRANSFER байт, читаются первые 512.
        let words: [u16; 256] = unsafe {
            let mut words = [0u16; 256];
            let src = self.data.as_ptr::<u16>();
            for (index, word) in words.iter_mut().enumerate() {
                *word = src.add(index).read_volatile();
            }
            words
        };

        let sectors = u64::from(words[100])
            | (u64::from(words[101]) << 16)
            | (u64::from(words[102]) << 32)
            | (u64::from(words[103]) << 48);
        if sectors == 0 {
            return Err(AhciError::BadIdentify);
        }

        // Слово 106 действительно, только если его старшие два бита равны 0b01;
        // иначе поле не заполнено, и о размере сектора диск ничего не сказал.
        let sector_size = if words[106] & 0xC000 == 0x4000 && words[106] & (1 << 12) != 0 {
            (u32::from(words[117]) | (u32::from(words[118]) << 16)) * 2
        } else {
            SECTOR_SIZE as u32
        };
        // Размер сектора принимается таким, каким его назвал диск: с Phase 26c
        // вся разметка считается в секторах носителя, и подгонять 4Kn под 512
        // больше не нужно — а именно подгонка и была бы потерей данных.
        if !disk::sector_size_supported(sector_size) {
            return Err(AhciError::UnsupportedSectorSize(sector_size));
        }

        self.sectors = sectors;
        self.sector_size = sector_size as usize;
        Ok(())
    }

    /// Выполнить одну команду в слоте 0.
    ///
    /// `write` означает направление передачи: данные идут от нас к устройству.
    /// Флаг попадает в заголовок команды, и перепутать его — значит записать
    /// туда, откуда собирались читать.
    fn run(
        &mut self,
        command: u8,
        lba: u64,
        count: u16,
        bytes: usize,
        write: bool,
    ) -> Result<(), AhciError> {
        // Заголовок команды слота 0.
        //
        // SAFETY: список команд выделен на 32 заголовка; пишется нулевой.
        unsafe {
            let header = self.command_list.as_ptr::<u32>();
            // Длина FIS в двойных словах (пять), направление, и один элемент
            // PRDT в старшей половине слова.
            let mut dw0 = 5u32;
            if write {
                dw0 |= 1 << 6;
            }
            if bytes > 0 {
                dw0 |= 1 << 16;
            }
            header.write_volatile(dw0);
            // Сколько байт передано — заполняет контроллер; обнуляется, чтобы
            // прошлое значение не было принято за нынешнее.
            header.add(1).write_volatile(0);
            header.add(2).write_volatile(self.table.phys().as_u64() as u32);
            header.add(3).write_volatile((self.table.phys().as_u64() >> 32) as u32);
            for index in 4..8 {
                header.add(index).write_volatile(0);
            }
        }

        // FIS команды и элемент PRDT.
        //
        // SAFETY: таблица выделена на FIS (0x80 байт с запасом) и один элемент
        // PRDT сразу за ней.
        unsafe {
            let fis = self.table.as_ptr::<u8>();
            for index in 0..0x80usize {
                fis.add(index).write_volatile(0);
            }
            fis.write_volatile(FIS_TYPE_H2D);
            fis.add(1).write_volatile(FIS_H2D_COMMAND);
            fis.add(2).write_volatile(command);
            fis.add(4).write_volatile(lba as u8);
            fis.add(5).write_volatile((lba >> 8) as u8);
            fis.add(6).write_volatile((lba >> 16) as u8);
            // Бит 6 — адресация LBA. Без него диск поймёт номер сектора как
            // «цилиндр, головка, сектор», то есть прочитает не то, что просили.
            fis.add(7).write_volatile(0x40);
            fis.add(8).write_volatile((lba >> 24) as u8);
            fis.add(9).write_volatile((lba >> 32) as u8);
            fis.add(10).write_volatile((lba >> 40) as u8);
            fis.add(12).write_volatile(count as u8);
            fis.add(13).write_volatile((count >> 8) as u8);

            if bytes > 0 {
                let prdt = fis.add(0x80).cast::<u32>();
                prdt.write_volatile(self.data.phys().as_u64() as u32);
                prdt.add(1).write_volatile((self.data.phys().as_u64() >> 32) as u32);
                prdt.add(2).write_volatile(0);
                // Счётчик байт хранится «на единицу меньше» — ноль означает один
                // байт, а не пустую передачу.
                prdt.add(3).write_volatile((bytes as u32 - 1) & 0x003F_FFFF);
            }
        }

        // Всё, что устройство прочитает, записано; только теперь можно
        // разрешать ему читать.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // SAFETY: окно порта отображено.
        unsafe {
            // Прошлые признаки завершения снимаются до запуска, иначе первое же
            // чтение `PxIS` покажет ошибку от предыдущей команды.
            write32(self.port, PX_IS, u32::MAX);
            wait_not_busy(self.port, PORT_TIMEOUT_MS)?;
            write32(self.port, PX_CI, 1);
        }

        // SAFETY: см. выше.
        unsafe { self.wait_for_completion() }?;

        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Дождаться, пока контроллер снимет бит слота, и проверить, чем кончилось.
    ///
    /// # Safety
    ///
    /// Окно порта должно быть отображено.
    unsafe fn wait_for_completion(&self) -> Result<(), AhciError> {
        let deadline = time::uptime_ms() + COMMAND_TIMEOUT_MS;
        loop {
            // SAFETY: контракт функции.
            let (ci, is, tfd) = unsafe {
                (
                    read32(self.port, PX_CI),
                    read32(self.port, PX_IS),
                    read32(self.port, PX_TFD),
                )
            };
            // Ошибка проверяется раньше завершения: при отказе устройство может
            // и не снять бит слота, и ожидание превратилось бы в таймаут с
            // невнятным сообщением вместо конкретного «диск ответил ошибкой».
            if is & IS_TFES != 0 || tfd & TFD_ERR != 0 {
                return Err(AhciError::Failed(tfd));
            }
            if ci & 1 == 0 {
                return Ok(());
            }
            if time::uptime_ms() >= deadline {
                return Err(AhciError::CommandTimeout);
            }
            core::hint::spin_loop();
        }
    }

    /// Прочитать сектора в буфер вызывающего.
    ///
    /// Данные идут через буфер DMA, а не напрямую: срез вызывающего живёт в
    /// куче, которая не обязана быть физически непрерывной, — устройству же
    /// сообщается физический адрес. Ровно то же самое и по той же причине
    /// делает virtio-blk.
    pub fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), AhciError> {
        if buf.is_empty() || buf.len() % self.sector_size != 0 {
            return Err(AhciError::BadTransfer);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let chunk = (buf.len() - done).min(MAX_TRANSFER);
            let sectors = chunk / self.sector_size;
            self.run(
                ATA_READ_DMA_EXT,
                lba + (done / self.sector_size) as u64,
                sectors as u16,
                chunk,
                false,
            )?;
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
    pub fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), AhciError> {
        if buf.is_empty() || buf.len() % self.sector_size != 0 {
            return Err(AhciError::BadTransfer);
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
            self.run(
                ATA_WRITE_DMA_EXT,
                lba + (done / self.sector_size) as u64,
                (chunk / self.sector_size) as u16,
                chunk,
                true,
            )?;
            done += chunk;
        }
        Ok(())
    }

    /// Довести записи до носителя.
    ///
    /// В отличие от virtio-blk, где возможность сброса не согласована и
    /// устройство обязано писать немедленно, у диска ATA есть кеш записи, и
    /// команда сброса — единственное, что отделяет «записали» от «переживёт
    /// выключение питания». Для системы, которая обещает не терять данные при
    /// сбое питания, это не мелочь.
    pub fn flush_cache(&mut self) -> Result<(), AhciError> {
        self.run(ATA_FLUSH_CACHE_EXT, 0, 0, 0, false)
    }
}

/// Остановить порт: контроллер не должен работать со списком команд, пока мы
/// меняем его адрес.
///
/// # Safety
///
/// Окно порта должно быть отображено.
unsafe fn stop(port: VirtAddr) -> Result<(), AhciError> {
    // SAFETY: контракт функции.
    unsafe {
        let cmd = read32(port, PX_CMD);
        write32(port, PX_CMD, cmd & !CMD_ST);
        wait_clear(port, PX_CMD, CMD_CR, PORT_TIMEOUT_MS)?;

        let cmd = read32(port, PX_CMD);
        write32(port, PX_CMD, cmd & !CMD_FRE);
        wait_clear(port, PX_CMD, CMD_FR, PORT_TIMEOUT_MS)?;
    }
    Ok(())
}

/// Запустить порт.
///
/// Порядок обратный остановке и он обязателен: приём ответов включается до
/// всего остального, иначе кадр, которым устройство представляется после сброса
/// линии, будет некуда положить — а из него берётся подпись, по которой диск
/// отличается от привода.
///
/// # Safety
///
/// См. [`stop`].
unsafe fn start_port(port: VirtAddr) -> Result<(), AhciError> {
    // SAFETY: контракт функции.
    unsafe {
        let cmd = read32(port, PX_CMD);
        write32(port, PX_CMD, cmd | CMD_FRE);

        reset_link(port)?;

        wait_not_busy(port, PORT_TIMEOUT_MS)?;
        let cmd = read32(port, PX_CMD);
        write32(port, PX_CMD, cmd | CMD_ST);
    }
    Ok(())
}

/// Пересобрать связь с устройством (COMRESET).
///
/// Нужно потому, что состояние порта до нас — не наше дело и не наша забота.
/// Прошивка могла поднять его (OVMF умеет SATA), могла не тронуть вовсе
/// (`ArmVirtQemu` не умеет), могла оставить наполовину настроенным. Сброс линии
/// приводит порт к состоянию, которое одинаково во всех трёх случаях, и
/// заставляет устройство представиться заново — а вместе с этим появляется
/// подпись в `PxSIG`.
///
/// # Safety
///
/// См. [`stop`]. Порт должен быть остановлен: сброс линии под работающей
/// обработкой команд — это команда, потерянная на полпути.
unsafe fn reset_link(port: VirtAddr) -> Result<(), AhciError> {
    // SAFETY: контракт функции.
    unsafe {
        let sctl = read32(port, PX_SCTL);
        write32(port, PX_SCTL, (sctl & !SCTL_DET_MASK) | SCTL_DET_RESET);
        // Спецификация требует держать сигнал не меньше миллисекунды. Две — с
        // запасом, и это единственная задержка во всём драйвере, которую
        // невозможно заменить ожиданием события: события ещё нет, устройство
        // как раз о себе и не заявило.
        spin_ms(2);
        write32(port, PX_SCTL, sctl & !SCTL_DET_MASK);

        let deadline = time::uptime_ms() + PORT_TIMEOUT_MS;
        loop {
            if read32(port, PX_SSTS) & SSTS_DET_MASK == SSTS_DET_PRESENT {
                break;
            }
            if time::uptime_ms() >= deadline {
                return Err(AhciError::PortTimeout);
            }
            core::hint::spin_loop();
        }

        // Сброс сам по себе поднимает биты ошибок связи — это не ошибки, а
        // след того, что мы только что сделали. Оставить их значило бы
        // объявить отказом первое же исправное чтение.
        write32(port, PX_SERR, u32::MAX);
    }
    Ok(())
}

/// Подождать указанное число миллисекунд по монотонным часам.
fn spin_ms(ms: u64) {
    let until = time::uptime_ms() + ms;
    while time::uptime_ms() < until {
        core::hint::spin_loop();
    }
}

/// Дождаться, пока в регистре погаснут указанные биты.
///
/// # Safety
///
/// См. [`stop`].
unsafe fn wait_clear(
    port: VirtAddr,
    offset: usize,
    mask: u32,
    timeout_ms: u64,
) -> Result<(), AhciError> {
    let deadline = time::uptime_ms() + timeout_ms;
    loop {
        // SAFETY: контракт функции.
        if unsafe { read32(port, offset) } & mask == 0 {
            return Ok(());
        }
        if time::uptime_ms() >= deadline {
            return Err(AhciError::PortTimeout);
        }
        core::hint::spin_loop();
    }
}

/// Дождаться, пока устройство перестанет быть занятым.
///
/// # Safety
///
/// См. [`stop`].
unsafe fn wait_not_busy(port: VirtAddr, timeout_ms: u64) -> Result<(), AhciError> {
    // SAFETY: контракт функции.
    unsafe { wait_clear(port, PX_TFD, TFD_BSY | TFD_DRQ, timeout_ms) }
}

// --- обращения к регистрам ---------------------------------------------------

/// # Safety
///
/// Адрес должен указывать в отображённое окно регистров.
unsafe fn read32(base: VirtAddr, offset: usize) -> u32 {
    // SAFETY: контракт функции. `volatile` обязателен: это регистры, и
    // компилятор не вправе ни выбросить чтение, ни повторить его.
    unsafe { (base.as_usize() as *const u8).add(offset).cast::<u32>().read_volatile() }
}

/// # Safety
///
/// См. [`read32`]. Запись меняет состояние устройства.
unsafe fn write32(base: VirtAddr, offset: usize, value: u32) {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *mut u8).add(offset).cast::<u32>().write_volatile(value) };
}

/// Мост к крейту `disk` — тот же трейт, что у virtio-blk, у образа на хосте и у
/// носителя прошивки в установщике. Именно он делает разбор GPT и чтение ext2
/// одинаковыми независимо от того, каким проводом подключён диск.
impl disk::BlockDevice for AhciDisk {
    fn sector_size(&self) -> u32 {
        self.sector_size as u32
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        if lba + (buf.len() / self.sector_size) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        self.read_sectors(lba, buf).map_err(|err| {
            kprintln!("ahci: read at LBA {lba} failed: {err}");
            disk::Error::Io
        })
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        if lba + (buf.len() / self.sector_size) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        self.write_sectors(lba, buf).map_err(|err| {
            kprintln!("ahci: write at LBA {lba} failed: {err}");
            disk::Error::Io
        })
    }

    fn flush(&mut self) -> disk::Result<()> {
        self.flush_cache().map_err(|err| {
            kprintln!("ahci: flush failed: {err}");
            disk::Error::Io
        })
    }
}

/// Отдать диск как объект трейта — в том виде, в каком его принимает
/// [`crate::block`].
impl AhciDisk {
    pub fn into_block_device(self) -> Box<dyn disk::BlockDevice + Send> {
        Box::new(self)
    }
}

// SAFETY: структура владеет своими буферами DMA и окном регистров, которое
// отображено на всё время жизни ядра. Ничего разделяемого между потоками в ней
// нет, а одновременный доступ исключён замком выше по стеку — тем же, что у
// virtio-blk: у порта одна очередь команд, и два запроса в неё не отправить.
unsafe impl Send for AhciDisk {}
