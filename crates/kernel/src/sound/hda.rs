//! Intel High Definition Audio (фаза 68a): звук на настоящих машинах.
//!
//! # Зачем, если есть virtio-snd
//!
//! virtio-snd существует только в QEMU. Встроенный звук ПК с 2004 года — это
//! контроллер HDA (класс PCI `04:03`) и кодеки на его шине; его же эмулируют
//! VirtualBox (ICH6 + STAC9221) и QEMU (`intel-hda` + `hda-output`), поэтому
//! драйвер проверяется на стенде целиком, как когда-то e1000.
//!
//! # Как устроено
//!
//! - **Контроллер.** Сброс (`GCTL.CRST`), после него кодеки отмечаются в
//!   `STATESTS`. Команды кодекам идут через кольцо `CORB`, ответы — через
//!   `RIRB`, оба в памяти DMA; драйвер ждёт ответ опросом указателя записи
//!   `RIRB` — команд немного, и все они при подъёме.
//! - **Кодек.** Корень → функциональная группа звука (AFG) → виджеты. Драйвер
//!   ищет разъём, способный выводить (`pin`, `PINCAP_OUT`) и подключённый
//!   (`config default` — не «нет соединения»), и по спискам соединений идёт от
//!   него к ЦАП (`audio output`) — через смесители и селекторы, если они на
//!   пути. Первым — встроенный динамик, потом линейный выход, потом наушники;
//!   остальные выходы, до которых доходит тот же ЦАП, включаются тоже: на
//!   ноутбуке звук идёт и в динамик, и в наушники.
//! - **Поток.** Первый выходной поток контроллера (они идут после входных),
//!   номер потока (`stream tag`) — 1, формат — 16 бит, 48 кГц, два канала.
//!   Буфер — кольцо из восьми периодов по 4096 байт (1024 кадра, ~21 мс), список
//!   дескрипторов (`BDL`) — по записи на период. Контроллер ходит по кольцу сам
//!   и не останавливается, поэтому сыгранный период драйвер **обнуляет**: если
//!   программа не успела, дальше прозвучит тишина, а не старый звук по кругу.
//!   Где играет устройство, говорит `LPIB` — смещение в кольце.
//!
//! # Вход: микрофон (28.11)
//!
//! При подъёме в том же кодеке ищутся входы: разъём, способный принимать
//! (`PINCAP_IN`), с `config default` «микрофон» или «линейный вход» и путь от
//! АЦП (`audio input`) к нему — через смесители и селекторы. Какой из них
//! слушать, решается при каждом старте записи: штекер в гнезде (`pin sense`)
//! важнее, микрофон важнее линейного входа, встроенный микрофон (`fixed`) —
//! после вставленных. На выбранном пути: выбор входа в селекторах, в
//! смесителях открыт только наш вход (остальные приглушены — иначе в запись
//! подмешался бы другой разъём), усиление 0 дБ, у микрофона — подъём около
//! +20 дБ на самом разъёме и опорное напряжение (`VRef` 80 %/50 %).
//! Поток — первый входной контроллера, номер 2, тот же формат, кольцо из
//! шестнадцати периодов (~340 мс). Драйвер читает по `LPIB` (сколько записано
//! в кольцо); не читали дольше кольца — пропущенное теряется, чтение
//! догоняет устройство.
//!
//! # Чего нет
//!
//! Прерываний (поток опрашивается, пока драйвер ждёт места), других
//! форматов, громкости, переключения при вставке наушников (unsolicited
//! responses отбрасываются), HDMI-кодеков как цели (у них нет аналоговых
//! разъёмов — выход на них драйвер не найдёт и честно скажет).

use core::sync::atomic::{Ordering, fence};

use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{PAGE_SIZE, PageFlags, VirtAddr};
use crate::pci::{self, Device};

/// Класс и подкласс PCI контроллера HDA.
const CLASS_MULTIMEDIA: u8 = 0x04;
const SUBCLASS_HDA: u8 = 0x03;
const VENDOR_INTEL: u16 = 0x8086;

// Регистры контроллера — раздел 3.3 спецификации HDA 1.0a.
const GCAP: usize = 0x00;
const GCTL: usize = 0x08;
const STATESTS: usize = 0x0E;
const INTCTL: usize = 0x20;
const CORBLBASE: usize = 0x40;
const CORBUBASE: usize = 0x44;
const CORBWP: usize = 0x48;
const CORBRP: usize = 0x4A;
const CORBCTL: usize = 0x4C;
const CORBSIZE: usize = 0x4E;
const RIRBLBASE: usize = 0x50;
const RIRBUBASE: usize = 0x54;
const RIRBWP: usize = 0x58;
const RINTCNT: usize = 0x5A;
const RIRBCTL: usize = 0x5C;
const RIRBSTS: usize = 0x5D;
const RIRBSIZE: usize = 0x5E;
const DPLBASE: usize = 0x70;
const DPUBASE: usize = 0x74;

const GCTL_CRST: u32 = 1;
const CORBRP_RST: u16 = 1 << 15;
const RIRBWP_RST: u16 = 1 << 15;
const DMA_RUN: u8 = 1 << 1;
/// `RIRBCTL.RINTCTL`: без него `RINTFL` не взводится, а QEMU, дойдя до
/// `RINTCNT` ответов, ждёт снятия `RINTFL` и встаёт. Само прерывание не
/// дойдёт: общий `GIE` в `INTCTL` выключен.
const RIRB_RESPONSE_FLAG: u8 = 1 << 0;
/// `RINTFL` и `RIRBOIS`: снимаются записью единиц.
const RIRBSTS_CLEAR: u8 = 0x05;

// Регистры потока: `0x80 + номер × 0x20`.
const STREAMS: usize = 0x80;
const STREAM_STRIDE: usize = 0x20;
const SD_CTL0: usize = 0x00;
const SD_CTL2: usize = 0x02;
const SD_STS: usize = 0x03;
const SD_LPIB: usize = 0x04;
const SD_CBL: usize = 0x08;
const SD_LVI: usize = 0x0C;
const SD_FMT: usize = 0x12;
const SD_BDPL: usize = 0x18;
const SD_BDPU: usize = 0x1C;
const SD_SRST: u8 = 1 << 0;
const SD_RUN: u8 = 1 << 1;
/// `BCIS`, `FIFOE`, `DESE` — снимаются записью единиц.
const SD_STS_CLEAR: u8 = 0x1C;

/// Номер потока, общий у контроллера и ЦАП. Ноль зарезервирован.
const STREAM_TAG: u8 = 1;
/// 48 кГц (база 48, без множителей), 16 бит, два канала.
const FORMAT_48K_16_STEREO: u16 = 0x0011;

// Глаголы кодека (раздел 7.3). Четырёхбитные глаголы (`0x2`, `0x3`) записаны
// так же, как двенадцатибитные, — `0x200`, `0x300` — и несут 16 бит данных.
const GET_PARAMETER: u32 = 0xF00;
const GET_CONNECTION_LIST: u32 = 0xF02;
const GET_CONFIG_DEFAULT: u32 = 0xF1C;
const SET_CONNECTION_SELECT: u32 = 0x701;
const SET_POWER_STATE: u32 = 0x705;
const SET_STREAM_CHANNEL: u32 = 0x706;
const SET_PIN_CONTROL: u32 = 0x707;
const SET_EAPD: u32 = 0x70C;
const SET_FORMAT: u32 = 0x200;
const SET_AMP: u32 = 0x300;

// Параметры (раздел 7.3.4).
const P_NODE_COUNT: u32 = 0x04;
/// Идентификатор изготовителя и модели кодека (параметр корневого узла).
const P_VENDOR_ID: u32 = 0x00;
const P_FUNCTION_TYPE: u32 = 0x05;
const P_WIDGET_CAPS: u32 = 0x09;
const P_PIN_CAPS: u32 = 0x0C;
const P_IN_AMP_CAPS: u32 = 0x0D;
const P_CONN_LEN: u32 = 0x0E;
const P_OUT_AMP_CAPS: u32 = 0x12;

const FUNCTION_AUDIO: u32 = 0x01;

// Виды виджетов (`widget caps`, биты 23:20).
const W_OUTPUT: u8 = 0x0;
const W_INPUT: u8 = 0x1;
const W_MIXER: u8 = 0x2;
const W_SELECTOR: u8 = 0x3;
const W_PIN: u8 = 0x4;

const WCAP_IN_AMP: u32 = 1 << 1;
const WCAP_OUT_AMP: u32 = 1 << 2;
const WCAP_AMP_OVERRIDE: u32 = 1 << 3;
const WCAP_CONN_LIST: u32 = 1 << 8;
const WCAP_POWER: u32 = 1 << 10;

const PINCAP_TRIGGER: u32 = 1 << 1;
const PINCAP_PRESENCE: u32 = 1 << 2;
const PINCAP_HP_DRIVE: u32 = 1 << 3;
const PINCAP_OUT: u32 = 1 << 4;
const PINCAP_IN: u32 = 1 << 5;
const PINCAP_VREF_50: u32 = 1 << 9;
const PINCAP_VREF_80: u32 = 1 << 12;
const PINCAP_EAPD: u32 = 1 << 16;

