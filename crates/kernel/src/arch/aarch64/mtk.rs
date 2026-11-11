//! То немногое, что приходится знать про MediaTek лично.
//!
//! Всё остальное описание машины приезжает из дерева устройств и ни к какому
//! производителю не привязано. Здесь — два исключения, и оба выяснены на
//! аппарате, а не выведены рассуждением.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use fdt::Fdt;

/// Ключ в старших битах регистра режима сторожевого таймера.
///
/// Без него запись не проходит вовсе — регистр её отбрасывает молча. Всё
/// остальное значение нулевое, и разрешение таймера в том числе.
const WATCHDOG_KEY: u32 = 0x2200_0000;

/// Регистр сброса всей машины и ключ к нему.
///
/// Тот же блок, что и сторожевой таймер: он и сбрасывает аппарат, когда его
/// перестают пинать, — значит умеет сделать это и по команде. Числа из
/// открытого драйвера `mtk_wdt.c`, где они одни на всё семейство.
const WATCHDOG_SWRST: usize = 0x14;
const WATCHDOG_SWRST_KEY: u32 = 0x1209;

/// Где лежит этот блок на **этой** машине. Ноль — дерево о нём не сказало.
///
/// Запоминается при гашении таймера, потому что второй раз дерево прочитать уже
/// негде: к тому мигу, когда перезагрузка понадобится, его память давно отдана
/// под кучу.
static WATCHDOG_BASE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Погасить сторожевой таймер — самым первым делом.
///
/// # Почему первым
///
/// Аппарат сбрасывается примерно через минуту, если таймер не пинать, а ядро до
/// первого пинка не доживает и дожить не должно. Но важнее другое: пока таймер
/// жив, «машина встала» и «машина перезагрузилась» — два разных исхода, а на
/// аппарате без линии и без экрана это **единственный** различимый бит. Гасить
/// его надо там, где он делит опыт пополам правильно; дважды выбранное не то
/// место стоило по заходу каждое.
///
/// Адрес берётся из дерева, а не из константы: у MT676x это `toprgu` по
/// `0x10007000`, но вбитое сюда число означало бы ядро, живущее ровно на одном
/// аппарате. У машин без такого узла (QEMU `virt`) функция не делает ничего.
///
/// # Safety
///
/// Вызывать до включения MMU или на тождественном отображении: адрес из дерева
/// — физический.
pub unsafe fn disable_watchdog(fdt: &Fdt<'_>) {
    let Some(node) = fdt
        .find("/toprgu")
        .or_else(|| fdt.find_compatible("mediatek,mt6589-wdt"))
        .or_else(|| fdt.find_compatible("mediatek,mt6577-wdt"))
    else {
        return;
    };
    let (address_cells, size_cells) = root_cells(fdt);
    let Some(region) = node.reg(address_cells, size_cells).next() else {
        return;
    };
    if region.address == 0 {
        return;
    }
    WATCHDOG_BASE.store(region.address, core::sync::atomic::Ordering::Relaxed);
    // SAFETY: адрес взят из дерева машины и указывает на регистры устройства;
    // запись обязана быть volatile, иначе компилятор вправе её выбросить.
    unsafe { (region.address as *mut u32).write_volatile(WATCHDOG_KEY) };
}

/// Эта ли машина — MediaTek.
///
/// Признак — найденный в дереве сторожевой таймер: его адрес запоминается при
/// загрузке, и на машине другого рода он остаётся нулём. Спрашивают об этом не
/// из любопытства, а там, где ответ меняет слова: у такой машины нет ACPI, то
/// есть нет и PSCI, и фраза «прошивка не объявила PSCI» сказала бы человеку о
/// его телефоне ровно ничего.
#[must_use]
pub fn present() -> bool {
    WATCHDOG_BASE.load(core::sync::atomic::Ordering::Relaxed) != 0
}

