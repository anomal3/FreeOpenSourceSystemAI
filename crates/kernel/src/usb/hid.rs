//! Отчёт boot-протокола клавиатуры → события ввода.
//!
//! # Что присылает клавиатура
//!
//! Восемь байт (HID 1.11, приложение B.1):
//!
//! ```text
//!   байт 0   битовая карта модификаторов
//!   байт 1   зарезервирован
//!   байт 2-7 до шести usage ID нажатых клавиш, 0 — пусто
//! ```
//!
//! # Почему нужно сравнение с прошлым отчётом
//!
//! Отчёт сообщает **состояние**, а не событие: «сейчас нажаты A и Shift». О том,
//! что клавишу отпустили, клавиатура не сообщает никак — она просто перестаёт её
//! упоминать. Значит нажатия и отпускания приходится вычислять сравнением двух
//! последовательных отчётов, и хранение предыдущего — не оптимизация, а
//! единственный способ вообще узнать об отпускании.
//!
//! Порядок клавиш в отчёте при этом произволен: спецификация разрешает
//! клавиатуре перекладывать их между отчётами, поэтому сравнивать надо
//! множества, а не позиции. Отсюда поиск по массиву, а не поэлементное сравнение
//! — на шести элементах это дешевле любой хеш-таблицы.
//!
//! # Потерянные отчёты
//!
//! Если отчёт потерян (переполнилась очередь событий, задача не успела), одно
//! отпускание пропадёт, и клавиша останется «нажатой» до следующего отчёта, в
//! котором её нет. Сравнение состояний это само и исправит на следующем же
//! отчёте — в отличие от потока событий, где потеря «отпустили» залипает
//! навсегда.

use crate::input::{KeyCode, post};
use crate::usb::key_for_usage;

/// Длина отчёта boot-протокола.
pub const REPORT_LEN: usize = 8;

/// Сколько клавиш вмещает отчёт.
const KEYS_IN_REPORT: usize = 6;

/// Смещение первой клавиши.
const KEYS_OFFSET: usize = 2;

/// Модификаторы в байте 0 и соответствующие им клавиши. Порядок — из
/// спецификации: биты идут слева направо от левого Ctrl к правому Meta.
const MODIFIERS: [KeyCode; 8] = [
    KeyCode::LeftCtrl,
    KeyCode::LeftShift,
    KeyCode::LeftAlt,
    KeyCode::LeftMeta,
    KeyCode::RightCtrl,
    KeyCode::RightShift,
    KeyCode::RightAlt,
    KeyCode::RightMeta,
];

/// Состояние клавиатуры: последний разобранный отчёт.
pub struct Keyboard {
    /// Битовая карта модификаторов из прошлого отчёта.
    modifiers: u8,
    /// Usage ID клавиш из прошлого отчёта.
    keys: [u8; KEYS_IN_REPORT],
    /// Сколько отчётов разобрано — для диагностики.
    reports: u64,
}

impl Keyboard {
    #[must_use]
    pub const fn new() -> Self {
        Self { modifiers: 0, keys: [0; KEYS_IN_REPORT], reports: 0 }
    }

    /// Сколько отчётов пришло с момента запуска.
    #[must_use]
    pub const fn reports(&self) -> u64 {
        self.reports
    }

    /// Разобрать отчёт и отправить события в подсистему ввода.
    ///
    /// Короткий отчёт игнорируется целиком: разобрать половину — значит выдать
    /// отпускание клавиш, о которых устройство ничего не сказало.
    pub fn handle_report(&mut self, report: &[u8]) {
        if report.len() < REPORT_LEN {
            return;
        }
        self.reports += 1;

        let modifiers = report[0];
        let changed = modifiers ^ self.modifiers;
        for (bit, code) in MODIFIERS.iter().enumerate() {
            let mask = 1u8 << bit;
            if changed & mask != 0 {
                post(*code, modifiers & mask != 0);
            }
        }
        self.modifiers = modifiers;

        let mut keys = [0u8; KEYS_IN_REPORT];
        keys.copy_from_slice(&report[KEYS_OFFSET..KEYS_OFFSET + KEYS_IN_REPORT]);

        // Отпускания — первыми. Порядок виден потребителю: если клавишу
        // отпустили и в том же отчёте нажали другую, естественнее увидеть
        // «отпустили, нажали», чем наоборот.
        for old in self.keys {
            if old != 0 && !keys.contains(&old) {
                if let Some(code) = key_for_usage(old) {
                    post(code, false);
                }
            }
        }
        for new in keys {
            if new != 0 && !self.keys.contains(&new) {
                if let Some(code) = key_for_usage(new) {
                    post(code, true);
                }
            }
        }

        self.keys = keys;
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}
