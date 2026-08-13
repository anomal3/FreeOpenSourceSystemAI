//! Контроллер прерываний ARM GIC (Generic Interrupt Controller), версия 2.
//!
//! # Что здесь есть и чего нет
//!
//! Реализован ровно тот минимум, без которого таймер не доедет до процессора:
//! включить distributor и CPU interface, опустить порог приоритета, разрешить
//! один INTID, подтвердить прерывание и сообщить о его завершении. Маршрутизации
//! по ядрам (`GICD_ITARGETSR`), приоритетных групп, SGI и всего, что нужно для
//! SMP, здесь нет — они появятся вместе со вторичными ядрами.
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
//! Поэтому «PPI 14» из документации на машину и `INTID 30` в коде — одно и то
//! же число, записанное в разных системах отсчёта: `16 + 14`.
//!
//! # Почему регистры читаются по identity-адресам
//!
//! Так же, как PL011 в [`super`]: пока ядро исполняется в нижней половине,
//! физический адрес окна и есть его виртуальный адрес. Отображение обеих
//! страниц (identity и прямое) делает [`super::paging`] по списку
//! [`MMIO_WINDOWS`].

// TODO(интеграция): снять, когда `main.rs` начнёт поднимать прерывания. До тех
// пор модуль недостижим по публичным путям, и весь его API числится мёртвым.
#![allow(dead_code)]

use core::ptr;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Раскладка окон MMIO на QEMU `-machine virt`
// ---------------------------------------------------------------------------

/// Distributor: общая для всех ядер часть контроллера.
///
/// Адрес верен для QEMU virt (`hw/arm/virt.c`, `VIRT_GIC_DIST`). Как и с
/// PL011, «общепринятого» адреса у GIC нет: на реальной плате его положение
/// описано в device tree (`interrupt-controller@...`) либо в ACPI MADT.
///
/// TODO(Phase 3): брать адреса из FDT/MADT, а не из константы.
pub const GICD_BASE_DEFAULT: usize = 0x0800_0000;

/// CPU interface: часть, через которую конкретное ядро подтверждает прерывания.
pub const GICC_BASE_DEFAULT: usize = 0x0801_0000;

/// GICv2m: приставка к GICv2, превращающая запись в память в обычное SPI.
///
/// Нужна потому, что у PCIe нет линий прерываний — есть только запись по
/// адресу, и кто-то обязан эту запись перехватить. В GICv3 этим занимается ITS,
/// у которой ради того же результата есть таблицы устройств, командная очередь
/// и собственный протокол; v2m делает то же самое одним регистром, и на машине
/// с GICv2 выбора всё равно нет.
///
/// Адрес верен для QEMU virt (`hw/arm/virt.c`, `VIRT_GIC_V2M`) — та же оговорка,
/// что и у [`GICD_BASE_DEFAULT`]: на настоящей плате его положение описано в MADT
/// (структура `GIC MSI Frame`) или в device tree.
pub const V2M_BASE: usize = 0x0802_0000;

/// `MSI_TYPER`: в битах 26:16 первый SPI, который умеет выдавать эта приставка,
/// в битах 9:0 — сколько их всего.
const V2M_MSI_TYPER: usize = 0x008;
/// `MSI_SETSPI_NS`: регистр, запись в который и порождает прерывание. Что
/// именно записано, и есть номер SPI.
const V2M_MSI_SETSPI_NS: usize = 0x040;

/// Размер каждого окна на этой машине.
pub const WINDOW_SIZE: usize = 0x1_0000;

/// Адреса, найденные в таблицах прошивки.
///
/// Раньше здесь стояли константы QEMU `virt`, и на первой же чужой машине —
/// VirtualBox на Apple Silicon — ядро сообщило «unsupported interrupt
/// controller (Unknown)» и осталось без таймера и без ввода. Угаданный адрес
/// работает ровно до тех пор, пока машина та же самая.
///
/// Значения по умолчанию оставлены как запасной путь: машина без ACPI (или с
/// MADT без записи distributor'а) — это по-прежнему QEMU `virt`, где они верны.
static GICD_ADDR: AtomicUsize = AtomicUsize::new(GICD_BASE_DEFAULT);
static GICC_ADDR: AtomicUsize = AtomicUsize::new(GICC_BASE_DEFAULT);
/// Redistributor GICv3: у v2 его не существует, поэтому ноль означает «нет».
static GICR_ADDR: AtomicUsize = AtomicUsize::new(0);

