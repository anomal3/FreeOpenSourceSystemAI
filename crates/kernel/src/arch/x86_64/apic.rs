//! Контроллер прерываний и системный таймер: 8259 в отставку, Local APIC в дело.
//!
//! # Почему 8259 обязательно маскировать
//!
//! После включения пара 8259 настроена так, как её оставила прошивка (обычно
//! IRQ0..7 на векторах 0x08..0x0F). Эти номера принадлежат исключениям: 0x08 —
//! двойная ошибка, 0x0D — #GP, 0x0E — #PF. То есть первый же тик системного
//! таймера приехал бы в обработчик двойной ошибки, а нажатие клавиши — в
//! обработчик отказа страницы, причём с кадром без кода ошибки (внешнее
//! прерывание его не кладёт), из-за чего разъехался бы и стек.
//!
//! Одного перепрограммирования мало: замаскированный 8259 всё равно способен
//! выдать «ложное» IRQ7/IRQ15, если линия дёрнулась и опала между стробами.
//! Поэтому делается и то и другое: сначала векторы переносятся в безопасный
//! диапазон 0x20..0x2F, затем маскируются все входы. Ложное прерывание, если
//! оно всё же случится, приедет на вектор, у которого есть обработчик, и будет
//! опознано по имени вместо того, чтобы притвориться исключением.
//!
//! # Почему Local APIC, а не 8259
//!
//! 8259 не умеет ничего из того, что понадобится дальше: ни доставки прерываний
//! на конкретное ядро, ни IPI для запуска вторичных ядер, ни таймера на ядро.
//! Всё это есть у Local APIC, и переучиваться потом дороже, чем начать с него.
//!
//! # Почему нужен EOI
//!
//! Приняв прерывание, APIC ставит его бит в ISR и **не снимает сам**. Пока бит
//! стоит, доставляются только прерывания более высокого приоритета, а тот же
//! вектор — никогда. Забытый EOI выглядит как «таймер тикнул ровно один раз и
//! замолчал».

use super::paging;
use super::{cpuid, inb, io_wait, outb, rdmsr, wrmsr};
use crate::irq::TIMER_HZ;
use crate::kprintln;
use crate::mm::{AddressSpace, PageFlags, PhysAddr};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// --- Назначенные ядром векторы ------------------------------------------------

/// Системный таймер. Первый вектор за пределами архитектурных исключений.
pub const VECTOR_TIMER: u8 = 0x20;

/// Спурьёзное прерывание.
///
/// 0xFF выбран не только «чтобы подальше»: у процессоров семейства P6 младшие
/// четыре бита этого вектора аппаратно зашиты в единицы, и любое другое
/// значение с ненулевым младшим полубайтом там просто не запишется.
pub const VECTOR_SPURIOUS: u8 = 0xFF;

// --- Legacy PIC 8259 ----------------------------------------------------------

const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// ICW1: начать инициализацию, ожидается ICW4.
const ICW1_INIT: u8 = 0x11;
/// ICW3 ведущего: ведомый подключён к линии IRQ2.
const ICW3_MASTER_HAS_SLAVE_ON_IRQ2: u8 = 1 << 2;
/// ICW3 ведомого: его каскадный идентификатор — 2.
const ICW3_SLAVE_ID: u8 = 2;
/// ICW4: режим 8086/8088 вместо MCS-80/85.
const ICW4_8086_MODE: u8 = 0x01;
/// Маска, закрывающая все восемь входов контроллера.
const PIC_MASK_ALL: u8 = 0xFF;

/// Куда переносятся векторы ведущего и ведомого контроллеров.
const PIC1_VECTOR_BASE: u8 = 0x20;
const PIC2_VECTOR_BASE: u8 = 0x28;

