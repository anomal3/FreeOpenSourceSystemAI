//! Контроллер прерываний ARM GIC (Generic Interrupt Controller), версии 2 и 3.
//!
//! # Что здесь есть и чего нет
//!
//! Реализован тот минимум, без которого прерывания не доедут до процессоров:
//! включить distributor и процессорный интерфейс, опустить порог приоритета,
//! разрешить INTID, подтвердить прерывание и сообщить о его завершении. С фазы
//! 43 к нему добавились три вещи, без которых второй процессор не работает:
//! процессорный интерфейс и redistributor на каждом процессоре, маршрут SPI к
//! загрузочному процессору (`GICD_ITARGETSR`) и SGI — межпроцессорные
//! прерывания. Приоритетных групп и распределения SPI между процессорами нет:
//! все внешние прерывания обслуживает загрузочный.
//!
//! # Нумерация прерываний
//!
//! GIC складывает все источники в одно плоское пространство INTID:
//!
//! * `0…15`   — SGI, программные межпроцессорные;
//! * `16…31`  — PPI, приватные для каждого ядра (сюда попадают таймеры);
//! * `32…1019`— SPI, разделяемые периферийные;
//! * `1020…1023` — служебные, из них `1023` означает «подтверждать нечего».
//!
//! Поэтому «PPI 14» из документации на машину и `INTID 30` в коде — одно и то же
//! число, записанное в разных системах отсчёта: `16 + 14`.
//!
//! # Почему регистры читаются по identity-адресам
//!
//! Так же, как PL011 в [`super`]: пока ядро исполняется в нижней половине,
//! физический адрес окна и есть его виртуальный адрес. Отображение окон делает
//! [`super::paging`] по списку [`mmio_windows`].

// TODO(интеграция): снять, когда `main.rs` начнёт поднимать прерывания. До тех
// пор модуль недостижим по публичным путям, и весь его API числится мёртвым.
#![allow(dead_code)]

use core::ptr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::smp::MAX_CPUS;

// ---------------------------------------------------------------------------
// Раскладка окон MMIO на QEMU `-machine virt`
// ---------------------------------------------------------------------------

/// Distributor: общая для всех ядер часть контроллера.
///
/// Адрес верен для QEMU virt (`hw/arm/virt.c`, `VIRT_GIC_DIST`). Как и с
/// PL011, «общепринятого» адреса у GIC нет: на реальной плате его положение
/// описано в device tree (`interrupt-controller@...`) либо в ACPI MADT.
pub const GICD_BASE_DEFAULT: usize = 0x0800_0000;

/// CPU interface: часть, через которую конкретное ядро подтверждает прерывания.
pub const GICC_BASE_DEFAULT: usize = 0x0801_0000;

/// GICv2m: приставка к GICv2, превращающая запись в память в обычное SPI.
///
/// Нужна потому, что у PCIe нет линий прерываний — есть только запись по
/// адресу, и кто-то обязан эту запись перехватить.
///
/// Адрес верен для QEMU virt (`hw/arm/virt.c`, `VIRT_GIC_V2M`) — та же оговорка,
/// что и у [`GICD_BASE_DEFAULT`].
pub const V2M_BASE: usize = 0x0802_0000;

/// `MSI_TYPER`: в битах 26:16 первый SPI, который умеет выдавать эта приставка,
/// в битах 9:0 — сколько их всего.
const V2M_MSI_TYPER: usize = 0x008;
/// `MSI_SETSPI_NS`: регистр, запись в который и порождает прерывание.
const V2M_MSI_SETSPI_NS: usize = 0x040;

/// Размер каждого окна на этой машине.
pub const WINDOW_SIZE: usize = 0x1_0000;

/// Redistributor одного процессора: два фрейма по 64 КиБ (RD и SGI).
const REDISTRIBUTOR_SIZE: usize = WINDOW_SIZE * 2;

/// Адреса, найденные в таблицах прошивки.
///
/// Раньше здесь стояли константы QEMU `virt`, и на первой же чужой машине —
/// VirtualBox на Apple Silicon — ядро сообщило «unsupported interrupt
/// controller (Unknown)» и осталось без таймера и без ввода. Угаданный адрес
/// работает ровно до тех пор, пока машина та же самая.
static GICD_ADDR: AtomicUsize = AtomicUsize::new(GICD_BASE_DEFAULT);
static GICC_ADDR: AtomicUsize = AtomicUsize::new(GICC_BASE_DEFAULT);
/// Redistributor GICv3 **загрузочного** процессора; у v2 его не существует,
/// поэтому ноль означает «нет».
static GICR_ADDR: AtomicUsize = AtomicUsize::new(0);

