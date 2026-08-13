//! Выключение и перезагрузка машины на x86-64.
//!
//! # Почему это оказалось не одной строчкой
//!
//! Выключить компьютер — операция, которую выполняет не процессор, а чипсет, и
//! команда на неё лежит в таблицах ACPI. Беда в том, что каноническое место
//! этой команды — объект `\_S5` в **DSDT**, а DSDT написан на AML: это
//! байт-код, для чтения которого нужен интерпретатор, а интерпретатор AML —
//! подсистема размером с половину этого ядра.
//!
//! Обходной путь существует и он законный. Начиная с ACPI 5.0 в FADT есть пара
//! регистров `SLEEP_CONTROL_REG`/`SLEEP_STATUS_REG`: в первый пишется тип сна,
//! и никакого AML для этого не нужно. Прошивки, которые их заполняют, — это
//! всё, что рассчитано на «hardware-reduced» платформы, и, к счастью, QEMU в
//! том числе.
//!
//! Если регистров нет, остаётся старый путь: `PM1a_CNT` с типом сна из `\_S5`.
//! Тип оттуда достаётся **разбором байт**, а не исполнением: в потоке AML
//! ищется имя `_S5_`, за ним пакет из четырёх элементов, и первые два — те
//! самые числа. Это ровно тот приём, которым пользуются все ядра, не желающие
//! тащить интерпретатор, и он честно ограничен: пакет, собранный не константами
//! (`Package` со ссылками на методы), так прочитать нельзя. Тогда выключение
//! объявляется недоступным, а не выполняется наугад.
//!
//! # Перезагрузка
//!
//! Тоже из FADT — `RESET_REG` и значение к нему. Там, где его нет, работает
//! исторический путь: импульс в порт `0x64`, тот самый контроллер клавиатуры,
//! который на заре PC умел дёргать линию сброса процессора. Он существует на
//! всём, что называется PC, и это единственная причина, по которой он здесь.

use crate::acpi::{self, read_u32, read_u64};
use crate::kprintln;

use super::{inb, outb, outw};

/// Смещения в FADT, все из спецификации ACPI.
const FADT_PM1A_CNT_BLK: usize = 64;
const FADT_PM1B_CNT_BLK: usize = 68;
const FADT_RESET_REG: usize = 116;
const FADT_RESET_VALUE: usize = 128;
const FADT_SLEEP_CONTROL_REG: usize = 244;
const FADT_DSDT64: usize = 140;
const FADT_DSDT32: usize = 40;

/// Обобщённый адрес ACPI: `space, width, offset, size, address`.
const GAS_ADDRESS_SPACE: usize = 0;
const GAS_ADDRESS: usize = 4;

/// Пространство адресов «порты ввода-вывода».
const GAS_SPACE_IO: u8 = 1;
/// Пространство адресов «память».
const GAS_SPACE_MEMORY: u8 = 0;

/// `PM1_CNT.SLP_EN` — «выполнить переход в сон», бит 13.
const SLP_EN: u16 = 1 << 13;
/// Куда в регистре ложится тип сна.
const SLP_TYP_SHIFT: u16 = 10;

/// В `SLEEP_CONTROL_REG` тип сна лежит на три бита выше, а команда — бит 5.
const SLEEP_CONTROL_TYP_SHIFT: u8 = 2;
const SLEEP_CONTROL_ENABLE: u8 = 1 << 5;

/// Как машина умеет выключаться.
enum Method {
    /// ACPI 5.0: один байт в регистр из FADT, без всякого AML.
    SleepControl { address: u64, space: u8, sleep_type: u8 },
    /// Классика: `PM1a_CNT` (и `PM1b_CNT`, если он есть) с типом из `\_S5`.
    Pm1 { pm1a: u16, pm1b: u16, typ_a: u8, typ_b: u8 },
}