/// Перенести векторы 8259 в безопасный диапазон и закрыть все входы.
fn disable_legacy_pic() {
    // SAFETY: порты 0x20/0x21 и 0xA0/0xA1 на PC-совместимых машинах закреплены
    // за парой 8259 и больше ничем не используются. Последовательность —
    // каноническая инициализация из даташита: ICW1..ICW4 подряд, затем маска.
    // `io_wait` между записями нужен потому, что контроллер медленнее шины и
    // может не успеть принять следующее слово.
    unsafe {
        outb(PIC1_COMMAND, ICW1_INIT);
        io_wait();
        outb(PIC2_COMMAND, ICW1_INIT);
        io_wait();
        outb(PIC1_DATA, PIC1_VECTOR_BASE);
        io_wait();
        outb(PIC2_DATA, PIC2_VECTOR_BASE);
        io_wait();
        outb(PIC1_DATA, ICW3_MASTER_HAS_SLAVE_ON_IRQ2);
        io_wait();
        outb(PIC2_DATA, ICW3_SLAVE_ID);
        io_wait();
        outb(PIC1_DATA, ICW4_8086_MODE);
        io_wait();
        outb(PIC2_DATA, ICW4_8086_MODE);
        io_wait();

        outb(PIC1_DATA, PIC_MASK_ALL);
        outb(PIC2_DATA, PIC_MASK_ALL);
    }
}

// --- Local APIC ---------------------------------------------------------------

/// `IA32_APIC_BASE`: где находится APIC и в каком он режиме.
const IA32_APIC_BASE: u32 = 0x1B;
/// Бит 8 — это загрузочный процессор. Только для диагностики.
const APIC_BASE_BSP: u64 = 1 << 8;
/// Бит 10 — режим x2APIC (регистры через MSR вместо MMIO).
const APIC_BASE_EXTD: u64 = 1 << 10;
/// Бит 11 — APIC включён вообще. При нуле все его регистры недоступны.
const APIC_BASE_ENABLE: u64 = 1 << 11;
/// Биты 12..51 — физический адрес окна MMIO.
const APIC_BASE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Смещения регистров в окне xAPIC. В режиме x2APIC тот же регистр адресуется
/// как MSR `X2APIC_MSR_BASE + offset / 16`.
const REG_ID: u32 = 0x020;
const REG_VERSION: u32 = 0x030;
const REG_TPR: u32 = 0x080;
const REG_EOI: u32 = 0x0B0;
const REG_SPURIOUS: u32 = 0x0F0;
const REG_LVT_TIMER: u32 = 0x320;
const REG_TIMER_INITIAL_COUNT: u32 = 0x380;
const REG_TIMER_CURRENT_COUNT: u32 = 0x390;
const REG_TIMER_DIVIDE: u32 = 0x3E0;

/// Первый MSR блока x2APIC.
const X2APIC_MSR_BASE: u32 = 0x800;

/// `SVR`, бит 8: программное включение APIC. Без него APIC остаётся глухим,
/// сколько бы ни был установлен бит `EN` в `IA32_APIC_BASE`.
const SVR_APIC_ENABLE: u32 = 1 << 8;

/// `LVT`, бит 16: вход замаскирован.
const LVT_MASKED: u32 = 1 << 16;
/// `LVT Timer`, биты 17..18 = 01: периодический режим.
const LVT_TIMER_PERIODIC: u32 = 1 << 17;

/// `Divide Configuration` = делить на 16.
///
/// Кодировка неочевидная: значение собирается из битов 0, 1 и 3, бит 2
/// пропущен. 0b0011 — это деление на 16, 0b1011 — на 1. Шестнадцать выбраны как
/// компромисс: счётчик 32-битный, и на неделёной шине в 1 ГГц (столько
/// показывает QEMU) он оборачивается за четыре секунды, чего для калибровки
/// хватает с запасом, но запаса этого хочется побольше.
const TIMER_DIVIDE_BY_16: u32 = 0b0011;
const TIMER_DIVISOR: u64 = 16;

/// Виртуальный адрес окна xAPIC; ноль означает «не в режиме xAPIC».
static MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
/// Работаем ли через MSR.
static X2APIC: AtomicBool = AtomicBool::new(false);

