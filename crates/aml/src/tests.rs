// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Проверки на байтах, снятых с настоящих таблиц.
//!
//! Все последовательности ниже не придуманы: они сняты дампом DSDT прямо из
//! загруженной системы — на x86-64 под OVMF с машиной Q35 и на AArch64 под
//! ArmVirtQemu с машиной `virt`. Придуманный AML проверял бы, что разборщик
//! понимает то, как мы его себе представляем; смысл же в том, чтобы он понимал
//! то, что пишут чужие прошивки.
//!
//! Длины пакетов пересчитаны под укороченные образцы: целая таблица — восемь
//! килобайт, и держать её в исходнике незачем. Кодировка при этом ровно та, что
//! была снята, включая переменный пакет (`0x13`) на ARM против обычного (`0x12`)
//! на x86-64 — расхождение, которое иначе осталось бы незамеченным.

use super::{Error, Route, routing};

/// Заголовок таблицы: разборщик его не читает, но пропускает, и без него
/// смещения сошлись бы случайно.
fn header() -> [u8; 36] {
    let mut bytes = [0u8; 36];
    bytes[..4].copy_from_slice(b"DSDT");
    bytes
}

fn table(body: &[u8]) -> alloc::vec::Vec<u8> {
    let mut bytes = alloc::vec::Vec::from(header());
    bytes.extend_from_slice(body);
    bytes
}

/// `Device (L00x)` с `_HID`, `_PRS` и `_CRS`, как их пишет ArmVirtQemu.
///
/// `_PRS` оставлен намеренно: он идёт **раньше** `_CRS` и содержит такой же
/// дескриптор. Разборщик, ищущий первый попавшийся дескриптор прерывания внутри
/// устройства, прошёл бы этот тест и ошибся бы на машине, где возможные ресурсы
/// не совпадают с действующими.
fn arm_link(name: &[u8; 4], gsi: u8, possible: u8) -> alloc::vec::Vec<u8> {
    let mut body = alloc::vec::Vec::new();
    // Name(_HID, "PNP0C0F")
    body.extend_from_slice(&[0x08, b'_', b'H', b'I', b'D', 0x0D]);
    body.extend_from_slice(b"PNP0C0F\0");
    // Name(_PRS, ResourceTemplate { Interrupt(Level, ActiveHigh, Exclusive, possible) })
    body.extend_from_slice(&[0x08, b'_', b'P', b'R', b'S', 0x11, 0x0E, 0x0A, 0x0B]);
    body.extend_from_slice(&[0x89, 0x06, 0x00, 0x01, 0x01, possible, 0x00, 0x00, 0x00, 0x79, 0x00]);
    // Name(_CRS, то же самое, но с действующей линией)
    body.extend_from_slice(&[0x08, b'_', b'C', b'R', b'S', 0x11, 0x0E, 0x0A, 0x0B]);
    body.extend_from_slice(&[0x89, 0x06, 0x00, 0x01, 0x01, gsi, 0x00, 0x00, 0x00, 0x79, 0x00]);

    let mut out = alloc::vec::Vec::from([0x5B, 0x82]);
    // Длина считается от себя самой: она, имя и тело.
    out.push(u8::try_from(1 + 4 + body.len()).expect("образец короткий"));
    out.extend_from_slice(name);
    out.extend_from_slice(&body);
    out
}

/// `Device (GSIx)` в том виде, в каком его пишет OVMF для Q35: флаги `0x09` —
/// уровень, активный высокий, разделяемое.
fn x86_link(name: &[u8; 4], gsi: u8) -> alloc::vec::Vec<u8> {
    let mut body = alloc::vec::Vec::new();
    body.extend_from_slice(&[0x08, b'_', b'H', b'I', b'D', 0x0C, 0x41, 0xD0, 0x0C, 0x0F]);
    body.extend_from_slice(&[0x08, b'_', b'U', b'I', b'D', 0x0A, gsi]);
    body.extend_from_slice(&[0x08, b'_', b'C', b'R', b'S', 0x11, 0x0E, 0x0A, 0x0B]);
    body.extend_from_slice(&[0x89, 0x06, 0x00, 0x09, 0x01, gsi, 0x00, 0x00, 0x00, 0x79, 0x00]);

    let mut out = alloc::vec::Vec::from([0x5B, 0x82]);
    out.push(u8::try_from(1 + 4 + body.len()).expect("образец короткий"));
    out.extend_from_slice(name);
    out.extend_from_slice(&body);
    out
}

