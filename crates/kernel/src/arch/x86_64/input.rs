//! Ввод на x86-64: собрать вместе клавиатуру, приём по UART и маршрутизацию их
//! прерываний.
//!
//! Модуль существует затем, чтобы остальное ядро вызывало одну функцию
//! [`init`] и не знало ни про I/O APIC, ни про ACPI, ни про i8042. На AArch64
//! есть модуль с тем же именем и тем же контрактом — и с совершенно другим
//! содержимым.
//!
//! # Порядок шагов и почему он такой
//!
//! 1. Найти I/O APIC (через MADT, иначе — по общепринятому адресу).
//! 2. Настроить устройство, пока его прерывание ещё замаскировано.
//! 3. И только потом снять маску.
//!
//! Обратный порядок дал бы прерывание от полунастроенного устройства. Для
//! клавиатуры это не абстрактный риск: настройка читает ответы клавиатуры из
//! того же однобайтового буфера, из которого их взял бы обработчик, — и один
//! съеденный обработчиком `ACK` останавливает всю последовательность.

use boot_info::BootInfo;

use super::{acpi, apic, i8042, ioapic};
use crate::input::Sources;
use crate::kprintln;
use crate::mm::PhysAddr;
use crate::sync::SpinLock;

/// IRQ шины ISA, на котором сидит клавиатура PS/2. Значение зафиксировано
/// платформой IBM PC и не менялось с 1981 года.
const IRQ_KEYBOARD: u8 = 1;

/// IRQ шины ISA для COM1.
const IRQ_SERIAL: u8 = 4;

/// Адрес окна I/O APIC, если ACPI недоступен.
///
/// Значение не выдумано: оно предписано спецификацией MP (Intel MultiProcessor
/// Specification 1.4, 3.2) как рекомендованное и с тех пор выставляется всеми
/// чипсетами, включая эмулируемый QEMU ICH9. Это резервный путь для машины без
/// ACPI, а не то, на что стоит полагаться: MADT остаётся единственным
/// источником, знающим про переопределения источников.
const FALLBACK_IO_APIC: u64 = 0xFEC0_0000;

/// Найденный контроллер. Хранится, потому что маскировать вход придётся и
/// потом — при выключении устройства или при переходе на USB-клавиатуру.
static IO_APIC: SpinLock<Option<ioapic::IoApic>> = SpinLock::new(None);

/// Поднять ввод: клавиатуру и приём по серийному порту.
///
/// Печатает, что получилось, и **не** отказывает целиком, если что-то из этого
/// не завелось: клавиатуры может не быть (`-machine microvm`), ACPI может не
/// быть, а серийный порт при этом работает — и наоборот. Ввод, доступный хотя бы
/// одним путём, лучше отсутствующего.
pub fn init(info: &BootInfo) -> Sources {
    let madt = describe_madt(info.acpi_rsdp);

    let Some(io_apic) = attach_io_apic(madt.as_ref()) else {
        kprintln!("  input       : no I/O APIC, device interrupts cannot be routed");
        let sources = Sources::default();
        crate::input::set_sources(sources);
        return sources;
    };

    let destination = destination();
    let sources = Sources {
        keyboard: start_keyboard(&io_apic, madt.as_ref(), destination),
        serial: start_serial(&io_apic, madt.as_ref(), destination),
    };

    *IO_APIC.lock() = Some(io_apic);
    crate::input::set_sources(sources);
    sources
}

/// Получатель прерываний: идентификатор локального APIC этого процессора.
///
/// Отдельный тип, а не `u8`, чтобы «получателя не удалось определить» нельзя
/// было случайно перепутать с «получатель — процессор 0»: в физическом режиме
/// адресации ноль — совершенно законный номер.
#[derive(Clone, Copy)]
struct Destination(u8);

/// Кому доставлять прерывания устройств.
///
/// Поле назначения в записи I/O APIC — байт, а идентификатор локального APIC в
/// режиме x2APIC 32-битный. Номер больше 255 физическим режимом адресации не
/// выражается, и молча обрезать его нельзя: прерывание уехало бы к чужому
/// (возможно, не существующему) процессору.
fn destination() -> Option<Destination> {
    let id = apic::local_id();
    if id > u32::from(u8::MAX) {
        kprintln!("  input       : local APIC id {id} does not fit physical destination mode");
        return None;
    }
    Some(Destination(id as u8))
}

/// Прочитать MADT и напечатать, что в ней нашлось.
fn describe_madt(rsdp: u64) -> Option<acpi::Madt> {
    // SAFETY: прямое отображение активно (таблицы ядра включены в
    // `take_over_memory`), а память с таблицами ACPI размечена прошивкой как
    // `AcpiReclaimable` и ядром пока не переиспользована: аллокатор кадров
    // раздаёт только `Usable`.
    match unsafe { acpi::find_madt(rsdp) } {
        Ok(madt) => {
            for entry in madt.io_apics() {
                kprintln!(
                    "  ioapic      : id {}, window {:#010x}, GSI base {}",
                    entry.id,
                    entry.address,
                    entry.gsi_base
                );
            }
            for entry in madt.overrides() {
                kprintln!(
                    "  ioapic      : ISA IRQ {} is wired to GSI {} ({}, active {})",
                    entry.source,
                    entry.gsi,
                    if entry.level_triggered() { "level" } else { "edge" },
                    if entry.active_low() { "low" } else { "high" }
                );
            }
            if madt.truncated != 0 {
                kprintln!("  ioapic      : {} MADT entries did not fit and were ignored", madt.truncated);
            }
            Some(madt)
        }
        Err(err) => {
            kprintln!("  acpi        : {err}; falling back to the standard I/O APIC address");
            None
        }
    }
}

