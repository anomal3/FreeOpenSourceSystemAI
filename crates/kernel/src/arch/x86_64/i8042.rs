//! Контроллер клавиатуры i8042 и разбор scancode set 1.
//!
//! # Почему PS/2 первым, хотя он мёртв
//!
//! На современном железе разъёма PS/2 нет, и клавиатура подключена по USB — то
//! есть настоящий драйвер ввода потребует xHCI и HID, а это недели работы (см.
//! оценку в плане проекта). Зато i8042 эмулируется QEMU на `-machine q35`,
//! адресуется двумя портами и не требует ни DMA, ни колец передачи. Это даёт
//! рабочий ввод сейчас — а вместе с ним и всё, что вокруг ввода: очередь
//! событий, раскладку, редактор строки, маршрутизацию прерываний устройств. К
//! моменту появления USB-стека ему останется только сложить события в готовую
//! очередь.
//!
//! # Два порта и одна путаница
//!
//! Порт 0x60 — данные, 0x64 — состояние (на чтение) и команды (на запись).
//! Дальше начинается то, из-за чего этот контроллер имеет репутацию: **команды
//! бывают двух разных видов**. Записанные в 0x64 адресованы самому контроллеру
//! (включить порт, прочитать конфигурацию, самотест), записанные в 0x60 — той
//! клавиатуре, которая к контроллеру подключена (сбросить, выбрать набор кодов,
//! начать сканирование). Отправить команду клавиатуры в 0x64 — самая частая
//! ошибка в этом драйвере, и она не даёт диагностики: контроллер просто
//! истолкует байт как свою команду.
//!
//! # Про наборы scancode и «трансляцию»
//!
//! Клавиатура после включения работает в наборе 2, а контроллер умеет на лету
//! переводить его в набор 1 (это бит 6 конфигурации, «translation»). Оба режима
//! рабочие, но выбирать надо осознанно: наборы отличаются кодами почти всех
//! клавиш, и код `0x1C` — это Enter в наборе 1 и `C` в наборе 2.
//!
//! Здесь трансляция **включается** явно, а клавиатуре явно задаётся набор 2.
//! То есть драйвер разбирает набор 1 и не зависит от того, что оставила
//! прошивка: OVMF поднимает свой драйвер PS/2 и настраивает контроллер под себя,
//! а её выбор нигде не задокументирован.
//!
//! Набор 1 удобнее набора 2 ровно одним: отпускание в нём — это тот же код со
//! старшим битом, а не префикс `0xF0` перед кодом. Один бит вместо состояния
//! автомата.
//!
//! # Настоящий ноутбук (0.8)
//!
//! На ASUS K53SD не заработали ни клавиатура, ни тачпад. Прежняя настройка
//! отказывалась целиком на первом же шаге без ACK — самотесте контроллера,
//! сбросе клавиатуры, выборе набора, — а контроллеры ноутбуков (это
//! встроенный контроллер, EC) отвечают на эти команды как попало; Linux
//! самотест на x86 по умолчанию и не делает. Теперь обязательны только порты и
//! конфигурация, а каждая команда клавиатуре — попытка: отказ записывается в
//! журнал, и клавиатура остаётся такой, какой её оставила прошивка (а в меню
//! прошивки она работала).
//!
//! Тачпад ноутбука — это мышь PS/2 на втором порту: пока драйвер не включит
//! особый режим, Synaptics и Elantech шлют обычные пакеты по три байта.
//! Второй порт теперь обслуживается: `0xD4` перед каждой командой мыши, сброс,
//! умолчания, включение отчётов, IRQ 12.

use super::interrupts::without_interrupts;
use super::{inb, io_wait, outb};
use crate::input::{KeyCode, post};
use crate::sync::SpinLock;

/// Порт данных.
const PORT_DATA: u16 = 0x60;
/// Порт состояния (чтение) и команд контроллера (запись).
const PORT_COMMAND: u16 = 0x64;

/// Бит 0 состояния: в выходном буфере есть байт для нас.
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
/// Бит 1: входной буфер занят, писать пока нельзя.
const STATUS_INPUT_FULL: u8 = 1 << 1;
/// Бит 5: байт в выходном буфере — от мыши (второго порта).
const STATUS_AUX: u8 = 1 << 5;