/// Версия, объявленная прошивкой в MADT. Ноль — прошивка промолчала.
///
/// Хранится отдельно от [`VERSION`] потому, что это разные утверждения: здесь
/// то, что сказали, там — то, с чем ядро решило работать.
static MADT_VERSION: AtomicU32 = AtomicU32::new(0);

/// Адрес distributor'а.
#[must_use]
pub fn gicd() -> usize {
    GICD_ADDR.load(Ordering::Relaxed)
}

/// Адрес процессорного интерфейса (GICv2).
#[must_use]
pub fn gicc() -> usize {
    GICC_ADDR.load(Ordering::Relaxed)
}

/// Адрес redistributor'а этого ядра (GICv3); ноль, если его нет.
#[must_use]
pub fn gicr() -> usize {
    GICR_ADDR.load(Ordering::Relaxed)
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

    // Прошивка, не заполнившая поле версии, всё равно себя выдаёт: redistributor
    // существует только у v3 и новее, у v2 такого понятия нет вовсе. Вывод из
    // наличия адреса надёжнее чтения регистра — он ничего не трогает.
    let version = if layout.version != 0 {
        u32::from(layout.version)
    } else if layout.redistributor.is_some() {
        3
    } else {
        0
    };
    MADT_VERSION.store(version, Ordering::Relaxed);
}

