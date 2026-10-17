//! virtio-gpu: экран, который показывает хост (фаза 67a).
//!
//! # Чем это отличается от stdvga и ramfb
//!
//! Там стол рисует прямо в память, которую адаптер показывает сам: каждая
//! точка уходит в некэшируемую память устройства, а эмулятор следит за
//! изменёнными страницами и перерисовывает их. Здесь картинка лежит в обычной
//! памяти машины — **ресурсе**, — и хост узнаёт об изменении только тогда,
//! когда ему скажут: `TRANSFER_TO_HOST_2D` переносит прямоугольник ресурса к
//! хосту, `RESOURCE_FLUSH` показывает его. Композитор и так знает, какие
//! прямоугольники изменил (`damage`), поэтому переносится ровно то, что
//! собрано, — и сборка кадра пишет в кэшируемую память, а не в память
//! устройства.
//!
//! # Почему загрузка без буфера от прошивки
//!
//! GOP у `VirtioGpuDxe` — только `Blt()`: линейного буфера, который загрузчик
//! мог бы передать ядру, нет (см. `xtask/src/qemu.rs`). Поэтому ресурс заводит
//! ядро само, прямо перед столом, и журнал загрузки на такой машине живёт
//! только в последовательной линии — как и задумано для машины без экрана до
//! стола.
//!
//! # Чего нет
//!
//! 3D (virgl, venus), нескольких экранов, EDID и смены размера окна хоста на
//! ходу: стол берёт размер, который устройство назвало при запуске, или тот,
//! что выбран в «Параметрах».
//!
//! # Аппаратный курсор (фаза 67b)
//!
//! Стрелку рисует хост поверх экрана: её картинка — свой ресурс 64×64 с
//! прозрачностью, отданный очереди `cursorq` (`UPDATE_CURSOR`), а движение
//! мыши — одна короткая команда `MOVE_CURSOR`. Композитор тогда стрелку не
//! рисует и при движении ничего не пересобирает: без курсора хоста каждое
//! движение стоило двух прямоугольников кадра — где стрелка была и где стала.

use core::sync::atomic::{Ordering, fence};

use boot_info::{Framebuffer, PixelFormat};

use super::{DESC_F_NEXT, DESC_F_WRITE, FEATURE_VERSION_1, Queue, Transport, VirtioError};
use crate::display::DisplayError;
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer};
use crate::mm::{PAGE_SIZE, PhysAddr};
use crate::pci::{self, Device};

/// Идентификатор устройства virtio-gpu в современном виде (`0x1040 + 16`).
pub const DEVICE_VIRTIO_GPU: u16 = 0x1050;

// Команды и ответы — раздел 5.7.6 спецификации virtio 1.1.
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_RESOURCE_UNREF: u32 = 0x0102;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
const CMD_UPDATE_CURSOR: u32 = 0x0300;
const CMD_MOVE_CURSOR: u32 = 0x0301;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// Байт 0 — синий, 1 — зелёный, 2 — красный, 3 — не используется: ровно то,
/// что `mini-ui` пишет в `u32` на little-endian (`0x00RRGGBB`).
const FORMAT_B8G8R8X8_UNORM: u32 = 2;
/// То же с прозрачностью в байте 3 — формат картинки курсора.
const FORMAT_B8G8R8A8_UNORM: u32 = 1;

/// Сторона картинки курсора: устройство принимает только 64×64.
pub const CURSOR_SIDE: u32 = 64;
/// Номер ресурса курсора — вне чередования ресурсов экрана (1 и 2).
const CURSOR_RESOURCE: u32 = 10;
/// Команда курсора: заголовок, место (экран, x, y, запас), ресурс, точка
/// касания (x, y) и запас.
const CURSOR_COMMAND_SIZE: usize = HEADER_SIZE + 32;

/// Заголовок каждой команды и ответа.
const HEADER_SIZE: usize = 24;
/// Ответ на `GET_DISPLAY_INFO`: заголовок и шестнадцать экранов по 24 байта.
const DISPLAY_INFO_SIZE: usize = HEADER_SIZE + 16 * 24;
/// Где в буфере команды лежит ответ.
const RESPONSE_AT: usize = 512;

