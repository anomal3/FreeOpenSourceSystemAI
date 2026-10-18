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
    crate::virtio::gpu::flush(x, y, w, h);
    // VMSVGA в режиме SVGA показывает изменённое тоже по команде (фаза 68h).
    #[cfg(target_arch = "x86_64")]
    vmsvga::flush(x, y, w, h);
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
    // VMSVGA (фаза 68h) — видеокарта VirtualBox по умолчанию и VMware.
    #[cfg(target_arch = "x86_64")]
    {
        bochs::present() || vmsvga::present()
    }
    // На `virt` экран даёт ramfb, и режим ему меняется записью в fw_cfg. Машины
    // с другим экраном у нас на этой архитектуре пока не встречалось; когда
    // встретится, ответ придётся выяснять так же, как на x86-64.
    #[cfg(target_arch = "aarch64")]
    {
        true
    }
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
    #[cfg(target_arch = "x86_64")]
    {
        if vmsvga::active() || (!bochs::present() && vmsvga::present()) {
            return vmsvga::set_mode(current, width, height);
        }
        bochs::set_mode(current, width, height)
    }
    #[cfg(target_arch = "aarch64")]
    {
        ramfb::set_mode(current, width, height)
    }
}
