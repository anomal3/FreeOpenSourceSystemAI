//! Проверки разбора на дескрипторах настоящих устройств.
//!
//! Дескрипторы взяты из исходников эмуляторов, а не сняты с нашего же разбора:
//! `usb-kbd`, `usb-mouse` и `usb-tablet` — из `hw/usb/dev-hid.c` QEMU, планшет
//! VirtualBox — из `src/VBox/Devices/Input/UsbMouse.cpp`. Сверять разбор с
//! самим собой бессмысленно; смысл имеет только сверка с тем, что устройство
//! действительно присылает.
//!
//! Половина этих устройств в QEMU недоступна вовсе — планшет VirtualBox,
//! устройство с `Report ID`, джойстик, — и проверить их прогоном стенда нечем.
//! Это и есть причина, по которой разбор живёт в отдельном крейте.

use std::vec;
use std::vec::Vec;

use crate::{Motion, parse};

/// `usb-kbd` из QEMU.
const QEMU_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x01, 0x95, 0x08, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7,
    0x15, 0x00, 0x25, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff, 0x81, 0x00, 0xc0,
];

/// `usb-mouse` из QEMU: относительные оси, три кнопки, колесо.
const QEMU_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, 0xc0, 0xc0,
];

/// `usb-tablet` из QEMU: абсолютные оси по 16 бит, колесо относительное.
const QEMU_TABLET: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x7f, 0x35, 0x00, 0x46, 0xff, 0x7f,
    0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08,
    0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
];

/// Планшет VirtualBox: пять кнопок, три бита набивки, единицы измерения —
/// элементы, которых у QEMU нет и которые обязаны быть пропущены без вреда.
const VBOX_TABLET: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x7f, 0x35, 0x00, 0x46, 0xff, 0x7f,
    0x65, 0x33, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x35, 0x00,
    0x45, 0x00, 0x65, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
];

/// Мышь, чьи отчёты пронумерованы: так устроены устройства, отдающие по одной
/// точке несколько разных отчётов.
const MOUSE_WITH_REPORT_ID: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x02, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
    0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08,
    0x95, 0x03, 0x81, 0x06, 0xc0, 0xc0,
];

/// Джойстик: те же usage X и Y, но курсором он становиться не должен.
const JOYSTICK: &[u8] = &[
    0x05, 0x01, 0x09, 0x04, 0xa1, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
    0x08, 0x95, 0x02, 0x81, 0x02, 0xc0,
];

#[test]
fn keyboard_of_qemu() {
    let parsed = parse(QEMU_KEYBOARD);
    let map = parsed.keyboard.expect("клавиатура не опознана");
    assert!(map.has_modifiers());
    assert_eq!(map.key_slots(), 6);
    // Клавиатура не должна оказаться ещё и указателем: usage X и Y у неё нет.
    assert!(parsed.pointer.is_none());

    // Ctrl и Shift нажаты, клавиши A и B в списке.
    let report = [0b0000_0011u8, 0, 0x04, 0x05, 0, 0, 0, 0];
    let decoded = map.decode(&report).expect("отчёт не разобран");
    assert_eq!(decoded[0], 0b0000_0011);
    assert_eq!(decoded[2], 0x04);
    assert_eq!(decoded[3], 0x05);
}

#[test]
fn mouse_of_qemu() {
    let map = parse(QEMU_MOUSE).pointer.expect("мышь не опознана");
    assert!(!map.is_absolute());
    assert_eq!(map.button_count(), 3);
    assert!(map.has_wheel());

    // Левая кнопка, −2 по X, +3 по Y, колесо на щелчок назад. Знак — главное,
    // что здесь проверяется: поле объявлено −127..127, и прочитанное без знака
    // смещение влево выглядело бы как прыжок на четверть экрана вправо.
    let decoded = map.decode(&[0b0000_0001, 0xFE, 0x03, 0xFF]).expect("отчёт не разобран");
    match decoded.motion {
        Motion::Relative { dx, dy } => {
            assert_eq!(dx, -2);
            assert_eq!(dy, 3);
        }
        other => panic!("движение разобрано как {other:?}"),
    }
    assert_eq!(decoded.wheel, -1);
    assert_eq!(decoded.buttons, 0b001);

    let decoded = map.decode(&[0b0000_0110, 0, 0, 0]).unwrap();
    assert_eq!(decoded.buttons, 0b110);
}