/// Сколько раз опрашивать кольцо, прежде чем признать устройство зависшим.
/// Команды исполняет поток QEMU сразу по уведомлению; предел — на случай,
/// когда не исполнит вовсе.
const POLL_LIMIT: u32 = 200_000_000;

/// Номер единственного ресурса: экран один, и картинка у него одна.
const RESOURCE: u32 = 1;

/// Устройство, если оно нашлось и стол рисует в него.
static GPU: crate::sync::SpinLock<Option<VirtioGpu>> = crate::sync::SpinLock::new(None);

struct VirtioGpu {
    /// Окно регистров: держится, пока живо устройство.
    _transport: Transport,
    control: Queue,
    /// Команда (с нуля) и ответ (с [`RESPONSE_AT`]) — общая память с устройством.
    command: DmaBuffer,
    /// Размер, который устройство предлагает для экрана 0.
    preferred: (u32, u32),
    /// Ресурс, показанный на экране: его память и размер.
    screen: Option<Screen>,
    /// Номер показанного ресурса.
    current: u32,
    /// Очередь курсора и буфер её команды.
    cursor_queue: Queue,
    cursor_command: DmaBuffer,
    /// Картинка курсора у хоста есть: ресурс заведён и отдан очереди.
    cursor: Option<Cursor>,
    /// Память ресурса курсора — заводится один раз.
    cursor_memory: Option<PhysAddr>,
}

#[derive(Clone, Copy)]
struct Cursor {
    hot: (u32, u32),
    /// Показан ли сейчас: погашенный экран прячет и стрелку.
    shown: bool,
}

#[derive(Clone, Copy)]
struct Screen {
    memory: PhysAddr,
    pages: usize,
    width: u32,
    height: u32,
}

/// Есть ли virtio-gpu, в который рисует стол.
#[must_use]
pub fn active() -> bool {
    GPU.lock().as_ref().is_some_and(|gpu| gpu.screen.is_some())
}

/// Найти устройство и поднять его — без экрана: экран заводит
/// [`take_screen`].
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn probe(root: &pci::Root) -> bool {
    // SAFETY: контракт функции.
    let Some(device) = (unsafe { pci::find_by_id(root, pci::VENDOR_VIRTIO, &[DEVICE_VIRTIO_GPU]) })
    else {
        return false;
    };
    // SAFETY: контракт функции.
    match unsafe { VirtioGpu::attach(&device) } {
        Ok(gpu) => {
            crate::devices::claim(device.address, "virtio-gpu");
            kprintln!(
                "  virtio-gpu  : at {}, screen 0 offers {}x{}",
                device.address,
                gpu.preferred.0,
                gpu.preferred.1
            );
            *GPU.lock() = Some(gpu);
            true
        }
        Err(err) => {
            kprintln!("  virtio-gpu  : at {} unusable: {err}", device.address);
            false
        }
    }
}

/// Размер, который устройство предлагает для экрана.
#[must_use]
pub fn preferred() -> Option<(u32, u32)> {
    GPU.lock().as_ref().map(|gpu| gpu.preferred)
}

/// Завести ресурс `width`×`height`, показать его на экране 0 и отдать
/// описание буфера, в который рисует стол. Прежний ресурс снимается **после**
/// того, как экран переключён на новый.
pub fn take_screen(width: u32, height: u32) -> Result<Framebuffer, DisplayError> {
    let mut guard = GPU.lock();
    let gpu = guard.as_mut().ok_or(DisplayError::NoDriver("no virtio-gpu"))?;
    gpu.make_screen(width, height)
}

/// Показать изменившийся прямоугольник. Зовёт композитор после того, как
/// собрал его в буфере. Без устройства — ничего.
pub fn flush(x: i32, y: i32, w: u32, h: u32) {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    let Some(screen) = gpu.screen else {
        return;
    };
    // Прямоугольник обрезается по ресурсу: хост отвергает выходящий за край,
    // и отказ стоил бы кадра.
    let left = x.max(0) as u32;
    let top = y.max(0) as u32;
    let right = (i64::from(x) + i64::from(w)).clamp(0, i64::from(screen.width)) as u32;
    let bottom = (i64::from(y) + i64::from(h)).clamp(0, i64::from(screen.height)) as u32;
    if left >= right || top >= bottom {
        return;
    }
    let rect = (left, top, right - left, bottom - top);
    let current = gpu.current;
    if let Err(err) = gpu.transfer(current, rect, screen.width).and_then(|()| gpu.resource_flush(rect)) {
        kprintln!("  virtio-gpu  : flush failed: {err}");
    }
}

