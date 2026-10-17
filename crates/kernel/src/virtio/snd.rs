//! virtio-snd: звук (фаза 67e).
//!
//! # Как устроено
//!
//! Устройство описывает потоки (`PCM_INFO`), драйвер выбирает выходной,
//! задаёт ему формат (`PCM_SET_PARAMS`: 16 бит со знаком, 48 кГц, два канала),
//! готовит и запускает (`PCM_PREPARE`, `PCM_START`). Звук идёт по очереди
//! передачи: каждый период — цепочка из трёх дескрипторов, заголовок с номером
//! потока, сами отсчёты и место под ответ устройства. Устройство возвращает
//! период, когда **сыграло** его, — значит, темп задаёт оно, а драйвер только
//! держит в очереди несколько периодов, чтобы между ними не было дыр.
//!
//! # Чего нет
//!
//! Записи (поток ввода), уведомлений (`eventq`), других форматов и частот.
//!
//! # Поток, а не отдельные звуки (фаза 67f)
//!
//! Программа отдаёт отсчёты порциями любого размера (`SYS_SOUND`), и поток
//! между вызовами не прерывается: недолитый период копится, пока не
//! наполнится, поток запускается, когда в очереди два периода, а
//! останавливается только по `drain` — когда сыграно всё. Порция в сотню
//! кадров поэтому не превращается в период тишины с щелчком.

use core::sync::atomic::{Ordering, fence};

use super::{DESC_F_NEXT, DESC_F_WRITE, FEATURE_VERSION_1, Queue, Transport, VirtioError};
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer};
use crate::pci::{self, Device};

/// Идентификатор устройства virtio-snd в современном виде (`0x1040 + 25`).
pub const DEVICE_VIRTIO_SND: u16 = 0x1059;

// Запросы и ответы — раздел 5.14.6 спецификации virtio 1.2.
const R_PCM_INFO: u32 = 0x0100;
const R_PCM_SET_PARAMS: u32 = 0x0101;
const R_PCM_PREPARE: u32 = 0x0102;
const R_PCM_RELEASE: u32 = 0x0103;
const R_PCM_START: u32 = 0x0104;
const R_PCM_STOP: u32 = 0x0105;
const S_OK: u32 = 0x8000;

const DIRECTION_OUTPUT: u8 = 0;
/// 16 бит со знаком — номер формата в спецификации.
const FORMAT_S16: u8 = 5;
/// 48 000 Гц — номер частоты в спецификации.
const RATE_48000: u8 = 7;
pub const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u8 = 2;

/// Описание потока в ответе `PCM_INFO`.
const PCM_INFO_SIZE: usize = 32;
/// Сколько потоков спрашиваем за раз — больше у QEMU не бывает.
const MAX_STREAMS: usize = 8;
/// Где в буфере команды лежит ответ.
const RESPONSE_AT: usize = 256;

/// Период — 1024 кадра по два канала по два байта, около 21 мс.
const PERIOD_BYTES: usize = 4096;
/// Периодов в очереди одновременно.
const SLOTS: usize = 4;
/// Раскладка буфера периода: заголовок, отсчёты, ответ.
const SLOT_DATA: usize = 16;
const SLOT_STATUS: usize = SLOT_DATA + PERIOD_BYTES;
const SLOT_SIZE: usize = SLOT_STATUS + 16;

/// Предел опроса кольца управления.
const POLL_LIMIT: u32 = 200_000_000;

/// Ждущий мьютекс, а не спин-лок: звук играется минуты, и драйвер спит,
/// пока устройство играет период, — под спин-локом с запрещёнными
/// прерываниями это остановило бы машину.
static SND: crate::sync::Mutex<Option<VirtioSnd>> = crate::sync::Mutex::new(None);

struct VirtioSnd {
    _transport: Transport,
    control: Queue,
    tx: Queue,
    command: DmaBuffer,
    slots: [DmaBuffer; SLOTS],
    /// Выходной поток, которому задан формат.
    stream: u32,
    /// Период в очереди устройства.
    in_flight: [bool; SLOTS],
    /// Период, который ещё доливается: место и сколько байт в нём.
    filling: Option<(usize, usize)>,
    /// Поток подготовлен (`PCM_PREPARE`) и запущен (`PCM_START`).
    prepared: bool,
    started: bool,
}

/// Есть ли звук.
#[must_use]
pub fn present() -> bool {
    SND.lock().is_some()
}