/// Перезагрузить аппарат.
///
/// # Зачем это здесь
///
/// Чтобы человек не держал кнопку. Круг работы над телефоном — прошить,
/// запустить, посмотреть — упирался в единственный шаг, который нельзя сделать
/// с компьютера: наше ядро не Android, и `adb` в нём нет, поэтому вернуть
/// аппарат в загрузчик можно было только зажав питание на десять секунд. За
/// вечер таких зажатий набирается два десятка.
///
/// Машина уходит в **обычную** загрузку, то есть в Android, а оттуда `adb`
/// доводит её до загрузчика сам. Просить загрузчик сразу открыть fastboot мы не
/// умеем: это делается записью в служебный раздел, а драйвера памяти у нас нет.
///
/// Возвращает `false`, если блока в дереве не нашлось, — на такой машине
/// перезагрузка отсюда невозможна, и притворяться иначе не стоит.
pub fn reboot() -> bool {
    let base = WATCHDOG_BASE.load(core::sync::atomic::Ordering::Relaxed);
    if base == 0 {
        return false;
    }
    // Через общее окно регистров (`mtk_regs::write32`), а не своим
    // отображением страницы (05.11): ту же страницу TOPRGU отображают и другие
    // — сброс блока связи (`0x10007018`), сторожевой таймер ниже, — а второе
    // отображение той же страницы ядро отвергает, и перезагрузка молча не
    // случилась бы. Возврата из удавшейся записи не предполагается.
    super::mtk_regs::write32(base + WATCHDOG_SWRST as u64, WATCHDOG_SWRST_KEY)
}

// ---------------------------------------------------------------------------
// Сторожевой таймер как страховка от зависания (05.11)
// ---------------------------------------------------------------------------
//
// NetSurf вешал телефон целиком: экран стоит, USB молчит, паники нет, выход —
// зажать кнопки, и журнал при этом пропадал. Теперь таймер TOPRGU, погашенный
// при входе ([`disable_watchdog`]), снова включается, когда заработала задача
// кнопки питания, и она же пинает его каждые 5–30 мс. Встал планировщик или
// прерывания — пинков нет, и через [`WATCHDOG_SECONDS`] машина сбрасывается
// сама; метка «в fastboot» в часах PMIC, поставленная перед включением,
// отправляет LK в fastboot, а не в MIUI, и журнал зависшей загрузки остаётся в
// памяти ([`crate::lastlog`]).
//
// Пинать из обработчика прерывания таймера нельзя нарочно: машина с живыми
// прерываниями, но вставшим планировщиком — тоже зависшая. Числа — из
// `mtk_wdt.c` (Linux), одни на всё семейство.

const WDT_MODE: u64 = 0x00;
const WDT_LENGTH: u64 = 0x04;
const WDT_RESTART: u64 = 0x08;
const WDT_STATUS: u64 = 0x0c;
const WDT_NONRST: u64 = 0x20;
const WDT_NONRST2: u64 = 0x24;
/// Биты `WDT_MODE`: разрешение; внешний вывод сброса (к PMIC); прерывание
/// вместо сброса; двойной режим (сначала прерывание, потом сброс).
const WDT_MODE_EN: u32 = 1 << 0;
const WDT_MODE_EXRST_EN: u32 = 1 << 2;
const WDT_MODE_IRQ_EN: u32 = 1 << 3;
const WDT_MODE_DUAL_EN: u32 = 1 << 6;
/// Ключ в младших битах `WDT_LENGTH` и сдвиг самого срока.
const WDT_LENGTH_KEY: u32 = 0x08;
const WDT_LENGTH_SHIFT: u32 = 5;
/// Единица срока — 512 тактов часов 32 кГц, 1/64 секунды; поле в 11 бит,
/// предел около 32 с.
const WDT_UNITS_PER_SECOND: u32 = 64;
const WDT_LENGTH_MAX: u32 = 0x7ff;
const WDT_RESTART_KEY: u32 = 0x1971;