#[test]
fn tablets_report_absolute_positions() {
    for (name, bytes, buttons) in [
        ("QEMU", QEMU_TABLET, 3u8),
        ("VirtualBox", VBOX_TABLET, 5u8),
    ] {
        let map = parse(bytes).pointer.unwrap_or_else(|| panic!("планшет {name} не опознан"));
        assert!(map.is_absolute(), "{name}: планшет принят за мышь");
        assert_eq!(map.range(), (0, 32767), "{name}: диапазон");
        assert_eq!(map.button_count(), buttons, "{name}: кнопки");
        assert!(map.has_wheel(), "{name}: колесо");

        // Середина по X, начало по Y. Байтовая раскладка у обоих совпадает,
        // хотя биты кнопок и набивки поделены по-разному: у VirtualBox пять
        // кнопок и три бита набивки, у QEMU три и пять.
        let decoded = map.decode(&[0, 0xFF, 0x3F, 0x00, 0x00, 0x00]).expect("отчёт не разобран");
        match decoded.motion {
            Motion::Absolute { x, y } => {
                assert!((32_000..34_000).contains(&x), "{name}: X оказался {x}, а ждали середину");
                assert_eq!(y, 0, "{name}: Y");
            }
            other => panic!("{name}: движение разобрано как {other:?}"),
        }

        // Дальний угол: наибольшее значение шкалы устройства обязано давать
        // наибольшую долю. Ошибка здесь означала бы недостижимый правый край
        // экрана — то, что человек заметит первым.
        let decoded = map.decode(&[0, 0xFF, 0x7F, 0xFF, 0x7F, 0x00]).unwrap();
        match decoded.motion {
            Motion::Absolute { x, y } => {
                assert_eq!(x, u16::MAX, "{name}: правый край");
                assert_eq!(y, u16::MAX, "{name}: нижний край");
            }
            other => panic!("{name}: движение разобрано как {other:?}"),
        }
    }
}

#[test]
fn numbered_reports_are_told_apart() {
    let map = parse(MOUSE_WITH_REPORT_ID).pointer.expect("мышь не опознана");
    assert_eq!(map.report_id, 2);

    let decoded = map.decode(&[0x02, 0b0000_0001, 0x05, 0xFB, 0x00]).expect("отчёт не разобран");
    match decoded.motion {
        Motion::Relative { dx, dy } => {
            assert_eq!(dx, 5);
            assert_eq!(dy, -5);
        }
        other => panic!("движение разобрано как {other:?}"),
    }

    // Отчёт с чужим номером — это отчёт другой части устройства. Разобранный
    // нашей картой, он выдал бы нажатия кнопок из чего попало.
    assert!(map.decode(&[0x03, 0xFF, 0xFF, 0xFF, 0xFF]).is_none());
}

#[test]
fn a_joystick_is_not_a_pointer() {
    // Оси у джойстика те же самые, и различает их только то, во что они
    // вложены. Без проверки коллекции верхнего уровня рулевой манипулятор стал
    // бы мышью — с курсором, уезжающим в угол и остающимся там.
    assert!(parse(JOYSTICK).pointer.is_none());
}

#[test]
fn broken_descriptors_yield_nothing_and_do_not_hang() {
    // Дескриптор приезжает от устройства, то есть это недоверенные данные: он
    // может быть испорчен, оборван или не быть дескриптором вовсе.
    let garbage = [0xFFu8; 64];
    let parsed = parse(&garbage);
    assert!(parsed.pointer.is_none() && parsed.keyboard.is_none());

    let zeros = [0x00u8; 32];
    let parsed = parse(&zeros);
    assert!(parsed.pointer.is_none() && parsed.keyboard.is_none());

    // Оборванный на середине дескриптор — обычное дело при коротком ответе
    // устройства: то, что успело объявиться, остаётся годным.
    let truncated = &QEMU_TABLET[..QEMU_TABLET.len() - 6];
    assert!(parse(truncated).pointer.is_some());
}