/// Отдать хосту картинку курсора (`CURSOR_SIDE`² точек, `0xAARRGGBB`) с
/// точкой касания `hot` и показать её в `(x, y)`. `false` — курсора хоста
/// нет (нет устройства или оно отказало), и стрелку рисует стол.
pub fn cursor_show(image: &[u32], hot: (u32, u32), x: i32, y: i32) -> bool {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return false;
    };
    if gpu.screen.is_none() {
        return false;
    }
    match gpu.make_cursor(image, hot).and_then(|()| gpu.cursor_command(CMD_UPDATE_CURSOR, CURSOR_RESOURCE, x, y)) {
        Ok(()) => {
            gpu.cursor = Some(Cursor { hot, shown: true });
            true
        }
        Err(err) => {
            kprintln!("  virtio-gpu  : no hardware cursor: {err}");
            false
        }
    }
}

/// Передвинуть курсор хоста. Без него — ничего.
pub fn cursor_move(x: i32, y: i32) {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    if !gpu.cursor.is_some_and(|cursor| cursor.shown) {
        return;
    }
    if let Err(err) = gpu.cursor_command(CMD_MOVE_CURSOR, CURSOR_RESOURCE, x, y) {
        kprintln!("  virtio-gpu  : cursor move failed: {err}");
    }
}

/// Спрятать или вернуть курсор хоста — погашенный экран прячет и стрелку.
pub fn cursor_visible(visible: bool, x: i32, y: i32) {
    let mut guard = GPU.lock();
    let Some(gpu) = guard.as_mut() else {
        return;
    };
    let Some(cursor) = gpu.cursor else {
        return;
    };
    if cursor.shown == visible {
        return;
    }
    // Ресурс ноль — «курсора нет»: так спецификация велит его прятать.
    let resource = if visible { CURSOR_RESOURCE } else { 0 };
    if gpu.cursor_command(CMD_UPDATE_CURSOR, resource, x, y).is_ok() {
        gpu.cursor = Some(Cursor { shown: visible, ..cursor });
    }
}

impl VirtioGpu {
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц.
    unsafe fn attach(device: &Device) -> Result<Self, VirtioError> {
        // Ответ на обращения к памяти и управление шиной — до первого
        // регистра, по той же причине, что у диска (см. `blk.rs`).
        // SAFETY: устройство не тронет память до `DRIVER_OK`.
        unsafe { device.enable_bus_master() };
        // SAFETY: контракт функции.
        let transport = unsafe { Transport::open(device) }?;
        // Просим одно — virtio 1.0. `VIRGL` и `EDID` требуют кода, которого
        // здесь нет.
        transport.negotiate(FEATURE_VERSION_1)?;
        let control = Queue::new(&transport, 0)?;
        let cursor_queue = Queue::new(&transport, 1)?;
        let command = dma::alloc(1024).map_err(VirtioError::NoMemory)?;
        let cursor_command = dma::alloc(CURSOR_COMMAND_SIZE).map_err(VirtioError::NoMemory)?;
        transport.set_driver_ok();

        let mut gpu = Self {
            _transport: transport,
            control,
            command,
            preferred: (1280, 800),
            screen: None,
            current: 0,
            cursor_queue,
            cursor_command,
            cursor: None,
            cursor_memory: None,
        };
        if let Some(size) = gpu.display_info()? {
            gpu.preferred = size;
        }
        Ok(gpu)
    }

    /// Размер экрана 0, если устройство называет его включённым.
    fn display_info(&mut self) -> Result<Option<(u32, u32)>, VirtioError> {
        self.header(CMD_GET_DISPLAY_INFO);
        self.submit(HEADER_SIZE, DISPLAY_INFO_SIZE, RESP_OK_DISPLAY_INFO)?;
        // Первый экран: прямоугольник (x, y, w, h), затем `enabled`.
        let width = self.response_u32(HEADER_SIZE + 8);
        let height = self.response_u32(HEADER_SIZE + 12);
        let enabled = self.response_u32(HEADER_SIZE + 16);
        Ok((enabled != 0 && width >= 320 && height >= 200).then_some((width, height)))
    }