/// Выключить машину.
///
/// Возвращается **только** если выключить не удалось: вызывающий обязан
/// сообщить об этом и остановить процессор, а не считать, что дело сделано.
///
/// # Safety
///
/// Вызывать после того, как всё, что нужно сохранить, сохранено: возврата из
/// удавшегося выключения не бывает.
pub unsafe fn power_off(rsdp: u64) {
    // SAFETY: RSDP пришёл от прошивки через hand-off, таблицы отображены
    // прямым отображением — см. `crate::acpi`.
    let Some(method) = (unsafe { find_method(rsdp) }) else {
        kprintln!("  power       : no way to power off found in ACPI tables");
        return;
    };

    match method {
        Method::SleepControl { address, space, sleep_type } => {
            let value = (sleep_type << SLEEP_CONTROL_TYP_SHIFT) | SLEEP_CONTROL_ENABLE;
            kprintln!("  power       : ACPI sleep control register, S5 type {sleep_type}");
            // SAFETY: адрес и пространство прочитаны из FADT; запись в них и
            // есть команда выключения.
            unsafe { write_gas(address, space, value) };
        }
        Method::Pm1 { pm1a, pm1b, typ_a, typ_b } => {
            kprintln!("  power       : ACPI PM1 control, S5 types {typ_a}/{typ_b}");
            // SAFETY: порты прочитаны из FADT.
            unsafe {
                outw(pm1a, (u16::from(typ_a) << SLP_TYP_SHIFT) | SLP_EN);
                if pm1b != 0 {
                    outw(pm1b, (u16::from(typ_b) << SLP_TYP_SHIFT) | SLP_EN);
                }
            }
        }
    }
}

/// Перезагрузить машину.
///
/// Возвращается только если не получилось ни одним из способов.
///
/// # Safety
///
/// См. [`power_off`].
pub unsafe fn reboot(rsdp: u64) {
    // SAFETY: см. `power_off`.
    if let Some(fadt) = unsafe { acpi::find_table(rsdp, b"FACP") }.ok() {
        if fadt.len() > FADT_RESET_VALUE {
            let space = fadt[FADT_RESET_REG + GAS_ADDRESS_SPACE];
            let address = read_u64(fadt, FADT_RESET_REG + GAS_ADDRESS);
            let value = fadt[FADT_RESET_VALUE];
            if address != 0 && (space == GAS_SPACE_IO || space == GAS_SPACE_MEMORY) {
                kprintln!("  power       : ACPI reset register");
                // SAFETY: адрес из FADT.
                unsafe { write_gas(address, space, value) };
            }
        }
    }

    // Запасной путь: импульс сброса через контроллер клавиатуры. Ждать, пока
    // он освободится, обязательно — команда, посланная в занятый контроллер,
    // теряется, и машина просто продолжит работать.
    kprintln!("  power       : falling back to the keyboard controller reset line");
    // SAFETY: порты i8042 существуют на любой машине, называющей себя PC;
    // запись 0xFE дёргает линию сброса.
    unsafe {
        for _ in 0..0x1_0000 {
            if inb(0x64) & 0x02 == 0 {
                break;
            }
        }
        outb(0x64, 0xFE);
    }
}

/// Записать значение в обобщённый адрес ACPI.
///
/// # Safety
///
/// Адрес должен быть получен из таблицы ACPI и описывать регистр, а не память
/// общего назначения.
unsafe fn write_gas(address: u64, space: u8, value: u8) {
    if space == GAS_SPACE_IO {
        // SAFETY: контракт функции.
        unsafe { outb(address as u16, value) };
    } else {
        // SAFETY: контракт функции; регистры ACPI в памяти доступны через
        // прямое отображение, как и остальные таблицы.
        unsafe {
            let ptr = crate::mm::PhysAddr::new(address).to_direct_map().as_usize() as *mut u8;
            ptr.write_volatile(value);
        }
    }
}