/// Срок, после которого непнутый таймер сбрасывает машину.
///
/// В сотни раз больше шага задачи кнопки и всё равно меньше, чем человек
/// смотрит на замёрзший экран, прежде чем зажать кнопки сам. Самое долгое
/// законное ожидание — удержание журнала загрузки на 30 с (`hold_boot_log`) —
/// идёт до запуска планировщика, когда таймер ещё погашен.
pub const WATCHDOG_SECONDS: u32 = 20;

/// Включать ли таймер (`oem wdt off` снимает).
static WATCHDOG_WANTED: AtomicBool = AtomicBool::new(true);
/// Таймер включён нами и пинается.
static WATCHDOG_ON: AtomicBool = AtomicBool::new(false);
/// Задача кнопки питания работает — пинать есть кому.
static WATCHDOG_KICKER: AtomicBool = AtomicBool::new(false);
static WATCHDOG_KICKS: AtomicU64 = AtomicU64::new(0);
static WATCHDOG_LAST_KICK_MS: AtomicU64 = AtomicU64::new(0);

fn watchdog_reg(offset: u64) -> Option<u64> {
    let base = WATCHDOG_BASE.load(Ordering::Relaxed);
    (base != 0).then_some(base + offset)
}

/// Завести таймер на `seconds` секунд и разрешить его.
fn watchdog_start(seconds: u32) -> bool {
    use super::mtk_regs::{read32, write32};
    let (Some(mode_reg), Some(length_reg), Some(restart_reg)) =
        (watchdog_reg(WDT_MODE), watchdog_reg(WDT_LENGTH), watchdog_reg(WDT_RESTART))
    else {
        return false;
    };
    let Some(mode) = read32(mode_reg) else {
        return false;
    };
    let length = seconds.saturating_mul(WDT_UNITS_PER_SECOND).clamp(1, WDT_LENGTH_MAX);
    // Сначала срок и пинок, потом разрешение: иначе таймер пошёл бы со сроком,
    // оставленным загрузчиком.
    if !(write32(length_reg, (length << WDT_LENGTH_SHIFT) | WDT_LENGTH_KEY)
        && write32(restart_reg, WDT_RESTART_KEY))
    {
        return false;
    }
    // Без прерывания и без двойного режима — сразу сброс: прерывание таймера
    // обработать некому. Без внешнего вывода — так же, как `reboot` выше:
    // сброс внутри SoC, PMIC питание с памяти не снимает, и журнал выживает.
    let mode = (mode & 0xff & !(WDT_MODE_IRQ_EN | WDT_MODE_DUAL_EN | WDT_MODE_EXRST_EN))
        | WDT_MODE_EN
        | WATCHDOG_KEY;
    if !write32(mode_reg, mode) {
        return false;
    }
    WATCHDOG_LAST_KICK_MS.store(crate::time::uptime_ms(), Ordering::Relaxed);
    WATCHDOG_ON.store(true, Ordering::Release);
    true
}

/// Запретить таймер.
fn watchdog_stop() -> bool {
    use super::mtk_regs::{read32, write32};
    WATCHDOG_ON.store(false, Ordering::Release);
    let Some(mode_reg) = watchdog_reg(WDT_MODE) else {
        return false;
    };
    let Some(mode) = read32(mode_reg) else {
        return false;
    };
    write32(mode_reg, (mode & 0xff & !WDT_MODE_EN) | WATCHDOG_KEY)
}

/// Пнуть таймер. Зовёт задача кнопки питания на каждом шаге.
pub fn watchdog_kick() {
    if !WATCHDOG_ON.load(Ordering::Acquire) {
        return;
    }
    if let Some(reg) = watchdog_reg(WDT_RESTART)
        && super::mtk_regs::write32(reg, WDT_RESTART_KEY)
    {
        WATCHDOG_KICKS.fetch_add(1, Ordering::Relaxed);
        WATCHDOG_LAST_KICK_MS.store(crate::time::uptime_ms(), Ordering::Relaxed);
    }
}

/// Самое долгое молчание задачи-пинальщика (мс), замеченное из тика.
pub static WATCHDOG_STARVED_MAX_MS: AtomicU64 = AtomicU64::new(0);