    fn make_screen(&mut self, width: u32, height: u32) -> Result<Framebuffer, DisplayError> {
        let bytes = u64::from(width) * 4 * u64::from(height);
        let pages = (bytes as usize).div_ceil(PAGE_SIZE);
        let memory = crate::mm::frame::with(|frames| frames.allocate_contiguous(pages))
            .flatten()
            .ok_or(DisplayError::OutOfMemory)?;
        // Свежие кадры — не обязательно нули, а первый кадр стола собирается
        // целиком позже: до него хост показал бы чужие байты.
        // SAFETY: кадры только что выделены и никому не отданы; прямое
        // отображение покрывает всю память машины.
        unsafe { core::ptr::write_bytes(memory.to_direct_map().as_usize() as *mut u8, 0, pages * PAGE_SIZE) };

        // Номера чередуются: новый ресурс заводится, пока старый ещё на
        // экране, и совпасть они не могут. Считается от показанного номера, а
        // не от «есть ли экран» — иначе вторая смена режима снова просила бы
        // занятый номер (так и было: устройство отвечало «неверный ресурс»).
        let id = if self.current == RESOURCE { RESOURCE + 1 } else { RESOURCE };
        let made = self
            .create(id, width, height, memory, pages)
            .and_then(|()| self.set_scanout(id, width, height));
        if let Err(err) = made {
            kprintln!("  virtio-gpu  : {width}x{height} refused: {err}");
            // SAFETY: устройство буфер не показывает — экран остался на прежнем.
            crate::mm::frame::with(|frames| unsafe { frames.free_contiguous(memory, pages) });
            return Err(DisplayError::Refused("virtio-gpu refused the resource"));
        }

        // Экран показывает новый ресурс — старый можно снять.
        if let Some(old) = self.screen.take() {
            let _ = self.detach_and_unref(self.current);
            // SAFETY: ресурс снят, устройство память больше не читает.
            crate::mm::frame::with(|frames| unsafe { frames.free_contiguous(old.memory, old.pages) });
        }
        self.screen = Some(Screen { memory, pages, width, height });
        self.current = id;
        Ok(Framebuffer {
            base: memory.to_direct_map().as_usize() as u64,
            size: (pages * PAGE_SIZE) as u64,
            width,
            height,
            stride: width,
            format: PixelFormat::Bgr,
        })
    }

    fn create(&mut self, id: u32, width: u32, height: u32, memory: PhysAddr, pages: usize) -> Result<(), VirtioError> {
        self.header(CMD_RESOURCE_CREATE_2D);
        self.put_u32(HEADER_SIZE, id);
        self.put_u32(HEADER_SIZE + 4, FORMAT_B8G8R8X8_UNORM);
        self.put_u32(HEADER_SIZE + 8, width);
        self.put_u32(HEADER_SIZE + 12, height);
        self.submit(HEADER_SIZE + 16, HEADER_SIZE, RESP_OK_NODATA)?;

        // Память ресурса — одним куском: кадры выделены подряд.
        self.header(CMD_RESOURCE_ATTACH_BACKING);
        self.put_u32(HEADER_SIZE, id);
        self.put_u32(HEADER_SIZE + 4, 1);
        self.put_u64(HEADER_SIZE + 8, memory.as_u64());
        self.put_u32(HEADER_SIZE + 16, (pages * PAGE_SIZE) as u32);
        self.put_u32(HEADER_SIZE + 20, 0);
        self.submit(HEADER_SIZE + 24, HEADER_SIZE, RESP_OK_NODATA)
    }

    fn set_scanout(&mut self, id: u32, width: u32, height: u32) -> Result<(), VirtioError> {
        self.header(CMD_SET_SCANOUT);
        self.put_rect(HEADER_SIZE, (0, 0, width, height));
        self.put_u32(HEADER_SIZE + 16, 0);
        self.put_u32(HEADER_SIZE + 20, id);
        self.submit(HEADER_SIZE + 24, HEADER_SIZE, RESP_OK_NODATA)
    }