const PIN_OUT_ENABLE: u32 = 0x40;
const PIN_IN_ENABLE: u32 = 0x20;
const PIN_HP_ENABLE: u32 = 0x80;
const PIN_VREF_50: u32 = 0x01;
const PIN_VREF_80: u32 = 0x04;
const EAPD_ON: u32 = 0x02;

/// `pin sense`: бит 31 — штекер в гнезде; запуск измерения — у разъёмов,
/// которым он нужен (`PINCAP_TRIGGER`).
const GET_PIN_SENSE: u32 = 0xF09;
const EXECUTE_PIN_SENSE: u32 = 0x709;

const AMP_OUTPUT: u32 = 1 << 15;
const AMP_INPUT: u32 = 1 << 14;
const AMP_LEFT_RIGHT: u32 = (1 << 13) | (1 << 12);
const AMP_MUTE: u32 = 1 << 7;

// Устройство по умолчанию в `config default` (биты 23:20) и подключение
// (биты 31:30).
const DEVICE_LINE_OUT: u32 = 0x0;
const DEVICE_SPEAKER: u32 = 0x1;
const DEVICE_HP_OUT: u32 = 0x2;
const DEVICE_LINE_IN: u32 = 0x8;
const DEVICE_MIC_IN: u32 = 0xA;
const PORT_NONE: u32 = 0x1;
const PORT_FIXED: u32 = 0x2;
/// `config default`, биты 11:8, бит 0: «гнездо без определения штекера».
const MISC_NO_PRESENCE: u32 = 1 << 8;

/// Номер входного потока у контроллера и АЦП.
const CAPTURE_TAG: u8 = 2;
/// Периодов в кольце записи: ~340 мс — запас на программу, которая читает
/// не каждые 20 мс.
const CAPTURE_PERIODS: usize = 16;
const CAPTURE_BYTES: usize = PERIOD_BYTES * CAPTURE_PERIODS;

/// Виджетов в группе не бывает больше — номер узла семибитный в коротких
/// списках и не больше 127 у всех известных кодеков.
const MAX_NODES: usize = 128;
/// Глубина пути от разъёма до ЦАП: разъём, смеситель, селектор, ЦАП.
const MAX_PATH: usize = 5;

/// Период — 1024 кадра по два канала по два байта, около 21 мс.
pub const PERIOD_BYTES: usize = 4096;
/// Периодов в кольце: ~170 мс.
const PERIODS: usize = 8;
const BUFFER_BYTES: usize = PERIOD_BYTES * PERIODS;

/// Ждущий мьютекс: см. `virtio::snd` — драйвер спит, пока играет период.
static HDA: crate::sync::Mutex<Option<Hda>> = crate::sync::Mutex::new(None);

#[derive(Debug, Clone, Copy)]
pub enum HdaError {
    BadBar,
    MapFailed,
    NoMemory(DmaError),
    /// Контроллер не вышел из сброса или не остановил DMA.
    Timeout(&'static str),
    /// Ни один кодек не отметился после сброса.
    NoCodec,
    /// Кодек не ответил на команду.
    CodecSilent(u32),
    /// У контроллера нет выходных потоков.
    NoStream,
    /// Адрес памяти DMA выше 4 ГиБ, а контроллер умеет только 32 бита.
    HighMemory,
    /// Ни в одном кодеке нет пути от ЦАП к подключённому выходу.
    NoOutput,
    /// Поток запущен, а позиция не движется.
    Stalled,
    /// Нет входа (микрофона, линейного входа) или входного потока.
    NoInput,
    /// Все места потребителей записи заняты.
    Busy,
}

impl core::fmt::Display for HdaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadBar => write!(f, "no memory BAR"),
            Self::MapFailed => write!(f, "cannot map registers"),
            Self::NoMemory(err) => write!(f, "no DMA memory: {err:?}"),
            Self::Timeout(what) => write!(f, "timeout: {what}"),
            Self::NoCodec => write!(f, "no codec on the link"),
            Self::CodecSilent(verb) => write!(f, "codec did not answer {verb:#010x}"),
            Self::NoStream => write!(f, "no output stream"),
            Self::HighMemory => write!(f, "DMA memory above 4 GiB, controller is 32-bit"),
            Self::NoOutput => write!(f, "no path from a DAC to a connected output"),
            Self::Stalled => write!(f, "stream position does not move"),
            Self::NoInput => write!(f, "no microphone or line input"),
            Self::Busy => write!(f, "too many recordings at once"),
        }
    }
}

struct Hda {
    regs: VirtAddr,
    /// Смещение регистров выходного потока.
    stream: usize,
    corb_entries: usize,
    rirb_entries: usize,
    /// `CORB` с начала страницы, `RIRB` — со второй половины.
    rings: DmaBuffer,
    rirb_read: usize,
    bdl: DmaBuffer,
    buffer: DmaBuffer,
    /// Кодек и путь — для журнала.
    codec: u8,
    dac: u8,
    pins: [u8; 4],
    pin_count: usize,
    /// Входы (28.11): разъёмы, до которых доходит АЦП.
    inputs: alloc::vec::Vec<Input>,
    /// Входных потоков у контроллера.
    input_streams: usize,
    // --- состояние потока ---
    running: bool,
    /// Период, в котором устройство было при последней проверке.
    play: usize,
    /// Самый старый отданный и ещё не сыгранный период.
    oldest: usize,
    /// Сколько периодов отдано и не сыграно (включая тот, что играет).
    queued: usize,
    /// Период, который доливается, и сколько байт в нём.
    filling: Option<(usize, usize)>,
    /// Когда позицию смотрели последний раз и когда она двигалась.
    checked_ms: u64,
    moved_ms: u64,
}

/// Вход кодека (28.11): разъём и путь к нему от АЦП.
#[derive(Clone, Copy)]
struct Input {
    codec: u8,
    pin: u8,
    pin_caps: u32,
    config: u32,
    /// АЦП первым, разъём последним.
    path: [u8; MAX_PATH],
    length: usize,
    /// Виджеты пути (для выбора входов и усилителей при включении).
    widgets: [Widget; MAX_PATH],
    afg_in_amp: u32,
    afg_out_amp: u32,
}

impl Input {
    fn adc(&self) -> u8 {
        self.path[0]
    }

    fn is_mic(&self) -> bool {
        (self.config >> 20) & 0xF == DEVICE_MIC_IN
    }

    /// «Mic in, front pink jack» — для журнала и имени устройства ALSA.
    fn describe(&self) -> alloc::string::String {
        let kind = if self.is_mic() { "Mic in" } else { "Line in" };
        let port = self.config >> 30;
        if port == PORT_FIXED {
            return alloc::format!("{kind}, built-in");
        }
        let place = match (self.config >> 24) & 0xF {
            0x1 => "rear ",
            0x2 => "front ",
            0x3 => "left ",
            0x4 => "right ",
            0x5 => "top ",
            0x6 => "bottom ",
            _ => "",
        };
        let color = match (self.config >> 12) & 0xF {
            0x1 => "black ",
            0x2 => "grey ",
            0x3 => "blue ",
            0x4 => "green ",
            0x5 => "red ",
            0x6 => "orange ",
            0x7 => "yellow ",
            0x8 => "purple ",
            0x9 => "pink ",
            0xE => "white ",
            _ => "",
        };
        alloc::format!("{kind}, {place}{color}jack")
    }
}

/// Запись с входа (28.11): свой поток, своё кольцо; регистры — те же, что у
/// [`Hda`], но под отдельным замком: вывод держит свой замок, пока ждёт места
/// в кольце, и запись не должна ждать его.
struct Capture {
    regs: VirtAddr,
    /// Смещение регистров входного потока.
    sd: usize,
    bdl: DmaBuffer,
    buffer: DmaBuffer,
    /// Сколько потребителей открыло запись.
    users: usize,
    /// Потребители: у каждого свой курсор. С общим две вкладки со звонком
    /// отнимали данные друг у друга (QEMU 28.11: «PcmReadi returning less
    /// than expected frames: 234 vs. 480»).
    readers: [Option<Reader>; READERS],
    started_ms: u64,
    /// Отдано потребителям и потеряно (не прочитано вовремя), в байтах.
    delivered: u64,
    lost: u64,
    /// Самый громкий отсчёт с начала записи — в журнал при остановке.
    peak: u16,
    /// Какой вход слушается — для журнала и имени устройства.
    what: alloc::string::String,
}

/// Курсор одного потребителя записи.
#[derive(Clone, Copy)]
struct Reader {
    /// Смещение в кольце, с которого читать.
    read: usize,
    /// Когда позицию смотрели последний раз.
    checked_ms: u64,
}

/// Сколько потребителей записи может быть сразу.
const READERS: usize = 8;

static CAPTURE: crate::sync::Mutex<Option<Capture>> = crate::sync::Mutex::new(None);

/// Есть ли звук.
#[must_use]
pub fn present() -> bool {
    HDA.lock().is_some()
}

/// Есть ли вход (микрофон или линейный вход), с которого можно писать.
#[must_use]
pub fn capture_present() -> bool {
    CAPTURE.lock().is_some()
}