// --- Команды контроллера (в порт 0x64) ----------------------------------------

const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_DISABLE_PORT2: u8 = 0xA7;
const CMD_ENABLE_PORT2: u8 = 0xA8;
const CMD_TEST_PORT2: u8 = 0xA9;
const CMD_TEST_PORT1: u8 = 0xAB;
/// Следующий байт в порт данных — мыши, а не клавиатуре.
const CMD_WRITE_PORT2: u8 = 0xD4;
const CMD_DISABLE_PORT1: u8 = 0xAD;
const CMD_ENABLE_PORT1: u8 = 0xAE;

/// Ответ на успешный тест порта.
const PORT_TEST_PASSED: u8 = 0x00;

// --- Биты конфигурации --------------------------------------------------------

/// Бит 0: разрешить прерывание от первого порта. Без него контроллер складывает
/// байты в буфер и молчит — драйвер работал бы только опросом.
const CONFIG_PORT1_INTERRUPT: u8 = 1 << 0;
/// Бит 1: то же для второго порта (мышь). Выключаем: мыши в этой фазе нет, а её
/// байты пришли бы в тот же буфер и были бы истолкованы как коды клавиш.
const CONFIG_PORT2_INTERRUPT: u8 = 1 << 1;
/// Бит 4: **выключить** тактирование первого порта. Единица здесь означает
/// «порт отключён», поэтому бит сбрасывается.
const CONFIG_PORT1_CLOCK_OFF: u8 = 1 << 4;
/// Бит 5: то же для второго порта — его мы, наоборот, выставляем.
const CONFIG_PORT2_CLOCK_OFF: u8 = 1 << 5;
/// Бит 6: трансляция набора 2 в набор 1.
const CONFIG_TRANSLATION: u8 = 1 << 6;

// --- Команды клавиатуры (в порт 0x60) -----------------------------------------

const KBD_SET_SCANCODE_SET: u8 = 0xF0;
const KBD_ENABLE_SCANNING: u8 = 0xF4;
const KBD_DISABLE_SCANNING: u8 = 0xF5;
const KBD_RESET: u8 = 0xFF;

/// Набор scancode, который запрашивается у клавиатуры (контроллер переведёт его
/// в набор 1).
const SCANCODE_SET_2: u8 = 0x02;

/// Клавиатура подтвердила команду.
const KBD_ACK: u8 = 0xFA;
/// Клавиатура просит повторить команду.
const KBD_RESEND: u8 = 0xFE;
/// Самотест клавиатуры пройден (приходит после сброса, вслед за ACK).
const KBD_SELF_TEST_PASSED: u8 = 0xAA;

/// Сколько раз опросить регистр состояния, прежде чем признать контроллер
/// неотвечающим.
///
/// Та же логика, что у `TX_SPIN_LIMIT` в UART: на машине без i8042 порт 0x64
/// читается как 0xFF, то есть «буфер всегда полон и всегда занят», и цикл
/// ожидания без предела стал бы вечным. Реальное ожидание — единицы микросекунд.
const WAIT_SPINS: u32 = 200_000;

/// Сколько раз повторить команду, получив `0xFE`.
const MAX_RESENDS: u32 = 3;

/// Почему клавиатура не поднялась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ps2Error {
    /// Порт состояния читается как 0xFF: контроллера на этой машине нет.
    Absent,
    /// Контроллер не отвечает: ожидание превысило [`WAIT_SPINS`].
    Timeout,
    /// Клавиатура не подтвердила команду.
    NoAck(u8),
}

impl core::fmt::Display for Ps2Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Absent => f.write_str("no i8042 controller on this machine"),
            Self::Timeout => f.write_str("the controller stopped responding"),
            Self::NoAck(got) => write!(f, "the keyboard answered {got:#04x} instead of ACK"),
        }
    }
}