    fn detach_and_unref(&mut self, id: u32) -> Result<(), VirtioError> {
        self.header(CMD_RESOURCE_DETACH_BACKING);
        self.put_u32(HEADER_SIZE, id);
        self.put_u32(HEADER_SIZE + 4, 0);
        self.submit(HEADER_SIZE + 8, HEADER_SIZE, RESP_OK_NODATA)?;
        self.header(CMD_RESOURCE_UNREF);
        self.put_u32(HEADER_SIZE, id);
        self.put_u32(HEADER_SIZE + 4, 0);
        self.submit(HEADER_SIZE + 8, HEADER_SIZE, RESP_OK_NODATA)
    }

    /// Перенести прямоугольник ресурса к хосту.
    fn transfer(&mut self, resource: u32, rect: (u32, u32, u32, u32), stride: u32) -> Result<(), VirtioError> {
        self.header(CMD_TRANSFER_TO_HOST_2D);
        self.put_rect(HEADER_SIZE, rect);
        // Смещение первой точки прямоугольника в памяти ресурса.
        let offset = (u64::from(rect.1) * u64::from(stride) + u64::from(rect.0)) * 4;
        self.put_u64(HEADER_SIZE + 16, offset);
        self.put_u32(HEADER_SIZE + 24, resource);
        self.put_u32(HEADER_SIZE + 28, 0);
        self.submit(HEADER_SIZE + 32, HEADER_SIZE, RESP_OK_NODATA)
    }

    /// Показать перенесённый прямоугольник.
    fn resource_flush(&mut self, rect: (u32, u32, u32, u32)) -> Result<(), VirtioError> {
        self.header(CMD_RESOURCE_FLUSH);
        self.put_rect(HEADER_SIZE, rect);
        self.put_u32(HEADER_SIZE + 16, self.current);
        self.put_u32(HEADER_SIZE + 20, 0);
        self.submit(HEADER_SIZE + 24, HEADER_SIZE, RESP_OK_NODATA)
    }

    /// Завести ресурс курсора и перенести в него картинку. Ресурс заводится
    /// один раз: повторный вызов (стол пересоздал указатель) только
    /// переносит картинку заново.
    fn make_cursor(&mut self, image: &[u32], _hot: (u32, u32)) -> Result<(), VirtioError> {
        let points = (CURSOR_SIDE * CURSOR_SIDE) as usize;
        if image.len() != points {
            return Err(VirtioError::RequestFailed(0));
        }
        let pages = (points * 4).div_ceil(PAGE_SIZE);
        let memory = match self.cursor_memory {
            Some(memory) => memory,
            None => {
                let memory = crate::mm::frame::with(|frames| frames.allocate_contiguous(pages))
                    .flatten()
                    .ok_or(VirtioError::NoMemory(crate::mm::dma::DmaError::NoContiguousFrames { pages }))?;
                self.cursor_memory = Some(memory);
                self.header(CMD_RESOURCE_CREATE_2D);
                self.put_u32(HEADER_SIZE, CURSOR_RESOURCE);
                self.put_u32(HEADER_SIZE + 4, FORMAT_B8G8R8A8_UNORM);
                self.put_u32(HEADER_SIZE + 8, CURSOR_SIDE);
                self.put_u32(HEADER_SIZE + 12, CURSOR_SIDE);
                self.submit(HEADER_SIZE + 16, HEADER_SIZE, RESP_OK_NODATA)?;
                self.header(CMD_RESOURCE_ATTACH_BACKING);
                self.put_u32(HEADER_SIZE, CURSOR_RESOURCE);
                self.put_u32(HEADER_SIZE + 4, 1);
                self.put_u64(HEADER_SIZE + 8, memory.as_u64());
                self.put_u32(HEADER_SIZE + 16, (pages * PAGE_SIZE) as u32);
                self.put_u32(HEADER_SIZE + 20, 0);
                self.submit(HEADER_SIZE + 24, HEADER_SIZE, RESP_OK_NODATA)?;
                memory
            }
        };
        // SAFETY: кадры выделены под курсор и принадлежат драйверу; прямое
        // отображение покрывает всю память машины.
        unsafe {
            core::ptr::copy_nonoverlapping(image.as_ptr(), memory.to_direct_map().as_usize() as *mut u32, points);
        }
        self.transfer(CURSOR_RESOURCE, (0, 0, CURSOR_SIDE, CURSOR_SIDE), CURSOR_SIDE)
    }