/// Имя входа, который будет слушаться (для `PCM_INFO`): «Mic in, rear pink
/// jack». Пусто — входа нет.
#[must_use]
pub fn capture_name() -> alloc::string::String {
    let guard = HDA.lock();
    let Some(hda) = guard.as_ref() else { return alloc::string::String::new() };
    hda.inputs.first().map(Input::describe).unwrap_or_default()
}

/// Начать запись (или прибавить потребителя к идущей). Ответ — номер
/// потребителя для [`capture_read`] и [`capture_stop`].
pub fn capture_start() -> Result<usize, HdaError> {
    let mut guard = CAPTURE.lock();
    let capture = guard.as_mut().ok_or(HdaError::NoInput)?;
    let slot = capture.readers.iter().position(Option::is_none).ok_or(HdaError::Busy)?;
    let now = crate::time::uptime_ms();
    if capture.users > 0 {
        // Новый потребитель слушает с того, что записывается сейчас.
        let read = capture.position();
        capture.readers[slot] = Some(Reader { read, checked_ms: now });
        capture.users += 1;
        return Ok(slot);
    }
    // Вход выбирается сейчас: штекер могли вставить после загрузки.
    let what = {
        let mut hda = HDA.lock();
        let hda = hda.as_mut().ok_or(HdaError::NoCodec)?;
        hda.enable_input()?
    };
    capture.program()?;
    capture.users = 1;
    capture.readers[slot] = Some(Reader { read: 0, checked_ms: now });
    capture.delivered = 0;
    capture.lost = 0;
    capture.peak = 0;
    capture.started_ms = now;
    capture.write8(capture.sd + SD_CTL0, SD_RUN);
    kprintln!("  hda         : capture started: {what}, stream {CAPTURE_TAG}, 48000 Hz 16-bit stereo");
    capture.what = what;
    Ok(slot)
}

/// Убрать потребителя `slot`; последний останавливает поток.
pub fn capture_stop(slot: usize) {
    let mut guard = CAPTURE.lock();
    let Some(capture) = guard.as_mut() else { return };
    if capture.readers.get(slot).is_none_or(Option::is_none) {
        return;
    }
    capture.readers[slot] = None;
    capture.users -= 1;
    if capture.users > 0 {
        return;
    }
    capture.stop_stream();
    let frames = capture.delivered / 4;
    let ms = crate::time::uptime_ms().saturating_sub(capture.started_ms);
    let position = capture.read32(capture.sd + SD_LPIB);
    kprintln!(
        "  hda         : capture stopped ({}): {frames} frames ({}.{} s) read in {ms} ms, {} frames lost, peak {}, position {position}",
        capture.what,
        frames / 48_000,
        frames % 48_000 * 10 / 48_000,
        capture.lost / 4,
        capture.peak
    );
}

/// Сколько байт записано и не прочитано потребителем `slot` (кратно кадру).
pub fn capture_available(slot: usize) -> usize {
    let mut guard = CAPTURE.lock();
    guard.as_mut().map_or(0, |capture| capture.available(slot))
}

/// Прочитать записанное потребителем `slot` (не больше `out`, кратно кадру);
/// не ждёт.
pub fn capture_read(slot: usize, out: &mut [u8]) -> usize {
    let mut guard = CAPTURE.lock();
    let limit = out.len();
    guard.as_mut().map_or(0, |capture| capture.take(slot, Some(out), limit))
}

/// Пропустить `bytes` записанного потребителем `slot` (программа отстала или
/// сдвинула указатель сама). Ответ — сколько пропущено.
pub fn capture_skip(slot: usize, bytes: usize) -> usize {
    let mut guard = CAPTURE.lock();
    guard.as_mut().map_or(0, |capture| {
        let skipped = capture.take(slot, None, bytes);
        capture.delivered -= skipped as u64;
        capture.lost += skipped as u64;
        skipped
    })
}

/// Запись программы (`SOUND_MIC_READ`, 28.11): потребитель входа, которого
/// держит программа; снимается вместе с ней (поле `Program::mic`) или по
/// `SOUND_MIC_CLOSE`.
pub struct CaptureGuard(usize);

impl CaptureGuard {
    /// Начать запись или прибавить потребителя (см. [`capture_start`]).
    pub fn start() -> Result<Self, HdaError> {
        capture_start().map(Self)
    }

    /// Номер потребителя для [`capture_read`].
    #[must_use]
    pub const fn slot(&self) -> usize {
        self.0
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        capture_stop(self.0);
    }
}

impl Capture {
    /// Сбросить поток и записать ему кольцо, длину, формат и номер.
    fn program(&mut self) -> Result<(), HdaError> {
        let sd = self.sd;
        self.stop_stream();
        self.write8(sd + SD_CTL0, SD_SRST);
        let _ = wait_regs(|| self.read8(sd + SD_CTL0) & SD_SRST != 0, 10);
        self.write8(sd + SD_CTL0, 0);
        wait_regs(|| self.read8(sd + SD_CTL0) & SD_SRST == 0, 100).map_err(|()| HdaError::Timeout("input stream reset"))?;
        self.write8(sd + SD_STS, SD_STS_CLEAR);
        self.buffer.zero();
        let base = self.buffer.phys().as_u64();
        for period in 0..CAPTURE_PERIODS {
            // SAFETY: шестнадцать записей по 16 байт — внутри страницы.
            unsafe {
                let entry = self.bdl.as_ptr::<u8>().add(period * 16);
                entry.cast::<u64>().write_volatile(base + (period * PERIOD_BYTES) as u64);
                entry.add(8).cast::<u32>().write_volatile(PERIOD_BYTES as u32);
                entry.add(12).cast::<u32>().write_volatile(0);
            }
        }
        fence(Ordering::SeqCst);
        let bdl = self.bdl.phys().as_u64();
        self.write32(sd + SD_BDPL, bdl as u32);
        self.write32(sd + SD_BDPU, (bdl >> 32) as u32);
        self.write32(sd + SD_CBL, CAPTURE_BYTES as u32);
        self.write16(sd + SD_LVI, (CAPTURE_PERIODS - 1) as u16);
        self.write16(sd + SD_FMT, FORMAT_48K_16_STEREO);
        self.write8(sd + SD_CTL2, CAPTURE_TAG << 4);
        Ok(())
    }

    fn stop_stream(&self) {
        let sd = self.sd;
        self.write8(sd + SD_CTL0, 0);
        let _ = wait_regs(|| self.read8(sd + SD_CTL0) & SD_RUN == 0, 100);
        self.write8(sd + SD_STS, SD_STS_CLEAR);
    }

    /// Записано и не прочитано, в байтах. Не смотрели дольше трёх четвертей
    /// кольца — устройство могло обойти его: старое теряется, чтение
    /// начинается с того, что записано сейчас.
    fn available(&mut self, slot: usize) -> usize {
        let position = self.position();
        let Some(Some(reader)) = self.readers.get_mut(slot) else { return 0 };
        let now = crate::time::uptime_ms();
        let ring_ms = (CAPTURE_BYTES / 4) as u64 * 1000 / 48_000;
        let mut behind = 0;
        if now.saturating_sub(reader.checked_ms) > ring_ms * 3 / 4 {
            behind = (position + CAPTURE_BYTES - reader.read) % CAPTURE_BYTES;
            reader.read = position;
        }
        reader.checked_ms = now;
        let ready = (position + CAPTURE_BYTES - reader.read) % CAPTURE_BYTES;
        self.lost += behind as u64;
        ready
    }

    /// Где пишет устройство: смещение в кольце, кратное кадру.
    fn position(&self) -> usize {
        (self.read32(self.sd + SD_LPIB) as usize % CAPTURE_BYTES) & !3
    }

    /// Снять до `limit` байт из кольца для потребителя `slot` (в `out`, если
    /// он есть).
    fn take(&mut self, slot: usize, mut out: Option<&mut [u8]>, limit: usize) -> usize {
        let count = self.available(slot).min(limit) & !3;
        let Some(Some(mut reader)) = self.readers.get(slot).copied() else { return 0 };
        let mut done = 0;
        while done < count {
            let run = (count - done).min(CAPTURE_BYTES - reader.read);
            // SAFETY: `read + run` не выходит за кольцо; устройство пишет
            // дальше `LPIB`, а не сюда.
            let data = unsafe { core::slice::from_raw_parts(self.buffer.as_ptr::<u8>().add(reader.read), run) };
            if let Some(out) = out.as_deref_mut() {
                out[done..done + run].copy_from_slice(data);
                for pair in data.chunks_exact(2) {
                    let sample = i16::from_le_bytes([pair[0], pair[1]]).unsigned_abs();
                    self.peak = self.peak.max(sample);
                }
            }
            done += run;
            reader.read = (reader.read + run) % CAPTURE_BYTES;
        }
        self.readers[slot] = Some(reader);
        self.delivered += done as u64;
        done
    }