/// Одна запись таблицы: устройство, вывод, имя связки.
fn entry(device: u8, pin: u8, link: &[u8; 4]) -> alloc::vec::Vec<u8> {
    let mut body = alloc::vec::Vec::new();
    if device == 0 {
        // Нулевое устройство прошивка пишет словом: `0x0000FFFF`.
        body.extend_from_slice(&[0x0B, 0xFF, 0xFF]);
    } else {
        body.extend_from_slice(&[0x0C, 0xFF, 0xFF, device, 0x00]);
    }
    match pin {
        0 => body.push(0x00),
        1 => body.push(0x01),
        other => body.extend_from_slice(&[0x0A, other]),
    }
    body.extend_from_slice(link);
    body.push(0x00);

    let mut out = alloc::vec::Vec::from([0x12]);
    out.push(u8::try_from(1 + 1 + body.len()).expect("запись короткая"));
    out.push(4);
    out.extend_from_slice(&body);
    out
}

fn var_package(name: &[u8; 4], entries: &[alloc::vec::Vec<u8>]) -> alloc::vec::Vec<u8> {
    let mut body = alloc::vec::Vec::from([0x0A, u8::try_from(entries.len()).expect("мало")]);
    for one in entries {
        body.extend_from_slice(one);
    }
    let mut out = alloc::vec::Vec::from([0x08]);
    out.extend_from_slice(name);
    out.push(0x13);
    out.extend_from_slice(&pkg_length(1 + body.len()));
    out.extend_from_slice(&body);
    out
}

fn package(name: &[u8; 4], entries: &[alloc::vec::Vec<u8>]) -> alloc::vec::Vec<u8> {
    let mut body = alloc::vec::Vec::from([u8::try_from(entries.len()).expect("мало")]);
    for one in entries {
        body.extend_from_slice(one);
    }
    let mut out = alloc::vec::Vec::from([0x08]);
    out.extend_from_slice(name);
    out.push(0x12);
    out.extend_from_slice(&pkg_length(1 + body.len()));
    out.extend_from_slice(&body);
    out
}

/// Закодировать длину так же, как это делает прошивка: коротко, пока влезает в
/// шесть бит, и двумя байтами дальше.
fn pkg_length(total: usize) -> alloc::vec::Vec<u8> {
    if total <= 0x3F {
        return alloc::vec::Vec::from([u8::try_from(total).expect("проверено")]);
    }
    let total = total + 1;
    alloc::vec::Vec::from([
        0x40 | u8::try_from(total & 0x0F).expect("маска"),
        u8::try_from((total >> 4) & 0xFF).expect("образец короткий"),
    ])
}

#[test]
fn arm_virt_routes_through_link_devices() {
    let mut body = alloc::vec::Vec::new();
    // Линии 35 и 36 — это SPI 3 и 4: именно их называет `virt` в QEMU.
    body.extend_from_slice(&arm_link(b"L000", 35, 99));
    body.extend_from_slice(&arm_link(b"L001", 36, 99));
    body.extend_from_slice(&var_package(
        b"_PRT",
        &[entry(0, 0, b"L000"), entry(0, 1, b"L001"), entry(1, 0, b"L001")],
    ));
    let dsdt = table(&body);

    let mut routes = [Route { device: 0, pin: 0, gsi: 0, level: false, active_low: false }; 8];
    let (written, dropped) = routing(&dsdt, &mut routes).expect("таблица разобрана");
    assert_eq!((written, dropped), (3, 0));

    assert_eq!(routes[0], Route { device: 0, pin: 0, gsi: 35, level: true, active_low: false });
    assert_eq!(routes[1], Route { device: 0, pin: 1, gsi: 36, level: true, active_low: false });
    // Свизл: у первого устройства INTA уходит на ту же линию, что INTB у
    // нулевого. Строка ради неё и добавлена — ошибка в разборе адреса дала бы
    // здесь устройство 0 и осталась бы невидимой на предыдущих двух.
    assert_eq!(routes[2], Route { device: 1, pin: 0, gsi: 36, level: true, active_low: false });
}