/// Найти устройство и подготовить выходной поток.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn probe(root: &pci::Root) -> bool {
    // SAFETY: контракт функции.
    let Some(device) = (unsafe { pci::find_by_id(root, pci::VENDOR_VIRTIO, &[DEVICE_VIRTIO_SND]) })
    else {
        return false;
    };
    // SAFETY: контракт функции.
    match unsafe { VirtioSnd::attach(&device) } {
        Ok(snd) => {
            crate::devices::claim(device.address, "virtio-snd");
            kprintln!(
                "  virtio-snd  : at {}, output stream {} at 48000 Hz, 16-bit stereo",
                device.address,
                snd.stream
            );
            *SND.lock() = Some(snd);
            true
        }
        Err(err) => {
            kprintln!("  virtio-snd  : at {} unusable: {err}", device.address);
            false
        }
    }
}

/// Сыграть синус `hz` длиной `ms` миллисекунд. Ждёт, пока устройство сыграет
/// всё: звук задаёт темп сам.
pub fn play_tone(hz: u32, ms: u32) -> Result<(), VirtioError> {
    let mut guard = SND.lock();
    let snd = guard.as_mut().ok_or(VirtioError::NoCapabilities)?;
    let frames = u64::from(SAMPLE_RATE) * u64::from(ms) / 1000;
    let mut tone = Tone::new(hz);
    let mut period = [0u8; PERIOD_BYTES];
    let mut left = frames;
    while left > 0 {
        let take = left.min((PERIOD_BYTES / 4) as u64) as usize;
        for frame in 0..take {
            let value = tone.next().to_le_bytes();
            period[frame * 4..frame * 4 + 2].copy_from_slice(&value);
            period[frame * 4 + 2..frame * 4 + 4].copy_from_slice(&value);
        }
        snd.feed(&period[..take * 4])?;
        left -= take as u64;
    }
    snd.drain()
}

/// Отдать устройству отсчёты (16 бит со знаком, два канала, 48 кГц): ждёт,
/// пока для них найдётся место в очереди, — темп задаёт устройство.
/// Хвост меньше периода копится до следующего вызова или до [`drain`].
pub fn feed(bytes: &[u8]) -> Result<(), VirtioError> {
    let mut guard = SND.lock();
    let snd = guard.as_mut().ok_or(VirtioError::NoCapabilities)?;
    snd.feed(bytes)
}

/// Доиграть всё отданное и остановить поток.
pub fn drain() -> Result<(), VirtioError> {
    let mut guard = SND.lock();
    let snd = guard.as_mut().ok_or(VirtioError::NoCapabilities)?;
    snd.drain()
}

/// Байт в периоде — столько стоит отдавать за раз, чтобы не ждать зря.
pub const PERIOD: usize = PERIOD_BYTES;

/// Синус на целых числах: фаза — 32-битный счётчик, отсчёт — по приближению
/// Бхаскары I, `16x(π−x) / (5π² − 4x(π−x))`. Погрешность — доли процента:
/// для гудка этого хватает с запасом, а плавающей точки в ядре нет.
struct Tone {
    phase: u32,
    step: u32,
}

impl Tone {
    fn new(hz: u32) -> Self {
        let step = ((u64::from(hz) << 32) / u64::from(SAMPLE_RATE)) as u32;
        Self { phase: 0, step }
    }

    /// Следующий отсчёт, громкость — четверть шкалы.
    fn next(&mut self) -> i16 {
        // Половина периода — `x` от 0 до π, в долях 1/65536 от π.
        let half = self.phase >> 31;
        let x = i64::from((self.phase >> 15) & 0xFFFF);
        let pi = 65_536i64;
        let product = x * (pi - x);
        let value = 16 * product * 8192 / (5 * pi * pi - 4 * product);
        self.phase = self.phase.wrapping_add(self.step);
        let value = value.clamp(0, i64::from(i16::MAX)) as i16;
        if half == 0 { value } else { -value }
    }
}

impl VirtioSnd {
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц.
    unsafe fn attach(device: &Device) -> Result<Self, VirtioError> {
        // SAFETY: устройство не тронет память до `DRIVER_OK`.
        unsafe { device.enable_bus_master() };
        // SAFETY: контракт функции.
        let transport = unsafe { Transport::open(device) }?;
        transport.negotiate(FEATURE_VERSION_1)?;
        let control = Queue::new(&transport, 0)?;
        let tx = Queue::new(&transport, 2)?;
        let command = dma::alloc(1024).map_err(VirtioError::NoMemory)?;
        let slots = [
            dma::alloc(SLOT_SIZE).map_err(VirtioError::NoMemory)?,
            dma::alloc(SLOT_SIZE).map_err(VirtioError::NoMemory)?,
            dma::alloc(SLOT_SIZE).map_err(VirtioError::NoMemory)?,
            dma::alloc(SLOT_SIZE).map_err(VirtioError::NoMemory)?,
        ];
        transport.set_driver_ok();

        // Число потоков — второе поле конфигурации (после числа гнёзд).
        // SAFETY: окно конфигурации отображено; поле — четыре байта с 4.
        let streams = unsafe {
            u32::from_le_bytes([
                transport.device_config8(4),
                transport.device_config8(5),
                transport.device_config8(6),
                transport.device_config8(7),
            ])
        };
        let mut snd = Self {
            _transport: transport,
            control,
            tx,
            command,
            slots,
            stream: 0,
            in_flight: [false; SLOTS],
            filling: None,
            prepared: false,
            started: false,
        };
        snd.stream = snd.find_output(streams)?;
        snd.set_params()?;
        Ok(snd)
    }