/// Прочитать регистр APIC.
fn read_reg(offset: u32) -> u32 {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: режим x2APIC подтверждён через CPUID и включён в
        // `IA32_APIC_BASE`, поэтому MSR блока 0x800 существуют. Смещения
        // регистров — из фиксированного списка констант этого модуля.
        return unsafe { rdmsr(X2APIC_MSR_BASE + (offset >> 4)) } as u32;
    }
    let base = MMIO_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    // SAFETY: `base` записан только в `init` и только после того, как окно
    // отображено как `PageFlags::DEVICE`; смещение — константа < 4096, то есть
    // внутри той же страницы. `volatile` обязателен: это регистр устройства, и
    // обычное чтение компилятор вправе выбросить или переупорядочить.
    unsafe { ((base + offset as usize) as *const u32).read_volatile() }
}

/// Записать регистр APIC.
fn write_reg(offset: u32, value: u32) {
    if X2APIC.load(Ordering::Relaxed) {
        // SAFETY: см. `read_reg`. Все регистры, в которые пишет этот модуль
        // (TPR, EOI, SVR, LVT, счётчики таймера), в x2APIC доступны на запись.
        unsafe { wrmsr(X2APIC_MSR_BASE + (offset >> 4), u64::from(value)) };
        return;
    }
    let base = MMIO_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return;
    }
    // SAFETY: см. `read_reg`.
    unsafe { ((base + offset as usize) as *mut u32).write_volatile(value) };
}

/// Подтвердить обработку прерывания.
///
/// Вызывается из обработчика, поэтому не печатает и не аллоцирует.
pub fn eoi() {
    write_reg(REG_EOI, 0);
}

// --- Выбор режима -------------------------------------------------------------

/// Лист CPUID с основными флагами возможностей.
const CPUID_FEATURES: u32 = 1;
/// `CPUID.01H:EDX[9]` — на кристалле есть Local APIC.
const CPUID_EDX_APIC: u32 = 1 << 9;
/// `CPUID.01H:ECX[21]` — APIC умеет режим x2APIC.
const CPUID_ECX_X2APIC: u32 = 1 << 21;

/// Включить APIC и выбрать режим доступа к его регистрам.
///
/// Возвращает `false`, если APIC на этой машине нет вовсе.
///
/// # Выбор между xAPIC и x2APIC
///
/// При наличии x2APIC берётся он. Причина не в скорости (её тут не измерить), а
/// в количестве предположений: регистры адресуются через MSR, а значит не нужно
/// ни отображать окно MMIO, ни задумываться о его кешируемости, ни надеяться,
/// что карта памяти прошивки вообще описывает 0xFEE0_0000 — на большинстве
/// машин не описывает. Меньше кода между «включили» и «работает» — меньше мест,
/// где всё может отказать беззвучно.
///
/// Режим xAPIC остаётся полноценным запасным путём: он единственный доступен на
/// процессорах до Nehalem и на части моделей CPU в эмуляторах.
fn enable_apic() -> bool {
    let features = cpuid(CPUID_FEATURES, 0);
    if features.edx & CPUID_EDX_APIC == 0 {
        kprintln!("  apic        : CPU reports no local APIC; no timer will be available");
        return false;
    }

    // SAFETY: наличие APIC подтверждено CPUID, а вместе с ним и наличие
    // `IA32_APIC_BASE` — он появился одновременно с APIC.
    let base_msr = unsafe { rdmsr(IA32_APIC_BASE) };
    let phys = PhysAddr::new(base_msr & APIC_BASE_ADDR_MASK);
    let is_bsp = base_msr & APIC_BASE_BSP != 0;

    if features.ecx & CPUID_ECX_X2APIC != 0 {
        // Переход в x2APIC разрешён только из состояния «включённый xAPIC»:
        // прямой скачок из выключенного (EN=0) сразу в EN=1,EXTD=1 архитектурно
        // объявлен недопустимым и даёт #GP. Поэтому два `wrmsr`, а не один.
        // SAFETY: тот же MSR, что прочитан выше; добавляется только бит `EN`, а
        // затем `EXTD`, поддержка которого подтверждена CPUID.
        unsafe {
            wrmsr(IA32_APIC_BASE, base_msr | APIC_BASE_ENABLE);
            wrmsr(IA32_APIC_BASE, base_msr | APIC_BASE_ENABLE | APIC_BASE_EXTD);
        }
        X2APIC.store(true, Ordering::Relaxed);
        kprintln!("  apic        : x2APIC mode (MSR block {X2APIC_MSR_BASE:#x}), bsp={is_bsp}");
        return true;
    }

    let Some(window) = map_apic_window(phys) else {
        kprintln!("  apic        : cannot map the xAPIC window at {phys:?}; no timer");
        return false;
    };
    // SAFETY: см. выше; выставляется только бит `EN`, базовый адрес и режим
    // сохраняются как есть.
    unsafe { wrmsr(IA32_APIC_BASE, base_msr | APIC_BASE_ENABLE) };
    MMIO_BASE.store(window, Ordering::Relaxed);
    kprintln!("  apic        : xAPIC mode, window {phys:?} -> {window:#018x}, bsp={is_bsp}");
    true
}