    fn read8(&self, offset: usize) -> u8 {
        // SAFETY: окно регистров отображено с семантикой устройства.
        unsafe { ((self.regs.as_usize() + offset) as *const u8).read_volatile() }
    }

    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *const u32).read_volatile() }
    }

    fn write8(&self, offset: usize, value: u8) {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *mut u8).write_volatile(value) }
    }

    fn write16(&self, offset: usize, value: u16) {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *mut u16).write_volatile(value) }
    }

    fn write32(&self, offset: usize, value: u32) {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *mut u32).write_volatile(value) }
    }
}

/// Ждать условия до `ms` миллисекунд, опросом.
fn wait_regs(done: impl Fn() -> bool, ms: u64) -> Result<(), ()> {
    let deadline = crate::time::uptime_ns() + ms * 1_000_000;
    loop {
        if done() {
            return Ok(());
        }
        if crate::time::uptime_ns() > deadline {
            return Err(());
        }
        core::hint::spin_loop();
    }
}

/// Найти контроллер, поднять его и кодек.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn probe(root: &pci::Root) -> bool {
    // Все контроллеры HDA по порядку, до первого, где есть путь к выходу
    // (18.11, ПК Романа: первым стоит звук HDMI/DP видеокарты NVIDIA — без
    // подключённой колонки там «no DAC path», — а до звука на плате AMD
    // `1022:15e3` дело не доходило, и звука не было вовсе).
    let mut devices = alloc::vec::Vec::new();
    // SAFETY: контракт функции.
    unsafe {
        pci::for_each(root, |device| {
            if device.class == CLASS_MULTIMEDIA && device.subclass == SUBCLASS_HDA && device.prog_if == 0 {
                devices.push(*device);
            }
            true
        });
    }
    // Звук видеокарт (HDMI/DP: NVIDIA 10de, Radeon/встроенная графика AMD
    // 1002) — последним: колонки обычно на плате (AMD 1022, Intel 8086).
    devices.sort_by_key(|d| matches!(d.vendor, 0x10de | 0x1002));
    for device in devices {
        // SAFETY: контракт функции.
        if unsafe { attach_one(&device) } {
            return true;
        }
    }
    false
}

/// Поднять один контроллер; `false` — им звук не вывести.
///
/// # Safety
///
/// См. [`probe`].
unsafe fn attach_one(device: &pci::Device) -> bool {
    let device = *device;
    // SAFETY: контракт функции.
    match unsafe { Hda::attach(&device) } {
        Ok(mut hda) => {
            crate::devices::claim(device.address, "hda");
            let mut pins = alloc::string::String::new();
            for pin in &hda.pins[..hda.pin_count] {
                if !pins.is_empty() {
                    pins.push(',');
                }
                pins.push_str(&alloc::format!("{pin}"));
            }
            kprintln!(
                "  hda         : at {} ({:04x}:{:04x}), codec {}, DAC {} -> pin {}, 48000 Hz 16-bit stereo",
                device.address,
                device.vendor,
                device.device,
                hda.codec,
                hda.dac,
                pins
            );
            let capture = hda.make_capture();
            *HDA.lock() = Some(hda);
            *CAPTURE.lock() = capture;
            true
        }
        Err(err) => {
            kprintln!("  hda         : at {} unusable: {err}", device.address);
            false
        }
    }
}

/// Отдать отсчёты (16 бит со знаком, два канала, 48 кГц): ждёт места в кольце.
pub fn feed(bytes: &[u8]) -> Result<(), HdaError> {
    let mut guard = HDA.lock();
    let hda = guard.as_mut().ok_or(HdaError::NoCodec)?;
    hda.feed(bytes)
}

/// Доиграть всё отданное и остановить поток.
pub fn drain() -> Result<(), HdaError> {
    let mut guard = HDA.lock();
    let hda = guard.as_mut().ok_or(HdaError::NoCodec)?;
    hda.drain()
}

/// Узел кодека, каким его видит поиск пути.
#[derive(Clone, Copy)]
struct Widget {
    kind: u8,
    caps: u32,
    /// Номера узлов из списка соединений (входы этого виджета). Шестнадцать
    /// (28.11): у селекторов перед АЦП Realtek (ALC897, ALC1220) их 10–11, и
    /// разъёмы микрофона стоят не первыми.
    inputs: [u8; 16],
    input_count: u8,
}

impl Widget {
    /// Узел, которого в группе нет: вид «никакой», а не ноль — ноль это ЦАП.
    const ABSENT: Self = Self { kind: 0xFF, caps: 0, inputs: [0; 16], input_count: 0 };
}

impl Hda {
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц.
    unsafe fn attach(device: &Device) -> Result<Self, HdaError> {
        // SAFETY: DMA начнётся только после того, как контроллеру дадут адреса.
        unsafe { device.enable_bus_master() };
        if device.vendor == VENDOR_INTEL {
            // `TCSEL` (0x44): класс трафика TC0. Без этого на части чипсетов
            // Intel звук идёт с треском — так делает и Linux.
            // SAFETY: регистр конфигурации контроллера HDA Intel.
            unsafe { device.write_config8(0x44, device.config8(0x44) & !0x07) };
        }
        let bar = device.memory_bar(0).ok_or(HdaError::BadBar)?;
        let regs = bar.to_direct_map();
        let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;
        // SAFETY: контракт функции; окно регистров HDA — 16 КиБ (с запасом на
        // потоки), семантика `DEVICE` обязательна.
        unsafe { crate::arch::map_active(regs, bar, 4 * PAGE_SIZE, flags) }
            .map_err(|_| HdaError::MapFailed)?;

        let rings = dma::alloc(PAGE_SIZE).map_err(HdaError::NoMemory)?;
        let bdl = dma::alloc(PAGE_SIZE).map_err(HdaError::NoMemory)?;
        let buffer = dma::alloc(BUFFER_BYTES).map_err(HdaError::NoMemory)?;
        rings.zero();
        bdl.zero();
        buffer.zero();

        let mut hda = Self {
            regs,
            stream: 0,
            corb_entries: 0,
            rirb_entries: 0,
            rings,
            rirb_read: 0,
            bdl,
            buffer,
            codec: 0,
            dac: 0,
            pins: [0; 4],
            pin_count: 0,
            inputs: alloc::vec::Vec::new(),
            input_streams: 0,
            running: false,
            play: 0,
            oldest: 0,
            queued: 0,
            filling: None,
            checked_ms: 0,
            moved_ms: 0,
        };

        let gcap = hda.read16(GCAP);
        let inputs = usize::from((gcap >> 8) & 0xF);
        let outputs = usize::from((gcap >> 12) & 0xF);
        if outputs == 0 {
            return Err(HdaError::NoStream);
        }
        let wide = gcap & 1 != 0;
        let highest = [rings.phys(), bdl.phys(), buffer.phys()]
            .iter()
            .map(|phys| phys.as_u64() + PAGE_SIZE as u64)
            .max()
            .unwrap_or(0);
        if !wide && highest > u64::from(u32::MAX) {
            return Err(HdaError::HighMemory);
        }
        hda.stream = STREAMS + inputs * STREAM_STRIDE;
        hda.input_streams = inputs;
        if inputs > 0 {
            // Прошивка могла оставить запись идущей.
            hda.write8(STREAMS + SD_CTL0, 0);
        }

        let codecs = hda.reset()?;
        hda.start_rings();

        // Первый кодек, в котором нашёлся путь к выходу. HDMI-кодеки стоят на
        // той же шине и выхода не дадут — их пропускаем.
        let mut last = HdaError::NoCodec;
        for codec in 0..15u8 {
            if codecs & (1 << codec) == 0 {
                continue;
            }
            match hda.setup_codec(codec) {
                Ok(()) => {
                    hda.codec = codec;
                    hda.program_stream()?;
                    // Входы бывают и на кодеке без выхода (QEMU: `hda-output`
                    // и `hda-micro` рядом).
                    for other in codec + 1..15u8 {
                        if codecs & (1 << other) != 0 {
                            if let Err(err) = hda.scan_inputs(other) {
                                kprintln!("  hda         : codec {other}: inputs: {err}");
                            }
                        }
                    }
                    return Ok(hda);
                }
                Err(err) => {
                    kprintln!("  hda         : codec {codec}: {err}");
                    last = err;
                }
            }
        }
        Err(last)
    }

    // --- контроллер ----------------------------------------------------------