/// Дождаться, пока входной буфер освободится, и записать байт.
fn write_port(port: u16, value: u8) -> Result<(), Ps2Error> {
    for _ in 0..WAIT_SPINS {
        // SAFETY: чтение 0x64 — это чтение регистра состояния, побочных эффектов
        // не имеет.
        if unsafe { inb(PORT_COMMAND) } & STATUS_INPUT_FULL == 0 {
            // SAFETY: буфер свободен; `port` — 0x60 или 0x64, оба закреплены за
            // i8042 на всех PC-совместимых машинах.
            unsafe { outb(port, value) };
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Ps2Error::Timeout)
}

/// Дождаться байта в выходном буфере и забрать его.
fn read_data() -> Result<u8, Ps2Error> {
    for _ in 0..WAIT_SPINS {
        // SAFETY: см. `write_port`.
        if unsafe { inb(PORT_COMMAND) } & STATUS_OUTPUT_FULL != 0 {
            // SAFETY: байт есть; чтение 0x60 извлекает его из буфера — побочный
            // эффект ожидаемый и необходимый.
            return Ok(unsafe { inb(PORT_DATA) });
        }
        core::hint::spin_loop();
    }
    Err(Ps2Error::Timeout)
}

/// Выбросить всё, что осталось в выходном буфере.
///
/// Нужно перед каждым шагом настройки: прошивка могла оставить там свой байт, и
/// он был бы принят за ответ на нашу команду.
fn flush() {
    for _ in 0..16 {
        // SAFETY: см. `write_port`.
        let status = unsafe { inb(PORT_COMMAND) };
        if status == 0xFF || status & STATUS_OUTPUT_FULL == 0 {
            return;
        }
        // SAFETY: байт есть, забираем и выбрасываем.
        let _ = unsafe { inb(PORT_DATA) };
        io_wait();
    }
}

/// Отправить команду клавиатуре и дождаться подтверждения.
fn keyboard_command(command: u8) -> Result<(), Ps2Error> {
    for _ in 0..MAX_RESENDS {
        write_port(PORT_DATA, command)?;
        match read_data()? {
            KBD_ACK => return Ok(()),
            // `0xFE` означает «не разобрала, повтори» — обычная ситуация на
            // настоящем железе при помехах в линии.
            KBD_RESEND => continue,
            other => return Err(Ps2Error::NoAck(other)),
        }
    }
    Err(Ps2Error::NoAck(KBD_RESEND))
}

/// Что поднялось на контроллере.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ps2Devices {
    /// Клавиатура (первый порт).
    pub keyboard: bool,
    /// Мышь или тачпад (второй порт).
    pub mouse: bool,
}

/// Отправить команду мыши (второй порт) и дождаться подтверждения.
fn mouse_command(command: u8) -> Result<(), Ps2Error> {
    for _ in 0..MAX_RESENDS {
        write_port(PORT_COMMAND, CMD_WRITE_PORT2)?;
        write_port(PORT_DATA, command)?;
        match read_data()? {
            KBD_ACK => return Ok(()),
            KBD_RESEND => continue,
            other => return Err(Ps2Error::NoAck(other)),
        }
    }
    Err(Ps2Error::NoAck(KBD_RESEND))
}