/// Отобразить окно регистров xAPIC и вернуть виртуальный адрес его начала.
///
/// Карта памяти UEFI описывает оперативную память и часть окон прошивки, но
/// регистры Local APIC в неё, как правило, не попадают: они не принадлежат ни
/// одному устройству на шине, а «висят» на самом процессоре. Поэтому
/// рассчитывать на то, что `build_kernel_address_space` их уже отобразил,
/// нельзя, и страница добавляется явно.
///
/// Адрес берётся из прямого отображения, а не identity: identity исчезнет
/// вместе с переездом ядра в верхнюю половину, а прямое — нет.
fn map_apic_window(phys: PhysAddr) -> Option<usize> {
    if !phys.is_page_aligned() {
        return None;
    }
    let virt = phys.to_direct_map();

    // SAFETY: функция вызывается из `init`, то есть уже после того, как ядро
    // переключилось на собственные таблицы (Phase 2 делает это до всего
    // остального). Полученный экземпляр живёт только внутри этого вызова, и
    // другого кода, правящего те же таблицы, в это время нет: ядро однопоточно,
    // а прерывания ещё запрещены.
    let mut space = unsafe { paging::active_address_space() };

    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;
    // SAFETY: страница по этому виртуальному адресу либо ещё не отображена,
    // либо отображена на тот же самый физический кадр (прямое отображение по
    // построению взаимно однозначно), поэтому запись не может увести из-под ног
    // работающий код. `map` сам откажет с `AlreadyMapped`, если это не так.
    let result = crate::mm::frame::with(|frames| unsafe { space.map(virt, phys, flags, frames) });

    match result {
        Some(Ok(())) => Some(virt.as_usize()),
        Some(Err(error)) => {
            kprintln!("  apic        : mapping the xAPIC window failed: {error}");
            None
        }
        None => {
            kprintln!("  apic        : frame allocator unavailable, cannot map the xAPIC window");
            None
        }
    }
}

// --- Калибровка таймера -------------------------------------------------------

/// Опорная частота PIT 8254: 105/88 МГц, поделённые на три. Число зашито в
/// железо IBM PC и с тех пор не менялось.
const PIT_FREQUENCY: u64 = 1_193_182;

/// Порты PIT и вентиля канала 2.
const PIT_CHANNEL2_DATA: u16 = 0x42;
const PIT_COMMAND: u16 = 0x43;
/// Порт управления: бит 0 — вентиль канала 2, бит 1 — динамик,
/// бит 5 — состояние выхода канала 2.
const PIT_CONTROL_PORT: u16 = 0x61;
const PIT_GATE2_ENABLE: u8 = 1 << 0;
const PIT_SPEAKER_ENABLE: u8 = 1 << 1;
const PIT_CHANNEL2_OUTPUT: u8 = 1 << 5;

/// Слово управления: канал 2, доступ младший-затем-старший байт, режим 0
/// (сигнал на выходе по достижении нуля), двоичный счёт.
const PIT_COMMAND_CH2_MODE0: u8 = 0b1011_0000;

/// Длительность мерного окна: 10 мс.
///
/// Меньше — и разрешение PIT (около 0.84 мкс) начинает заметно влиять на
/// результат; больше — и загрузка ощутимо замедляется на ровном месте. При 10 мс
/// погрешность одного отсчёта PIT даёт около 0.01 %, что для 100 Гц не значит
/// ничего.
const CALIBRATION_LATCH: u64 = PIT_FREQUENCY / 100;