#[test]
fn short_reports_do_not_read_past_the_end() {
    // Устройство вправе прислать меньше, чем объявило: короткий пакет — не
    // ошибка. Недостающие биты читаются нулями, и это ровно то, что значит
    // «устройство про них не сказало».
    let map = parse(QEMU_TABLET).pointer.unwrap();
    let decoded = map.decode(&[0b0000_0001, 0xFF]).expect("короткий отчёт не разобран");
    assert_eq!(decoded.buttons, 0b001);
    match decoded.motion {
        Motion::Absolute { y, .. } => assert_eq!(y, 0),
        other => panic!("движение разобрано как {other:?}"),
    }
}

/// Поле шире тридцати двух бит отбрасывается, а не читается со сдвигом.
///
/// Найдено фаззером (`cargo xtask fuzz --target usb-hid`). `Report Size` — один
/// байт, то есть устройство вправе назвать 255, а чтение собирало значение
/// сдвигами по `u32`: `1 << 40` — это паника «attempt to shift left with
/// overflow» в отладочной сборке и молчаливый мусор в оптимизированной. Чужая
/// мышь останавливала ядро на разборе своего же дескриптора.
///
/// Проверяется не только отсутствие паники: поле, которого мы прочитать не
/// можем, обязано **пропасть**, а не приехать обрезанным до тридцати двух бит.
/// Обрезок — это правдоподобная бессмыслица, и она хуже отсутствия поля.
#[test]
fn a_field_wider_than_thirty_two_bits_is_dropped() {
    // Мышь QEMU, у которой у полей X и Y размер подменён на 255.
    let mut descriptor = QEMU_MOUSE.to_vec();
    let mut patched = 0;
    for index in 0..descriptor.len() - 1 {
        if descriptor[index] == 0x75 && descriptor[index + 1] == 0x08 {
            descriptor[index + 1] = 0xFF;
            patched += 1;
        }
    }
    assert!(patched > 0, "в дескрипторе мыши есть Report Size 8");

    let parsed = parse(&descriptor);
    // Кнопки объявлены полями по одному биту и остаются на месте; координаты
    // объявлены широкими и пропадают вместе со своим usage.
    if let Some(map) = parsed.pointer {
        let decoded = map.decode(&[0b0000_0011, 0x10, 0x20, 0x00]);
        if let Some(decoded) = decoded {
            match decoded.motion {
                Motion::Relative { dx, dy, .. } => {
                    assert_eq!((dx, dy), (0, 0), "широкое поле не должно давать значение");
                }
                other => panic!("движение разобрано как {other:?}"),
            }
        }
    }

    // И то же самое, но размером ровно 32: это предел, и он обязан работать.
    let mut wide = QEMU_MOUSE.to_vec();
    for index in 0..wide.len() - 1 {
        if wide[index] == 0x75 && wide[index + 1] == 0x08 {
            wide[index + 1] = 32;
        }
    }
    let _ = parse(&wide);
}

/// Нулевой размер поля не уводит вычитание под ноль.
///
/// Тот же класс, что и предыдущая проверка, и найден тем же способом: у
/// знакового поля старший бит ищется как `size - 1`, а размер приходит из
/// дескриптора. Ноль там — паника на вычитании беззнакового.
#[test]
fn a_field_of_zero_bits_is_dropped() {
    let mut descriptor = QEMU_MOUSE.to_vec();
    for index in 0..descriptor.len() - 1 {
        if descriptor[index] == 0x75 {
            descriptor[index + 1] = 0;
        }
    }
    let parsed = parse(&descriptor);
    if let Some(map) = parsed.pointer {
        let _ = map.decode(&[0x00, 0x00, 0x00, 0x00]);
    }
}

