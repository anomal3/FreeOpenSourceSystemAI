//! Выключение и перезагрузка машины на AArch64.
//!
//! Здесь всё устроено проще, чем на x86-64, и по другой причине. У ARM нет
//! чипсета, которому можно написать в регистр: выключение — это **просьба к
//! прошивке**, которая живёт на более высоком уровне привилегий и остаётся в
//! памяти после нашей загрузки. Интерфейс к ней называется PSCI (Power State
//! Coordination Interface) и состоит из номера функции в регистре и одной
//! инструкции.
//!
//! Инструкций, однако, две — `SMC` и `HVC`, — и выбрать неправильную значит
//! получить неопределённое исключение вместо выключения. Какая из них верна,
//! зависит от того, где сидит прошивка: на голом железе это `SMC` (переход в
//! EL3, к Trusted Firmware), под гипервизором — `HVC` (переход в EL2). Гадать
//! не нужно: ответ записан в FADT, в поле `ARM_BOOT_ARCH`, и мы его читаем.
//!
//! Машина, у которой в FADT не объявлен PSCI, выключиться не может — и это
//! сообщается вслух, а не изображается остановкой процессора.

use core::arch::asm;

use crate::acpi::{self, read_u16};
use crate::kprintln;

/// Смещение поля `ARM_BOOT_ARCH` в FADT.
const FADT_ARM_BOOT_ARCH: usize = 129;

/// Бит 0: прошивка поддерживает PSCI.
const ARM_BOOT_PSCI_COMPLIANT: u16 = 1 << 0;
/// Бит 1: обращаться к ней надо через `HVC`, а не через `SMC`.
const ARM_BOOT_PSCI_USE_HVC: u16 = 1 << 1;

/// Номера функций PSCI 0.2 — они одинаковы у всех реализаций.
const PSCI_SYSTEM_OFF: u32 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u32 = 0x8400_0009;

/// Как звать прошивку.
#[derive(Clone, Copy)]
enum Conduit {
    Smc,
    Hvc,
}

/// Выключить машину.
///
/// Возвращается только если выключить не удалось.
///
/// # Safety
///
/// Вызывать после того, как всё нужное сохранено: возврата из удавшегося
/// выключения не бывает.
pub unsafe fn power_off(rsdp: u64) {
    // SAFETY: контракт функции.
    let Some(conduit) = (unsafe { conduit(rsdp) }) else {
        kprintln!("  power       : firmware does not advertise PSCI, cannot power off");
        return;
    };
    kprintln!("  power       : PSCI SYSTEM_OFF");
    // SAFETY: номер функции — из спецификации PSCI, способ вызова — из FADT.
    unsafe { call(conduit, PSCI_SYSTEM_OFF) };
}

/// Перезагрузить машину.
///
/// # Safety
///
/// См. [`power_off`].
pub unsafe fn reboot(rsdp: u64) {
    // SAFETY: контракт функции.
    let Some(conduit) = (unsafe { conduit(rsdp) }) else {
        kprintln!("  power       : firmware does not advertise PSCI, cannot reboot");
        return;
    };
    kprintln!("  power       : PSCI SYSTEM_RESET");
    // SAFETY: см. `power_off`.
    unsafe { call(conduit, PSCI_SYSTEM_RESET) };
}

/// Чем звать прошивку — по данным FADT.
///
/// # Safety
///
/// См. [`power_off`].
unsafe fn conduit(rsdp: u64) -> Option<Conduit> {
    // SAFETY: контракт функции.
    let fadt = unsafe { acpi::find_table(rsdp, b"FACP") }.ok()?;
    if fadt.len() <= FADT_ARM_BOOT_ARCH + 2 {
        return None;
    }
    let flags = read_u16(fadt, FADT_ARM_BOOT_ARCH);
    if flags & ARM_BOOT_PSCI_COMPLIANT == 0 {
        return None;
    }
    Some(if flags & ARM_BOOT_PSCI_USE_HVC != 0 {
        Conduit::Hvc
    } else {
        Conduit::Smc
    })
}

/// Номер функции PSCI `CPU_ON` в 64-разрядном соглашении SMC.
const PSCI_CPU_ON: u32 = 0xC400_0003;

/// Ответ PSCI «функция не поддерживается» — и наш ответ, когда звать некого.
const PSCI_NOT_SUPPORTED: i64 = -1;

/// Объявляет ли прошивка PSCI.
///
/// # Safety
///
/// См. [`power_off`].
pub unsafe fn psci_available(rsdp: u64) -> bool {
    // SAFETY: контракт функции.
    unsafe { conduit(rsdp) }.is_some()
}

/// Разбудить процессор `mpidr`: начать исполнение в EL1 с физического адреса
/// `entry`, положив `context` в `x0`.
///
/// `Err` несёт код PSCI: `-4` (уже включён), `-2` (неверные параметры) и так
/// далее — печатает его вызывающий, потому что знает, кого будил.
///
/// # Safety
///
/// `entry` — физический адрес кода, способного работать с выключенным MMU; всё,
/// что он читает по `context`, обязано лежать по физическим адресам.
pub unsafe fn cpu_on(rsdp: u64, mpidr: u64, entry: u64, context: u64) -> Result<(), i64> {
    // SAFETY: контракт функции.
    let Some(conduit) = (unsafe { conduit(rsdp) }) else {
        return Err(PSCI_NOT_SUPPORTED);
    };
    // SAFETY: номер функции из спецификации PSCI, способ вызова — из FADT.
    let result = unsafe { call_with(conduit, PSCI_CPU_ON, mpidr, entry, context) };
    if result == 0 { Ok(()) } else { Err(result) }
}

/// Вызвать функцию PSCI с тремя аргументами и вернуть её ответ.
///
/// # Safety
///
/// См. [`call`].
unsafe fn call_with(conduit: Conduit, function: u32, a1: u64, a2: u64, a3: u64) -> i64 {
    let result: u64;
    // SAFETY: контракт функции; SMC Calling Convention возвращает ответ в x0 и
    // вправе испортить x1..x3.
    unsafe {
        match conduit {
            Conduit::Smc => asm!(
                "smc #0",
                inlateout("x0") u64::from(function) => result,
                inlateout("x1") a1 => _,
                inlateout("x2") a2 => _,
                inlateout("x3") a3 => _,
                options(nostack),
            ),
            Conduit::Hvc => asm!(
                "hvc #0",
                inlateout("x0") u64::from(function) => result,
                inlateout("x1") a1 => _,
                inlateout("x2") a2 => _,
                inlateout("x3") a3 => _,
                options(nostack),
            ),
        }
    }
    result as i64
}

/// Вызвать функцию PSCI.
///
/// # Safety
///
/// Номер должен быть настоящей функцией PSCI, а способ вызова — тем, который
/// объявила прошивка: `SMC` там, где её нет в EL2, приводит к исключению.
unsafe fn call(conduit: Conduit, function: u32) {
    // SAFETY: контракт функции. Аргументы передаются в x0..x3, как велит
    // соглашение SMC Calling Convention; возврата от SYSTEM_OFF не бывает, но
    // код написан так, будто бывает, — на случай отказа прошивки.
    unsafe {
        match conduit {
            Conduit::Smc => asm!(
                "smc #0",
                in("x0") u64::from(function),
                in("x1") 0u64,
                in("x2") 0u64,
                in("x3") 0u64,
                options(nostack),
            ),
            Conduit::Hvc => asm!(
                "hvc #0",
                in("x0") u64::from(function),
                in("x1") 0u64,
                in("x2") 0u64,
                in("x3") 0u64,
                options(nostack),
            ),
        }
    }
}