    /// Первый выходной поток, который умеет 16 бит, 48 кГц и два канала.
    fn find_output(&mut self, streams: u32) -> Result<u32, VirtioError> {
        let count = (streams as usize).min(MAX_STREAMS);
        if count == 0 {
            return Err(VirtioError::NoQueue(0));
        }
        self.header(R_PCM_INFO);
        self.put_u32(4, 0);
        self.put_u32(8, count as u32);
        self.put_u32(12, PCM_INFO_SIZE as u32);
        self.submit(16, 4 + count * PCM_INFO_SIZE)?;
        for index in 0..count {
            let at = 4 + index * PCM_INFO_SIZE;
            let formats = self.response_u64(at + 8);
            let rates = self.response_u64(at + 16);
            let direction = self.response_u8(at + 24);
            let channels_min = self.response_u8(at + 25);
            let channels_max = self.response_u8(at + 26);
            if direction == DIRECTION_OUTPUT
                && formats & (1 << FORMAT_S16) != 0
                && rates & (1 << RATE_48000) != 0
                && (channels_min..=channels_max).contains(&CHANNELS)
            {
                return Ok(index as u32);
            }
        }
        Err(VirtioError::NoQueue(2))
    }

    fn set_params(&mut self) -> Result<(), VirtioError> {
        self.header(R_PCM_SET_PARAMS);
        self.put_u32(4, self.stream);
        self.put_u32(8, (PERIOD_BYTES * SLOTS) as u32);
        self.put_u32(12, PERIOD_BYTES as u32);
        self.put_u32(16, 0);
        // Каналы, формат, частота — по байту, и байт запаса.
        self.put_u32(20, u32::from(CHANNELS) | (u32::from(FORMAT_S16) << 8) | (u32::from(RATE_48000) << 16));
        self.submit(24, 4)
    }

    fn stream_command(&mut self, code: u32) -> Result<(), VirtioError> {
        self.header(code);
        self.put_u32(4, self.stream);
        self.submit(8, 4)
    }

    /// Долить отсчёты в периоды и отдать полные устройству.
    fn feed(&mut self, mut bytes: &[u8]) -> Result<(), VirtioError> {
        // Кадр — четыре байта; обрывок кадра отбрасывается, а не сдвигает
        // каналы местами до конца звука.
        let whole = bytes.len() / 4 * 4;
        bytes = &bytes[..whole];
        if !self.prepared {
            self.stream_command(R_PCM_PREPARE)?;
            self.prepared = true;
        }
        while !bytes.is_empty() {
            let (slot, used) = match self.filling {
                Some(filling) => filling,
                None => (self.free_slot()?, 0),
            };
            let take = (PERIOD_BYTES - used).min(bytes.len());
            // SAFETY: место не в очереди устройства — его никто не читает;
            // смещения внутри буфера периода.
            unsafe {
                let data = self.slots[slot].as_ptr::<u8>().add(SLOT_DATA + used);
                core::ptr::copy_nonoverlapping(bytes.as_ptr(), data, take);
            }
            bytes = &bytes[take..];
            if used + take == PERIOD_BYTES {
                self.filling = None;
                self.offer(slot, PERIOD_BYTES)?;
            } else {
                self.filling = Some((slot, used + take));
            }
        }
        Ok(())
    }

    /// Доиграть всё и остановить поток.
    fn drain(&mut self) -> Result<(), VirtioError> {
        if let Some((slot, used)) = self.filling.take() {
            self.offer(slot, used)?;
        }
        if self.in_flight.iter().any(|busy| *busy) && !self.started {
            self.stream_command(R_PCM_START)?;
            self.started = true;
        }
        let result = (|| {
            while self.in_flight.iter().any(|busy| *busy) {
                self.reclaim(true)?;
            }
            Ok(())
        })();
        if self.started {
            let _ = self.stream_command(R_PCM_STOP);
            self.started = false;
        }
        if self.prepared {
            let _ = self.stream_command(R_PCM_RELEASE);
            self.prepared = false;
        }
        // Что не вернулось — забирается: следующий звук не начнётся с него.
        while self.tx.take_used().is_some() {}
        self.in_flight = [false; SLOTS];
        result
    }

