//! Смена режима экрана на работающей машине (фаза С6a).
//!
//! # Почему это драйвер, а не просьба к прошивке
//!
//! До этой фазы разрешение выбирал загрузчик через GOP, и выбор вступал в силу
//! со следующего запуска: GOP живёт только до `ExitBootServices`, и после него
//! прошивке режим не закажешь. Переключить экран на ходу может только тот, кто
//! говорит с видеоадаптером сам. Таких адаптеров здесь два — те, на которых
//! система работает в QEMU:
//!
//! * x86-64: стандартный VGA QEMU (PCI `1234:1111`) с интерфейсом Bochs VBE —
//!   [`bochs`]; VMware SVGA II (`15ad:0405`) — видеокарта VirtualBox и
//!   VMware, [`vmsvga`] (фаза 68h);
//! * AArch64: `ramfb`, который настраивается записью файла `etc/ramfb` через
//!   fw_cfg, — [`ramfb`].
//!
//! На всякой другой машине ответ — [`DisplayError::NoDriver`], и это законное
//! состояние: выбор по-прежнему запоминается для следующего запуска, а окно
//! говорит об этом словами.
//!
//! # Чего здесь нет намеренно
//!
//! Догадок об адресах. Адаптер x86 ищется на шине PCI, и переключается он
//! только тогда, когда картинка, которую дала прошивка, лежит в его памяти:
//! иначе на экране не он, и менять ему режим значит портить чужой экран.
//! fw_cfg на AArch64 ищется в DSDT по его ACPI ID, а не по адресу, который
//! у `virt` «всегда такой».

#[cfg(target_arch = "x86_64")]
mod bochs;
#[cfg(target_arch = "x86_64")]
pub mod intel;
pub mod nvidia;
pub mod nvidia_disp;
// `gpu gametest` (20.11): утренняя проверка игры на видеокарте одной командой.
pub mod nvidia_gametest;
pub mod nvidia_gsp;
// NVK поверх GSP-RM (18.11, WP-A): видеопамять, адресные пространства GPU,
// golden context GR, каналы и бэкенд `nvgpu::GpuBackend` для слоя DRM.
pub mod nvidia_rm_chan;
pub mod nvidia_rm_gr;
pub mod nvidia_rm_info;
pub mod nvidia_rm_vmm;
pub mod nvidia_rm_vram;
pub mod outputs;
#[cfg(target_arch = "x86_64")]
mod vmsvga;
#[cfg(target_arch = "aarch64")]
mod ramfb;

use boot_info::Framebuffer;

/// Почему режим не сменился.
#[derive(Debug, Clone, Copy)]
pub enum DisplayError {
    /// Адаптера, которым умеем управлять, на машине нет.
    NoDriver(&'static str),
    /// Адаптер есть, но отказал или ответил не то.
    Refused(&'static str),
    /// Режим не помещается в память адаптера.
    ///
    /// Только у x86: память `ramfb` — обычная память гостя, и её предел — это
    /// [`Self::OutOfMemory`], а не объём на плате.
    #[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
    TooLarge { needed: u64, available: u64 },
    /// Не хватило кадров или отображения.
    OutOfMemory,
}

impl core::fmt::Display for DisplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoDriver(why) => write!(f, "no driver: {why}"),
            Self::Refused(why) => write!(f, "the adapter refused: {why}"),
            Self::TooLarge { needed, available } => write!(
                f,
                "needs {} KiB of video memory, the adapter has {} KiB",
                needed / 1024,
                available / 1024
            ),
            Self::OutOfMemory => f.write_str("out of memory for the new frame buffer"),
        }
    }
}

/// Физический адрес буфера по его `base`.
///
/// `base` бывает двух родов: буфер от прошивки ядро видит по identity (адрес и
/// есть физический), а буфер, выданный [`set_mode`], — в прямом отображении.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
fn physical(base: u64) -> u64 {
    let direct = crate::mm::PHYS_MAP_BASE as u64;
    if base >= direct { base - direct } else { base }
}