/// Поднять контроллер, клавиатуру и мышь.
///
/// Прерывания при выходе разрешены в самом контроллере, но до процессора они
/// не дойдут, пока входы не размаскированы в I/O APIC — это делает
/// вызывающий. Разделение сознательное: пока обработчик не установлен,
/// прерываний быть не должно.
///
/// Обязательны здесь только доступ к портам и конфигурация; всё, что дальше, —
/// попытки, и их отказы печатаются, а не обрывают настройку (см. заголовок
/// модуля, «Настоящий ноутбук»).
///
/// # Safety
///
/// Функция монопольно распоряжается портами 0x60/0x64. Вызывать один раз и до
/// того, как размаскированы входы I/O APIC: последовательность настройки читает
/// ответы устройств из того же буфера, откуда их взял бы обработчик прерывания,
/// и параллельный обработчик съел бы ожидаемый ACK.
pub unsafe fn init() -> Result<Ps2Devices, Ps2Error> {
    // SAFETY: чтение регистра состояния.
    if unsafe { inb(PORT_COMMAND) } == 0xFF {
        return Err(Ps2Error::Absent);
    }

    // Оба порта выключаются на всё время настройки: иначе нажатие клавиши в
    // середине последовательности положило бы scancode в буфер, и он был бы
    // прочитан как ответ на команду.
    write_port(PORT_COMMAND, CMD_DISABLE_PORT1)?;
    write_port(PORT_COMMAND, CMD_DISABLE_PORT2)?;
    flush();

    // Конфигурация читается, а не собирается с нуля: в ней есть биты, значение
    // которых зависит от платформы (системный флаг), и обнулять их наугад
    // незачем. Прерывания выключены до конца настройки.
    write_port(PORT_COMMAND, CMD_READ_CONFIG)?;
    let original = read_data()?;
    let config = (original | CONFIG_TRANSLATION) & !(CONFIG_PORT1_INTERRUPT | CONFIG_PORT2_INTERRUPT);
    write_port(PORT_COMMAND, CMD_WRITE_CONFIG)?;
    write_port(PORT_DATA, config)?;
    crate::kprintln!("  ps2         : controller config {original:#04x} -> {config:#04x}");

    // Самотеста контроллера (0xAA) нет намеренно: на части ноутбуков он
    // сбрасывает контроллер, и клавиатура после него молчит; Linux на x86 его
    // по умолчанию не делает. Тест порта — только для журнала.
    write_port(PORT_COMMAND, CMD_TEST_PORT1)?;
    match read_data() {
        Ok(PORT_TEST_PASSED) => {}
        Ok(other) => crate::kprintln!("  ps2         : keyboard port test returned {other:#04x}; trying anyway"),
        Err(err) => crate::kprintln!("  ps2         : keyboard port test: {err}; trying anyway"),
    }
    write_port(PORT_COMMAND, CMD_ENABLE_PORT1)?;
    flush();

    let keyboard = start_keyboard();
    let mouse = start_mouse();

    // Прерывания разрешаются последним действием: к этому моменту в буфере
    // ничего нашего не осталось, и первый же байт будет настоящим.
    let mut config = config & !CONFIG_PORT1_CLOCK_OFF;
    config |= CONFIG_PORT1_INTERRUPT;
    if mouse {
        config = (config & !CONFIG_PORT2_CLOCK_OFF) | CONFIG_PORT2_INTERRUPT;
    } else {
        config |= CONFIG_PORT2_CLOCK_OFF;
    }
    flush();
    write_port(PORT_COMMAND, CMD_WRITE_CONFIG)?;
    write_port(PORT_DATA, config)?;
    Ok(Ps2Devices { keyboard, mouse })
}

/// Клавиатура: сброс, набор кодов 2 (контроллер переводит в 1), сканирование.
/// Каждый шаг — попытка: прошивка уже оставила клавиатуру работающей.
fn start_keyboard() -> bool {
    let mut answered = false;
    let note = |step: &str, result: Result<(), Ps2Error>| match result {
        Ok(()) => true,
        Err(err) => {
            crate::kprintln!("  ps2         : keyboard {step}: {err}; keeping the firmware's setup");
            false
        }
    };
    let _ = note("disable scanning", keyboard_command(KBD_DISABLE_SCANNING));
    flush();
    if note("reset", keyboard_command(KBD_RESET)) {
        answered = true;
        // После ACK клавиатура сообщает результат самотеста — не все.
        match read_data() {
            Ok(KBD_SELF_TEST_PASSED) => {}
            Ok(_) | Err(_) => flush(),
        }
    }
    if note("scancode set", keyboard_command(KBD_SET_SCANCODE_SET))
        && note("scancode set 2", keyboard_command(SCANCODE_SET_2))
    {
        answered = true;
    }
    answered |= note("enable scanning", keyboard_command(KBD_ENABLE_SCANNING));
    flush();
    if !answered {
        crate::kprintln!("  ps2         : the keyboard answers no command; listening anyway");
    }
    // Клавиатура включена в любом случае: молчание на команды ещё не значит
    // молчание на клавиши.
    true
}