    /// Свободное место под период — ждёт, пока устройство вернёт сыгранный.
    fn free_slot(&mut self) -> Result<usize, VirtioError> {
        loop {
            if let Some(slot) = self.in_flight.iter().position(|busy| !*busy) {
                return Ok(slot);
            }
            self.reclaim(true)?;
        }
    }

    /// Забрать сыгранные периоды. `wait` — ждать хотя бы одного.
    fn reclaim(&mut self, wait: bool) -> Result<(), VirtioError> {
        // Срок — на случай устройства, которое не играет вовсе: четыре
        // периода — меньше десятой секунды, секунда с запасом.
        let deadline = crate::time::uptime_ms() + 1_000;
        loop {
            let mut any = false;
            while let Some((head, _)) = self.tx.take_used() {
                any = true;
                let slot = usize::from(head) / 3;
                if slot < SLOTS {
                    self.in_flight[slot] = false;
                    let status = self.slot_u32(slot, SLOT_STATUS);
                    if status != S_OK {
                        return Err(VirtioError::RequestFailed((status & 0xff) as u8));
                    }
                }
            }
            if any || !wait {
                return Ok(());
            }
            if crate::time::uptime_ms() > deadline {
                return Err(VirtioError::Timeout);
            }
            crate::sched::sleep_ms(2);
        }
    }

    /// Отдать период устройству и, когда в очереди их два, запустить поток.
    fn offer(&mut self, slot: usize, bytes: usize) -> Result<(), VirtioError> {
        let buffer = &self.slots[slot];
        // SAFETY: буфер принадлежит драйверу и не в очереди устройства.
        unsafe {
            let base = buffer.as_ptr::<u8>();
            base.cast::<u32>().write_volatile(self.stream);
            base.add(SLOT_STATUS).cast::<u32>().write_volatile(0xFFFF_FFFF);
        }
        let phys = buffer.phys().as_u64();
        let head = (slot * 3) as u16;
        self.tx.set_descriptor(head, phys, 4, DESC_F_NEXT, head + 1);
        self.tx.set_descriptor(head + 1, phys + SLOT_DATA as u64, bytes as u32, DESC_F_NEXT, head + 2);
        self.tx.set_descriptor(head + 2, phys + SLOT_STATUS as u64, 8, DESC_F_WRITE, 0);
        fence(Ordering::SeqCst);
        self.tx.offer(head);
        self.tx.kick();
        self.in_flight[slot] = true;
        if !self.started && self.in_flight.iter().filter(|busy| **busy).count() >= 2 {
            self.stream_command(R_PCM_START)?;
            self.started = true;
        }
        Ok(())
    }

    fn slot_u32(&self, slot: usize, at: usize) -> u32 {
        // SAFETY: смещение внутри буфера периода.
        unsafe { self.slots[slot].as_ptr::<u8>().add(at).cast::<u32>().read_volatile() }
    }

    // --- буфер команды -----------------------------------------------------

    fn header(&mut self, code: u32) {
        // SAFETY: буфер выделен на килобайт.
        unsafe { core::ptr::write_bytes(self.command.as_ptr::<u8>(), 0, 1024) };
        self.put_u32(0, code);
    }

    fn put_u32(&mut self, at: usize, value: u32) {
        // SAFETY: смещения команд меньше `RESPONSE_AT`.
        unsafe { self.command.as_ptr::<u8>().add(at).cast::<u32>().write_unaligned(value) };
    }

    fn response_u8(&self, at: usize) -> u8 {
        // SAFETY: ответ лежит с `RESPONSE_AT` и не длиннее буфера.
        unsafe { self.command.as_ptr::<u8>().add(RESPONSE_AT + at).read_volatile() }
    }

    fn response_u64(&self, at: usize) -> u64 {
        // SAFETY: см. выше.
        unsafe { self.command.as_ptr::<u8>().add(RESPONSE_AT + at).cast::<u64>().read_unaligned() }
    }

    fn submit(&mut self, request: usize, response: usize) -> Result<(), VirtioError> {
        let phys = self.command.phys().as_u64();
        self.control.set_descriptor(0, phys, request as u32, DESC_F_NEXT, 1);
        self.control.set_descriptor(1, phys + RESPONSE_AT as u64, response as u32, DESC_F_WRITE, 0);
        fence(Ordering::SeqCst);
        self.control.submit_and_wait(POLL_LIMIT, None)?;
        fence(Ordering::SeqCst);
        // SAFETY: ответ всегда начинается с поля состояния.
        let status = unsafe { self.command.as_ptr::<u8>().add(RESPONSE_AT).cast::<u32>().read_unaligned() };
        if status != S_OK {
            return Err(VirtioError::RequestFailed((status & 0xff) as u8));
        }
        Ok(())
    }
}