/// Имя адаптера по умолчанию — для журнала.
#[cfg(target_arch = "x86_64")]
const DRIVER: &str = "bochs vbe";
#[cfg(target_arch = "aarch64")]
const DRIVER: &str = "ramfb";

/// Имя драйвера, который сейчас держит экран, — для журнала.
#[must_use]
pub fn driver() -> &'static str {
    if crate::virtio::gpu::active() {
        return "virtio-gpu";
    }
    #[cfg(target_arch = "x86_64")]
    if vmsvga::active() {
        return "vmsvga";
    }
    #[cfg(target_arch = "x86_64")]
    if intel::active() {
        return "intel";
    }
    DRIVER
}

/// Отметить адаптеры, которыми ядро умеет управлять, при загрузке (фаза 68h).
pub fn claim_adapters() {
    #[cfg(target_arch = "x86_64")]
    vmsvga::claim();
}

/// Показать изменившийся прямоугольник (фаза 67a).
///
/// У stdvga и ramfb делать нечего: адаптер показывает свою память сам. У
/// virtio-gpu картинка лежит в обычной памяти, и хосту надо сказать, что в ней
/// изменилось, — композитор зовёт это после каждого собранного прямоугольника.
pub fn flush(x: i32, y: i32, w: u32, h: u32) {
    crate::console::overlay_redraw(y, h);
    crate::virtio::gpu::flush(x, y, w, h);
    // VMSVGA в режиме SVGA показывает изменённое тоже по команде (фаза 68h).
    #[cfg(target_arch = "x86_64")]
    vmsvga::flush(x, y, w, h);
}

/// Показать изменившийся прямоугольник одного монитора — в его
/// собственных координатах (16.11, несколько мониторов).
///
/// `primary` — это основной монитор: только поверх него лежит консоль
/// аварийного вывода ([`crate::console::overlay_redraw`]), она рисует в
/// буфер основного экрана. У выхода прошивки (и у одного экрана stdvga,
/// VMSVGA, ramfb, Intel) — ровно прежний [`flush`]; у virtio-gpu — его
/// выход; NVIDIA показывает свою память сама, сказать ей нечего.
pub fn flush_output(backend: outputs::Backend, primary: bool, x: i32, y: i32, w: u32, h: u32) {
    match backend {
        outputs::Backend::Firmware => {
            if primary {
                flush(x, y, w, h);
            }
        }
        outputs::Backend::Virtio(scanout) => {
            if primary {
                crate::console::overlay_redraw(y, h);
            }
            crate::virtio::gpu::flush_scanout(scanout, x, y, w, h);
        }
        outputs::Backend::Nvidia(_) => {
            if primary {
                crate::console::overlay_redraw(y, h);
            }
        }
    }
}