/// Что обязано быть отображено как [`crate::mm::PageFlags::DEVICE`], чтобы
/// драйвер заработал.
///
/// Список нужен снаружи: карта памяти UEFI окна GIC, как правило, не описывает
/// вовсе (это MMIO, а не память), а те регионы `Reserved`, что в ней есть,
/// подкачка отображает по своим правилам. Полагаться на случайное попадание
/// нельзя — первое же обращение к незамапленному distributor'у дало бы data
/// abort ровно в тот момент, когда обработчика отказов ещё нет.
#[must_use]
pub fn mmio_windows() -> [(usize, usize); 4] {
    [
        // Distributor у GICv3 занимает 64 КиБ вместо 4 КиБ, поэтому окно берётся
        // с запасом сразу — лишняя отображённая страница устройства не стоит
        // ничего, а недостающая даёт data abort там, где обработчика ещё нет.
        (gicd(), WINDOW_SIZE),
        (gicc(), WINDOW_SIZE),
        // Redistributor: два фрейма по 64 КиБ на ядро (RD и SGI), поэтому
        // окно вдвое шире. Ноль означает «нет v3», и такое окно отбрасывает
        // тот, кто отображает.
        (gicr(), WINDOW_SIZE * 2),
        // Окно v2m отображается всегда, даже когда MSI никому не понадобятся:
        // его отсутствие обнаружилось бы только при первом обращении, то есть в
        // драйвере, а не при разборе контроллера.
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
/// Конфигурация «уровень/фронт», по два бита на INTID.
const GICD_ICFGR: usize = 0x0C00;

/// `GICD_PIDR2` в раскладке GICv1/GICv2: distributor занимает 4 КиБ, и блок
/// идентификации лежит в его конце.
const GICD_PIDR2_V2: usize = 0x0FE8;
/// Тот же регистр в раскладке GICv3/GICv4: там distributor занимает 64 КиБ.
/// Два разных смещения — единственный способ отличить версии, не зная заранее,
/// какая именно железка стоит.
const GICD_PIDR2_V3: usize = 0xFFE8;

/// Биты 7:4 `PIDR2` — `ArchRev`, номер версии архитектуры GIC.
const PIDR2_ARCH_REV_SHIFT: u32 = 4;
const PIDR2_ARCH_REV_MASK: u32 = 0xF;

/// Бит 0 `GICD_CTLR`. При одном состоянии безопасности (QEMU virt без
/// `secure=on`) это просто «distributor включён»; при двух — `EnableGrp0`.
const GICD_CTLR_ENABLE: u32 = 1 << 0;

/// Биты 4:0 `GICD_TYPER`: `ITLinesNumber`. Число поддерживаемых INTID равно
/// `32 * (ITLinesNumber + 1)`.
const GICD_TYPER_IT_LINES_MASK: u32 = 0x1F;

// ---------------------------------------------------------------------------
// Регистры CPU interface
// ---------------------------------------------------------------------------

const GICC_CTLR: usize = 0x0000;
/// Priority Mask: прерывания с приоритетом **численно не меньше** этого
/// значения до ядра не доходят.
const GICC_PMR: usize = 0x0004;
const GICC_BPR: usize = 0x0008;
/// Interrupt Acknowledge: чтение возвращает INTID и переводит его в состояние
/// active.
const GICC_IAR: usize = 0x000C;
/// End Of Interrupt: запись снимает состояние active.
const GICC_EOIR: usize = 0x0010;

const GICC_CTLR_ENABLE: u32 = 1 << 0;

/// Порог приоритета.
///
/// На GIC меньшее число означает более высокий приоритет, а PMR задаёт границу
/// «строго выше которой пропускаем». `0xF0` пропускает всё, кроме самого
/// низкого приоритета, и при этом остаётся выразимым на реализациях, где
/// значащими являются лишь старшие биты байта приоритета (QEMU реализует 5, GIC
/// разрешает не меньше 4). Ноль в PMR заблокировал бы всё — самая частая ошибка
/// при первой настройке.
const PMR_ALLOW_ALL: u32 = 0xF0;

/// Значение, которое `GICC_IAR` возвращает, когда подтверждать нечего.
///
/// Такое чтение — не ошибка: прерывание могло пропасть между сигналом и
/// подтверждением (например, устройство сняло линию). Важно другое — на
/// spurious INTID **нельзя** отвечать `EOIR`, иначе счётчик активных прерываний
/// в контроллере уедет в минус.
pub const SPURIOUS_INTID: u32 = 1023;

/// Биты 9:0 `GICC_IAR` — собственно INTID. Старшие биты 12:10 несут номер
/// ядра-отправителя и значимы только для SGI, поэтому в `GICC_EOIR` возвращать
/// надо всё прочитанное слово целиком, а не отфильтрованный INTID.
pub const INTID_MASK: u32 = 0x3FF;

// ---------------------------------------------------------------------------
// Версия контроллера
// ---------------------------------------------------------------------------

/// Какой GIC нашёлся на машине.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Version {
    /// GICv2 — единственная версия, с которой этот драйвер умеет работать.
    V2,
    /// GICv3 или GICv4: distributor совместим, но CPU interface у них не в
    /// MMIO, а в системных регистрах (`ICC_*_EL1`), и без redistributor'а
    /// разрешить PPI нельзя. Требует отдельного драйвера.
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
/// как Device окне (см. [`MMIO_WINDOWS`]), а смещение — быть кратно 4.
unsafe fn read(base: usize, offset: usize) -> u32 {
    // SAFETY: `read_volatile` обязателен — значение регистра меняет контроллер,
    // и компилятору нельзя разрешать кэшировать или выбрасывать это чтение.
    // Выравнивание гарантировано вызывающим.
    unsafe { ptr::read_volatile((base + offset) as *const u32) }
}

/// # Safety
///
/// См. [`read`]. Кроме того, запись меняет состояние контроллера, и за
/// корректность значения отвечает вызывающий.
unsafe fn write(base: usize, offset: usize, value: u32) {
    // SAFETY: `write_volatile` не даёт компилятору объединить или переставить
    // записи в регистры — для контроллера значим и сам факт записи, и порядок.
    unsafe { ptr::write_volatile((base + offset) as *mut u32, value) }
}

/// Побайтовая запись — только для `GICD_IPRIORITYR`, где на каждый INTID
/// отведён отдельный байт и запись словом задела бы соседей.
///
/// # Safety
///
/// См. [`write`]; требование к выравниванию здесь снимается.
unsafe fn write_byte(base: usize, offset: usize, value: u8) {
    // SAFETY: те же соображения, что и в [`write`].
    unsafe { ptr::write_volatile((base + offset) as *mut u8, value) }
}

// ---------------------------------------------------------------------------
// Настройка
// ---------------------------------------------------------------------------

/// Определить версию контроллера и, если это GICv2, привести его в рабочее
/// состояние: distributor и CPU interface включены, порог приоритета опущен,
/// все прерывания запрещены и не висят в pending.
///
/// Сами прерывания процессору при этом ещё не разрешены — за это отвечает
/// [`super::interrupts::enable`].
///
/// # Safety
///
/// Окна [`MMIO_WINDOWS`] должны быть уже отображены как Device-память, а
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
            // Прошивка (UEFI) успела включить свои источники — таймер
            // watchdog'а, UART, что угодно. Их обработчиков у нас нет, а
            // векторы уже наши: не запретив всё разом, мы получили бы первое
            // же прерывание в никуда.
            write(gicd(), GICD_ICENABLER + offset, !0);
            write(gicd(), GICD_ICPENDR + offset, !0);
            // Group 0. При одном состоянии безопасности (QEMU virt без
            // `secure=on`) регистр не реализован и запись игнорируется; при
            // двух — это единственная группа, которую включает
            // `GICD_CTLR.EnableGrp0`.
            write(gicd(), GICD_IGROUPR + offset, 0);
        }

        write(gicc(), GICC_PMR, PMR_ALLOW_ALL);
        // `GICC_BPR` не трогаем: значение по умолчанию отключает приоритетную
        // вытесняемость, а вложенных прерываний у ядра пока и быть не должно.

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

/// `GICR_WAKER`: пока в нём стоит `ProcessorSleep`, redistributor не доставляет
/// ядру ничего.
const GICR_WAKER: usize = 0x0014;
const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
/// `ChildrenAsleep`: снимается железом после пробуждения — до этого момента
/// программировать redistributor нельзя.
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

/// Второй фрейм redistributor'а — тот, где живут регистры SGI и PPI.
const GICR_SGI_FRAME: usize = 0x1_0000;
/// Смещения внутри фрейма SGI совпадают с одноимёнными у distributor'а.
const GICR_IGROUPR0: usize = 0x0080;
const GICR_ISENABLER0: usize = 0x0100;
const GICR_ICENABLER0: usize = 0x0180;
const GICR_ICPENDR0: usize = 0x0280;
const GICR_IPRIORITYR: usize = 0x0400;

/// `GICD_CTLR` в раскладке v3 при одном состоянии безопасности: бит 4 включает
/// affinity routing, без которого distributor работать откажется, а бит 1 —
/// доставку прерываний группы 1.
const GICD_CTLR_ARE: u32 = 1 << 4;
const GICD_CTLR_ENABLE_GRP1: u32 = 1 << 1;

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
        // 1. Разбудить redistributor. Прошивка оставляет его спящим, и все
        // дальнейшие записи в него до пробуждения теряются молча.
        if redistributor != 0 {
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

            // 2. Приватные прерывания: всё запретить (прошивка успела включить
            // своё), снять ожидающие, объявить группой 1 — той, которую ядро
            // потом разрешит через `ICC_IGRPEN1_EL1`.
            let sgi = redistributor + GICR_SGI_FRAME;
            write(sgi, GICR_ICENABLER0, !0);
            write(sgi, GICR_ICPENDR0, !0);
            write(sgi, GICR_IGROUPR0, !0);
        }

        // 3. Distributor: то же самое для SPI — и только для них.
        //
        // Блок 0 (номера 0…31, то есть SGI и PPI) пропускается, и это не
        // экономия. При включённом affinity routing — а без него v3 работать
        // отказывается — банкованные регистры distributor'а для приватных
        // прерываний объявлены RES0: их место заняли одноимённые регистры
        // redistributor'а, настроенные шагом выше. Так же поступает Linux, у
        // которого цикл в `gic_dist_init` начинается ровно с 32.
        //
        // Цена ошибки выяснилась на VirtualBox 7.2.14: запись `0xffffffff` в
        // `GICD_ICENABLER0` сняла процесс виртуальной машины (`brk` в
        // `PGMPhysWrite`, поток EMT-0). Запись, которая ничего не делает на
        // одной машине, на другой оказалась не безобидной, а смертельной.
        write(gicd(), GICD_CTLR, 0);
        let lines = it_lines();
        for block in 1..lines {
            let offset = block * 4;
            write(gicd(), GICD_ICENABLER + offset, !0);
            write(gicd(), GICD_ICPENDR + offset, !0);
            write(gicd(), GICD_IGROUPR + offset, !0);
        }
        write(gicd(), GICD_CTLR, GICD_CTLR_ARE | GICD_CTLR_ENABLE_GRP1);

        // 4. Процессорный интерфейс — системными регистрами. `SRE` включается
        // первым: пока он нулевой, остальные `ICC_*` недоступны и обращение к
        // ним даёт исключение.
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
/// Приватные прерывания (PPI и SGI, номера до 32) живут в redistributor'е этого
/// ядра, а не в distributor'е — в этом главное отличие от v2, и таймер, который
/// как раз PPI, включается именно там.
///
/// # Safety
///
/// См. [`enable_interrupt`].
unsafe fn enable_interrupt_v3(intid: u32, priority: u8) {
    let redistributor = gicr();
    if intid < 32 {
        // Приватное прерывание — и другого пути к нему, кроме redistributor'а,
        // у v3 нет. Раньше здесь был запасной: без адреса redistributor'а
        // разрешение писалось в distributor. Толку от него не было никогда
        // (для номеров до 32 эти регистры при affinity routing объявлены RES0),
        // а на VirtualBox такая запись снимает виртуальную машину целиком.
        // Молчать об этом не приходится: [`super::interrupts::init`] печатает
        // предупреждение сразу, как только видит v3 без redistributor'а.
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
/// где он лежит у GICv2, — и делала это на любой машине, ещё не зная, какая
/// она. У GICv3 distributor занимает 64 КиБ, и `0x0FE8` внутри него —
/// **reserved**: спецификация о таком чтении не обещает ничего.
///
/// «Ничего» оказалось буквальным. VirtualBox 7.2.14 на Apple Silicon держит
/// distributor по адресу `0xfcd30000`, и чтение `0xfcd30fe8` не вернуло мусор,
/// а сняло весь процесс виртуальной машины: поток EMT-0 упал в `brk` внутри
/// `PGMPhysRead`. Гипервизор, которого гость может уронить чтением, — дефект
/// гипервизора, но пробное чтение зарезервированного регистра сделали мы, и
/// делать его было незачем: прошивка уже сказала в MADT, какая тут версия.
///
/// Поэтому порядок теперь такой: сказанное прошивкой принимается как есть, а
/// щупать регистры остаётся только там, где она промолчала.
///
/// # Safety
///
/// Окно distributor'а должно быть отображено; чтение регистров идентификации
/// побочных эффектов не имеет.
unsafe fn detect_version() -> Version {
    match MADT_VERSION.load(Ordering::Relaxed) {
        // GICv1 программируется тем же минимальным набором регистров, что и v2:
        // отличия (виртуализация, deactivate-split) мы не используем.
        1 | 2 => return Version::V2,
        3 | 4 => return Version::V3,
        _ => {}
    }

    // Прошивка не сказала ничего: ни поля версии, ни redistributor'а. Такая
    // машина — либо QEMU `virt` без ACPI, либо плата с device tree, до которого
    // очередь ещё не дошла; в обоих случаях адрес distributor'а взят из
    // константы, то есть предполагается раскладка v2. С неё и начинаем.
    //
    // SAFETY: контракт функции.
    let rev_v2 = unsafe { arch_rev(GICD_PIDR2_V2) };
    if rev_v2 == 1 || rev_v2 == 2 {
        return Version::V2;
    }
    // SAFETY: та же страница окна distributor'а; в раскладке v2 это смещение
    // читается как ноль, поэтому проверка безопасна для обеих версий.
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
/// [`init`] должен был обнаружить GICv2; окна отображены.
pub unsafe fn enable_interrupt(intid: u32, priority: u8) {
    if version() == Some(Version::V3) {
        // SAFETY: контракт функции; у v3 приватные прерывания живут в
        // redistributor'е, и путь туда отдельный.
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
        // `GICD_ICFGR` не трогаем сознательно: для PPI это поле read-only (тип
        // сигнала задан железом), а для SPI значение по умолчанию —
        // level-sensitive, что верно для всей периферии QEMU virt.
        write(gicd(), GICD_ISENABLER + index, bit);
    }
}

/// Объявить прерывание срабатывающим по фронту, а не по уровню.
///
/// Нужно ровно для MSI и ровно поэтому: запись в `MSI_SETSPI_NS` — это
/// **импульс**, а не поднятая и удерживаемая линия. Прерывание, оставленное
/// уровневым (значение по умолчанию, верное для остальной периферии virt), в
/// этом случае не доставляется вовсе — GIC ждёт уровня, которого никто не
/// держит. Выглядит это как полностью настроенный MSI-X, от которого не приходит
/// ни одного прерывания, и найти причину по симптому невозможно: не срабатывает
/// ни один обработчик, включая обработчик неизвестных INTID.
///
/// `GICD_ICFGR` отводит по два бита на прерывание; старший из них и означает
/// «по фронту».
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
/// `None`, если приставки нет: регистр читается нулями там, где окно не занято
/// устройством, а диапазон нулевой длины ни на что не годен.
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
///
/// Адрес — регистр приставки, данные — сам номер прерывания. Разница с x86-64
/// ровно в этом: там номер ядра и вектор закодированы в адресе и данных по
/// правилам APIC, здесь адрес один на все прерывания, а различает их
/// записанное значение.
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
/// Вызывать только из обработчика IRQ: чтение переводит прерывание в состояние
/// active, и без парного `EOI` контроллер больше ничего не пропустит.
#[must_use]
pub unsafe fn acknowledge() -> u32 {
    // SAFETY: контракт функции.
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
/// использованным: повторный или выдуманный `EOI` ломает учёт активных
/// прерываний в контроллере.
pub unsafe fn end_of_interrupt(iar: u32) {
    // SAFETY: контракт функции.
    // SAFETY: обе ветви — контракт функции.
    unsafe {
        if version() == Some(Version::V3) {
            end_of_interrupt_v3(iar);
        } else {
            write(gicc(), GICC_EOIR, iar);
        }
    }
}