/// Мышь (тачпад) на втором порту: есть ли порт, сброс, умолчания, отчёты.
fn start_mouse() -> bool {
    if write_port(PORT_COMMAND, CMD_ENABLE_PORT2).is_err() || write_port(PORT_COMMAND, CMD_TEST_PORT2).is_err() {
        return false;
    }
    match read_data() {
        Ok(PORT_TEST_PASSED) => {}
        Ok(other) => {
            crate::kprintln!("  ps2         : no mouse port (test returned {other:#04x})");
            let _ = write_port(PORT_COMMAND, CMD_DISABLE_PORT2);
            return false;
        }
        Err(_) => {
            crate::kprintln!("  ps2         : no mouse port");
            let _ = write_port(PORT_COMMAND, CMD_DISABLE_PORT2);
            return false;
        }
    }
    let _ = write_port(PORT_COMMAND, CMD_ENABLE_PORT2);
    flush();
    if let Err(err) = mouse_command(KBD_RESET) {
        crate::kprintln!("  ps2         : nothing answers on the mouse port ({err})");
        let _ = write_port(PORT_COMMAND, CMD_DISABLE_PORT2);
        return false;
    }
    // После сброса: 0xAA (самотест) и номер устройства (0 — обычная мышь).
    let test = read_data().unwrap_or(0);
    let id = read_data().unwrap_or(0xFF);
    flush();
    // Умолчания (100 отчётов в секунду, обычный режим) и включение отчётов.
    let defaults = mouse_command(0xF6);
    let enable = mouse_command(KBD_ENABLE_SCANNING);
    match (defaults, enable) {
        (_, Ok(())) => {
            crate::kprintln!("  ps2         : mouse on port 2 (self-test {test:#04x}, id {id:#04x}), 3-byte packets");
            true
        }
        (_, Err(err)) => {
            crate::kprintln!("  ps2         : the mouse did not enable reporting: {err}");
            let _ = write_port(PORT_COMMAND, CMD_DISABLE_PORT2);
            false
        }
    }
}

// --- Разбор scancode set 1 ----------------------------------------------------

/// Бит 7 кода: это отпускание, а не нажатие.
const RELEASE_FLAG: u8 = 0x80;
/// Префикс расширенного кода.
const PREFIX_EXTENDED: u8 = 0xE0;
/// Префикс последовательности Pause/Break.
const PREFIX_PAUSE: u8 = 0xE1;

/// Сколько байт занимает остаток последовательности Pause после префикса.
///
/// Полная последовательность — `E1 1D 45 E1 9D C5`: шесть байт, из которых
/// первый уже прочитан. Отпускания у Pause не существует вовсе, поэтому событие
/// порождается один раз, а остальные пять байт проглатываются.
const PAUSE_TAIL: u8 = 5;

/// Состояние разбора потока scancode.
struct Decoder {
    /// Получен префикс `0xE0`.
    extended: bool,
    /// Сколько байт осталось проглотить (хвост Pause).
    skip: u8,
}

impl Decoder {
    const fn new() -> Self {
        Self { extended: false, skip: 0 }
    }
}

static DECODER: SpinLock<Decoder> = SpinLock::new(Decoder::new());

/// Обработчик прерывания клавиатуры.
///
/// Вычитывает **все** накопившиеся байты, а не один: контроллер выставляет
/// прерывание по факту непустого буфера, и выйдя после первого байта, мы
/// получили бы либо повторное прерывание на каждый байт (в лучшем случае), либо
/// зависший буфер (в худшем, при доставке по фронту).
pub fn on_interrupt() {
    for _ in 0..16 {
        // SAFETY: чтение регистра состояния побочных эффектов не имеет.
        let status = unsafe { inb(PORT_COMMAND) };
        if status == 0xFF || status & STATUS_OUTPUT_FULL == 0 {
            return;
        }
        // SAFETY: байт есть; чтение извлекает его из буфера.
        let byte = unsafe { inb(PORT_DATA) };
        if status & STATUS_AUX != 0 {
            feed_mouse(byte);
        } else {
            feed(byte);
        }
    }
}

/// Пакет мыши, собираемый по байту.
struct MousePacket {
    bytes: [u8; 3],
    len: usize,
}

static MOUSE: SpinLock<MousePacket> = SpinLock::new(MousePacket { bytes: [0; 3], len: 0 });