/// Из тика таймера (10.11, диагностика): запомнить, сколько задача кнопки
/// питания молчала (голодание задач видно в сводке Wi-Fi). Не пинает:
/// пинок отсюда прятал бы настоящее зависание задач. Печати нет: тик — в
/// прерывании, а замок журнала мог держать прерванный код.
pub fn watchdog_tick() {
    if !WATCHDOG_ON.load(Ordering::Acquire) {
        return;
    }
    let silent = crate::time::uptime_ms().saturating_sub(WATCHDOG_LAST_KICK_MS.load(Ordering::Relaxed));
    WATCHDOG_STARVED_MAX_MS.fetch_max(silent, Ordering::Relaxed);
}

/// Задача кнопки питания заработала: поставить метку «в fastboot» и включить
/// таймер.
///
/// Метка — первой, и без неё таймер не включается: сброс, попавший в MIUI,
/// стоит человеку `adb reboot bootloader`, а журналу — жизни (Android
/// переписывает память). LK снимает метку сам, когда открывает по ней
/// fastboot; перезагрузка по просьбе человека снимает её перед сбросом
/// ([`prepare_clean_reboot`]).
pub fn watchdog_arm() {
    WATCHDOG_KICKER.store(true, Ordering::Release);
    if !WATCHDOG_WANTED.load(Ordering::Relaxed) {
        return;
    }
    match super::mtk_regs::mark_fastboot() {
        Ok(pdn1) => {
            crate::kprintln!("  watchdog    : RTC fastboot mark set (RTC_PDN1 {pdn1:#06x}): a reset lands in fastboot");
        }
        Err(err) => {
            crate::kprintln!("  watchdog    : cannot set the RTC fastboot mark ({err:?}); watchdog stays off");
            return;
        }
    }
    if watchdog_start(WATCHDOG_SECONDS) {
        crate::kprintln!("  watchdog    : on, {WATCHDOG_SECONDS} s, kicked by the power key task (`oem wdt off` stops it)");
    } else {
        crate::kprintln!("  watchdog    : TOPRGU does not answer; watchdog stays off");
    }
}

/// `oem wdt on | off`: включить вместе с меткой «в fastboot» или выключить
/// вместе с ней.
pub fn watchdog_enable(on: bool) -> Result<(), &'static str> {
    if on {
        if !WATCHDOG_KICKER.load(Ordering::Acquire) {
            return Err("power key task not running: nothing would kick it");
        }
        super::mtk_regs::mark_fastboot().map_err(|_| "cannot set the RTC fastboot mark")?;
        WATCHDOG_WANTED.store(true, Ordering::Relaxed);
        if !watchdog_start(WATCHDOG_SECONDS) {
            return Err("TOPRGU does not answer");
        }
        crate::kprintln!("  watchdog    : on by fastboot");
    } else {
        WATCHDOG_WANTED.store(false, Ordering::Relaxed);
        if !watchdog_stop() {
            return Err("TOPRGU does not answer");
        }
        super::mtk_regs::clear_fastboot().map_err(|_| "watchdog off, but the RTC fastboot mark stays")?;
        crate::kprintln!("  watchdog    : off by fastboot");
    }
    Ok(())
}