/// Сколько раз опросить выход PIT, прежде чем признать его неработающим.
///
/// Ровно та же логика, что у `TX_SPIN_LIMIT` в UART: на машине без PIT порт
/// читается как 0x00 или 0xFF, и цикл ожидания стал бы вечным. Настоящее
/// ожидание — десятки тысяч итераций, так что запас здесь двузначный.
const PIT_SPIN_LIMIT: u32 = 2_000_000;

/// Разумные границы измеренной частоты счётчика. Всё, что вне их, — признак
/// того, что мерили не то: программировать таймер по такому числу опаснее, чем
/// остаться без таймера.
const MIN_PLAUSIBLE_HZ: u64 = 100_000;
const MAX_PLAUSIBLE_HZ: u64 = 10_000_000_000;

/// Измерить частоту счётчика Local APIC по независимому источнику.
///
/// # Почему вообще нужна калибровка
///
/// Архитектура не задаёт частоту, с которой считает таймер Local APIC: это
/// частота шины (или ядра, делённая на известный множитель), и она разная у
/// каждой модели процессора и у каждого эмулятора. Единственный способ узнать
/// её — сравнить с источником, чья частота задана извне.
///
/// # Почему PIT, а не что-то другое
///
/// ACPI PM Timer подошёл бы не хуже, но до него надо добраться: разобрать RSDP,
/// найти FADT, вычитать оттуда адрес порта. Это отдельная подсистема, которой у
/// ядра пока нет. PIT же адресуется четырьмя фиксированными портами, известен с
/// 1981 года и присутствует на всём, что притворяется PC-совместимым, включая
/// `-machine q35`.
///
/// # Почему не TSC-deadline
///
/// Он избавил бы от делителя и от 32-битного счётчика, но не от калибровки: имя
/// «deadline» означает абсолютное значение TSC, а частоту TSC всё равно
/// пришлось бы измерять тем же PIT. Вдобавок режим строго одноразовый — на
/// каждом тике обработчик обязан взвести следующий дедлайн, и промах (тик
/// пришёл позже, чем взвели) даёт не сдвиг фазы, как у периодического таймера,
/// а потерянный тик. Ради регулярного 100 Гц это лишний риск.
///
/// Возвращает частоту счётчика в герцах (уже с учётом делителя).
fn calibrate() -> Option<u64> {
    // Канал 2 выбран потому, что только у него вентиль и выход доступны
    // программе (порт 0x61); каналы 0 и 1 заведены на контроллер прерываний и
    // на регенерацию памяти, и наблюдать их без прерываний нельзя.
    //
    // SAFETY: порты 0x40..0x43 и 0x61 закреплены за PIT и регистром управления
    // на всех PC-совместимых машинах. Динамик при этом выключается (бит 1
    // сбрасывается), поэтому измерение остаётся беззвучным, а никакого другого
    // потребителя у канала 2 в ядре нет.
    let elapsed = unsafe {
        let control = inb(PIT_CONTROL_PORT);
        outb(PIT_CONTROL_PORT, (control & !PIT_SPEAKER_ENABLE) | PIT_GATE2_ENABLE);

        outb(PIT_COMMAND, PIT_COMMAND_CH2_MODE0);
        // Счёт начинается по записи старшего байта, поэтому таймер APIC
        // запускается сразу следом: разница в одну запись в порт на фоне 10 мс
        // пренебрежимо мала.
        outb(PIT_CHANNEL2_DATA, (CALIBRATION_LATCH & 0xFF) as u8);
        outb(PIT_CHANNEL2_DATA, (CALIBRATION_LATCH >> 8) as u8);
        write_reg(REG_TIMER_INITIAL_COUNT, u32::MAX);

        // В режиме 0 выход опускается при записи слова управления и поднимается
        // при достижении нуля. Если он уже поднят — PIT не считает, и ждать
        // нечего.
        if inb(PIT_CONTROL_PORT) & PIT_CHANNEL2_OUTPUT != 0 {
            write_reg(REG_TIMER_INITIAL_COUNT, 0);
            return None;
        }

        let mut spins = 0u32;
        while inb(PIT_CONTROL_PORT) & PIT_CHANNEL2_OUTPUT == 0 {
            spins += 1;
            if spins >= PIT_SPIN_LIMIT {
                write_reg(REG_TIMER_INITIAL_COUNT, 0);
                return None;
            }
        }

        let remaining = read_reg(REG_TIMER_CURRENT_COUNT);
        // Остановить счётчик: нулевой начальный счёт — это «таймер выключен».
        write_reg(REG_TIMER_INITIAL_COUNT, 0);
        u32::MAX - remaining
    };

    if elapsed == 0 {
        return None;
    }

    let hz = u64::from(elapsed) * PIT_FREQUENCY / CALIBRATION_LATCH;
    if !(MIN_PLAUSIBLE_HZ..=MAX_PLAUSIBLE_HZ).contains(&hz) {
        return None;
    }
    Some(hz)
}