#[test]
fn x86_prefers_the_apic_table() {
    let mut body = alloc::vec::Vec::new();
    body.extend_from_slice(&x86_link(b"GSIE", 20));
    body.extend_from_slice(&x86_link(b"LNKE", 11));
    // Порядок как в настоящей таблице: сначала режим PIC, потом APIC. Разборщик
    // обязан взять второй, хотя первый попадается раньше.
    body.extend_from_slice(&package(b"PRTP", &[entry(3, 0, b"LNKE")]));
    body.extend_from_slice(&package(b"PRTA", &[entry(3, 0, b"GSIE")]));
    let dsdt = table(&body);

    let mut routes = [Route { device: 0, pin: 0, gsi: 0, level: false, active_low: false }; 8];
    let (written, _) = routing(&dsdt, &mut routes).expect("таблица разобрана");
    assert_eq!(written, 1);
    assert_eq!(routes[0], Route { device: 3, pin: 0, gsi: 20, level: true, active_low: false });
}

#[test]
fn a_line_named_in_place_needs_no_link() {
    // Третья форма: источник — ноль, номер линии стоит следующим полем. Так
    // пишут прошивки, у которых связок нет вовсе.
    let mut one = alloc::vec::Vec::from([0x12, 0x00, 0x04]);
    one.extend_from_slice(&[0x0C, 0xFF, 0xFF, 0x02, 0x00]);
    one.push(0x01);
    one.push(0x00);
    one.extend_from_slice(&[0x0A, 0x13]);
    one[1] = u8::try_from(one.len() - 1).expect("запись короткая");

    let dsdt = table(&package(b"_PRT", &[one]));
    let mut routes = [Route { device: 0, pin: 0, gsi: 0, level: false, active_low: false }; 4];
    let (written, _) = routing(&dsdt, &mut routes).expect("таблица разобрана");
    assert_eq!(written, 1);
    assert_eq!(routes[0], Route { device: 2, pin: 1, gsi: 19, level: true, active_low: true });
}

#[test]
fn a_full_output_reports_what_did_not_fit() {
    let mut body = alloc::vec::Vec::new();
    body.extend_from_slice(&arm_link(b"L000", 35, 35));
    body.extend_from_slice(&var_package(
        b"_PRT",
        &[entry(0, 0, b"L000"), entry(1, 0, b"L000"), entry(2, 0, b"L000")],
    ));
    let dsdt = table(&body);

    let mut routes = [Route { device: 0, pin: 0, gsi: 0, level: false, active_low: false }; 1];
    let (written, dropped) = routing(&dsdt, &mut routes).expect("таблица разобрана");
    // Молча обрезанная таблица выглядела бы как «у этих устройств нет
    // прерывания», и причину пришлось бы искать долго.
    assert_eq!((written, dropped), (1, 2));
}

#[test]
fn a_method_without_a_data_table_is_an_honest_refusal() {
    // `Method (_PRT, 0)` — байт 0x14, а не 0x08. Разборщик обязан не найти
    // таблицу, а не принять за неё первые попавшиеся байты.
    let mut body = alloc::vec::Vec::from([0x14, 0x06]);
    body.extend_from_slice(b"_PRT");
    body.push(0x00);
    let dsdt = table(&body);

    let mut routes = [Route { device: 0, pin: 0, gsi: 0, level: false, active_low: false }; 4];
    assert_eq!(routing(&dsdt, &mut routes), Err(Error::NoTable));
}

#[test]
fn an_entry_whose_link_is_missing_is_skipped_not_guessed() {
    // Связка названа, но её объявления в таблице нет. Выдумать номер линии
    // здесь означало бы разрешить прерывание на чужом входе контроллера.
    let dsdt = table(&var_package(b"_PRT", &[entry(0, 0, b"L000"), entry(1, 0, b"L001")]));
    let mut routes = [Route { device: 0, pin: 0, gsi: 0, level: false, active_low: false }; 4];
    let (written, dropped) = routing(&dsdt, &mut routes).expect("таблица разобрана");
    assert_eq!((written, dropped), (0, 0));
}

extern crate alloc;
extern crate std;
