//! Звук для программ Linux: минимальный ALSA ядра (12.11.2026, шаг 7 плана
//! Proton).
//!
//! # Зачем
//!
//! Wine (`winealsa.so`) говорит со звуком через libasound, а та — с ядром
//! ioctl-ами `/dev/snd/controlC0` и `/dev/snd/pcmC0D0p`. Своего сервера звука
//! (PulseAudio) у нас нет, и он не нужен: libasound со штатным `plug` сама
//! переводит формат, частоту и число каналов — ядру хватает одного режима,
//! того, что умеет [`crate::sound`]: S16_LE, два канала, 48 кГц.
//!
//! # Как устроено
//!
//! Карта одна, устройство воспроизведения одно, режим один: период 1024
//! кадра, буфер 4 периода. `HW_REFINE`/`HW_PARAMS` сужают любой запрос до
//! него (или `EINVAL`, если запрос его исключает). Данные идут только
//! `WRITEI_FRAMES` (доступ RW_INTERLEAVED); `mmap` статуса не даём — libasound
//! откатывается на `SYNC_PTR`. Указатель карты (`hw_ptr`) идёт по часам с
//! момента запуска со скоростью 48 000 кадров в секунду и не обгоняет
//! записанное: недобор — тишина, без XRUN (Wine её переживает молча).
//! Записанное отдаётся [`crate::sound::feed`] сразу; карты нет — отбрасывается
//! (темп всё равно держат часы).

use alloc::sync::Arc;
use alloc::vec;
use core::any::Any;

use super::e;
use super::ipc::{get, put, read_user, wait_io, write_user};
use crate::sync::Mutex;
use crate::user::files::Object;

const RATE: u64 = 48_000;
const CHANNELS: u32 = 2;
const FRAME_BYTES: u64 = 4;
const PERIOD_FRAMES: u64 = 1024;
const PERIODS: u64 = 4;
const BUFFER_FRAMES: u64 = PERIOD_FRAMES * PERIODS;

pub const CONTROL_PATH: &str = "/dev/snd/controlC0";
pub const PCM_PATH: &str = "/dev/snd/pcmC0D0p";

// Состояния потока (`snd_pcm_state_t`).
const STATE_OPEN: i32 = 0;
const STATE_SETUP: i32 = 1;
const STATE_PREPARED: i32 = 2;
const STATE_RUNNING: i32 = 3;
const STATE_DRAINING: i32 = 5;

/// Версии протоколов: PCM 2.0.13 (до `USER_PVERSION`), управление 2.0.7.
const PCM_VERSION: i32 = (2 << 16) | 13;
const CTL_VERSION: i32 = (2 << 16) | 7;

/// Есть ли такой путь (для `stat`/`access`).
#[must_use]
pub fn exists(linux: &str) -> bool {
    matches!(linux, CONTROL_PATH | PCM_PATH | "/dev/snd")
}

