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

use crate::mm::VirtAddr;

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::{ARCH_ID, ARCH_NAME, Serial, halt, wait_for_interrupt};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::{ARCH_ID, ARCH_NAME, Serial, halt, wait_for_interrupt};

/// Адресное пространство ядра — реализация [`crate::mm::AddressSpace`] для
/// текущей архитектуры. На x86-64 это дерево от PML4, на AArch64 — пара
/// деревьев под `TTBR0_EL1` и `TTBR1_EL1`.
#[cfg(target_arch = "x86_64")]
pub use x86_64::PageTable as KernelSpace;
#[cfg(target_arch = "aarch64")]
pub use aarch64::PageTables as KernelSpace;

/// Почему не удалось собрать адресное пространство.
///
/// Тип различается по архитектурам: на x86-64 к ошибкам отображения добавляется
/// «загрузчик не передал карту памяти», выразить которое через
/// [`crate::mm::MapError`] нечем.
#[cfg(target_arch = "x86_64")]
pub use x86_64::paging::BuildError as SpaceError;
#[cfg(target_arch = "aarch64")]
pub type SpaceError = crate::mm::MapError;

#[cfg(target_arch = "x86_64")]
pub use x86_64::build_kernel_address_space;
#[cfg(target_arch = "aarch64")]
pub use aarch64::build_kernel_address_space;

/// Уйти в пользовательский режим и вернуться с кодом завершения программы.
///
/// [`swap_user_return_stack`] переставляет то, что процессор помнит о
/// возвращении из третьего кольца: значение принадлежит задаче, и планировщик
/// меняет его вместе с ней.
#[cfg(target_arch = "x86_64")]
pub use x86_64::user::{enter_user, return_to_kernel, swap_user_return_stack};
#[cfg(target_arch = "aarch64")]
pub use aarch64::user::{enter_user, return_to_kernel, swap_user_return_stack};

/// Куда процессор переключит стек при входе в ядро из третьего кольца.
///
/// На x86-64 это поле `RSP0` в TSS. На AArch64 отдельного поля нет вовсе:
/// ловушка приходит на `SP_EL1`, а он и есть стек текущей задачи в кольце ядра,
/// и переставляет его само переключение контекста. Поэтому здесь пусто — но имя
/// существует, чтобы планировщик не знал об этой разнице.
#[cfg(target_arch = "x86_64")]
pub use x86_64::gdt::set_trap_stack;

/// См. документацию x86-64 варианта выше.
#[cfg(target_arch = "aarch64")]
pub fn set_trap_stack(_top: usize) {}

/// Добавить отображение в уже активное адресное пространство ядра.
///
/// Всё, что появляется после инициализации памяти, отображается через эту точку:
/// окна регистров PCI и xHCI, буферы DMA. Экземпляр [`KernelSpace`], собранный
/// при запуске, до этих потребителей не доживает — он локален для инициализации
/// памяти, а хранить его глобально означало бы завести ещё один изменяемый
/// синглтон ради нескольких записей в таблицу.
#[cfg(target_arch = "x86_64")]
pub use x86_64::paging::map_active;
#[cfg(target_arch = "aarch64")]
pub use aarch64::paging::map_active;

/// Адресные пространства программ. Один и тот же набор имён на обеих
/// архитектурах, за которым скрываются довольно разные вещи: на x86-64
/// переключается `CR3` и вместе с ним обе половины адресного пространства, на
/// AArch64 — только `TTBR0_EL1`, потому что верхняя половина у ядра и программы
/// общая по построению.
///
/// | имя                      | что делает                                      |
/// |--------------------------|-------------------------------------------------|
/// | [`kernel_root`]          | корень таблиц ядра                              |
/// | [`new_user_space`]       | клон корня ядра с пустым окном под программу    |
/// | [`space_at`]             | дерево с заданным корнем — чтобы его наполнить  |
/// | [`translate`]            | чем отображён адрес в дереве, и отображён ли    |
/// | [`activate_space`]       | переключить процессор на дерево программы       |
/// | [`activate_kernel_space`]| вернуться на дерево ядра                        |
/// | [`free_user_space`]      | разобрать окно программы и вернуть кадры в пул  |
#[cfg(target_arch = "x86_64")]
pub use x86_64::paging::{
    activate_kernel_space, activate_space, free_user_space, kernel_root, new_user_space, space_at,
    translate,
};
#[cfg(target_arch = "aarch64")]
pub use aarch64::paging::{
    activate_kernel_space, activate_space, free_user_space, kernel_root, new_user_space, space_at,
    translate,
};

