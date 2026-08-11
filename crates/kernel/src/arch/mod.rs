//! Архитектурно-зависимый слой — зачаток будущего HAL.
//!
//! Всё, что различается между x86_64 и aarch64, живёт здесь и наружу отдаётся
//! единым набором имён:
//!
//! | имя          | что это                                             |
//! |--------------|-----------------------------------------------------|
//! | [`ARCH_NAME`]| человекочитаемое имя архитектуры для баннера         |
//! | [`ARCH_ID`]  | та же архитектура в терминах `boot_info::Arch`       |
//! | [`Serial`]   | конкретный UART платформы, реализует `SerialDevice`  |
//! | [`halt`]     | необратимая остановка процессора                     |
//!
//! Остальной код ядра не содержит ни одного `#[cfg(target_arch)]`: выбор
//! реализации происходит ровно один раз, вот в этом модуле.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::{ARCH_ID, ARCH_NAME, Serial, halt};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::{ARCH_ID, ARCH_NAME, Serial, halt};

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!(
    "kernel supports only x86_64-unknown-none and aarch64-unknown-none; \
     add an src/arch/<arch>.rs module to port it"
);