// --- Инициализация ------------------------------------------------------------

/// Замаскировать 8259, поднять Local APIC и запустить системный таймер.
///
/// Прерывания при выходе остаются запрещёнными: таймер уже тикает, но флаг `IF`
/// сброшен, и первое прерывание будет доставлено только после `sti`.
pub fn init() {
    disable_legacy_pic();
    kprintln!("  8259        : vectors moved to {PIC1_VECTOR_BASE:#04x}, all inputs masked");

    if !enable_apic() {
        return;
    }

    // Программное включение и вектор для спурьёзных прерываний. До этой записи
    // APIC не доставляет ничего, поэтому она идёт раньше настройки таймера.
    write_reg(REG_SPURIOUS, SVR_APIC_ENABLE | u32::from(VECTOR_SPURIOUS));

    // Порог приоритета: ноль означает «пропускать всё». Прошивка могла оставить
    // здесь ненулевое значение, и тогда таймер, чей приоритет определяется
    // старшим полубайтом вектора (0x20 -> класс 2), молча не доставлялся бы.
    write_reg(REG_TPR, 0);

    let id = read_reg(REG_ID);
    let version = read_reg(REG_VERSION);
    kprintln!(
        "  apic        : id {}, version {:#04x}, {} LVT entries",
        if X2APIC.load(Ordering::Relaxed) { id } else { id >> 24 },
        version & 0xFF,
        ((version >> 16) & 0xFF) + 1
    );

    start_timer();
}

/// Откалибровать и запустить периодический таймер.
fn start_timer() {
    write_reg(REG_TIMER_DIVIDE, TIMER_DIVIDE_BY_16);
    // На время измерения вход замаскирован и режим одноразовый: досчитав до
    // нуля, счётчик должен остановиться, а не начать сначала.
    write_reg(REG_LVT_TIMER, LVT_MASKED);

    let Some(hz) = calibrate() else {
        kprintln!("  timer       : calibration against the PIT failed; timer left disabled");
        kprintln!("                (a timer running at an unknown rate would make uptime lie)");
        return;
    };

    let initial = hz / u64::from(TIMER_HZ);
    if initial == 0 || initial > u64::from(u32::MAX) {
        kprintln!("  timer       : {TIMER_HZ} Hz is out of range for a {hz} Hz counter; disabled");
        return;
    }

    // Порядок важен: сначала LVT (вектор и периодический режим), затем
    // начальный счёт. Запись начального счёта и есть запуск, и к этому моменту
    // APIC уже должен знать, какой вектор доставлять.
    write_reg(REG_LVT_TIMER, LVT_TIMER_PERIODIC | u32::from(VECTOR_TIMER));
    write_reg(REG_TIMER_INITIAL_COUNT, initial as u32);

    kprintln!(
        "  timer       : {} Hz on vector {:#04x}, count {} (bus {}.{:03} MHz)",
        TIMER_HZ,
        VECTOR_TIMER,
        initial,
        hz * TIMER_DIVISOR / 1_000_000,
        (hz * TIMER_DIVISOR / 1000) % 1000
    );
}