/// Строки для `oem wdt`: состояние, регистры TOPRGU и метка в часах.
pub fn watchdog_report() -> alloc::vec::Vec<alloc::string::String> {
    use alloc::format;
    let read = |offset| watchdog_reg(offset).and_then(super::mtk_regs::read32).unwrap_or(0xffff_ffff);
    let ago = crate::time::uptime_ms().saturating_sub(WATCHDOG_LAST_KICK_MS.load(Ordering::Relaxed));
    let mut lines = alloc::vec::Vec::new();
    lines.push(format!(
        "wdt {}, {WATCHDOG_SECONDS} s, {} kicks, last {ago} ms ago",
        if WATCHDOG_ON.load(Ordering::Relaxed) { "on" } else { "off" },
        WATCHDOG_KICKS.load(Ordering::Relaxed),
    ));
    lines.push(format!("mode {:#x} length {:#x}", read(WDT_MODE), read(WDT_LENGTH)));
    lines.push(format!(
        "status {:#x} nonrst {:#x} {:#x}",
        read(WDT_STATUS),
        read(WDT_NONRST),
        read(WDT_NONRST2)
    ));
    lines.push(match super::mtk_regs::fastboot_marked() {
        Ok((set, pdn1)) => format!(
            "rtc fastboot mark {} (RTC_PDN1 {pdn1:#06x})",
            if set { "set" } else { "clear" }
        ),
        Err(err) => format!("RTC_PDN1 unreadable: {err:?}"),
    });
    lines
}

/// Перед перезагрузкой по просьбе человека: снять метку «в fastboot»
/// (поставленную при старте ради таймера) — машина уходит в обычную загрузку,
/// как и раньше, — и отметить журнал чистым: следующая загрузка скажет
/// «clean reboot», а не «watchdog?».
pub fn prepare_clean_reboot() {
    if let Err(err) = super::mtk_regs::clear_fastboot() {
        crate::kprintln!("  watchdog    : cannot clear the RTC fastboot mark ({err:?}); the reboot may land in fastboot");
    }
    crate::lastlog::mark_clean();
}

/// Ширина и высота панели `dandelion`. См. [`super::fdt_boot::DEFAULT_SCREEN`].
const WIDTH: u32 = 720;

/// Спросить у контроллера дисплея, откуда он сканирует экран.
///
/// # Почему не из `/chosen`
///
/// Загрузчик кладёт в `/chosen` адрес памяти, которую он **резервирует** под
/// кадровый буфер ядра. Показывает он в этот момент другую: свою заставку. Это
/// выяснилось единственным доступным способом — заливкой: по адресу из
/// `/chosen` экран не менялся минутами, а по адресу из регистра заставка
/// стёрлась сразу.
///
/// Драйвера панели писать не пришлось и не придётся: к моменту входа в ядро
/// панель уже зажжена загрузчиком и сканирует буфер. Достаточно узнать, какой.
///
/// Адреса блоков берутся из дерева, смещения регистров — из открытого драйвера
/// MediaTek в ядре Linux (`mtk_disp_ovl.c`, `mtk_disp_rdma.c`), где они одни и
/// те же во всём семействе. Ни одно из этих чисел не угадано.
///
/// Возвращает адрес буфера и **шаг строки в точках**.
///
/// # Safety
///
/// Вызывать до включения MMU или на тождественном отображении.
pub unsafe fn scanned_buffer(fdt: &Fdt<'_>) -> Option<(u64, u32)> {
    // Смещения внутри блока: адрес слоя и шаг строки. У наложения и у RDMA они
    // разные, поэтому идут парой вместе с путём узла.
    const OVL: (usize, usize) = (0x0f40 / 4, 0x0044 / 4);
    const RDMA: (usize, usize) = (0x0f00 / 4, 0x0f08 / 4);

    let (address_cells, size_cells) = root_cells(fdt);
    for (path, (address_register, pitch_register)) in
        [("/disp_ovl0", OVL), ("/disp_ovl0_2l", OVL), ("/disp_rdma0", RDMA)]
    {
        let Some(node) = fdt.find(path) else { continue };
        let Some(region) = node.reg(address_cells, size_cells).next() else { continue };
        if region.address == 0 {
            continue;
        }
        let block = region.address as *const u32;
        // SAFETY: адрес блока взят из дерева машины; чтение регистра ничего не
        // меняет, а volatile обязателен — обычное чтение компилятор вправе
        // выбросить.
        let (address, pitch) = unsafe {
            (
                u64::from(block.add(address_register).read_volatile()),
                block.add(pitch_register).read_volatile(),
            )
        };

        // Годится только то, что похоже на ОЗУ: у этой машины оно начинается с
        // 0x40000000. Ноль означает выключенный слой, мелкое число — что мы
        // прочитали не тот регистр, и рисовать по нему нельзя.
        if address < 0x4000_0000 {
            continue;
        }

        // Шаг строки — в байтах, и только младшие шестнадцать бит: в старших
        // живут признаки слоя. Первая картинка на аппарате рисовалась в
        // предположении «шаг равен ширине», и рамка расползлась диагональными
        // пунктирами, а окно — косыми полосами. Сам аппарат об этом знает,
        // спросить дешевле, чем подбирать.
        let pitch = (pitch & 0xffff) / 4;
        let pitch = if pitch >= WIDTH { pitch } else { WIDTH };
        return Some((address, pitch));
    }
    None
}