/// Смещение поля у самого края шестнадцати бит не переполняет сложение.
///
/// Найдено фаззером на глубоком прогоне (`--target usb-hid --seed 2`, итерация
/// 1252): «attempt to add with overflow». Смещение растёт по объявленным
/// размерам и упирается в 65535 (дальше оно насыщается), а к нему прибавляется
/// номер бита — и сложение переполнялось. Насыщение здесь и есть верный смысл:
/// бит за пределами отчёта означает «устройство про него не сказало», и читаться
/// он обязан нулём, а не паникой.
#[test]
fn an_offset_at_the_edge_of_sixteen_bits_does_not_overflow() {
    // Дескриптор, который сначала объявляет постоянное поле шириной во весь
    // отчёт, а затем — настоящие поля: смещение к тому моменту насыщено.
    let mut descriptor: Vec<u8> = vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xa1, 0x01, // Collection (Application)
        0x09, 0x01, // Usage (Pointer)
        0xa1, 0x00, // Collection (Physical)
        0x75, 0xff, // Report Size (255)
        0x95, 0xff, // Report Count (255)
        0x81, 0x01, // Input (Constant) — 65025 бит, смещение почти у предела
    ];
    // Ещё столько же: теперь смещение насыщено наверняка.
    for _ in 0..4 {
        descriptor.extend_from_slice(&[0x75, 0xff, 0x95, 0xff, 0x81, 0x01]);
    }
    descriptor.extend_from_slice(&[
        0x05, 0x09, // Usage Page (Button)
        0x19, 0x01, 0x29, 0x03, // Usage Minimum/Maximum (1..3)
        0x15, 0x00, 0x25, 0x01, // Logical Minimum/Maximum
        0x95, 0x03, 0x75, 0x01, // Report Count 3, Size 1
        0x81, 0x02, // Input (Data, Variable)
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x30, 0x09, 0x31, // Usage X, Y
        0x15, 0x81, 0x25, 0x7f, // −127..127
        0x75, 0x08, 0x95, 0x02, // Size 8, Count 2
        0x81, 0x06, // Input (Data, Variable, Relative)
        0xc0, 0xc0,
    ]);

    let parsed = parse(&descriptor);
    if let Some(map) = parsed.pointer {
        // Отчёт короче любого из этих смещений: всё читается нулями, и
        // единственное утверждение здесь — что оно читается, а не падает.
        let _ = map.decode(&[0x01, 0x02, 0x03, 0x04]);
    }

    // И то же для клавиатуры: список клавиш лежит массивом, и его смещение
    // считается умножением номера на размер — там переполнялось и умножение.
    let mut keyboard: Vec<u8> = vec![0x05, 0x07, 0xa1, 0x01];
    for _ in 0..5 {
        keyboard.extend_from_slice(&[0x75, 0xff, 0x95, 0xff, 0x81, 0x01]);
    }
    keyboard.extend_from_slice(&[
        0x95, 0xff, 0x75, 0xff, // Count 255, Size 255
        0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff,
        0x81, 0x00, // Input (Data, Array)
        0xc0,
    ]);
    let parsed = parse(&keyboard);
    if let Some(map) = parsed.keyboard {
        let _ = map.decode(&[0x00, 0x00, 0x00, 0x00]);
    }
}

// ---------------------------------------------------------------------------
// Gamepads (24.11.2026)
// ---------------------------------------------------------------------------

use crate::pad::{self, Gip, PadState, Wireless, decode_gip, decode_x360, decode_x360_wireless};