/// Найти способ выключения в таблицах.
///
/// # Safety
///
/// См. [`power_off`].
unsafe fn find_method(rsdp: u64) -> Option<Method> {
    // SAFETY: контракт функции.
    let fadt = unsafe { acpi::find_table(rsdp, b"FACP") }.ok()?;

    // Сначала ACPI 5.0: если регистр есть, тип сна для S5 равен пяти по
    // определению, и DSDT читать не нужно вовсе.
    if fadt.len() > FADT_SLEEP_CONTROL_REG + 12 {
        let space = fadt[FADT_SLEEP_CONTROL_REG + GAS_ADDRESS_SPACE];
        let address = read_u64(fadt, FADT_SLEEP_CONTROL_REG + GAS_ADDRESS);
        if address != 0 && (space == GAS_SPACE_IO || space == GAS_SPACE_MEMORY) {
            return Some(Method::SleepControl { address, space, sleep_type: 5 });
        }
    }

    // Иначе — PM1 плюс тип сна из DSDT.
    if fadt.len() <= FADT_PM1B_CNT_BLK + 4 {
        return None;
    }
    let pm1a = u16::try_from(read_u32(fadt, FADT_PM1A_CNT_BLK)).ok()?;
    let pm1b = u16::try_from(read_u32(fadt, FADT_PM1B_CNT_BLK)).unwrap_or(0);
    if pm1a == 0 {
        return None;
    }

    // SAFETY: контракт функции.
    let (typ_a, typ_b) = unsafe { sleep_type_from_dsdt(fadt) }?;
    Some(Method::Pm1 { pm1a, pm1b, typ_a, typ_b })
}

/// Достать типы сна S5 из DSDT, разбирая байты, а не исполняя их.
///
/// В потоке AML ищется имя `_S5_`, за которым идёт `PackageOp` (0x12), длина
/// пакета, число элементов и сами элементы. Нужны первые два, и каждый из них
/// — либо `ZeroOp`/`OneOp` (константы 0 и 1), либо `BytePrefix` (0x0A) с
/// байтом следом. Всё остальное означает пакет, собранный не константами, и
/// тогда честный ответ — «не умею», а не догадка.
///
/// # Safety
///
/// См. [`power_off`].
unsafe fn sleep_type_from_dsdt(fadt: &[u8]) -> Option<(u8, u8)> {
    let address = if fadt.len() > FADT_DSDT64 + 8 && read_u64(fadt, FADT_DSDT64) != 0 {
        read_u64(fadt, FADT_DSDT64)
    } else {
        u64::from(read_u32(fadt, FADT_DSDT32))
    };
    if address == 0 {
        return None;
    }

    // SAFETY: контракт функции; таблица читается тем же способом, что и
    // остальные — через прямое отображение, с проверкой длины из заголовка.
    let dsdt = unsafe { acpi::table_at(address, b"DSDT") }.ok()?;

    let mut at = acpi::SDT_HEADER_LEN;
    while at + 8 < dsdt.len() {
        if &dsdt[at..at + 4] == b"_S5_" {
            let mut cursor = at + 4;
            // Между именем и пакетом стоит `PackageOp`; спецификация допускает
            // здесь `NameOp`, если имя встретилось внутри объявления.
            if dsdt.get(cursor) == Some(&0x12) {
                cursor += 1;
                // Байт длины пакета: старшие два бита говорят, сколько байт
                // занимает сама длина. Нам она не нужна — нужно её пропустить.
                let lead = *dsdt.get(cursor)?;
                cursor += 1 + usize::from(lead >> 6);
                // Число элементов.
                cursor += 1;
                let first = read_aml_byte(dsdt, &mut cursor)?;
                let second = read_aml_byte(dsdt, &mut cursor).unwrap_or(first);
                return Some((first, second));
            }
        }
        at += 1;
    }
    None
}

/// Прочитать одну константу AML: `Zero`, `One` или байт с префиксом.
fn read_aml_byte(bytes: &[u8], cursor: &mut usize) -> Option<u8> {
    match bytes.get(*cursor)? {
        0x00 => {
            *cursor += 1;
            Some(0)
        }
        0x01 => {
            *cursor += 1;
            Some(1)
        }
        0x0A => {
            let value = *bytes.get(*cursor + 1)?;
            *cursor += 2;
            Some(value)
        }
        _ => None,
    }
}