/// Прерывания и исключения. Обе реализации выставляют один набор имён:
/// `init`, `enable`, `disable`, `enabled`, `without_interrupts`.
#[cfg(target_arch = "x86_64")]
pub use x86_64::interrupts;
#[cfg(target_arch = "aarch64")]
pub use aarch64::interrupts;

/// Устройства ввода. Обе реализации выставляют одно имя — `init(&BootInfo) ->
/// crate::input::Sources` — и складывают события в общую очередь
/// [`crate::input`].
///
/// Содержимое у них при этом разное настолько, насколько вообще возможно: на
/// x86-64 это i8042 с маршрутизацией через I/O APIC и разбором ACPI, на AArch64 —
/// приём по PL011, потому что клавиатуры на этой машине не существует. Ровно для
/// того граница здесь и проведена.
#[cfg(target_arch = "x86_64")]
pub use x86_64::input;
#[cfg(target_arch = "aarch64")]
pub use aarch64::input;

/// Переключение контекста задач: тип сохранённого состояния и сам примитив.
#[cfg(target_arch = "x86_64")]
pub use x86_64::context::{Context, switch_context};
#[cfg(target_arch = "aarch64")]
pub use aarch64::context::{Context, switch_context};

/// Продолжение, получающее управление уже на собственном стеке ядра.
///
/// Аргумент — физический адрес `BootInfo`, а не ссылка: всё, что лежало на
/// старом стеке, после переключения недействительно, поэтому передавать туда
/// можно только то, что помещается в регистр.
pub type StackEntry = extern "C" fn(usize) -> !;

/// Переключить процессор на стек ядра и передать управление `entry`.
///
/// # Safety
///
/// `stack_top` обязан указывать на вершину отображённой на запись области, а
/// `entry` — на исполняемый код. Возврата не происходит: кадр текущей функции
/// остаётся на покинутом стеке.
#[cfg(target_arch = "x86_64")]
pub unsafe fn switch_stack(stack_top: VirtAddr, entry: StackEntry, arg: usize) -> ! {
    // На `x86_64-unknown-none` соглашение `extern "C"` и есть System V, поэтому
    // приведение ниже согласует только имена типов, но не сами соглашения о
    // вызове. Арх-модуль объявляет продолжение через `sysv64` намеренно — там
    // это утверждение не должно зависеть от настроек таргета.
    let entry: extern "sysv64" fn(*mut u8) -> ! =
        // SAFETY: оба типа — указатели на функцию с одним и тем же ABI, одним
        // аргументом указательной ширины и расходящимся результатом.
        unsafe { core::mem::transmute(entry) };
    // SAFETY: условия переданы вызывающим через контракт этой функции.
    unsafe { x86_64::switch_stack(stack_top, entry, arg as *mut u8) }
}

/// См. документацию x86-64 варианта выше.
///
/// # Safety
///
/// Те же требования.
#[cfg(target_arch = "aarch64")]
pub unsafe fn switch_stack(stack_top: VirtAddr, entry: StackEntry, arg: usize) -> ! {
    // SAFETY: условия переданы вызывающим через контракт этой функции.
    unsafe { aarch64::switch_stack(stack_top, entry, arg) }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!(
    "kernel supports only x86_64-unknown-none and aarch64-unknown-none; \
     add an src/arch/<arch>.rs module to port it"
);