    /// Сбросить контроллер; ответ — маска отметившихся кодеков.
    fn reset(&mut self) -> Result<u16, HdaError> {
        // Кольца и потоки останавливаются до сброса: прошивка могла оставить
        // их работающими.
        self.write8(CORBCTL, 0);
        self.write8(RIRBCTL, 0);
        self.wait(|hda| hda.read8(CORBCTL) & DMA_RUN == 0 && hda.read8(RIRBCTL) & DMA_RUN == 0, 100)
            .map_err(|()| HdaError::Timeout("stopping rings"))?;
        self.write8(self.stream + SD_CTL0, 0);
        self.write32(INTCTL, 0);
        self.write32(DPLBASE, 0);
        self.write32(DPUBASE, 0);
        self.write16(STATESTS, 0x7FFF);

        self.write32(GCTL, self.read32(GCTL) & !GCTL_CRST);
        self.wait(|hda| hda.read32(GCTL) & GCTL_CRST == 0, 100)
            .map_err(|()| HdaError::Timeout("entering reset"))?;
        spin_us(100);
        self.write32(GCTL, self.read32(GCTL) | GCTL_CRST);
        self.wait(|hda| hda.read32(GCTL) & GCTL_CRST != 0, 100)
            .map_err(|()| HdaError::Timeout("leaving reset"))?;
        // Кодекам дано 521 мкс, чтобы отметиться; ждём с запасом. И не по
        // первому: на ASUS K53SD HDMI-кодек видеокарты (адрес 3) отмечался
        // раньше Realtek (адрес 0), маска читалась сразу — и звуковой кодек
        // оставался незамеченным (0.8). Как в Linux: ещё миллисекунда, потом
        // маска.
        let _ = self.wait(|hda| hda.read16(STATESTS) & 0x7FFF != 0, 100);
        spin_us(1000);
        let codecs = self.read16(STATESTS) & 0x7FFF;
        kprintln!("  hda         : codecs at {codecs:#06x}");
        if codecs == 0 {
            return Err(HdaError::NoCodec);
        }
        Ok(codecs)
    }

    /// Завести `CORB` и `RIRB` наибольшего размера, который умеет контроллер.
    fn start_rings(&mut self) {
        let phys = self.rings.phys().as_u64();
        let (corb_code, corb_entries) = ring_size(self.read8(CORBSIZE));
        let (rirb_code, rirb_entries) = ring_size(self.read8(RIRBSIZE));
        self.corb_entries = corb_entries;
        self.rirb_entries = rirb_entries;

        self.write8(CORBSIZE, corb_code);
        self.write32(CORBLBASE, phys as u32);
        self.write32(CORBUBASE, (phys >> 32) as u32);
        // Сброс указателя чтения: у одних контроллеров бит читается единицей,
        // пока идёт сброс, у других (QEMU) не держится вовсе — ждём недолго.
        self.write16(CORBRP, CORBRP_RST);
        let _ = self.wait(|hda| hda.read16(CORBRP) & CORBRP_RST != 0, 1);
        self.write16(CORBRP, 0);
        let _ = self.wait(|hda| hda.read16(CORBRP) & CORBRP_RST == 0, 1);
        self.write16(CORBWP, 0);
        self.write8(CORBCTL, DMA_RUN);

        let rirb = phys + 2048;
        self.write8(RIRBSIZE, rirb_code);
        self.write32(RIRBLBASE, rirb as u32);
        self.write32(RIRBUBASE, (rirb >> 32) as u32);
        self.write16(RIRBWP, RIRBWP_RST);
        self.rirb_read = 0;
        self.write16(RINTCNT, 1);
        self.write8(RIRBSTS, RIRBSTS_CLEAR);
        self.write8(RIRBCTL, DMA_RUN | RIRB_RESPONSE_FLAG);
    }

    /// Послать команду кодеку и дождаться ответа.
    fn command(&mut self, codec: u8, node: u8, verb: u32, payload: u32) -> Result<u32, HdaError> {
        let word = (u32::from(codec) << 28) | (u32::from(node) << 20) | (verb << 8) | payload;
        let write = (usize::from(self.read16(CORBWP) & 0xFF) + 1) % self.corb_entries;
        // SAFETY: `CORB` — первые килобайт страницы колец, индекс меньше числа
        // записей.
        unsafe { self.rings.as_ptr::<u32>().add(write).write_volatile(word) };
        fence(Ordering::SeqCst);
        self.write16(CORBWP, write as u16);

        let deadline = crate::time::uptime_ns() + 50_000_000;
        loop {
            let written = usize::from(self.read16(RIRBWP) & 0xFF) % self.rirb_entries;
            while self.rirb_read != written {
                self.rirb_read = (self.rirb_read + 1) % self.rirb_entries;
                fence(Ordering::SeqCst);
                // SAFETY: `RIRB` — со смещения 2048, по восемь байт на запись.
                let (response, extra) = unsafe {
                    let entry = self.rings.as_ptr::<u8>().add(2048 + self.rirb_read * 8).cast::<u32>();
                    (entry.read_volatile(), entry.add(1).read_volatile())
                };
                self.write8(RIRBSTS, RIRBSTS_CLEAR);
                // Незапрошенные ответы (вставили наушники) — не наш ответ.
                if extra & 0x10 == 0 {
                    return Ok(response);
                }
            }
            if crate::time::uptime_ns() > deadline {
                return Err(HdaError::CodecSilent(word));
            }
            core::hint::spin_loop();
        }
    }

    fn parameter(&mut self, codec: u8, node: u8, parameter: u32) -> Result<u32, HdaError> {
        self.command(codec, node, GET_PARAMETER, parameter)
    }

    // --- кодек ---------------------------------------------------------------

    /// Найти в кодеке путь от ЦАП к выходу и включить его.
    fn setup_codec(&mut self, codec: u8) -> Result<(), HdaError> {
        let count = self.parameter(codec, 0, P_NODE_COUNT)?;
        let (first, groups) = ((count >> 16) & 0xFF, count & 0xFF);
        let mut afg = None;
        for node in first..first + groups {
            let node = node as u8;
            if self.parameter(codec, node, P_FUNCTION_TYPE)? & 0xFF == FUNCTION_AUDIO {
                afg = Some(node);
                break;
            }
        }
        let afg = afg.ok_or(HdaError::NoOutput)?;
        self.command(codec, afg, SET_POWER_STATE, 0)?;
        let afg_out_amp = self.parameter(codec, afg, P_OUT_AMP_CAPS)?;
        let afg_in_amp = self.parameter(codec, afg, P_IN_AMP_CAPS)?;

        let count = self.parameter(codec, afg, P_NODE_COUNT)?;
        let first = ((count >> 16) & 0xFF) as usize;
        let last = (first + (count & 0xFF) as usize).min(MAX_NODES);
        let mut widgets = [Widget::ABSENT; MAX_NODES];
        // Разъёмы — для журнала, если путь не найдётся (0.8, ALC на ASUS K53SD).
        let mut pin_info = [(0u32, 0u32); MAX_NODES];
        // Разъёмы-кандидаты: (приоритет, узел).
        let mut pins: [(u8, u8); 16] = [(0, 0); 16];
        let mut pin_count = 0;
        for node in first..last {
            let caps = self.parameter(codec, node as u8, P_WIDGET_CAPS)?;
            let widget = &mut widgets[node];
            widget.kind = ((caps >> 20) & 0xF) as u8;
            widget.caps = caps;
            if caps & WCAP_CONN_LIST != 0 {
                let (inputs, count) = self.connections(codec, node as u8)?;
                widgets[node].inputs = inputs;
                widgets[node].input_count = count;
            }
            if widgets[node].kind == W_PIN {
                let pin_caps = self.parameter(codec, node as u8, P_PIN_CAPS)?;
                let config = self.command(codec, node as u8, GET_CONFIG_DEFAULT, 0)?;
                pin_info[node] = (pin_caps, config);
                if pin_caps & PINCAP_OUT == 0 || config >> 30 == PORT_NONE {
                    continue;
                }
                let rank = match (config >> 20) & 0xF {
                    DEVICE_SPEAKER => 0,
                    DEVICE_LINE_OUT => 1,
                    DEVICE_HP_OUT => 2,
                    _ => continue,
                };
                if pin_count < pins.len() {
                    pins[pin_count] = (rank, node as u8);
                    pin_count += 1;
                }
            }
        }
        pins[..pin_count].sort_unstable();
        self.collect_inputs(codec, &widgets, &pin_info, first, last, afg_in_amp, afg_out_amp);

        // Главный выход — первый, от которого есть путь к ЦАП; остальные —
        // если до них доходит тот же ЦАП.
        let mut dac = None;
        for &(_, pin) in &pins[..pin_count] {
            let mut path = [0u8; MAX_PATH];
            let Some(length) = find_path(&widgets, pin, dac, &mut path, 0) else {
                continue;
            };
            dac = Some(path[length - 1]);
            self.enable_path(codec, &widgets, &path[..length], afg_out_amp, afg_in_amp)?;
            if self.pin_count < self.pins.len() {
                self.pins[self.pin_count] = pin;
                self.pin_count += 1;
            }
        }
        let Some(dac) = dac else {
            // Граф кодека — в журнал: по нему и чинится поиск пути на чужой
            // машине (`dmesg hda`).
            let vendor = self.parameter(codec, 0, P_VENDOR_ID).unwrap_or(0);
            kprintln!("  hda         : codec {codec} {vendor:08x}, AFG {afg}, nodes {first}..{last}: no DAC path");
            for node in first..last {
                let widget = &widgets[node];
                let inputs = &widget.inputs[..usize::from(widget.input_count)];
                let (pin_caps, config) = pin_info[node];
                kprintln!(
                    "  hda         :   node {node:#04x} type {:x} caps {:08x} in {inputs:02x?}{}",
                    widget.kind,
                    widget.caps,
                    if widget.kind == W_PIN { alloc::format!(" pincaps {pin_caps:08x} config {config:08x}") } else { alloc::string::String::new() }
                );
            }
            return Err(HdaError::NoOutput);
        };
        self.dac = dac;
        self.command(codec, dac, SET_FORMAT, u32::from(FORMAT_48K_16_STEREO))?;
        self.command(codec, dac, SET_STREAM_CHANNEL, u32::from(STREAM_TAG) << 4)?;
        Ok(())
    }