/// DualShock 4 (054c:05c4), input report 1 and the first output and feature
/// reports, as the pad sends it (`hid-sony` and every published dump): X/Y
/// the left stick, Z/Rz the right one, a 4-bit hat, 14 buttons, a 6-bit
/// counter of the vendor page, Rx/Ry the triggers.
const DUALSHOCK4: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25,
    0x07, 0x35, 0x00, 0x46, 0x3b, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00,
    0x05, 0x09, 0x19, 0x01, 0x29, 0x0e, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0e, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x15, 0x00, 0x25, 0x7f, 0x81, 0x02, 0x05,
    0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
    0x06, 0x00, 0xff, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, 0x85, 0x05, 0x09, 0x22, 0x95, 0x1f, 0x91,
    0x02, 0x85, 0x04, 0x09, 0x23, 0x95, 0x24, 0xb1, 0x02, 0xc0,
];

/// A plain DirectInput pad: four 8-bit axes X, Y, Z, Rz, a hat and twelve
/// buttons, no report number, no analog triggers.
const GENERIC_PAD: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xa1, 0x01, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95, 0x04, 0x09,
    0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35, 0x81, 0x02, 0x25, 0x07, 0x46, 0x3b, 0x01, 0x75, 0x04,
    0x95, 0x01, 0x65, 0x14, 0x09, 0x39, 0x81, 0x42, 0x65, 0x00, 0x75, 0x01, 0x95, 0x0c, 0x25, 0x01,
    0x45, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0c, 0x81, 0x02, 0xc0,
];