/// Redistributor'ы остальных процессоров, по номеру процессора.
///
/// У GICv3 он у каждого процессора свой, и приватные прерывания — таймер и
/// межпроцессорные — включаются только в нём. Нулевой элемент не используется:
/// у загрузочного адрес лежит в [`GICR_ADDR`], куда его положили ещё до того,
/// как появилось понятие «номер процессора».
static REDISTRIBUTORS: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

/// Длина диапазона, в котором прошивка перечислила redistributor'ы. Ноль —
/// диапазона нет, адреса приехали в записях процессоров.
static GICR_SPAN: AtomicUsize = AtomicUsize::new(0);

/// Версия, объявленная прошивкой в MADT. Ноль — прошивка промолчала.
static MADT_VERSION: AtomicU32 = AtomicU32::new(0);

/// Адрес distributor'а.
#[must_use]
pub fn gicd() -> usize {
    GICD_ADDR.load(Ordering::Relaxed)
}

/// Адрес процессорного интерфейса (GICv2).
///
/// Один на все процессоры, и это не упрощение: окно GICC банковано — каждый
/// процессор видит по этому адресу свой собственный интерфейс.
#[must_use]
pub fn gicc() -> usize {
    GICC_ADDR.load(Ordering::Relaxed)
}

/// Адрес redistributor'а **текущего** процессора (GICv3); ноль, если его нет.
#[must_use]
pub fn gicr() -> usize {
    let cpu = crate::smp::cpu();
    if cpu == 0 {
        GICR_ADDR.load(Ordering::Relaxed)
    } else {
        REDISTRIBUTORS[cpu].load(Ordering::Relaxed)
    }
}

/// Принять раскладку, прочитанную из MADT.
///
/// Вызывается **до** построения таблиц страниц: окна отображаются по этим
/// адресам, и узнать их позже было бы уже поздно.
pub fn configure(layout: &super::acpi::GicLayout) {
    GICD_ADDR.store(layout.distributor, Ordering::Relaxed);
    if let Some(cpu) = layout.cpu_interface {
        GICC_ADDR.store(cpu, Ordering::Relaxed);
    }
    if let Some(redistributor) = layout.redistributor {
        GICR_ADDR.store(redistributor, Ordering::Relaxed);
    }
    GICR_SPAN.store(layout.redistributor_span, Ordering::Relaxed);

    // Прошивка, не заполнившая поле версии, всё равно себя выдаёт: redistributor
    // существует только у v3 и новее.
    let version = if layout.version != 0 {
        u32::from(layout.version)
    } else if layout.redistributor.is_some() {
        3
    } else {
        0
    };
    MADT_VERSION.store(version, Ordering::Relaxed);
}

/// Сколько отображать под redistributor'ы: весь объявленный диапазон, но не
/// больше, чем на [`MAX_CPUS`] процессоров, и не меньше одного.
///
/// Весь диапазон, а не окно загрузочного: redistributor второго процессора
/// ищется чтением регистров соседних фреймов (см. [`find_redistributor`]), и
/// неотображённый фрейм означал бы отказ страницы посреди поиска.
fn redistributor_window() -> usize {
    GICR_SPAN
        .load(Ordering::Relaxed)
        .min(MAX_CPUS * REDISTRIBUTOR_SIZE)
        .max(REDISTRIBUTOR_SIZE)
}

/// Что обязано быть отображено как [`crate::mm::PageFlags::DEVICE`], чтобы
/// драйвер заработал.
#[must_use]
pub fn mmio_windows() -> [(usize, usize); 4] {
    [
        // Distributor у GICv3 занимает 64 КиБ вместо 4 КиБ, поэтому окно берётся
        // с запасом сразу.
        (gicd(), WINDOW_SIZE),
        (gicc(), WINDOW_SIZE),
        // Ноль означает «нет v3», и такое окно отбрасывает тот, кто отображает.
        (GICR_ADDR.load(Ordering::Relaxed), redistributor_window()),
        // Окно v2m отображается всегда, даже когда MSI никому не понадобятся.
        (V2M_BASE, WINDOW_SIZE),
    ]
}