/// Screen off (22.11, DPMS-like): monitors to standby (`false`) or lit
/// again (`true`). NVIDIA: DisplayPort sink to D3 and the head/SOR released
/// (no signal), wake = D0, link training and the head again, a full modeset
/// of that head if the quick way fails ([`nvidia_disp::set_power`]);
/// virtio-gpu: scanouts disabled. Firmware frame buffers (GOP, stdvga,
/// VMSVGA, Intel) cannot - the desktop paints them black. Returns what
/// happened, for the log.
pub fn set_power(on: bool) -> alloc::string::String {
    let mut parts: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
    let virtio = crate::virtio::gpu::set_power(on);
    if virtio > 0 {
        parts.push(alloc::format!("virtio-gpu {virtio} output(s) {}", if on { "on" } else { "off" }));
    }
    if nvidia_gsp::running() {
        if let Some(text) = nvidia_disp::set_power(on) {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        return alloc::string::String::from(if on { "firmware display" } else { "firmware display: black only, no DPMS" });
    }
    parts.join("; ")
}

/// G3: what a page flip of an output to a client buffer answered.
#[derive(Debug)]
pub enum FlipError {
    /// Not now (the previous flip is not on screen yet, the channel or the
    /// display is busy): try again next frame, the display keeps what it shows.
    Pending(#[allow(dead_code)] &'static str),
    /// Not possible: copy into the frame buffer instead.
    Refused(alloc::string::String),
}

/// G3: why the display of `backend` cannot scan a client buffer out
/// (`None` - it can try). The firmware frame buffer (stdvga, GOP, Intel,
/// VMSVGA) is one fixed surface.
#[must_use]
pub fn flip_unsupported(backend: outputs::Backend) -> Option<&'static str> {
    match backend {
        outputs::Backend::Firmware => Some("the display is the firmware frame buffer (no page flip)"),
        outputs::Backend::Virtio(_) | outputs::Backend::Nvidia(_) => None,
    }
}

/// G3: show the client buffer at `phys` (physically contiguous, the output's
/// size, `pitch` bytes per row, XRGB8888) on the output instead of its frame
/// buffer; `None` - the frame buffer again (already repainted). NVIDIA: a
/// page flip of the head's window channel (latched at vblank). virtio-gpu:
/// a resource backed by the client buffer put on the scanout - the stand-in
/// that lets QEMU test the flip logic (the host still copies).
pub fn flip(backend: outputs::Backend, phys: Option<u64>, pitch: u32) -> Result<(), FlipError> {
    match backend {
        outputs::Backend::Nvidia(id) => nvidia_disp::scanout(id, phys, pitch),
        outputs::Backend::Virtio(scanout) => crate::virtio::gpu::flip_scanout(scanout, phys).map_err(|why| FlipError::Refused(why.into())),
        outputs::Backend::Firmware => match phys {
            None => Ok(()),
            Some(_) => Err(FlipError::Refused("the firmware frame buffer cannot flip".into())),
        },
    }
}

/// G3: the flipped client buffer changed `(x, y, w, h)` (output coordinates)
/// in place. NVIDIA scans the memory itself; virtio-gpu needs a transfer.
pub fn flip_refresh(backend: outputs::Backend, x: i32, y: i32, w: u32, h: u32) {
    if let outputs::Backend::Virtio(scanout) = backend {
        crate::virtio::gpu::flip_refresh(scanout, x, y, w, h);
    }
}

/// G3: may a display still read `[phys, phys + len)` (a flipped client
/// buffer, or the one shown before a flip that is not on screen yet)? The
/// Wayland server releases such a buffer only after this says `false`.
#[must_use]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn flip_scanning(phys: u64, len: u64) -> bool {
    nvidia_disp::scanning(phys, len)
}

/// G3: does any display scan system memory or have a flip pending (cheap)?
#[must_use]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn flip_any() -> bool {
    nvidia_disp::flip_any()
}

/// G3: pointer over a flipped output, drawn by the display's cursor:
/// `image` - new picture ([`FLIP_CURSOR_SIDE`]² points `0xAARRGGBB`, all
/// zero hides it), `None` - keep it; `at` - top-left in output coordinates.
/// `Some(false)` - no cursor here (the desktop must compose the pointer),
/// `None` - busy, try again next frame.
pub fn flip_cursor(backend: outputs::Backend, image: Option<&[u32]>, at: (i32, i32)) -> Option<bool> {
    match backend {
        outputs::Backend::Nvidia(id) => match nvidia_disp::cursor_set(id, image, at) {
            Ok(()) => Some(true),
            Err(None) => None,
            Err(Some(why)) => {
                crate::kprintln!("  display     : no hardware cursor over the flipped output: {why}");
                Some(false)
            }
        },
        // virtio-gpu has one host cursor, already used by the desktop when
        // there is one monitor (`hw_cursor`); with more the desktop draws.
        outputs::Backend::Virtio(_) | outputs::Backend::Firmware => Some(false),
    }
}