    /// Список соединений узла (до шестнадцати входов).
    fn connections(&mut self, codec: u8, node: u8) -> Result<([u8; 16], u8), HdaError> {
        let length = self.parameter(codec, node, P_CONN_LEN)?;
        let long = length & 0x80 != 0;
        let total = (length & 0x7F) as usize;
        let (per_word, bits) = if long { (2, 16) } else { (4, 8) };
        let mask = (1u32 << bits) - 1;
        let range_bit = 1u32 << (bits - 1);
        let mut inputs = [0u8; 16];
        let mut count = 0usize;
        let mut previous = 0u32;
        let mut index = 0;
        while index < total && count < inputs.len() {
            let word = self.command(codec, node, GET_CONNECTION_LIST, index as u32)?;
            for slot in 0..per_word {
                if index + slot >= total {
                    break;
                }
                let entry = (word >> (slot * bits)) & mask;
                let value = entry & (range_bit - 1);
                // Диапазон: «от предыдущего до этого». Узлы в нём не
                // считаются отдельными входами для селектора — но путь через
                // смеситель найти позволяют.
                if entry & range_bit != 0 && previous != 0 {
                    for between in previous + 1..=value {
                        if count < inputs.len() {
                            inputs[count] = between as u8;
                            count += 1;
                        }
                    }
                } else if count < inputs.len() {
                    inputs[count] = value as u8;
                    count += 1;
                }
                previous = value;
            }
            index += per_word;
        }
        Ok((inputs, count as u8))
    }

    /// Запомнить входы кодека (28.11): разъёмы «микрофон»/«линейный вход»,
    /// способные принимать, и кратчайший путь к каждому от какого-нибудь АЦП.
    #[allow(clippy::too_many_arguments)]
    fn collect_inputs(
        &mut self,
        codec: u8,
        widgets: &[Widget; MAX_NODES],
        pin_info: &[(u32, u32); MAX_NODES],
        first: usize,
        last: usize,
        afg_in_amp: u32,
        afg_out_amp: u32,
    ) {
        for pin in first..last {
            let (pin_caps, config) = pin_info[pin];
            let device = (config >> 20) & 0xF;
            if widgets[pin].kind != W_PIN
                || pin_caps & PINCAP_IN == 0
                || config >> 30 == PORT_NONE
                || !matches!(device, DEVICE_MIC_IN | DEVICE_LINE_IN)
            {
                continue;
            }
            let mut best: Option<([u8; MAX_PATH], usize)> = None;
            for adc in first..last {
                if widgets[adc].kind != W_INPUT {
                    continue;
                }
                let mut path = [0u8; MAX_PATH];
                if let Some(length) = find_input_path(widgets, adc as u8, pin as u8, &mut path, 0) {
                    if best.is_none_or(|(_, shortest)| length < shortest) {
                        best = Some((path, length));
                    }
                }
            }
            let Some((path, length)) = best else {
                kprintln!("  hda         : codec {codec}: input pin {pin:#04x} (config {config:08x}) has no ADC path");
                continue;
            };
            let mut on_path = [Widget::ABSENT; MAX_PATH];
            for (slot, &node) in on_path.iter_mut().zip(&path[..length]) {
                *slot = widgets[usize::from(node)];
            }
            self.inputs.push(Input {
                codec,
                pin: pin as u8,
                pin_caps,
                config,
                path,
                length,
                widgets: on_path,
                afg_in_amp,
                afg_out_amp,
            });
        }
        // Порядок без штекеров: микрофон в гнезде, встроенный, линейный вход.
        self.inputs.sort_by_key(|input| match (input.is_mic(), input.config >> 30 == PORT_FIXED) {
            (true, false) => 0,
            (true, true) => 1,
            _ => 2,
        });
    }

    /// Входы кодека, который выход не дал (его не трогали): группа звука,
    /// виджеты, разъёмы — и [`Self::collect_inputs`].
    fn scan_inputs(&mut self, codec: u8) -> Result<(), HdaError> {
        let count = self.parameter(codec, 0, P_NODE_COUNT)?;
        let (first, groups) = ((count >> 16) & 0xFF, count & 0xFF);
        let mut afg = None;
        for node in first..first + groups {
            if self.parameter(codec, node as u8, P_FUNCTION_TYPE)? & 0xFF == FUNCTION_AUDIO {
                afg = Some(node as u8);
                break;
            }
        }
        let Some(afg) = afg else { return Ok(()) };
        self.command(codec, afg, SET_POWER_STATE, 0)?;
        let afg_out_amp = self.parameter(codec, afg, P_OUT_AMP_CAPS)?;
        let afg_in_amp = self.parameter(codec, afg, P_IN_AMP_CAPS)?;
        let count = self.parameter(codec, afg, P_NODE_COUNT)?;
        let first = ((count >> 16) & 0xFF) as usize;
        let last = (first + (count & 0xFF) as usize).min(MAX_NODES);
        let mut widgets = [Widget::ABSENT; MAX_NODES];
        let mut pin_info = [(0u32, 0u32); MAX_NODES];
        for node in first..last {
            let caps = self.parameter(codec, node as u8, P_WIDGET_CAPS)?;
            widgets[node].kind = ((caps >> 20) & 0xF) as u8;
            widgets[node].caps = caps;
            if caps & WCAP_CONN_LIST != 0 {
                let (inputs, count) = self.connections(codec, node as u8)?;
                widgets[node].inputs = inputs;
                widgets[node].input_count = count;
            }
            if widgets[node].kind == W_PIN {
                let pin_caps = self.parameter(codec, node as u8, P_PIN_CAPS)?;
                let config = self.command(codec, node as u8, GET_CONFIG_DEFAULT, 0)?;
                pin_info[node] = (pin_caps, config);
            }
        }
        self.collect_inputs(codec, &widgets, &pin_info, first, last, afg_in_amp, afg_out_amp);
        Ok(())
    }

    /// Кольцо и поток записи, если есть вход; что нашлось — в журнал.
    fn make_capture(&mut self) -> Option<Capture> {
        let vendor = self.parameter(self.codec, 0, P_VENDOR_ID).unwrap_or(0);
        if self.inputs.is_empty() {
            kprintln!("  hda         : codec {} {vendor:08x}: no microphone or line input pin", self.codec);
            return None;
        }
        for index in 0..self.inputs.len() {
            let input = self.inputs[index];
            let vendor = self.parameter(input.codec, 0, P_VENDOR_ID).unwrap_or(0);
            kprintln!(
                "  hda         : input: codec {} {vendor:08x}, {} (pin {:#04x}, config {:08x}) -> ADC {:#04x}, path {:02x?}",
                input.codec,
                input.describe(),
                input.pin,
                input.config,
                input.adc(),
                &input.path[..input.length]
            );
        }
        if self.input_streams == 0 {
            kprintln!("  hda         : the controller has no input stream: no recording");
            return None;
        }
        let (Ok(bdl), Ok(buffer)) = (dma::alloc(PAGE_SIZE), dma::alloc(CAPTURE_BYTES)) else {
            kprintln!("  hda         : no DMA memory for recording");
            return None;
        };
        let wide = self.read16(GCAP) & 1 != 0;
        let highest = bdl.phys().as_u64().max(buffer.phys().as_u64() + CAPTURE_BYTES as u64);
        if !wide && highest > u64::from(u32::MAX) {
            kprintln!("  hda         : recording memory above 4 GiB, controller is 32-bit");
            return None;
        }
        bdl.zero();
        buffer.zero();
        Some(Capture {
            regs: self.regs,
            sd: STREAMS,
            bdl,
            buffer,
            users: 0,
            readers: [None; READERS],
            started_ms: 0,
            delivered: 0,
            lost: 0,
            peak: 0,
            what: alloc::string::String::new(),
        })
    }

    /// Штекер в гнезде входа: `None` — гнездо этого не знает (встроенный
    /// микрофон, нет определения).
    fn sense(&mut self, input: &Input) -> Result<Option<bool>, HdaError> {
        if input.pin_caps & PINCAP_PRESENCE == 0 || input.config & MISC_NO_PRESENCE != 0 || input.config >> 30 == PORT_FIXED {
            return Ok(None);
        }
        if input.pin_caps & PINCAP_TRIGGER != 0 {
            self.command(input.codec, input.pin, EXECUTE_PIN_SENSE, 0)?;
            spin_us(200);
        }
        let sense = self.command(input.codec, input.pin, GET_PIN_SENSE, 0)?;
        Ok(Some(sense & (1 << 31) != 0))
    }