    /// Команда очереди курсора. Ответа у неё нет: устройство только
    /// возвращает буфер.
    fn cursor_command(&mut self, kind: u32, resource: u32, x: i32, y: i32) -> Result<(), VirtioError> {
        let hot = self.cursor.map_or((0, 0), |cursor| cursor.hot);
        // SAFETY: буфер выделен на `CURSOR_COMMAND_SIZE` и принадлежит драйверу.
        let base = unsafe { self.cursor_command.as_ptr::<u8>() };
        let put = |at: usize, value: u32| {
            // SAFETY: буфер выделен на `CURSOR_COMMAND_SIZE`, смещения меньше.
            unsafe { base.add(at).cast::<u32>().write_unaligned(value) };
        };
        // SAFETY: см. выше.
        unsafe { core::ptr::write_bytes(base, 0, CURSOR_COMMAND_SIZE) };
        put(0, kind);
        put(HEADER_SIZE, 0);
        put(HEADER_SIZE + 4, x.max(0) as u32);
        put(HEADER_SIZE + 8, y.max(0) as u32);
        put(HEADER_SIZE + 16, resource);
        put(HEADER_SIZE + 20, hot.0);
        put(HEADER_SIZE + 24, hot.1);
        let phys = self.cursor_command.phys().as_u64();
        self.cursor_queue.set_descriptor(0, phys, CURSOR_COMMAND_SIZE as u32, 0, 0);
        fence(Ordering::SeqCst);
        self.cursor_queue.submit_and_wait(POLL_LIMIT, None)
    }

    // --- буфер команды -----------------------------------------------------

    fn header(&mut self, kind: u32) {
        // SAFETY: буфер выделен на килобайт; заголовок — первые 24 байта.
        unsafe { core::ptr::write_bytes(self.command.as_ptr::<u8>(), 0, RESPONSE_AT + DISPLAY_INFO_SIZE) };
        self.put_u32(0, kind);
    }

    fn put_u32(&mut self, at: usize, value: u32) {
        // SAFETY: смещения команд меньше `RESPONSE_AT`.
        unsafe { self.command.as_ptr::<u8>().add(at).cast::<u32>().write_unaligned(value) };
    }

    fn put_u64(&mut self, at: usize, value: u64) {
        // SAFETY: см. выше.
        unsafe { self.command.as_ptr::<u8>().add(at).cast::<u64>().write_unaligned(value) };
    }

    fn put_rect(&mut self, at: usize, rect: (u32, u32, u32, u32)) {
        self.put_u32(at, rect.0);
        self.put_u32(at + 4, rect.1);
        self.put_u32(at + 8, rect.2);
        self.put_u32(at + 12, rect.3);
    }

    fn response_u32(&self, at: usize) -> u32 {
        // SAFETY: ответ лежит с `RESPONSE_AT`, его длина не больше
        // `DISPLAY_INFO_SIZE`, буфер — килобайт.
        unsafe { self.command.as_ptr::<u8>().add(RESPONSE_AT + at).cast::<u32>().read_unaligned() }
    }

    /// Отдать команду длиной `request` и дождаться ответа длиной `response`
    /// с типом `expected`.
    fn submit(&mut self, request: usize, response: usize, expected: u32) -> Result<(), VirtioError> {
        let phys = self.command.phys().as_u64();
        self.control.set_descriptor(0, phys, request as u32, DESC_F_NEXT, 1);
        self.control.set_descriptor(1, phys + RESPONSE_AT as u64, response as u32, DESC_F_WRITE, 0);
        fence(Ordering::SeqCst);
        // Ждём опросом: команда короткая, хост исполняет её сразу, а стол
        // ждёт ответа, чтобы собрать следующий кадр поверх показанного.
        self.control.submit_and_wait(POLL_LIMIT, None)?;
        fence(Ordering::SeqCst);
        let answer = self.response_u32(0);
        if answer != expected {
            return Err(VirtioError::RequestFailed((answer & 0xff) as u8));
        }
        Ok(())
    }
}
