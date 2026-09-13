//! Оформление, которое переживает выключение.
//!
//! # Что здесь чинится
//!
//! Тему можно было переключить с фазы 7 — правым щелчком по столу или в
//! «Параметрах». Но жила она в `AtomicBool` внутри `mini-ui` и пропадала с
//! выключением: человек, выбравший светлую тему, получал тёмную при каждой
//! загрузке. Настройка, которую надо задавать заново после каждого включения, —
//! это не настройка, а привычка мириться.
//!
//! # Почему свой файл, а не строка в `system.cfg`
//!
//! Потому что `system.cfg` пишет установщик, и в нём лежит то, что выбирали
//! **до** первой загрузки: язык, раскладка, пояс, имя пользователя. Оформление
//! выбирают потом и меняют часто. Разные файлы — разная судьба при «сбросе к
//! заводским»: вернуть вид стола, не тронув учётную запись, иначе было бы
//! нельзя.

use crate::config;

/// Имя файла настроек.
pub const CONFIG: &str = "desktop.cfg";

/// Сколько байт читать: файл — две строки.
const LIMIT: usize = 4 * 1024;

/// Прочитать тему при загрузке и применить её.
///
/// Отсутствие файла — обычное состояние только что установленной системы, а не
/// отказ: тема остаётся той, что зашита умолчанием.
pub fn adopt() {
    let Some((bytes, source)) = config::read(CONFIG, LIMIT) else {
        return;
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return;
    };
    let path = config::path(CONFIG, source);
    if let Some(dark) = sysconf::theme_dark(text) {
        mini_ui::theme::set_dark(dark);
        crate::kprintln!(
            "  desktop     : theme {} from {}",
            if dark { "dark" } else { "light" },
            path
        );
    }
    // Персонализация (фаза С6): акцент, обои, высота заголовка. Каждый ключ
    // читается отдельно и молча пропускается, если его нет или он испорчен —
    // опечатка в одном не обязана отменять остальные.
    if let Some(accent) = sysconf::value(text, "accent").and_then(mini_ui::theme::Accent::from_tag) {
        mini_ui::theme::set_accent(accent);
        crate::kprintln!("  desktop     : accent {} from {}", accent.tag(), path);
    }
    if let Some(wall) = sysconf::value(text, "wallpaper").and_then(mini_ui::theme::Wallpaper::from_tag)
    {
        mini_ui::theme::set_wallpaper(wall);
        crate::kprintln!("  desktop     : wallpaper {} from {}", wall.tag(), path);
    }
    if let Some(height) = sysconf::title_bar(text) {
        if mini_ui::theme::set_title_h(height) {
            crate::kprintln!("  desktop     : title bar {height} px from {path}");
        }
    }
}

/// Запомнить тему.
///
/// Зовётся из обоих мест, где тему меняют, — из окна «Параметры» и из меню
/// стола. Двух дорог к одному действию быть не должно, а раз уж они есть,
/// записывать обязаны обе: тема, сохраняющаяся только из окна, — это ошибка,
/// которую человек обнаружит через сутки и не свяжет с тем, как он её менял.
pub fn store_theme(dark: bool) -> Result<(), crate::vfs::VfsError> {
    store_key("theme", if dark { "dark" } else { "light" })
}

/// Запомнить акцент.
pub fn store_accent(accent: mini_ui::theme::Accent) -> Result<(), crate::vfs::VfsError> {
    store_key("accent", accent.tag())
}

/// Запомнить обои.
pub fn store_wallpaper(wall: mini_ui::theme::Wallpaper) -> Result<(), crate::vfs::VfsError> {
    store_key("wallpaper", wall.tag())
}

/// Запомнить высоту заголовка.
pub fn store_title_bar(height: u32) -> Result<(), crate::vfs::VfsError> {
    store_key("titlebar", &alloc::format!("{height}"))
}

/// Правится одна строка файла, остальные остаются как были.
fn store_key(key: &str, value: &str) -> Result<(), crate::vfs::VfsError> {
    let text = config::text(CONFIG, LIMIT);
    let updated = config::replace_key(&text, key, value);
    config::write(CONFIG, &updated)
}

// Разбор строки `theme=` и его тесты — в `sysconf`: ядро под хост не
// собирается, и `#[cfg(test)]` здесь никогда бы не запустился.