/// Отобразить окно того контроллера, который обслуживает клавиатуру.
fn attach_io_apic(madt: Option<&acpi::Madt>) -> Option<ioapic::IoApic> {
    let (address, gsi_base) = match madt {
        Some(madt) => {
            let (gsi, _) = madt.gsi_for_irq(IRQ_KEYBOARD);
            match madt.io_apic_for_gsi(gsi) {
                Some(entry) => (u64::from(entry.address), entry.gsi_base),
                None => {
                    kprintln!("  ioapic      : MADT lists no controller covering GSI {gsi}");
                    return None;
                }
            }
        }
        None => (FALLBACK_IO_APIC, 0),
    };

    // SAFETY: ядро исполняется на собственных таблицах страниц, прерывания
    // устройств ещё не размаскированы, и другого владельца у этого контроллера
    // нет — модуль единственный, кто его касается.
    match unsafe { ioapic::IoApic::attach(PhysAddr::new(address), gsi_base) } {
        Ok(apic) => {
            // Идентификатор читается из самого контроллера, а не берётся из
            // MADT: расхождение между ними означает, что окно отображено не на
            // тот контроллер, — и это единственный способ такое заметить до
            // того, как прерывания просто не начнут приходить.
            kprintln!(
                "  ioapic      : id {}, version {:#04x}, {} inputs from GSI {}, all masked",
                apic.id(),
                apic.version(),
                apic.entries(),
                apic.gsi_base()
            );
            Some(apic)
        }
        Err(err) => {
            kprintln!("  ioapic      : cannot map the window at {address:#010x}: {err}");
            None
        }
    }
}

/// Настроить клавиатуру и завести её прерывание.
fn start_keyboard(
    io_apic: &ioapic::IoApic,
    madt: Option<&acpi::Madt>,
    destination: Option<Destination>,
) -> bool {
    // SAFETY: вызывается однократно и до размаскирования входа I/O APIC, то есть
    // обработчик прерывания не может вклиниться в последовательность настройки и
    // забрать из буфера ответ клавиатуры.
    if let Err(err) = unsafe { i8042::init() } {
        kprintln!("  keyboard    : PS/2 unavailable: {err}");
        return false;
    }

    let routed = route(io_apic, madt, IRQ_KEYBOARD, apic::VECTOR_KEYBOARD, destination);
    if routed {
        kprintln!(
            "  keyboard    : PS/2 on vector {:#04x}, scancode set 1 (translated)",
            apic::VECTOR_KEYBOARD
        );
    } else {
        // Клавиатура настроена, но прерывание не доходит. Опрос показывает, есть
        // ли в буфере байты вообще, — это разделяет «сломана маршрутизация» и
        // «сломана клавиатура», которые снаружи выглядят одинаково.
        kprintln!("  keyboard    : configured but its interrupt could not be routed");
        if i8042::poll_once() {
            kprintln!("  keyboard    : bytes are arriving; only the routing is broken");
        }
    }
    routed
}

/// Завести приём по серийному порту.
fn start_serial(
    io_apic: &ioapic::IoApic,
    madt: Option<&acpi::Madt>,
    destination: Option<Destination>,
) -> bool {
    // SAFETY: порт проинициализирован самым первым действием ядра
    // (`serial::init`), поэтому `DLAB` сброшен; вход IRQ 4 размаскируется ниже,
    // уже после того, как обработчик стоит в IDT.
    unsafe { super::enable_serial_rx() };

    let routed = route(io_apic, madt, IRQ_SERIAL, apic::VECTOR_SERIAL, destination);
    if routed {
        kprintln!("  serial in   : COM1 on vector {:#04x}", apic::VECTOR_SERIAL);
    } else {
        kprintln!("  serial in   : COM1 receive interrupt could not be routed");
    }
    routed
}

/// Направить IRQ шины ISA в вектор, учитывая переопределение из MADT.
fn route(
    io_apic: &ioapic::IoApic,
    madt: Option<&acpi::Madt>,
    irq: u8,
    vector: u8,
    destination: Option<Destination>,
) -> bool {
    let Some(Destination(destination)) = destination else {
        return false;
    };

    // Без MADT считаем, что IRQ совпадает с GSI, а тип срабатывания — принятый
    // для ISA (фронт, активный высокий). Это верное умолчание по спецификации, а
    // не догадка; неверным оно становится только при наличии переопределения —
    // то есть ровно тогда, когда MADT у нас есть.
    let (gsi, over) = match madt {
        Some(madt) => madt.gsi_for_irq(irq),
        None => (u32::from(irq), None),
    };
    let level = over.is_some_and(|entry| entry.level_triggered());
    let active_low = over.is_some_and(|entry| entry.active_low());

    // SAFETY: обработчики всех векторов установлены `interrupts::init` ещё до
    // этого момента — IDT заполнена целиком, включая внешние векторы.
    unsafe { io_apic.route(gsi, vector, destination, level, active_low) }
}