// ---------------------------------------------------------------------------
// Регистры distributor (GICv2, IHI0048B, глава 4.3)
// ---------------------------------------------------------------------------

const GICD_CTLR: usize = 0x0000;
const GICD_TYPER: usize = 0x0004;
/// Group select, по биту на INTID.
const GICD_IGROUPR: usize = 0x0080;
/// Interrupt Set-Enable, по биту на INTID.
const GICD_ISENABLER: usize = 0x0100;
/// Interrupt Clear-Enable.
const GICD_ICENABLER: usize = 0x0180;
/// Interrupt Clear-Pending.
const GICD_ICPENDR: usize = 0x0280;
/// Приоритеты, по **байту** на INTID.
const GICD_IPRIORITYR: usize = 0x0400;
/// Кому доставлять SPI: по байту на INTID, бит на процессорный интерфейс.
const GICD_ITARGETSR: usize = 0x0800;
/// Конфигурация «уровень/фронт», по два бита на INTID.
const GICD_ICFGR: usize = 0x0C00;
/// Software Generated Interrupt Register: запись порождает SGI.
const GICD_SGIR: usize = 0x0F00;

/// `GICD_SGIR`, биты 25:24 = `01`: всем процессорам, кроме себя.
const SGIR_ALL_BUT_SELF: u32 = 0b01 << 24;

/// `GICD_PIDR2` в раскладке GICv1/GICv2.
const GICD_PIDR2_V2: usize = 0x0FE8;
/// Тот же регистр в раскладке GICv3/GICv4.
const GICD_PIDR2_V3: usize = 0xFFE8;

/// Биты 7:4 `PIDR2` — `ArchRev`, номер версии архитектуры GIC.
const PIDR2_ARCH_REV_SHIFT: u32 = 4;
const PIDR2_ARCH_REV_MASK: u32 = 0xF;

/// Бит 0 `GICD_CTLR`.
const GICD_CTLR_ENABLE: u32 = 1 << 0;

/// Биты 4:0 `GICD_TYPER`: `ITLinesNumber`.
const GICD_TYPER_IT_LINES_MASK: u32 = 0x1F;

// ---------------------------------------------------------------------------
// Регистры CPU interface
// ---------------------------------------------------------------------------

const GICC_CTLR: usize = 0x0000;
/// Priority Mask: прерывания с приоритетом **численно не меньше** этого
/// значения до ядра не доходят.
const GICC_PMR: usize = 0x0004;
const GICC_BPR: usize = 0x0008;
/// Interrupt Acknowledge.
const GICC_IAR: usize = 0x000C;
/// End Of Interrupt.
const GICC_EOIR: usize = 0x0010;

const GICC_CTLR_ENABLE: u32 = 1 << 0;

/// Порог приоритета: пропускать всё, кроме самого низкого.
const PMR_ALLOW_ALL: u32 = 0xF0;

/// Значение, которое `GICC_IAR` возвращает, когда подтверждать нечего.
pub const SPURIOUS_INTID: u32 = 1023;

/// Биты 9:0 `GICC_IAR` — собственно INTID. Старшие биты 12:10 несут номер
/// ядра-отправителя и значимы только для SGI, поэтому в `GICC_EOIR` возвращать
/// надо всё прочитанное слово целиком.
pub const INTID_MASK: u32 = 0x3FF;

// ---------------------------------------------------------------------------
// Версия контроллера
// ---------------------------------------------------------------------------

/// Какой GIC нашёлся на машине.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Version {
    /// GICv2.
    V2,
    /// GICv3 или GICv4: процессорный интерфейс в системных регистрах, у каждого
    /// ядра свой redistributor.
    V3,
    /// Ничего похожего на GIC по этим адресам не отвечает.
    Unknown,
}

/// Кэш результата определения версии: `0` — ещё не определяли.
static VERSION: AtomicU32 = AtomicU32::new(0);

const VERSION_V2: u32 = 1;
const VERSION_V3: u32 = 2;
const VERSION_UNKNOWN: u32 = 3;