/// G3: can the display of `backend` draw the pointer over a flipped
/// output ([`flip_cursor`])? NVIDIA: a head cursor (may still fail on first
/// use); others: no.
#[must_use]
pub const fn flip_has_cursor(backend: outputs::Backend) -> bool {
    matches!(backend, outputs::Backend::Nvidia(_))
}

/// G3: side of the [`flip_cursor`] picture.
pub const FLIP_CURSOR_SIDE: u32 = nvidia_disp::CURSOR_SIDE;

/// G3: the flip of the output ended - its cursor off.
pub fn flip_cursor_off(backend: outputs::Backend) {
    if let outputs::Backend::Nvidia(id) = backend {
        nvidia_disp::cursor_off(id);
    }
}

/// Отдать курсор адаптеру (фаза 67b): картинка `side`×`side` точек
/// (`0xAARRGGBB`) с точкой касания `hot`. `false` — адаптер так не умеет, и
/// стрелку рисует стол.
pub fn cursor_show(image: &[u32], hot: (u32, u32), x: i32, y: i32) -> bool {
    crate::virtio::gpu::cursor_show(image, hot, x, y)
}

/// Сторона картинки курсора у адаптера.
pub const CURSOR_SIDE: u32 = crate::virtio::gpu::CURSOR_SIDE;

/// Передвинуть курсор адаптера.
pub fn cursor_move(x: i32, y: i32) {
    crate::virtio::gpu::cursor_move(x, y);
}

/// Спрятать или вернуть курсор адаптера.
pub fn cursor_visible(visible: bool, x: i32, y: i32) {
    crate::virtio::gpu::cursor_visible(visible, x, y);
}

/// Умеет ли эта машина менять режим экрана на ходу.
///
/// # Зачем спрашивать заранее
///
/// Потому что отказ приходит слишком поздно. Человек выбирает разрешение,
/// нажимает — и ничего не происходит; со стороны это неотличимо от поломки.
/// На ноутбуке ASUS K53SD так и вышло: список показывался, выбор принимался, а
/// экран оставался прежним, потому что за ним настоящая видеокарта, а не
/// стандартный адаптер QEMU. Драйвера Intel HD у системы нет, и режим там
/// задаёт прошивка при загрузке — это надо говорить словами, а не молчанием.
#[must_use]
pub fn can_switch() -> bool {
    // virtio-gpu (фаза 67a) — на обеих архитектурах: режим у него — это размер
    // ресурса, который заводит драйвер.
    if crate::virtio::gpu::active() {
        return true;
    }
    // NVIDIA (этап 6): режимы выставляет GSP-RM, когда он запущен.
    if nvidia_gsp::running() {
        return true;
    }
    // VMSVGA (фаза 68h) — видеокарта VirtualBox по умолчанию и VMware;
    // Intel (фаза 70c) — только к родному разрешению панели.
    #[cfg(target_arch = "x86_64")]
    {
        bochs::present() || vmsvga::present() || native().is_some()
    }
    // На `virt` экран даёт ramfb, и режим ему меняется записью в fw_cfg. Машины
    // с другим экраном у нас на этой архитектуре пока не встречалось; когда
    // встретится, ответ придётся выяснять так же, как на x86-64.
    #[cfg(target_arch = "aarch64")]
    {
        true
    }
}