/// Полоса «идёт загрузка» поперёк верха экрана.
///
/// # Зачем
///
/// Пока её не было, заставка загрузчика висела неподвижно всю загрузку, и
/// «идёт» не отличалось от «повисло»: Роман ждал по две минуты, не зная, живой
/// аппарат или нет. Полоса рисуется до всего долгого, и первый же её вид
/// отвечает сразу на три вопроса — код исполняется, дерево разобрано, адрес
/// буфера верный.
///
/// `step` — номер этапа: каждый закрашивает свою строку, так что полоса растёт
/// сверху вниз и по ней видно, где именно ядро остановилось.
///
/// Старший байт точки — прозрачность, и он обязан быть `0xff`. С нулём слой
/// смешивается с фоном и выводит чёрное: заливка идёт, заставка стирается,
/// экран чернеет. Выглядит как «не работает»; не работает при этом один байт.
///
/// # Safety
///
/// `base` обязан быть адресом кадрового буфера, доступного на запись, а
/// `stride` — его настоящим шагом строки в точках.
pub unsafe fn progress(base: u64, stride: u32, step: u32) {
    /// Высота одной ступени в строках экрана.
    const ROWS: u32 = 12;
    /// Цвета ступеней: зелёный, жёлтый, голубой. Больше трёх не нужно — дальше
    /// начинает печатать консоль, и она видна сама.
    const COLOURS: [u32; 3] = [0xff00_c000, 0xffc0_c000, 0xff00_c0c0];

    let colour = COLOURS[(step as usize) % COLOURS.len()];
    let first = step * ROWS;
    for row in first..first + ROWS {
        for column in 0..WIDTH {
            let offset = (row * stride + column) as usize;
            // SAFETY: контракт функции; полоса лежит в первых строках буфера,
            // то есть заведомо внутри него.
            unsafe { (base as *mut u32).add(offset).write_volatile(colour) };
        }
    }
}

/// Размеры ячеек корня. Если их нет — те, что предписывает формат.
fn root_cells(fdt: &Fdt<'_>) -> (usize, usize) {
    let Some(root) = fdt.nodes().next() else {
        return (2, 1);
    };
    (
        root.property_u64("#address-cells").unwrap_or(2) as usize,
        root.property_u64("#size-cells").unwrap_or(1) as usize,
    )
}