/// Принять байт мыши; третий байт пакета — событие.
///
/// Первый байт пакета узнаётся по биту 3 (всегда единица): потерянный байт
/// иначе сдвинул бы все следующие пакеты, и курсор поехал бы наискось.
fn feed_mouse(byte: u8) {
    let packet = {
        let Some(mut mouse) = MOUSE.try_lock() else {
            return;
        };
        if mouse.len == 0 && byte & 0x08 == 0 {
            return;
        }
        let at = mouse.len;
        mouse.bytes[at] = byte;
        mouse.len += 1;
        if mouse.len < 3 {
            return;
        }
        mouse.len = 0;
        mouse.bytes
    };
    let [flags, x, y] = packet;
    // Переполнение (биты 6 и 7) — приращение недостоверно, пакет выбрасывается.
    if flags & 0xC0 != 0 {
        return;
    }
    let dx = i32::from(x) - if flags & 0x10 != 0 { 256 } else { 0 };
    let dy = i32::from(y) - if flags & 0x20 != 0 { 256 } else { 0 };
    // У PS/2 «вверх» — плюс, у экрана — минус.
    crate::input::post_pointer(dx, -dy, 0, crate::input::Buttons::from_bits(flags & 0x07));
}

/// Скормить декодеру один scancode.
fn feed(byte: u8) {
    // Состояние берётся и меняется под локом, события отправляются уже без него:
    // `post` берёт собственный лок, и держать оба одновременно значило бы
    // заводить порядок захвата на ровном месте.
    let event = {
        let Some(mut decoder) = DECODER.try_lock() else {
            return;
        };

        if decoder.skip > 0 {
            decoder.skip -= 1;
            None
        } else if byte == PREFIX_EXTENDED {
            decoder.extended = true;
            None
        } else if byte == PREFIX_PAUSE {
            decoder.skip = PAUSE_TAIL;
            Some((KeyCode::Pause, true))
        } else {
            let extended = core::mem::replace(&mut decoder.extended, false);
            let pressed = byte & RELEASE_FLAG == 0;
            let code = byte & !RELEASE_FLAG;
            let key = if extended { extended_key(code) } else { plain_key(code) };
            key.map(|key| (key, pressed))
        }
    };

    if let Some((code, pressed)) = event {
        post(code, pressed);
    }
}

/// Код набора 1 без префикса.
const fn plain_key(code: u8) -> Option<KeyCode> {
    let key = match code {
        0x01 => KeyCode::Escape,
        0x02 => KeyCode::Digit1,
        0x03 => KeyCode::Digit2,
        0x04 => KeyCode::Digit3,
        0x05 => KeyCode::Digit4,
        0x06 => KeyCode::Digit5,
        0x07 => KeyCode::Digit6,
        0x08 => KeyCode::Digit7,
        0x09 => KeyCode::Digit8,
        0x0A => KeyCode::Digit9,
        0x0B => KeyCode::Digit0,
        0x0C => KeyCode::Minus,
        0x0D => KeyCode::Equal,
        0x0E => KeyCode::Backspace,
        0x0F => KeyCode::Tab,
        0x10 => KeyCode::Q,
        0x11 => KeyCode::W,
        0x12 => KeyCode::E,
        0x13 => KeyCode::R,
        0x14 => KeyCode::T,
        0x15 => KeyCode::Y,
        0x16 => KeyCode::U,
        0x17 => KeyCode::I,
        0x18 => KeyCode::O,
        0x19 => KeyCode::P,
        0x1A => KeyCode::LeftBracket,
        0x1B => KeyCode::RightBracket,
        0x1C => KeyCode::Enter,
        0x1D => KeyCode::LeftCtrl,
        0x1E => KeyCode::A,
        0x1F => KeyCode::S,
        0x20 => KeyCode::D,
        0x21 => KeyCode::F,
        0x22 => KeyCode::G,
        0x23 => KeyCode::H,
        0x24 => KeyCode::J,
        0x25 => KeyCode::K,
        0x26 => KeyCode::L,
        0x27 => KeyCode::Semicolon,
        0x28 => KeyCode::Apostrophe,
        0x29 => KeyCode::Grave,
        0x2A => KeyCode::LeftShift,
        0x2B => KeyCode::Backslash,
        0x2C => KeyCode::Z,
        0x2D => KeyCode::X,
        0x2E => KeyCode::C,
        0x2F => KeyCode::V,
        0x30 => KeyCode::B,
        0x31 => KeyCode::N,
        0x32 => KeyCode::M,
        0x33 => KeyCode::Comma,
        0x34 => KeyCode::Period,
        0x35 => KeyCode::Slash,
        0x36 => KeyCode::RightShift,
        0x37 => KeyCode::KeypadAsterisk,
        0x38 => KeyCode::LeftAlt,
        0x39 => KeyCode::Space,
        0x3A => KeyCode::CapsLock,
        0x3B => KeyCode::F1,
        0x3C => KeyCode::F2,
        0x3D => KeyCode::F3,
        0x3E => KeyCode::F4,
        0x3F => KeyCode::F5,
        0x40 => KeyCode::F6,
        0x41 => KeyCode::F7,
        0x42 => KeyCode::F8,
        0x43 => KeyCode::F9,
        0x44 => KeyCode::F10,
        0x45 => KeyCode::NumLock,
        0x46 => KeyCode::ScrollLock,
        0x47 => KeyCode::Keypad7,
        0x48 => KeyCode::Keypad8,
        0x49 => KeyCode::Keypad9,
        0x4A => KeyCode::KeypadMinus,
        0x4B => KeyCode::Keypad4,
        0x4C => KeyCode::Keypad5,
        0x4D => KeyCode::Keypad6,
        0x4E => KeyCode::KeypadPlus,
        0x4F => KeyCode::Keypad1,
        0x50 => KeyCode::Keypad2,
        0x51 => KeyCode::Keypad3,
        0x52 => KeyCode::Keypad0,
        0x53 => KeyCode::KeypadPeriod,
        0x57 => KeyCode::F11,
        0x58 => KeyCode::F12,
        // 0x00 — переполнение буфера клавиатуры, 0x54..0x56 — коды, которых нет
        // на обычной клавиатуре. Событие с `KeyCode::Unknown` порождать не
        // будем: это шум, а не потерянная клавиша.
        _ => return None,
    };
    Some(key)
}