    /// Выбрать вход (штекер, потом вид) и включить путь до АЦП; ответ —
    /// описание для журнала.
    fn enable_input(&mut self) -> Result<alloc::string::String, HdaError> {
        let mut best: Option<(u8, usize, Option<bool>)> = None;
        for index in 0..self.inputs.len() {
            let input = self.inputs[index];
            // Разъём, на который идёт звук (переназначаемые гнёзда), входом
            // не делаем.
            if self.pins[..self.pin_count].contains(&input.pin) {
                continue;
            }
            let plugged = self.sense(&input)?;
            let fixed = input.config >> 30 == PORT_FIXED;
            let rank = match (plugged, input.is_mic()) {
                (Some(true), true) => 0,
                (Some(true), false) => 1,
                (None, true) if fixed => 2,
                (None, true) => 3,
                (None, false) => 4,
                (Some(false), true) => 5,
                (Some(false), false) => 6,
            };
            if best.is_none_or(|(top, _, _)| rank < top) {
                best = Some((rank, index, plugged));
            }
        }
        let (_, index, plugged) = best.ok_or(HdaError::NoInput)?;
        let input = self.inputs[index];
        let codec = input.codec;
        let path = &input.path[..input.length];
        for (step, &node) in path.iter().enumerate() {
            let widget = input.widgets[step];
            if widget.caps & WCAP_POWER != 0 {
                self.command(codec, node, SET_POWER_STATE, 0)?;
            }
            let in_caps = if widget.caps & WCAP_AMP_OVERRIDE != 0 {
                self.parameter(codec, node, P_IN_AMP_CAPS)?
            } else {
                input.afg_in_amp
            };
            let next = path.get(step + 1).copied();
            let choice = next.and_then(|next| {
                widget.inputs[..usize::from(widget.input_count)].iter().position(|&n| n == next)
            });
            if let Some(choice) = choice {
                if widget.kind != W_MIXER && widget.input_count > 1 {
                    self.command(codec, node, SET_CONNECTION_SELECT, choice as u32)?;
                }
                if widget.caps & WCAP_IN_AMP != 0 {
                    if widget.kind == W_MIXER {
                        // Только наш вход: остальные разъёмы не подмешиваются.
                        for other in 0..usize::from(widget.input_count) {
                            if other != choice {
                                self.command(codec, node, SET_AMP, AMP_INPUT | AMP_LEFT_RIGHT | ((other as u32 & 0xF) << 8) | AMP_MUTE)?;
                            }
                        }
                    }
                    let payload = AMP_INPUT | AMP_LEFT_RIGHT | ((choice as u32 & 0xF) << 8) | zero_db(in_caps);
                    self.command(codec, node, SET_AMP, payload)?;
                }
            }
            if widget.caps & WCAP_OUT_AMP != 0 && widget.kind != W_PIN {
                let caps = if widget.caps & WCAP_AMP_OVERRIDE != 0 {
                    self.parameter(codec, node, P_OUT_AMP_CAPS)?
                } else {
                    input.afg_out_amp
                };
                self.command(codec, node, SET_AMP, AMP_OUTPUT | AMP_LEFT_RIGHT | zero_db(caps))?;
            }
            if widget.kind == W_PIN {
                let mut control = PIN_IN_ENABLE;
                if input.is_mic() {
                    // Опорное напряжение для электретного микрофона.
                    if input.pin_caps & PINCAP_VREF_80 != 0 {
                        control |= PIN_VREF_80;
                    } else if input.pin_caps & PINCAP_VREF_50 != 0 {
                        control |= PIN_VREF_50;
                    }
                }
                self.command(codec, node, SET_PIN_CONTROL, control)?;
                if widget.caps & WCAP_IN_AMP != 0 {
                    // Подъём микрофона на разъёме (~+20 дБ, как ставит Linux);
                    // линейному входу — 0 дБ.
                    let gain = boost(in_caps, if input.is_mic() { 20 } else { 0 });
                    self.command(codec, node, SET_AMP, AMP_INPUT | AMP_LEFT_RIGHT | gain)?;
                }
            }
        }
        let adc = input.adc();
        self.command(codec, adc, SET_FORMAT, u32::from(FORMAT_48K_16_STEREO))?;
        self.command(codec, adc, SET_STREAM_CHANNEL, u32::from(CAPTURE_TAG) << 4)?;
        let state = match plugged {
            Some(true) => "plugged",
            Some(false) => "NOT plugged",
            None => "no jack sense",
        };
        Ok(alloc::format!(
            "{} ({state}), codec {codec} pin {:#04x} -> ADC {adc:#04x}",
            input.describe(),
            input.pin
        ))
    }

    /// Включить путь: разъём → … → ЦАП.
    fn enable_path(
        &mut self,
        codec: u8,
        widgets: &[Widget; MAX_NODES],
        path: &[u8],
        afg_out_amp: u32,
        afg_in_amp: u32,
    ) -> Result<(), HdaError> {
        for (step, &node) in path.iter().enumerate() {
            let widget = widgets[usize::from(node)];
            if widget.caps & WCAP_POWER != 0 {
                self.command(codec, node, SET_POWER_STATE, 0)?;
            }
            // Какой вход ведёт к следующему узлу пути.
            let next = path.get(step + 1).copied();
            let input = next.and_then(|next| {
                widget.inputs[..usize::from(widget.input_count)].iter().position(|&n| n == next)
            });
            if let Some(input) = input {
                if widget.kind != W_MIXER && widget.input_count > 1 {
                    self.command(codec, node, SET_CONNECTION_SELECT, input as u32)?;
                }
                if widget.caps & WCAP_IN_AMP != 0 {
                    let caps = if widget.caps & WCAP_AMP_OVERRIDE != 0 {
                        self.parameter(codec, node, P_IN_AMP_CAPS)?
                    } else {
                        afg_in_amp
                    };
                    let payload = AMP_INPUT | AMP_LEFT_RIGHT | ((input as u32 & 0xF) << 8) | zero_db(caps);
                    self.command(codec, node, SET_AMP, payload)?;
                }
            }
            if widget.caps & WCAP_OUT_AMP != 0 {
                let caps = if widget.caps & WCAP_AMP_OVERRIDE != 0 {
                    self.parameter(codec, node, P_OUT_AMP_CAPS)?
                } else {
                    afg_out_amp
                };
                self.command(codec, node, SET_AMP, AMP_OUTPUT | AMP_LEFT_RIGHT | zero_db(caps))?;
            }
            if widget.kind == W_PIN {
                let pin_caps = self.parameter(codec, node, P_PIN_CAPS)?;
                let mut control = PIN_OUT_ENABLE;
                if pin_caps & PINCAP_HP_DRIVE != 0 {
                    control |= PIN_HP_ENABLE;
                }
                self.command(codec, node, SET_PIN_CONTROL, control)?;
                if pin_caps & PINCAP_EAPD != 0 {
                    self.command(codec, node, SET_EAPD, EAPD_ON)?;
                }
            }
        }
        Ok(())
    }

    // --- поток ---------------------------------------------------------------

    /// Сбросить поток и записать ему кольцо, длину, формат и номер.
    fn program_stream(&mut self) -> Result<(), HdaError> {
        let sd = self.stream;
        self.write8(sd + SD_CTL0, 0);
        self.wait(|hda| hda.read8(sd + SD_CTL0) & SD_RUN == 0, 100)
            .map_err(|()| HdaError::Timeout("stopping stream"))?;
        self.write8(sd + SD_CTL0, SD_SRST);
        let _ = self.wait(|hda| hda.read8(sd + SD_CTL0) & SD_SRST != 0, 10);
        self.write8(sd + SD_CTL0, 0);
        self.wait(|hda| hda.read8(sd + SD_CTL0) & SD_SRST == 0, 100)
            .map_err(|()| HdaError::Timeout("stream reset"))?;
        self.write8(sd + SD_STS, SD_STS_CLEAR);

        let base = self.buffer.phys().as_u64();
        for period in 0..PERIODS {
            // SAFETY: в списке восемь записей по 16 байт — внутри страницы.
            unsafe {
                let entry = self.bdl.as_ptr::<u8>().add(period * 16);
                entry.cast::<u64>().write_volatile(base + (period * PERIOD_BYTES) as u64);
                entry.add(8).cast::<u32>().write_volatile(PERIOD_BYTES as u32);
                // Без `IOC`: прерываний драйвер не ждёт.
                entry.add(12).cast::<u32>().write_volatile(0);
            }
        }
        fence(Ordering::SeqCst);
        let bdl = self.bdl.phys().as_u64();
        self.write32(sd + SD_BDPL, bdl as u32);
        self.write32(sd + SD_BDPU, (bdl >> 32) as u32);
        self.write32(sd + SD_CBL, BUFFER_BYTES as u32);
        self.write16(sd + SD_LVI, (PERIODS - 1) as u16);
        self.write16(sd + SD_FMT, FORMAT_48K_16_STEREO);
        self.write8(sd + SD_CTL2, STREAM_TAG << 4);

        self.running = false;
        self.play = 0;
        self.oldest = 0;
        self.queued = 0;
        self.filling = None;
        Ok(())
    }