/// Открыть устройство звука; `None` — путь не наш.
pub fn open(linux: &str, flags: usize) -> Option<i64> {
    const O_NONBLOCK: usize = 0o4000;
    const O_CLOEXEC: usize = 0o2_000_000;
    let object: Arc<dyn Object> = match linux {
        CONTROL_PATH => Arc::new(Control),
        PCM_PATH => Arc::new(Pcm { state: Mutex::new(Stream::default()) }),
        _ => return None,
    };
    Some(match crate::user::with_current(|program| {
        let fd = program.files.install_object(object, flags & O_NONBLOCK != 0)?;
        program.files.set_cloexec(fd, flags & O_CLOEXEC != 0);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    })
}

/// ioctl на дескриптор звука; `None` — дескриптор не наш.
pub fn ioctl(fd: usize, request: usize, arg: usize) -> Option<i64> {
    let (object, nonblock) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    let kind = (request >> 8) & 0xff;
    let nr = request & 0xff;
    if let Some(pcm) = object.as_any().downcast_ref::<Pcm>() {
        return Some(if kind == usize::from(b'A') { pcm.ioctl(nr, arg, nonblock) } else { -e::ENOTTY });
    }
    if object.as_any().downcast_ref::<Control>().is_some() {
        return Some(if kind == usize::from(b'U') { control_ioctl(nr, arg) } else { -e::ENOTTY });
    }
    None
}

fn answer(result: Result<(), i64>) -> i64 {
    match result {
        Ok(()) => 0,
        Err(err) => err,
    }
}

fn put_str(buf: &mut [u8], at: usize, len: usize, text: &str) {
    let bytes = text.as_bytes();
    let n = bytes.len().min(len - 1);
    buf[at..at + n].copy_from_slice(&bytes[..n]);
}

// ---- управление (controlC0) ---------------------------------------------------

struct Control;

impl Object for Control {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, i64> {
        Err(-e::EAGAIN)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        (false, false, false)
    }
    fn mode(&self) -> u32 {
        0o020_000 // S_IFCHR
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn control_ioctl(nr: usize, arg: usize) -> i64 {
    match nr {
        // PVERSION
        0x00 => answer(put(arg, CTL_VERSION)),
        // CARD_INFO: struct snd_ctl_card_info, 376 байт.
        0x01 => {
            let mut info = vec![0u8; 376];
            put_str(&mut info, 8, 16, "FossAi");
            put_str(&mut info, 24, 16, "FossAi");
            put_str(&mut info, 40, 32, "FossAi sound");
            put_str(&mut info, 72, 80, "FossAi sound (virtio-snd or HDA), 48 kHz stereo");
            put_str(&mut info, 168, 80, "FossAi");
            answer(write_user(arg, &info))
        }
        // ELEM_LIST: регуляторов нет — пустой список.
        0x10 => {
            let Ok(mut list) = read_user(arg, 16) else { return -e::EFAULT };
            list[8..16].fill(0); // used, count
            answer(write_user(arg, &list))
        }
        // SUBSCRIBE_EVENTS, POWER_STATE
        0x16 | 0xd1 => {
            let _ = put(arg, 0i32);
            0
        }
        // PCM_NEXT_DEVICE: -1 → 0 → -1.
        0x30 => match get::<i32>(arg) {
            Ok(device) => answer(put(arg, if device < 0 { 0i32 } else { -1 })),
            Err(err) => err,
        },
        // PCM_INFO: только устройство 0, воспроизведение, подустройство 0.
        0x31 => {
            let Ok(head) = read_user(arg, 12) else { return -e::EFAULT };
            let device = u32::from_ne_bytes([head[0], head[1], head[2], head[3]]);
            let subdevice = u32::from_ne_bytes([head[4], head[5], head[6], head[7]]);
            let stream = i32::from_ne_bytes([head[8], head[9], head[10], head[11]]);
            if device != 0 || stream != 0 || subdevice != 0 {
                return -e::ENOENT;
            }
            answer(write_user(arg, &pcm_info()))
        }
        // PCM_PREFER_SUBDEVICE
        0x32 => 0,
        _ => -e::ENOTTY,
    }
}

/// struct snd_pcm_info, 288 байт.
fn pcm_info() -> alloc::vec::Vec<u8> {
    let mut info = vec![0u8; 288];
    put_str(&mut info, 16, 64, "FossAi PCM");
    put_str(&mut info, 80, 80, "FossAi PCM");
    put_str(&mut info, 160, 32, "subdevice #0");
    info[200..204].copy_from_slice(&1u32.to_ne_bytes()); // subdevices_count
    info[204..208].copy_from_slice(&1u32.to_ne_bytes()); // subdevices_avail
    info
}

// ---- воспроизведение (pcmC0D0p) -------------------------------------------------

struct Stream {
    state: i32,
    /// Кадры, записанные программой и отданные картой, по модулю `boundary`.
    appl: u64,
    hw: u64,
    boundary: u64,
    avail_min: u64,
    start_threshold: u64,
    /// Когда `hw` сдвигался последний раз, нс от загрузки (пока RUNNING).
    clock: u64,
    /// Доля кадра, не вошедшая в `hw` (нс × частота), — чтобы не терять время.
    rest: u64,
}

impl Default for Stream {
    fn default() -> Self {
        Self {
            state: STATE_OPEN,
            appl: 0,
            hw: 0,
            boundary: BUFFER_FRAMES << 40,
            avail_min: PERIOD_FRAMES,
            start_threshold: 1,
            clock: 0,
            rest: 0,
        }
    }
}

impl Stream {
    fn wrap(&self, value: u64) -> u64 {
        value % self.boundary
    }
    /// Записано, но не сыграно.
    fn queued(&self) -> u64 {
        (self.appl + self.boundary - self.hw) % self.boundary
    }
    fn avail(&self) -> u64 {
        BUFFER_FRAMES.saturating_sub(self.queued())
    }
    /// Сдвинуть указатель карты по часам.
    fn update(&mut self) {
        let now = crate::time::uptime_ns();
        if self.state != STATE_RUNNING && self.state != STATE_DRAINING {
            self.clock = now;
            return;
        }
        let total = (now.saturating_sub(self.clock)) * RATE + self.rest;
        let frames = total / 1_000_000_000;
        self.rest = total % 1_000_000_000;
        self.clock = now;
        let step = frames.min(self.queued());
        if step < frames {
            // Недобор: карта играет тишину, долг не копится.
            self.rest = 0;
        }
        self.hw = self.wrap(self.hw + step);
        if self.state == STATE_DRAINING && self.queued() == 0 {
            self.state = STATE_SETUP;
        }
    }
    fn start(&mut self) {
        self.state = STATE_RUNNING;
        self.clock = crate::time::uptime_ns();
        self.rest = 0;
    }
    /// Через сколько мс освободится `need` кадров.
    fn wait_ms(&self, need: u64) -> u64 {
        let missing = need.saturating_sub(self.avail());
        (missing * 1000).div_ceil(RATE).max(1)
    }
}

struct Pcm {
    state: Mutex<Stream>,
}

impl Object for Pcm {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        let mut stream = self.state.lock();
        stream.update();
        let writable = match stream.state {
            STATE_PREPARED | STATE_RUNNING => stream.avail() >= stream.avail_min.max(1),
            STATE_DRAINING => false,
            _ => true,
        };
        (false, writable, false)
    }
    fn mode(&self) -> u32 {
        0o020_000
    }
    fn deadline(&self) -> Option<u64> {
        let mut stream = self.state.lock();
        stream.update();
        (stream.state == STATE_RUNNING || stream.state == STATE_DRAINING)
            .then(|| crate::time::uptime_ms() + stream.wait_ms(stream.avail_min.max(1)))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// Параметры `snd_pcm_hw_params` (номера).
const P_ACCESS: usize = 0;
const P_FORMAT: usize = 1;
const P_SUBFORMAT: usize = 2;
const HW_PARAMS_BYTES: usize = 608;
const MASKS_AT: usize = 4;
const INTERVALS_AT: usize = 260;
const RMASK_AT: usize = 512;

/// Точные значения целых интервалов (номер параметра → значение).
const FIXED: [(usize, u32); 10] = [
    (8, 16),                                     // SAMPLE_BITS
    (9, 32),                                     // FRAME_BITS
    (10, CHANNELS),                              // CHANNELS
    (11, RATE as u32),                           // RATE
    (13, PERIOD_FRAMES as u32),                  // PERIOD_SIZE
    (14, (PERIOD_FRAMES * FRAME_BYTES) as u32),  // PERIOD_BYTES
    (15, PERIODS as u32),                        // PERIODS
    (17, BUFFER_FRAMES as u32),                  // BUFFER_SIZE
    (18, (BUFFER_FRAMES * FRAME_BYTES) as u32),  // BUFFER_BYTES
    (19, 0),                                     // TICK_TIME
];

/// Времена — не целые микросекунды: открытый интервал вокруг.
const TIMES: [(usize, u32); 2] = [
    (12, (PERIOD_FRAMES * 1_000_000 / RATE) as u32), // PERIOD_TIME, 21333.(3)
    (16, (BUFFER_FRAMES * 1_000_000 / RATE) as u32), // BUFFER_TIME, 85333.(3)
];

fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn set_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_ne_bytes());
}

/// Сузить маску параметра до одного бита; `false` — бит запрещён запросом.
fn refine_mask(buf: &mut [u8], param: usize, bit: usize) -> bool {
    let at = MASKS_AT + param * 32;
    let word = u32_at(buf, at + (bit / 32) * 4);
    if word & (1 << (bit % 32)) == 0 {
        return false;
    }
    buf[at..at + 32].fill(0);
    set_u32(buf, at + (bit / 32) * 4, 1 << (bit % 32));
    true
}

/// Интервал: `min, max`, биты openmin/openmax/integer/empty.
fn interval(buf: &[u8], param: usize) -> (u32, u32, u32) {
    let at = INTERVALS_AT + (param - 8) * 12;
    (u32_at(buf, at), u32_at(buf, at + 4), u32_at(buf, at + 8))
}

fn set_interval(buf: &mut [u8], param: usize, min: u32, max: u32, bits: u32) {
    let at = INTERVALS_AT + (param - 8) * 12;
    set_u32(buf, at, min);
    set_u32(buf, at + 4, max);
    set_u32(buf, at + 8, bits);
}

/// Содержит ли интервал запроса отрезок `(lo, hi)` (точку при `lo == hi`).
fn admits(buf: &[u8], param: usize, lo: u32, hi: u32, open: bool) -> bool {
    let (min, max, bits) = interval(buf, param);
    if bits & 8 != 0 {
        return false;
    }
    let (openmin, openmax) = (bits & 1 != 0, bits & 2 != 0);
    if open {
        // Пересечение с открытым (lo, hi).
        min < hi && max > lo
    } else {
        (min < lo || (min == lo && !openmin)) && (lo < max || (lo == max && !openmax))
    }
}

/// `HW_REFINE`/`HW_PARAMS`: сузить запрос до нашего режима.
fn refine(buf: &mut [u8]) -> Result<(), i64> {
    if !refine_mask(buf, P_ACCESS, 3) // RW_INTERLEAVED
        || !refine_mask(buf, P_FORMAT, 2) // S16_LE
        || !refine_mask(buf, P_SUBFORMAT, 0)
    {
        return Err(-e::EINVAL);
    }
    for (param, value) in FIXED {
        if !admits(buf, param, value, value, false) {
            return Err(-e::EINVAL);
        }
        set_interval(buf, param, value, value, 4); // integer
    }
    for (param, value) in TIMES {
        if !admits(buf, param, value, value + 1, true) {
            return Err(-e::EINVAL);
        }
        set_interval(buf, param, value, value + 1, 3); // openmin | openmax
    }
    set_u32(buf, RMASK_AT, 0);
    set_u32(buf, RMASK_AT + 4, !0); // cmask: всё могло измениться
    set_u32(buf, RMASK_AT + 8, 0x0001_0110); // info: INTERLEAVED | BLOCK_TRANSFER | BATCH
    set_u32(buf, RMASK_AT + 12, 16); // msbits
    set_u32(buf, RMASK_AT + 16, RATE as u32); // rate_num
    set_u32(buf, RMASK_AT + 20, 1); // rate_den
    buf[RMASK_AT + 24..RMASK_AT + 32].fill(0); // fifo_size
    Ok(())
}

impl Pcm {
    fn ioctl(&self, nr: usize, arg: usize, nonblock: bool) -> i64 {
        match nr {
            // PVERSION
            0x00 => answer(put(arg, PCM_VERSION)),
            // INFO
            0x01 => answer(write_user(arg, &pcm_info())),
            // TSTAMP, TTSTAMP, USER_PVERSION
            0x02..=0x04 => 0,
            // HW_REFINE, HW_PARAMS
            0x10 | 0x11 => {
                let Ok(mut buf) = read_user(arg, HW_PARAMS_BYTES) else { return -e::EFAULT };
                if let Err(err) = refine(&mut buf) {
                    return err;
                }
                if nr == 0x11 {
                    let mut stream = self.state.lock();
                    *stream = Stream { state: STATE_SETUP, ..Stream::default() };
                }
                answer(write_user(arg, &buf))
            }
            // HW_FREE
            0x12 => {
                self.state.lock().state = STATE_OPEN;
                0
            }
            // SW_PARAMS: struct snd_pcm_sw_params, 136 байт.
            0x13 => {
                let Ok(buf) = read_user(arg, 136) else { return -e::EFAULT };
                let word = |at: usize| u64::from_ne_bytes(buf[at..at + 8].try_into().unwrap_or([0; 8]));
                let mut stream = self.state.lock();
                stream.avail_min = word(16).max(1);
                stream.start_threshold = word(32);
                let boundary = word(64);
                if boundary >= BUFFER_FRAMES {
                    stream.boundary = boundary;
                }
                0
            }
            // STATUS, STATUS_EXT: struct snd_pcm_status, 152 байта.
            0x20 | 0x24 => {
                let mut stream = self.state.lock();
                stream.update();
                let mut status = vec![0u8; 152];
                status[0..4].copy_from_slice(&stream.state.to_ne_bytes());
                let now = crate::time::uptime_ns();
                let ts = |buf: &mut [u8], at: usize| {
                    buf[at..at + 8].copy_from_slice(&(now / 1_000_000_000).to_ne_bytes());
                    buf[at + 8..at + 16].copy_from_slice(&(now % 1_000_000_000).to_ne_bytes());
                };
                ts(&mut status, 24);
                status[40..48].copy_from_slice(&stream.appl.to_ne_bytes());
                status[48..56].copy_from_slice(&stream.hw.to_ne_bytes());
                status[56..64].copy_from_slice(&stream.queued().to_ne_bytes());
                status[64..72].copy_from_slice(&stream.avail().to_ne_bytes());
                status[72..80].copy_from_slice(&stream.avail().to_ne_bytes());
                drop(stream);
                answer(write_user(arg, &status))
            }
            // DELAY
            0x21 => {
                let mut stream = self.state.lock();
                stream.update();
                let delay = stream.queued() as i64;
                drop(stream);
                answer(put(arg, delay))
            }
            // HWSYNC
            0x22 => {
                self.state.lock().update();
                0
            }
            // SYNC_PTR: struct snd_pcm_sync_ptr, 136 байт.
            0x23 => self.sync_ptr(arg),
            // CHANNEL_INFO: только для mmap; честный ответ на всякий случай.
            0x32 => {
                let Ok(channel) = get::<u32>(arg) else { return -e::EFAULT };
                let mut info = [0u8; 24];
                info[0..4].copy_from_slice(&channel.to_ne_bytes());
                info[16..20].copy_from_slice(&(channel * 16).to_ne_bytes());
                info[20..24].copy_from_slice(&32u32.to_ne_bytes());
                answer(write_user(arg, &info))
            }
            // PREPARE, RESET
            0x40 | 0x41 => {
                let mut stream = self.state.lock();
                if stream.state == STATE_OPEN {
                    return -e::EBADFD;
                }
                stream.state = STATE_PREPARED;
                stream.appl = 0;
                stream.hw = 0;
                stream.rest = 0;
                0
            }
            // START
            0x42 => {
                let mut stream = self.state.lock();
                if stream.state != STATE_PREPARED {
                    return -e::EBADFD;
                }
                stream.start();
                0
            }
            // DROP
            0x43 => {
                let mut stream = self.state.lock();
                if stream.state != STATE_OPEN {
                    stream.state = STATE_SETUP;
                }
                0
            }
            // DRAIN: доиграть записанное.
            0x44 => self.drain(nonblock),
            // PAUSE, RESUME, XRUN: не нужны libasound для воспроизведения Wine.
            0x45 | 0x47 | 0x48 => -e::ENOSYS,
            // REWIND, FORWARD: назад/вперёд нельзя — 0 кадров.
            0x46 | 0x49 => 0,
            // WRITEI_FRAMES: struct snd_xferi { result, buf, frames }.
            0x50 => self.writei(arg, nonblock),
            // LINK, UNLINK
            0x60 | 0x61 => -e::ENOSYS,
            _ => -e::ENOTTY,
        }
    }

    fn sync_ptr(&self, arg: usize) -> i64 {
        const HWSYNC: u32 = 1;
        const APPL: u32 = 2;
        const AVAIL_MIN: u32 = 4;
        let Ok(mut buf) = read_user(arg, 136) else { return -e::EFAULT };
        let flags = u32_at(&buf, 0);
        let word = |buf: &[u8], at: usize| u64::from_ne_bytes(buf[at..at + 8].try_into().unwrap_or([0; 8]));
        let mut stream = self.state.lock();
        if flags & APPL == 0 {
            stream.appl = stream.wrap(word(&buf, 72));
        }
        if flags & AVAIL_MIN == 0 {
            stream.avail_min = word(&buf, 80).max(1);
        }
        let _ = flags & HWSYNC;
        stream.update();
        // status: state, hw_ptr, tstamp, suspended_state.
        buf[8..12].copy_from_slice(&stream.state.to_ne_bytes());
        buf[16..24].copy_from_slice(&stream.hw.to_ne_bytes());
        let now = crate::time::uptime_ns();
        buf[24..32].copy_from_slice(&(now / 1_000_000_000).to_ne_bytes());
        buf[32..40].copy_from_slice(&(now % 1_000_000_000).to_ne_bytes());
        // control: appl_ptr, avail_min.
        buf[72..80].copy_from_slice(&stream.appl.to_ne_bytes());
        buf[80..88].copy_from_slice(&stream.avail_min.to_ne_bytes());
        drop(stream);
        answer(write_user(arg, &buf))
    }

    fn writei(&self, arg: usize, nonblock: bool) -> i64 {
        let Ok(head) = read_user(arg, 24) else { return -e::EFAULT };
        let data = usize::from_ne_bytes(head[8..16].try_into().unwrap_or([0; 8]));
        let frames = u64::from_ne_bytes(head[16..24].try_into().unwrap_or([0; 8]));
        let result = wait_io(
            nonblock,
            || {
                let mut stream = self.state.lock();
                stream.update();
                Some(crate::time::uptime_ms() + stream.wait_ms(1))
            },
            || {
                let mut stream = self.state.lock();
                match stream.state {
                    STATE_PREPARED | STATE_RUNNING => {}
                    STATE_OPEN | STATE_SETUP => return Err(-e::EBADFD),
                    _ => return Err(-e::EPIPE),
                }
                stream.update();
                let take = frames.min(stream.avail());
                if take == 0 && frames > 0 {
                    return Err(-e::EAGAIN);
                }
                Ok(take)
            },
        );
        let take = match result {
            Ok(take) => take,
            Err(err) => return err,
        };
        let bytes = match read_user(data, (take * FRAME_BYTES) as usize) {
            Ok(bytes) => bytes,
            Err(err) => return err,
        };
        if crate::sound::present() {
            let _ = crate::sound::feed(&bytes);
        }
        {
            let mut stream = self.state.lock();
            stream.update();
            stream.appl = stream.wrap(stream.appl + take);
            if stream.state == STATE_PREPARED && stream.queued() >= stream.start_threshold.max(1) {
                stream.start();
            }
        }
        let _ = put(arg, take as i64);
        0
    }

    fn drain(&self, nonblock: bool) -> i64 {
        {
            let mut stream = self.state.lock();
            match stream.state {
                STATE_PREPARED if stream.queued() > 0 => {
                    stream.start();
                    stream.state = STATE_DRAINING;
                }
                STATE_RUNNING => stream.state = STATE_DRAINING,
                STATE_PREPARED => {
                    stream.state = STATE_SETUP;
                    return 0;
                }
                _ => return 0,
            }
        }
        let done = wait_io(
            nonblock,
            || {
                let mut stream = self.state.lock();
                stream.update();
                Some(crate::time::uptime_ms() + stream.wait_ms(BUFFER_FRAMES))
            },
            || {
                let mut stream = self.state.lock();
                stream.update();
                if stream.state == STATE_DRAINING { Err(-e::EAGAIN) } else { Ok(()) }
            },
        );
        if crate::sound::present() {
            let _ = crate::sound::drain();
        }
        answer(done)
    }
}