/// Версия, определённая при [`init`]. `None`, если [`init`] ещё не звали.
#[must_use]
pub fn version() -> Option<Version> {
    match VERSION.load(Ordering::Relaxed) {
        VERSION_V2 => Some(Version::V2),
        VERSION_V3 => Some(Version::V3),
        VERSION_UNKNOWN => Some(Version::Unknown),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Доступ к регистрам
// ---------------------------------------------------------------------------

/// # Safety
///
/// `base + offset` обязан указывать на существующий регистр GIC в отображённом
/// как Device окне, а смещение — быть кратно 4.
unsafe fn read(base: usize, offset: usize) -> u32 {
    // SAFETY: `read_volatile` обязателен — значение регистра меняет контроллер.
    unsafe { ptr::read_volatile((base + offset) as *const u32) }
}

/// # Safety
///
/// См. [`read`]. Кроме того, запись меняет состояние контроллера.
unsafe fn write(base: usize, offset: usize, value: u32) {
    // SAFETY: `write_volatile` не даёт компилятору объединить или переставить
    // записи в регистры.
    unsafe { ptr::write_volatile((base + offset) as *mut u32, value) }
}

/// Побайтовая запись — для регистров, где на каждый INTID отведён отдельный
/// байт и запись словом задела бы соседей.
///
/// # Safety
///
/// См. [`write`]; требование к выравниванию здесь снимается.
unsafe fn write_byte(base: usize, offset: usize, value: u8) {
    // SAFETY: те же соображения, что и в [`write`].
    unsafe { ptr::write_volatile((base + offset) as *mut u8, value) }
}

/// Побайтовое чтение — пара к [`write_byte`].
///
/// # Safety
///
/// См. [`read`]; требование к выравниванию здесь снимается.
unsafe fn read_byte(base: usize, offset: usize) -> u8 {
    // SAFETY: те же соображения, что и в [`read`].
    unsafe { ptr::read_volatile((base + offset) as *const u8) }
}

// ---------------------------------------------------------------------------
// Настройка
// ---------------------------------------------------------------------------

/// Определить версию контроллера и привести его в рабочее состояние.
///
/// Сами прерывания процессору при этом ещё не разрешены — за это отвечает
/// [`super::interrupts::enable`].
///
/// # Safety
///
/// Окна [`mmio_windows`] должны быть уже отображены как Device-память, а
/// вызывающий обязан гарантировать, что параллельно с контроллером никто не
/// работает (на этой фазе — одно ядро с запрещёнными прерываниями).
pub unsafe fn init() -> Version {
    // SAFETY: контракт функции требует отображённых окон.
    let version = unsafe { detect_version() };
    VERSION.store(
        match version {
            Version::V2 => VERSION_V2,
            Version::V3 => VERSION_V3,
            Version::Unknown => VERSION_UNKNOWN,
        },
        Ordering::Relaxed,
    );

    if version == Version::V3 {
        // SAFETY: версия подтверждена, окна отображены, конкурентов нет.
        unsafe { init_v3() };
        return version;
    }
    if version != Version::V2 {
        return version;
    }

    // SAFETY: версия подтверждена, окна отображены, конкурентов нет.
    unsafe {
        // Пока идёт настройка, контроллер выключен: иначе на полпути может
        // прилететь прерывание, оставшееся включённым от прошивки.
        write(gicd(), GICD_CTLR, 0);
        write(gicc(), GICC_CTLR, 0);

        let lines = it_lines();
        for block in 0..lines {
            let offset = block * 4;
            // Прошивка (UEFI) успела включить свои источники. Их обработчиков у
            // нас нет, а векторы уже наши.
            write(gicd(), GICD_ICENABLER + offset, !0);
            write(gicd(), GICD_ICPENDR + offset, !0);
            write(gicd(), GICD_IGROUPR + offset, 0);
        }

        write(gicc(), GICC_PMR, PMR_ALLOW_ALL);

        write(gicd(), GICD_CTLR, GICD_CTLR_ENABLE);
        write(gicc(), GICC_CTLR, GICC_CTLR_ENABLE);
    }

    version
}

// ---------------------------------------------------------------------------
// GICv3
//
// От v2 он отличается не деталями, а устройством: процессорного интерфейса в
// памяти больше нет — его место заняли системные регистры `ICC_*`, — а у
// каждого ядра появился свой redistributor, через который включаются приватные
// прерывания (PPI и SGI). Distributor остался, но управляет только SPI.
// ---------------------------------------------------------------------------

/// `GICR_TYPER`: в битах 63:32 — сродство процессора, которому принадлежит этот
/// redistributor.
const GICR_TYPER: usize = 0x0008;
/// Бит 4 `GICR_TYPER`: последний redistributor диапазона.
const GICR_TYPER_LAST: u64 = 1 << 4;
/// `GICR_WAKER`: пока в нём стоит `ProcessorSleep`, redistributor не доставляет
/// ядру ничего.
const GICR_WAKER: usize = 0x0014;
const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
/// `ChildrenAsleep`: снимается железом после пробуждения.
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

/// Второй фрейм redistributor'а — тот, где живут регистры SGI и PPI.
const GICR_SGI_FRAME: usize = 0x1_0000;
const GICR_IGROUPR0: usize = 0x0080;
const GICR_ISENABLER0: usize = 0x0100;
const GICR_ICENABLER0: usize = 0x0180;
const GICR_ICPENDR0: usize = 0x0280;
const GICR_IPRIORITYR: usize = 0x0400;

/// `GICD_CTLR` в раскладке v3 при одном состоянии безопасности.
const GICD_CTLR_ARE: u32 = 1 << 4;
const GICD_CTLR_ENABLE_GRP1: u32 = 1 << 1;

/// `ICC_SGI1R_EL1`, бит 40 (`IRM`): всем процессорам, кроме себя.
const SGI1R_ALL_BUT_SELF: u64 = 1 << 40;
/// Биты 27:24 того же регистра — номер SGI.
const SGI1R_INTID_SHIFT: u64 = 24;

/// Разбудить redistributor и привести его приватные прерывания в исходное
/// состояние.
///
/// # Safety
///
/// `redistributor` — адрес redistributor'а **текущего** процессора в
/// отображённом окне.
unsafe fn wake_redistributor(redistributor: usize) {
    // SAFETY: контракт функции.
    unsafe {
        // 1. Разбудить redistributor. Прошивка оставляет его спящим, и все
        // дальнейшие записи в него до пробуждения теряются молча.
        let waker = read(redistributor, GICR_WAKER) & !GICR_WAKER_PROCESSOR_SLEEP;
        write(redistributor, GICR_WAKER, waker);
        // Ожидание с потолком: испорченный или отсутствующий redistributor
        // иначе подвесил бы загрузку навсегда.
        for _ in 0..100_000 {
            if read(redistributor, GICR_WAKER) & GICR_WAKER_CHILDREN_ASLEEP == 0 {
                break;
            }
            core::hint::spin_loop();
        }

        // 2. Приватные прерывания: всё запретить, снять ожидающие, объявить
        // группой 1 — той, которую ядро потом разрешит через `ICC_IGRPEN1_EL1`.
        let sgi = redistributor + GICR_SGI_FRAME;
        write(sgi, GICR_ICENABLER0, !0);
        write(sgi, GICR_ICPENDR0, !0);
        write(sgi, GICR_IGROUPR0, !0);
    }
}

/// Включить процессорный интерфейс GICv3 текущего процессора — системными
/// регистрами.
///
/// # Safety
///
/// Версия подтверждена; регистры `ICC_*` банкованы, и действует вызов только на
/// том процессоре, который его исполняет.
unsafe fn enable_cpu_interface_v3() {
    // SAFETY: контракт функции.
    unsafe {
        // `SRE` включается первым: пока он нулевой, остальные `ICC_*`
        // недоступны и обращение к ним даёт исключение.
        core::arch::asm!(
            "mrs {tmp}, ICC_SRE_EL1",
            "orr {tmp}, {tmp}, #1",
            "msr ICC_SRE_EL1, {tmp}",
            "isb",
            tmp = out(reg) _,
            options(nostack)
        );
        core::arch::asm!(
            "msr ICC_PMR_EL1, {pmr}",
            "msr ICC_IGRPEN1_EL1, {one}",
            "isb",
            pmr = in(reg) u64::from(PMR_ALLOW_ALL),
            one = in(reg) 1u64,
            options(nostack)
        );
    }
}

/// Настроить GICv3.
///
/// # Safety
///
/// Окна distributor'а и redistributor'а отображены как Device-память, версия
/// подтверждена, конкурентов нет.
unsafe fn init_v3() {
    let redistributor = gicr();

    // SAFETY: контракт функции.
    unsafe {
        if redistributor != 0 {
            wake_redistributor(redistributor);
        }

        // 3. Distributor: то же самое для SPI — и только для них.
        //
        // Блок 0 (номера 0…31, то есть SGI и PPI) пропускается, и это не
        // экономия. При включённом affinity routing банкованные регистры
        // distributor'а для приватных прерываний объявлены RES0: их место заняли
        // одноимённые регистры redistributor'а, настроенные шагом выше.
        //
        // Цена ошибки выяснилась на VirtualBox 7.2.14: запись `0xffffffff` в
        // `GICD_ICENABLER0` сняла процесс виртуальной машины.
        write(gicd(), GICD_CTLR, 0);
        let lines = it_lines();
        for block in 1..lines {
            let offset = block * 4;
            write(gicd(), GICD_ICENABLER + offset, !0);
            write(gicd(), GICD_ICPENDR + offset, !0);
            write(gicd(), GICD_IGROUPR + offset, !0);
        }
        write(gicd(), GICD_CTLR, GICD_CTLR_ARE | GICD_CTLR_ENABLE_GRP1);

        // 4. Процессорный интерфейс — системными регистрами.
        enable_cpu_interface_v3();
    }
}

/// Подтвердить прерывание на GICv3: `ICC_IAR1_EL1`.
///
/// # Safety
///
/// Интерфейс включён [`init_v3`].
unsafe fn acknowledge_v3() -> u32 {
    let value: u64;
    // SAFETY: контракт функции; чтение регистра переводит прерывание в active.
    unsafe {
        core::arch::asm!("mrs {}, ICC_IAR1_EL1", out(reg) value, options(nostack));
    }
    value as u32
}

/// Сообщить о завершении на GICv3: `ICC_EOIR1_EL1`.
///
/// # Safety
///
/// `iar` — значение, полученное из [`acknowledge_v3`], и используется один раз.
unsafe fn end_of_interrupt_v3(iar: u32) {
    // SAFETY: контракт функции.
    unsafe {
        core::arch::asm!("msr ICC_EOIR1_EL1, {}", in(reg) u64::from(iar), options(nostack));
    }
}

/// Разрешить INTID на GICv3.
///
/// Приватные прерывания (PPI и SGI, номера до 32) живут в redistributor'е
/// **текущего** ядра, а не в distributor'е — в этом главное отличие от v2, и
/// таймер, который как раз PPI, включается именно там.
///
/// # Safety
///
/// См. [`enable_interrupt`].
unsafe fn enable_interrupt_v3(intid: u32, priority: u8) {
    let redistributor = gicr();
    if intid < 32 {
        // Приватное прерывание — и другого пути к нему, кроме redistributor'а,
        // у v3 нет. Молчать об этом не приходится: [`super::interrupts::init`]
        // печатает предупреждение сразу, как только видит v3 без
        // redistributor'а.
        if redistributor == 0 {
            return;
        }
        let sgi = redistributor + GICR_SGI_FRAME;
        // SAFETY: контракт функции; адрес redistributor'а получен из MADT, его
        // окно отображено вместе с остальными окнами контроллера.
        unsafe {
            write_byte(sgi, GICR_IPRIORITYR + intid as usize, priority);
            write(sgi, GICR_ISENABLER0, 1 << intid);
        }
        return;
    }

    let index = (intid / 32) as usize * 4;
    // SAFETY: контракт функции; SPI живут в distributor'е при любой версии.
    unsafe {
        write_byte(gicd(), GICD_IPRIORITYR + intid as usize, priority);
        write(gicd(), GICD_ISENABLER + index, 1 << (intid % 32));
    }
}

/// Сколько блоков по 32 INTID поддерживает distributor.
///
/// # Safety
///
/// Окно distributor'а должно быть отображено.
unsafe fn it_lines() -> usize {
    // SAFETY: контракт функции.
    let typer = unsafe { read(gicd(), GICD_TYPER) };
    (typer & GICD_TYPER_IT_LINES_MASK) as usize + 1
}

/// Определить версию контроллера.
///
/// # Почему слово прошивки здесь главнее регистра
///
/// Раньше эта функция начинала с чтения `PIDR2` по смещению `0x0FE8` — тому,
/// где он лежит у GICv2, — и делала это на любой машине. У GICv3 `0x0FE8` внутри
/// distributor'а — **reserved**, и VirtualBox 7.2.14 на Apple Silicon на таком
/// чтении снял весь процесс виртуальной машины. Поэтому сказанное прошивкой
/// принимается как есть, а щупать регистры остаётся только там, где она
/// промолчала.
///
/// # Safety
///
/// Окно distributor'а должно быть отображено.
unsafe fn detect_version() -> Version {
    match MADT_VERSION.load(Ordering::Relaxed) {
        1 | 2 => return Version::V2,
        3 | 4 => return Version::V3,
        _ => {}
    }

    // SAFETY: контракт функции.
    let rev_v2 = unsafe { arch_rev(GICD_PIDR2_V2) };
    if rev_v2 == 1 || rev_v2 == 2 {
        return Version::V2;
    }
    // SAFETY: та же страница окна distributor'а.
    let rev_v3 = unsafe { arch_rev(GICD_PIDR2_V3) };
    if rev_v3 == 3 || rev_v3 == 4 {
        return Version::V3;
    }
    Version::Unknown
}

/// # Safety
///
/// См. [`read`].
unsafe fn arch_rev(pidr2_offset: usize) -> u32 {
    // SAFETY: контракт функции.
    let pidr2 = unsafe { read(gicd(), pidr2_offset) };
    (pidr2 >> PIDR2_ARCH_REV_SHIFT) & PIDR2_ARCH_REV_MASK
}

/// Разрешить один INTID и задать ему приоритет.
///
/// # Safety
///
/// [`init`] должен был опознать контроллер; окна отображены.
pub unsafe fn enable_interrupt(intid: u32, priority: u8) {
    if version() == Some(Version::V3) {
        // SAFETY: контракт функции.
        unsafe { enable_interrupt_v3(intid, priority) };
        return;
    }

    let index = (intid / 32) as usize * 4;
    let bit = 1u32 << (intid % 32);

    // SAFETY: контракт функции; смещения вычислены по раскладке из IHI0048B.
    unsafe {
        // Приоритет обязан быть численно меньше порога PMR, иначе прерывание
        // разрешено, приходит в контроллер и молча в нём остаётся.
        write_byte(gicd(), GICD_IPRIORITYR + intid as usize, priority);
        if intid >= 32 {
            // Кому доставлять. Пока процессор один, контроллер этот регистр не
            // реализует вовсе и доставляет всё единственному — поэтому раньше
            // здесь ничего и не писалось. С двумя процессорами у QEMU после
            // сброса поле нулевое, то есть «никому»: разрешённое прерывание
            // UART или xHCI просто не приходило бы. Адресат — текущий
            // процессор: его собственный бит отдаёт банкованный `ITARGETSR0`.
            let own = read_byte(gicd(), GICD_ITARGETSR);
            if own != 0 {
                write_byte(gicd(), GICD_ITARGETSR + intid as usize, own);
            }
        }
        write(gicd(), GICD_ISENABLER + index, bit);
    }
}

/// Объявить прерывание срабатывающим по фронту, а не по уровню.
///
/// Нужно ровно для MSI и ровно поэтому: запись в `MSI_SETSPI_NS` — это
/// **импульс**, а не поднятая и удерживаемая линия.
///
/// # Safety
///
/// [`init`] должен был обнаружить GICv2; окна отображены. `intid` обязан быть
/// SPI (32 и выше): у PPI это поле только для чтения.
pub unsafe fn set_edge_triggered(intid: u32) {
    const EDGE: u32 = 0b10;
    let register = (intid / 16) as usize * 4;
    let shift = (intid % 16) * 2;

    // SAFETY: контракт функции; раскладка регистра из IHI0048B, 4.3.13.
    unsafe {
        let current = read(gicd(), GICD_ICFGR + register);
        let updated = (current & !(0b11 << shift)) | (EDGE << shift);
        write(gicd(), GICD_ICFGR + register, updated);
    }
}

/// Какой диапазон SPI выдаёт приставка v2m: первый номер и сколько их.
///
/// # Safety
///
/// Окно [`V2M_BASE`] должно быть отображено как Device-память.
#[must_use]
pub unsafe fn v2m_spi_range() -> Option<(u32, u32)> {
    // SAFETY: контракт функции.
    let typer = unsafe { read(V2M_BASE, V2M_MSI_TYPER) };
    let base = (typer >> 16) & 0x7FF;
    let count = typer & 0x3FF;
    if count == 0 { None } else { Some((base, count)) }
}

/// Куда и что должно записать устройство, чтобы поднять SPI `intid`.
#[must_use]
pub fn msi_target(intid: u32) -> (u64, u32) {
    ((V2M_BASE + V2M_MSI_SETSPI_NS) as u64, intid)
}

/// Подтвердить прерывание и узнать его источник.
///
/// Возвращает **сырое** слово `GICC_IAR`: его же, целиком, надо потом передать
/// в [`end_of_interrupt`].
///
/// # Safety
///
/// Вызывать только из обработчика IRQ.
#[must_use]
pub unsafe fn acknowledge() -> u32 {
    // SAFETY: обе ветви — контракт функции.
    unsafe {
        if version() == Some(Version::V3) {
            acknowledge_v3()
        } else {
            read(gicc(), GICC_IAR)
        }
    }
}

/// Сообщить контроллеру, что обработка закончена.
///
/// # Safety
///
/// `iar` обязан быть значением, полученным из [`acknowledge`] и ещё не
/// использованным.
pub unsafe fn end_of_interrupt(iar: u32) {
    // SAFETY: обе ветви — контракт функции.
    unsafe {
        if version() == Some(Version::V3) {
            end_of_interrupt_v3(iar);
        } else {
            write(gicc(), GICC_EOIR, iar);
        }
    }
}

// ---------------------------------------------------------------------------
// Остальные процессоры
// ---------------------------------------------------------------------------

/// Найти redistributor процессора со сродством `affinity` в диапазоне,
/// объявленном прошивкой.
///
/// Нужно потому, что прошивка вправе описать redistributor'ы одним диапазоном,
/// не сказав, какой кому принадлежит, — так делает QEMU. Ответ знает сам
/// redistributor: сродство своего процессора он хранит в `GICR_TYPER`.
#[must_use]
pub fn find_redistributor(affinity: u32) -> Option<usize> {
    let base = GICR_ADDR.load(Ordering::Relaxed);
    let span = GICR_SPAN.load(Ordering::Relaxed).min(MAX_CPUS * REDISTRIBUTOR_SIZE);
    if base == 0 || span == 0 {
        return None;
    }
    let mut at = 0;
    while at + REDISTRIBUTOR_SIZE <= span {
        let frame = base + at;
        // SAFETY: весь диапазон, ограниченный тем же пределом, отображён при
        // запуске (см. `redistributor_window`); `GICR_TYPER` — регистр только
        // для чтения без побочных эффектов.
        let typer = unsafe { ptr::read_volatile((frame + GICR_TYPER) as *const u64) };
        if (typer >> 32) as u32 == affinity {
            return Some(frame);
        }
        if typer & GICR_TYPER_LAST != 0 {
            break;
        }
        at += REDISTRIBUTOR_SIZE;
    }
    None
}

/// Запомнить redistributor процессора `index` — до того, как тот проснётся.
pub fn set_redistributor(index: usize, address: usize) {
    if index > 0 && index < MAX_CPUS {
        REDISTRIBUTORS[index].store(address, Ordering::Relaxed);
    }
}

/// Размер окна одного redistributor'а — для тех, кто его отображает.
#[must_use]
pub const fn redistributor_size() -> usize {
    REDISTRIBUTOR_SIZE
}

/// Включить на проснувшемся процессоре его часть контроллера.
///
/// У GICv2 это процессорный интерфейс: окно общее, но банкованное, и включение
/// на загрузочном не значит ничего для остальных. У GICv3 — redistributor этого
/// процессора и его системные регистры.
///
/// # Safety
///
/// Вызывать на том процессоре, которому это нужно, после [`init`] на
/// загрузочном; для GICv3 — после [`set_redistributor`] для него.
pub unsafe fn init_secondary() {
    // SAFETY: контракт функции.
    unsafe {
        match version() {
            Some(Version::V2) => {
                write(gicc(), GICC_PMR, PMR_ALLOW_ALL);
                write(gicc(), GICC_CTLR, GICC_CTLR_ENABLE);
            }
            Some(Version::V3) => {
                let redistributor = gicr();
                if redistributor != 0 {
                    wake_redistributor(redistributor);
                }
                enable_cpu_interface_v3();
            }
            _ => {}
        }
    }
}

/// Послать SGI `intid` всем процессорам, кроме текущего.
///
/// # Safety
///
/// Контроллер опознан и включён; `intid` меньше 16.
pub unsafe fn send_sgi_to_others(intid: u32) {
    // SAFETY: контракт функции.
    unsafe {
        if version() == Some(Version::V3) {
            core::arch::asm!(
                "msr ICC_SGI1R_EL1, {}",
                in(reg) SGI1R_ALL_BUT_SELF | (u64::from(intid) << SGI1R_INTID_SHIFT),
                options(nostack)
            );
        } else {
            write(gicd(), GICD_SGIR, SGIR_ALL_BUT_SELF | intid);
        }
    }
}