/// Родное разрешение панели, которое прошивка не дала (фаза 70c): оно
/// запоминается при запуске стола ([`probe`]).
static NATIVE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Узнать при запуске стола, есть ли у экрана режим лучше, чем дала прошивка.
///
/// Сейчас это одна Intel-графика с масштабировщиком панели. Пятый пункт меню
/// загрузки (`BOOT_FIRMWARE_DISPLAY`) запрещает и это: им возвращают машину,
/// у которой экран от перенастройки гаснет.
pub fn probe(current: &Framebuffer, boot_flags: u64) -> Option<(u32, u32)> {
    if boot_flags & boot_info::BOOT_FIRMWARE_DISPLAY != 0 {
        FIRMWARE_ONLY.store(true, core::sync::atomic::Ordering::Relaxed);
        crate::kprintln!("  display     : keeping the firmware's mode, as the boot menu asked");
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    {
        let found = intel::native(current);
        match found {
            Some((w, h)) => {
                NATIVE.store((u64::from(w) << 32) | u64::from(h), core::sync::atomic::Ordering::Relaxed);
                FIRMWARE.store(
                    (u64::from(current.width) << 32) | u64::from(current.height),
                    core::sync::atomic::Ordering::Relaxed,
                );
                crate::kprintln!(
                    "  display     : the panel is {w}x{h}, the firmware stretches {}x{} onto it",
                    current.width,
                    current.height
                );
            }
            None => {}
        }
        found
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = current;
        None
    }
}

/// В меню загрузки выбран «экран прошивки»: никаких драйверов видеокарт.
static FIRMWARE_ONLY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Попросили ли оставить экран прошивки (тогда видеокарты не трогаются).
#[must_use]
pub fn firmware_only() -> bool {
    FIRMWARE_ONLY.load(core::sync::atomic::Ordering::Relaxed)
}

/// Режим, который дала прошивка, — второй пункт списка у Intel.
static FIRMWARE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Режимы, которые этот экран умеет на ходу, — для «Параметров».
///
/// У стандартных адаптеров эмуляторов — общий список; у Intel-графики —
/// только два: режим прошивки и родной режим панели (частот этот драйвер не
/// меняет, а другие размеры без них — та же растяжка, что и была).
#[must_use]
pub fn modes(common: &[(u32, u32)]) -> alloc::vec::Vec<(u32, u32)> {
    let firmware = FIRMWARE.load(core::sync::atomic::Ordering::Relaxed);
    match native() {
        Some(native) if firmware != 0 => alloc::vec![((firmware >> 32) as u32, firmware as u32), native],
        _ => common.to_vec(),
    }
}

/// Родной режим панели, если [`probe`] его нашёл.
#[must_use]
pub fn native() -> Option<(u32, u32)> {
    let packed = NATIVE.load(core::sync::atomic::Ordering::Relaxed);
    (packed != 0).then(|| ((packed >> 32) as u32, packed as u32))
}

/// Переключить экран в `width`×`height`.
///
/// `current` — то, на чём стол рисует сейчас. Возвращается описание нового
/// фреймбуфера, и `base` в нём — адрес, **по которому к пикселям обращается
/// ядро**, а не физический: новый буфер отображается в прямое отображение, а
/// не в identity, которое строилось один раз под буфер от прошивки.
///
/// Формат точки тот же, что у `current`: оба адаптера отдают BGRX, и менять
/// порядок каналов на ходу незачем — `mini-ui` выбирает его один раз.
pub fn set_mode(current: &Framebuffer, width: u32, height: u32) -> Result<Framebuffer, DisplayError> {
    if width < 320 || height < 200 || width > 4096 || height > 4096 {
        return Err(DisplayError::Refused("mode outside 320x200..4096x4096"));
    }
    if crate::virtio::gpu::active() {
        return crate::virtio::gpu::take_screen(width, height);
    }
    if nvidia_gsp::running() {
        return nvidia_disp::set_primary(width, height, 0);
    }
    #[cfg(target_arch = "x86_64")]
    {
        if vmsvga::active() || (!bochs::present() && vmsvga::present()) {
            return vmsvga::set_mode(current, width, height);
        }
        if intel::active() || native().is_some() {
            return intel::set_mode(current, width, height);
        }
        bochs::set_mode(current, width, height)
    }
    #[cfg(target_arch = "aarch64")]
    {
        ramfb::set_mode(current, width, height)
    }
}