#[test]
fn a_dualshock4_is_a_gamepad_with_its_own_layout() {
    let parsed = parse(DUALSHOCK4);
    assert!(parsed.pointer.is_none() && parsed.keyboard.is_none());
    let map = parsed.gamepad.expect("DualShock 4 is a gamepad");
    assert_eq!((map.report_id, map.usage), (1, 5));
    assert_eq!(map.axis_count(), 6);
    assert_eq!(map.button_count(), 14);
    assert!(map.has_hat());
    // Left stick centre-right/up, right stick right/centre, hat right,
    // cross, L1, PS, R2 fully pressed.
    let report = [0x01, 0x80, 0x00, 0xff, 0x80, 0x22, 0x01, 0x01, 0x00, 0xff];
    let state = map.to_pad(&report).expect("report 1");
    assert_eq!(
        state,
        PadState {
            lx: 128,
            ly: -32768,
            rx: 32767,
            ry: 128,
            lt: 0,
            rt: 255,
            hat_x: 1,
            hat_y: 0,
            buttons: pad::BTN_A | pad::BTN_LB | pad::BTN_GUIDE,
        }
    );
    // Hat "null state" (8): centred. Square and triangle: X and Y.
    let state = map.to_pad(&[0x01, 0x80, 0x80, 0x80, 0x80, 0x98, 0x00, 0x00, 0x00, 0x00]).unwrap();
    assert_eq!((state.hat_x, state.hat_y), (0, 0));
    assert_eq!(state.buttons, pad::BTN_X | pad::BTN_Y);
    // A report of another number is not the pad state.
    assert!(map.to_pad(&[0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
}

#[test]
fn a_generic_pad_gets_digital_triggers_from_buttons_7_and_8() {
    let map = parse(GENERIC_PAD).gamepad.expect("a gamepad");
    assert_eq!((map.report_id, map.axis_count(), map.button_count()), (0, 4, 12));
    // Hat up-left (7), button 7 (LT) and 10 (Start).
    let state = map.to_pad(&[0x00, 0xff, 0x80, 0x80, 0x07, 0x24]).unwrap();
    assert_eq!((state.lx, state.ly), (-32768, 32767));
    assert_eq!((state.hat_x, state.hat_y), (-1, -1));
    assert_eq!((state.lt, state.rt), (255, 0));
    assert_eq!(state.buttons, pad::BTN_START);
}

#[test]
fn the_joystick_without_buttons_is_still_a_two_axis_pad() {
    let map = parse(JOYSTICK).gamepad.expect("two axes");
    assert_eq!(map.usage, 4);
    let state = map.to_pad(&[0xff, 0x00]).unwrap();
    assert_eq!((state.lx, state.ly), (32767, -32768));
}

#[test]
fn mice_and_keyboards_are_not_gamepads() {
    for descriptor in [QEMU_KEYBOARD, QEMU_MOUSE, QEMU_TABLET, VBOX_TABLET] {
        assert!(parse(descriptor).gamepad.is_none());
    }
}

#[test]
fn xbox360_report() {
    let mut report = [0u8; 20];
    report[1] = 0x14;
    report[2] = 0x11; // up, Start
    report[3] = 0x34; // Guide, A, B
    report[4] = 0x40;
    report[5] = 0xff;
    report[6..8].copy_from_slice(&0x1234i16.to_le_bytes());
    report[8..10].copy_from_slice(&i16::MAX.to_le_bytes());
    report[10..12].copy_from_slice(&i16::MIN.to_le_bytes());
    let state = decode_x360(&report).unwrap();
    assert_eq!(
        state,
        PadState {
            lx: 0x1234,
            ly: i16::MIN,
            rx: i16::MIN,
            ry: -1,
            lt: 0x40,
            rt: 0xff,
            hat_x: 0,
            hat_y: -1,
            buttons: pad::BTN_START | pad::BTN_GUIDE | pad::BTN_A | pad::BTN_B,
        }
    );
    // LED state message, not input.
    assert!(decode_x360(&[0x01, 0x03, 0x0e, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    assert!(decode_x360(&report[..10]).is_none());
}

#[test]
fn xbox360_wireless_messages() {
    assert_eq!(decode_x360_wireless(&[0x08, 0x80]), Wireless::Presence(true));
    assert_eq!(decode_x360_wireless(&[0x08, 0x00]), Wireless::Presence(false));
    let mut packet = [0u8; 29];
    packet[1] = 0x01;
    packet[4 + 3] = 0x10; // A
    assert!(matches!(decode_x360_wireless(&packet), Wireless::Input(state) if state.buttons == pad::BTN_A));
}

#[test]
fn xbox_one_messages() {
    let mut report = [0u8; 18];
    report[0] = 0x20;
    report[2] = 5;
    report[3] = 0x0e;
    report[4] = 0x14; // Start, A
    report[5] = 0x11; // up, LB
    report[6..8].copy_from_slice(&1023u16.to_le_bytes());
    report[8..10].copy_from_slice(&512u16.to_le_bytes());
    report[10..12].copy_from_slice(&i16::MIN.to_le_bytes());
    report[14..16].copy_from_slice(&i16::MAX.to_le_bytes());
    report[16..18].copy_from_slice(&1i16.to_le_bytes());
    let Gip::Input(state) = decode_gip(&report) else { panic!("input") };
    assert_eq!(
        state,
        PadState {
            lx: i16::MIN,
            ly: -1,
            rx: i16::MAX,
            ry: -2,
            lt: 255,
            rt: 128,
            hat_x: 0,
            hat_y: -1,
            buttons: pad::BTN_START | pad::BTN_A | pad::BTN_LB,
        }
    );
    assert_eq!(decode_gip(&[0x07, 0x30, 0x09, 0x02, 0x01, 0x5b]), Gip::Guide { pressed: true, ack: Some(9) });
    assert_eq!(decode_gip(&[0x07, 0x20, 0x0a, 0x02, 0x00, 0x5b]), Gip::Guide { pressed: false, ack: None });
    assert_eq!(decode_gip(&[0x02, 0x20, 0x01, 0x1c, 0x00]), Gip::Announce);
    assert_eq!(pad::gip_ack(9)[2], 9);
    // Power on for everyone, the S init for the One S.
    assert_eq!(pad::gip_init(0x045e, 0x02dd).count(), 1);
    assert_eq!(pad::gip_init(0x045e, 0x02ea).count(), 2);
    assert_eq!(pad::gip_init(0x0e6f, 0x02a4).count(), 3);
}

#[test]
fn pressed_buttons_by_name() {
    let state = PadState { buttons: pad::BTN_A | pad::BTN_START, ..PadState::default() };
    assert_eq!(std::format!("{}", state.pressed()), "A Start");
    assert_eq!(std::format!("{}", PadState::default().pressed()), "-");
}