    fn feed(&mut self, mut bytes: &[u8]) -> Result<(), HdaError> {
        // Обрывок кадра отбрасывается, как у virtio-snd.
        let whole = bytes.len() / 4 * 4;
        bytes = &bytes[..whole];
        while !bytes.is_empty() {
            let (slot, used) = match self.filling {
                Some(filling) => filling,
                None => (self.free_slot()?, 0),
            };
            let take = (PERIOD_BYTES - used).min(bytes.len());
            // SAFETY: период не отдан устройству и лежит внутри буфера.
            unsafe {
                let data = self.buffer.as_ptr::<u8>().add(slot * PERIOD_BYTES + used);
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), data, take);
            }
            bytes = &bytes[take..];
            if used + take == PERIOD_BYTES {
                self.filling = None;
                self.offer()?;
            } else {
                self.filling = Some((slot, used + take));
            }
        }
        Ok(())
    }

    /// Доиграть всё и остановить поток: кольцо снова начнётся с нуля.
    fn drain(&mut self) -> Result<(), HdaError> {
        if let Some((slot, used)) = self.filling.take() {
            // Хвост периода — тишина.
            // SAFETY: внутри периода `slot`.
            unsafe {
                let data = self.buffer.as_ptr::<u8>().add(slot * PERIOD_BYTES + used);
                core::ptr::write_bytes(data, 0, PERIOD_BYTES - used);
            }
            self.offer()?;
        }
        if self.queued > 0 && !self.running {
            self.start();
        }
        let result = (|| {
            while self.queued > 0 {
                self.reclaim(true)?;
            }
            Ok(())
        })();
        let reset = self.program_stream();
        self.buffer.zero();
        result.and(reset)
    }

    /// Период, куда писать следующим, — ждёт, пока в кольце есть место.
    fn free_slot(&mut self) -> Result<usize, HdaError> {
        // Играющий период не трогаем никогда: отдано может быть на один
        // меньше, чем периодов в кольце.
        while self.queued >= PERIODS - 1 {
            self.reclaim(true)?;
        }
        self.reclaim(false)?;
        if self.queued == 0 {
            // Всё сыграно (или ещё не начато): писать — со следующего за
            // тем, что играет сейчас, а до запуска — с начала кольца.
            self.oldest = if self.running { (self.play + 1) % PERIODS } else { 0 };
        }
        Ok((self.oldest + self.queued) % PERIODS)
    }

    fn offer(&mut self) -> Result<(), HdaError> {
        fence(Ordering::SeqCst);
        self.queued += 1;
        if !self.running && self.queued >= 2 {
            self.start();
        }
        Ok(())
    }

    fn start(&mut self) {
        let sd = self.stream;
        self.write8(sd + SD_STS, SD_STS_CLEAR);
        self.write8(sd + SD_CTL0, SD_RUN);
        self.running = true;
        self.play = 0;
        let now = crate::time::uptime_ms();
        self.checked_ms = now;
        self.moved_ms = now;
    }

    /// Учесть сыгранные периоды. `wait` — ждать, пока сыграется хоть один.
    fn reclaim(&mut self, wait: bool) -> Result<(), HdaError> {
        if !self.running {
            return Ok(());
        }
        let before = self.queued;
        loop {
            let now = crate::time::uptime_ms();
            let position = self.read32(self.stream + SD_LPIB) as usize;
            let current = (position / PERIOD_BYTES).min(PERIODS - 1);
            // Если позицию не смотрели дольше, чем звучит отданное, — круг
            // мог пройти незамеченным: всё отданное сыграно.
            let lapped = now.saturating_sub(self.checked_ms) >= period_ms() * (PERIODS as u64 - 1);
            self.checked_ms = now;
            if lapped {
                self.buffer.zero();
                self.queued = 0;
                self.play = current;
                self.moved_ms = now;
            }
            while self.play != current {
                self.zero_period(self.play);
                if self.queued > 0 && self.play == self.oldest {
                    self.queued -= 1;
                    self.oldest = (self.oldest + 1) % PERIODS;
                }
                self.play = (self.play + 1) % PERIODS;
                self.moved_ms = now;
            }
            if !wait || self.queued < before || self.queued == 0 {
                return Ok(());
            }
            if now.saturating_sub(self.moved_ms) > 1_000 {
                return Err(HdaError::Stalled);
            }
            crate::sched::sleep_ms(2);
        }
    }

    fn zero_period(&self, period: usize) {
        // SAFETY: период внутри буфера; устройство его уже прошло.
        unsafe { core::ptr::write_bytes(self.buffer.as_ptr::<u8>().add(period * PERIOD_BYTES), 0, PERIOD_BYTES) };
    }

    // --- регистры ------------------------------------------------------------

    /// Ждать условия до `ms` миллисекунд, опросом.
    fn wait(&self, done: impl Fn(&Self) -> bool, ms: u64) -> Result<(), ()> {
        let deadline = crate::time::uptime_ns() + ms * 1_000_000;
        loop {
            if done(self) {
                return Ok(());
            }
            if crate::time::uptime_ns() > deadline {
                return Err(());
            }
            core::hint::spin_loop();
        }
    }

    fn read8(&self, offset: usize) -> u8 {
        // SAFETY: окно регистров отображено с семантикой устройства.
        unsafe { ((self.regs.as_usize() + offset) as *const u8).read_volatile() }
    }

    fn read16(&self, offset: usize) -> u16 {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *const u16).read_volatile() }
    }

    fn read32(&self, offset: usize) -> u32 {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *const u32).read_volatile() }
    }

    fn write8(&self, offset: usize, value: u8) {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *mut u8).write_volatile(value) }
    }

    fn write16(&self, offset: usize, value: u16) {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *mut u16).write_volatile(value) }
    }

    fn write32(&self, offset: usize, value: u32) {
        // SAFETY: см. `read8`.
        unsafe { ((self.regs.as_usize() + offset) as *mut u32).write_volatile(value) }
    }
}

/// Длительность периода в миллисекундах (округлено вверх).
const fn period_ms() -> u64 {
    ((PERIOD_BYTES / 4) as u64 * 1000).div_ceil(48_000)
}

/// Код размера кольца и число записей: наибольший из тех, что умеет
/// контроллер (биты 7:4 — 2, 16, 256 записей).
fn ring_size(size: u8) -> (u8, usize) {
    if size & 0x40 != 0 {
        (2, 256)
    } else if size & 0x20 != 0 {
        (1, 16)
    } else {
        (0, 2)
    }
}

/// Усиление 0 дБ из возможностей усилителя, без приглушения.
fn zero_db(caps: u32) -> u32 {
    let offset = caps & 0x7F;
    let steps = (caps >> 8) & 0x7F;
    offset.min(steps)
}

/// Шаг усилителя, ближайший снизу к `db` дБ над нулём (шаг — `(size + 1)/4`
/// дБ, ноль — `offset`), без приглушения.
fn boost(caps: u32, db: u32) -> u32 {
    let offset = caps & 0x7F;
    let steps = (caps >> 8) & 0x7F;
    let size = ((caps >> 16) & 0x7F) + 1;
    (offset + db * 4 / size).min(steps)
}

/// Путь от АЦП `node` до разъёма `pin` через смесители и селекторы, поиском
/// в глубину. Ответ — длина пути в `path` (АЦП первым).
fn find_input_path(widgets: &[Widget; MAX_NODES], node: u8, pin: u8, path: &mut [u8; MAX_PATH], depth: usize) -> Option<usize> {
    if depth >= MAX_PATH || usize::from(node) >= MAX_NODES {
        return None;
    }
    path[depth] = node;
    if depth > 0 && node == pin {
        return Some(depth + 1);
    }
    let widget = &widgets[usize::from(node)];
    if depth > 0 && !matches!(widget.kind, W_MIXER | W_SELECTOR) {
        return None;
    }
    for &input in &widget.inputs[..usize::from(widget.input_count)] {
        if let Some(length) = find_input_path(widgets, input, pin, path, depth + 1) {
            return Some(length);
        }
    }
    None
}

/// Путь от узла `node` до ЦАП (если `want` — до этого ЦАП) поиском в глубину
/// по спискам соединений. Ответ — длина пути в `path`.
fn find_path(
    widgets: &[Widget; MAX_NODES],
    node: u8,
    want: Option<u8>,
    path: &mut [u8; MAX_PATH],
    depth: usize,
) -> Option<usize> {
    if depth >= MAX_PATH || usize::from(node) >= MAX_NODES {
        return None;
    }
    path[depth] = node;
    let widget = &widgets[usize::from(node)];
    if widget.kind == W_OUTPUT && depth > 0 {
        return match want {
            Some(dac) if dac != node => None,
            _ => Some(depth + 1),
        };
    }
    if depth > 0 && !matches!(widget.kind, W_MIXER | W_SELECTOR) {
        return None;
    }
    for &input in &widget.inputs[..usize::from(widget.input_count)] {
        if let Some(length) = find_path(widgets, input, want, path, depth + 1) {
            return Some(length);
        }
    }
    None
}

/// Подождать `us` микросекунд, не уступая процессор.
fn spin_us(us: u64) {
    let deadline = crate::time::uptime_ns() + us * 1_000;
    while crate::time::uptime_ns() < deadline {
        core::hint::spin_loop();
    }
}
