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
//!   [`bochs`];
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

/// Имя драйвера — для журнала.
#[cfg(target_arch = "x86_64")]
pub const DRIVER: &str = "bochs vbe";
#[cfg(target_arch = "aarch64")]
pub const DRIVER: &str = "ramfb";

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
    #[cfg(target_arch = "x86_64")]
    {
        bochs::present()
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
    #[cfg(target_arch = "x86_64")]
    {
        bochs::set_mode(current, width, height)
    }
    #[cfg(target_arch = "aarch64")]
    {
        ramfb::set_mode(current, width, height)
    }
}