/// Снять тактирование со всех блоков инфраструктуры.
///
/// # Зачем это понадобилось
///
/// Из семи шин I²C аппарата четыре не отвечают вовсе: регистр состояния не
/// меняется ни от чего, и передача упирается в предел ожидания. Так выглядит
/// блок, которому не подаётся такт, — загрузчик включает ровно то, что нужно
/// ему самому (питание, зарядка, панель), а остальное оставляет выключенным.
///
/// Тачскрина нет ни на одной из трёх живых шин: та, что отвечает честно, несёт
/// единственное устройство по адресу `0x6b`. Значит, он на одной из молчащих, и
/// пока к ней не подведён такт, спрашивать её бессмысленно.
///
/// # Почему всё сразу, а не нужное
///
/// Чтобы включить именно нужный, надо знать соответствие «номер такта в дереве
/// → регистр и бит», а это таблица конкретного кристалла, и у нас её нет.
/// Регистры же снятия известны: у всего семейства MediaTek их четыре, и запись
/// единиц в них означает «не глушить ничего». Включённый лишний блок стоит
/// потребления; невключённый нужный стоит захода к аппарату.
///
/// Печатается состояние до и после — иначе «включили» неотличимо от «записали
/// в никуда», а это ровно та разница, ради которой всё и делается.
///
/// # Safety
///
/// Вызывать на активных таблицах ядра; окно блока отображается здесь же.
pub unsafe fn ungate_infra(fdt: &Fdt<'_>) {
    let Some(node) = fdt
        .find("/infracfg_ao@10001000")
        .or_else(|| fdt.find_compatible("mediatek,infracfg_ao"))
        .or_else(|| fdt.find_compatible("mediatek,mt6765-infracfg"))
    else {
        crate::kprintln!("  clocks      : no infracfg node in the device tree");
        return;
    };
    let (address_cells, size_cells) = root_cells(fdt);
    let Some(region) = node.reg(address_cells, size_cells).next() else {
        return;
    };
    let base = region.address;
    if base == 0 || !crate::arch::aarch64::mtk_touch::map_device_page(base) {
        return;
    }

    // Четвёрка «снять / состояние» — раскладка, одинаковая у всего семейства.
    const GATES: [(usize, usize); 4] = [(0x84, 0x90), (0x8c, 0x94), (0xa8, 0xac), (0xc4, 0xc8)];

    for (index, (clear, state)) in GATES.iter().enumerate() {
        // SAFETY: блок назван деревом и отображён выше как память устройства.
        let before = unsafe { ((base as usize + state) as *const u32).read_volatile() };
        // SAFETY: см. выше. Единица в регистре снятия означает «этот блок не
        // глушить»; биты, которым нечего снимать, запись игнорирует.
        unsafe { ((base as usize + clear) as *mut u32).write_volatile(u32::MAX) };
        // SAFETY: см. выше.
        let after = unsafe { ((base as usize + state) as *const u32).read_volatile() };
        crate::kprintln!("  clocks      : infra{index} gates {before:#010x} -> {after:#010x}");
    }
}

/// Управление одним выводом общего назначения.
///
/// # Откуда взяты числа
///
/// Из таблиц драйвера MediaTek для MT6765 (`pinctrl-mtk-mt6765.h`), и они
/// регулярны — что и позволяет обойтись формулой вместо таблицы на двести
/// строк. Сверено на выводе 174, том самом, которым держится в сбросе
/// тачскрин: режим `0x450` бит 24, направление `0x050` бит 14, значение
/// `0x150` бит 14. Формулы дают ровно это.
///
/// | что | адрес | сколько бит на вывод |
/// |---|---|---|
/// | режим | `0x300 + (вывод / 8) * 0x10` | 4, начиная с `(вывод % 8) * 4` |
/// | направление | `0x000 + (вывод / 32) * 0x10` | 1, бит `вывод % 32` |
/// | значение | `0x100 + (вывод / 32) * 0x10` | 1, бит `вывод % 32` |
///
/// # Safety
///
/// `base` обязан быть отображённым окном контроллера выводов, а `pin` —
/// существующим выводом. Ошибка здесь означает переключение чужой линии, и
/// последствия у неё физические, а не программные.
pub unsafe fn gpio_output(base: u64, pin: u32, high: bool) {
    let base_address = base as usize;
    let direction = base_address + (pin as usize / 32) * 0x10;
    let value = base_address + 0x100 + (pin as usize / 32) * 0x10;
    let bit = 1u32 << (pin % 32);

    // Режим 0 — «просто вывод». Пока стоит любой другой, линией распоряжается
    // тот блок, за которым она закреплена, и запись значения не делает ничего.
    // SAFETY: контракт функции.
    unsafe { gpio_mode(base, pin, 0) };

    // SAFETY: контракт функции; volatile обязателен — это регистры.
    unsafe {
        let current = (direction as *const u32).read_volatile();
        (direction as *mut u32).write_volatile(current | bit);

        let current = (value as *const u32).read_volatile();
        let next = if high { current | bit } else { current & !bit };
        (value as *mut u32).write_volatile(next);
    }
}