/// Код набора 1 после префикса `0xE0`.
const fn extended_key(code: u8) -> Option<KeyCode> {
    let key = match code {
        0x1C => KeyCode::KeypadEnter,
        0x1D => KeyCode::RightCtrl,
        0x35 => KeyCode::KeypadSlash,
        0x37 => KeyCode::PrintScreen,
        0x38 => KeyCode::RightAlt,
        0x47 => KeyCode::Home,
        0x48 => KeyCode::Up,
        0x49 => KeyCode::PageUp,
        0x4B => KeyCode::Left,
        0x4D => KeyCode::Right,
        0x4F => KeyCode::End,
        0x50 => KeyCode::Down,
        0x51 => KeyCode::PageDown,
        0x52 => KeyCode::Insert,
        0x53 => KeyCode::Delete,
        0x5B => KeyCode::LeftMeta,
        0x5C => KeyCode::RightMeta,
        0x5D => KeyCode::Menu,
        // `E0 2A` и `E0 36` — «фальшивый Shift», который клавиатура вставляет
        // сама вокруг PrintScreen и навигационных клавиш при включённом Num
        // Lock. Приняв его за настоящий, драйвер выдавал бы заглавные буквы
        // после нажатия «вверх».
        0x2A | 0x36 => return None,
        // `E0 46` — Ctrl+Break; отдельной клавиши для него у нас нет.
        _ => return None,
    };
    Some(key)
}

/// Опросить контроллер один раз, не дожидаясь прерывания.
///
/// Нужно ровно в одном месте — в диагностике при запуске, чтобы отличить
/// «клавиатура настроена, но прерывание не доходит» от «клавиатура молчит».
/// Возвращает `true`, если в буфере что-то было.
pub fn poll_once() -> bool {
    without_interrupts(|| {
        // SAFETY: чтение регистра состояния.
        let status = unsafe { inb(PORT_COMMAND) };
        if status == 0xFF || status & STATUS_OUTPUT_FULL == 0 {
            return false;
        }
        // SAFETY: байт есть.
        let byte = unsafe { inb(PORT_DATA) };
        if status & STATUS_AUX != 0 {
            feed_mouse(byte);
        } else {
            feed(byte);
        }
        true
    })
}