/// Переключить вывод на функцию другого блока.
///
/// # Зачем это понадобилось
///
/// Загрузчик настраивает выводы только тех блоков, которыми пользуется сам.
/// Шина `spi3`, на которой висит тачскрин, ему не нужна — и её четыре линии
/// остаются в том виде, в каком их оставил сброс. Контроллер при этом работает
/// исправно: такт бежит, передача занимает измеримое время (520 опросов на
/// восемь байт), — только сигналы никуда не выходят, потому что выводы за ним
/// не закреплены. Снаружи это неотличимо от молчащего устройства.
///
/// Номера выводов и номер функции взяты из описания кристалла
/// (`mt6765-pinfunc.h`): у `spi3` это выводы 21, 22, 23 и 24, функция 1 у всех
/// четырёх.
///
/// # Safety
///
/// См. [`gpio_output`]. Переключение чужой линии на чужую функцию — ошибка с
/// физическими последствиями.
pub unsafe fn gpio_mode(base: u64, pin: u32, function: u32) -> u32 {
    let mode = base as usize + 0x300 + (pin as usize / 8) * 0x10;
    let shift = (pin % 8) * 4;
    // SAFETY: контракт функции; volatile обязателен — это регистры.
    unsafe {
        let current = (mode as *const u32).read_volatile();
        let next = (current & !(0xf << shift)) | ((function & 0xf) << shift);
        (mode as *mut u32).write_volatile(next);
        // Читается обратно и отдаётся наружу: запись в регистр, которым
        // распоряжается кто-то ещё, проходит молча и не значит ничего.
        // Прочитанное назад — единственное доказательство, что она подействовала.
        ((mode as *const u32).read_volatile() >> shift) & 0xf
    }
}

/// Настроить вывод на приём и прочитать его уровень.
///
/// Нужно ровно для одного опыта — обмена по SPI вручную, без контроллера (см.
/// [`super::mtk_touch`]). Он отвечает на вопрос, который иначе не закрыть:
/// молчит кристалл или неверно настроен блок, который с ним говорит. Снаружи
/// эти два случая неразличимы, а чинятся в разных местах.
///
/// # Safety
///
/// См. [`gpio_output`].
pub unsafe fn gpio_input(base: u64, pin: u32) -> bool {
    let direction = base as usize + (pin as usize / 32) * 0x10;
    let data = base as usize + 0x200 + (pin as usize / 32) * 0x10;
    let bit = 1u32 << (pin % 32);

    // SAFETY: контракт функции.
    unsafe {
        gpio_mode(base, pin, 0);
        let current = (direction as *const u32).read_volatile();
        (direction as *mut u32).write_volatile(current & !bit);
        (data as *const u32).read_volatile() & bit != 0
    }
}

/// Прочитать настоящий уровень на выводе, которым мы же и управляем.
///
/// Не то же самое, что прочитать, что мы туда записали: регистр значения
/// хранит наше желание, а регистр входа — то, что на выводе на самом деле.
/// Линия, которую держит внизу кто-то ещё, выглядит по записанному значению
/// отпущенной, а по входу — прижатой. Разница между этими двумя регистрами и
/// есть ответ на вопрос «отпустили ли мы сброс».
///
/// # Safety
///
/// См. [`gpio_output`]. Направление вывода не меняется: он остаётся выходом,
/// а вход у MediaTek читается и на выходе.
pub unsafe fn gpio_level(base: u64, pin: u32) -> bool {
    let data = base as usize + 0x200 + (pin as usize / 32) * 0x10;
    let bit = 1u32 << (pin % 32);
    // SAFETY: контракт функции.
    unsafe { (data as *const u32).read_volatile() & bit != 0 }
}
