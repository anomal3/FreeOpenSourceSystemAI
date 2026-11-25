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

/// Какой программой открывать файлы — строка `open_with=` (см.
/// `sysconf::filetypes`): нужна на каждом двойном щелчке, читать файл на
/// каждый незачем.
static OPEN_WITH: crate::sync::SpinLock<alloc::string::String> = crate::sync::SpinLock::new(alloc::string::String::new());

/// Сколько байт читать. Строки короткие, кроме мест значков стола (путь на
/// значок, до [`super::icons`] сотни штук) — им и нужен запас.
const LIMIT: usize = 16 * 1024;

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
    // Фон: картинка, цвет, расположение (22.11, `wallpaper`).
    super::wallpaper::adopt(text, &path);
    // Электропитание: через сколько гаснет экран. Здесь же, а не в своём файле,
    // потому что это настройка того же рода — её выбирают потом и меняют часто,
    // а установщик о ней ничего не знает.
    if let Some(seconds) = sysconf::screen_off_seconds(text) {
        crate::power::set_screen_off_after(seconds);
        if seconds == 0 {
            crate::kprintln!("  power       : screen never turns off, from {path}");
        } else {
            crate::kprintln!("  power       : screen off after {seconds} s, from {path}");
        }
    }
    // Масштаб стола, выбранный вручную (01.11).
    if let Some(choice) = sysconf::value(text, "scale").and_then(mini_ui::theme::scale_choice_from_tag) {
        mini_ui::theme::set_scale_choice(choice);
        crate::kprintln!("  desktop     : scale {} from {}", mini_ui::theme::scale_choice_tag(choice), path);
    }
    // Места значков на столе (01.11).
    if let Some(places) = sysconf::value(text, "icon_places") {
        super::icons::set_places(places);
    }
    // Громкость (02.11, трей).
    if let Some(volume) = sysconf::value(text, "volume").and_then(|value| value.trim().parse::<u8>().ok()) {
        crate::sound::set_volume(volume);
    }
    // Программы для типов файлов (02.11, «Всегда открывать этой программой»).
    if let Some(list) = sysconf::value(text, "open_with") {
        *OPEN_WITH.lock() = alloc::string::String::from(list);
    }
    // Раскладка мониторов (16.11, «Параметры → Дисплей»): только в память —
    // мониторов в реестре ещё нет, стол переезжает на неё, когда их узнает
    // (`ui::attach_outputs`, автозапуск NVIDIA).
    if let Some(list) = sysconf::value(text, "outputs") {
        let saved = crate::display::outputs::parse_saved(list);
        crate::kprintln!("  desktop     : monitor layout for {} monitor(s) from {}", saved.len(), path);
        crate::display::outputs::set_saved(saved);
    }
    // Ярлыки, убранные со стола «Удалить ярлык» (24.10).
    if let Some(list) = sysconf::value(text, "hidden_shortcuts") {
        super::icons::set_hidden(list);
    }
    // System desktop icons: shown, names, pictures (24.11, `deskicons`).
    super::deskicons::adopt(text);
    // Что показывает панель рядом с часами (фаза 69b).
    for (pref, key) in [
        (super::settings::PanelPref::Uptime, "panel_uptime"),
        (super::settings::PanelPref::Date, "panel_date"),
        (super::settings::PanelPref::Labels, "panel_labels"),
        (super::settings::PanelPref::Animations, "animations"),
    ] {
        match sysconf::value(text, key) {
            Some("on") => super::panel::set_shows(pref, true),
            Some("off") => super::panel::set_shows(pref, false),
            _ => continue,
        }
        crate::kprintln!("  desktop     : {key} {} from {}", sysconf::value(text, key).unwrap_or(""), path);
    }
    // «Оформление» и глубина цвета (20.11, см. `look`) — после `animations=`:
    // строка `look=` новее и главнее.
    super::look::adopt(text, &path);
    // Тема значков (22.11, `appicon`).
    super::appicon::adopt(text, &path);
    // Панель задач и «Пуск» одним набором (20.11, см. `deskcfg`).
    super::deskcfg::adopt(text, &path);
    // Заставка (22.11, `saver`).
    super::saver::adopt(text, &path);
    // Подробная загрузка (24.11): what «Восстановление» shows; the loader
    // itself reads `\FREEOS\BOOT.CFG`.
    if let Some(mode) = sysconf::value(text, "boot_log") {
        BOOT_VERBOSE.store(mode.trim() == "verbose", core::sync::atomic::Ordering::Relaxed);
    }
    if let Some(height) = sysconf::title_bar(text) {
        if mini_ui::theme::set_title_h(height) {
            crate::kprintln!("  desktop     : title bar {height} px from {path}");
        }
    }
}

/// «Подробная загрузка» chosen in «Параметрах» (24.11).
static BOOT_VERBOSE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Is the verbose boot chosen for the next boots.
#[must_use]
pub fn boot_verbose() -> bool {
    BOOT_VERBOSE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Choose the normal (logo and bar) or verbose (log) boot for the next
/// boots: `\FREEOS\BOOT.CFG` for the loader and `boot_log=` here. `Ok` -
/// what to tell the person.
pub fn set_boot_verbose(on: bool) -> Result<alloc::string::String, alloc::string::String> {
    BOOT_VERBOSE.store(on, core::sync::atomic::Ordering::Relaxed);
    let mode = if on { "verbose" } else { "normal" };
    crate::kprintln!("  settings    : boot {mode}");
    let said = alloc::string::String::from(if on { "со следующей загрузки — журнал вместо логотипа" } else { "со следующей загрузки — логотип и полоса" });
    let loader = crate::slot::request_boot_log(on);
    let stored = store_line("boot_log", mode);
    match (loader, stored) {
        (Ok(()), Ok(())) => Ok(said),
        (Err(err), _) => Err(alloc::format!("{said}: загрузчику не записано ({err})")),
        (_, Err(err)) => Err(alloc::format!("{said}, но не запомнено: {err}")),
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

/// Запомнить срок гашения экрана. Ноль — не гасить.
pub fn store_screen_off(seconds: u32) -> Result<(), crate::vfs::VfsError> {
    if seconds == 0 {
        store_key("screen_off", "never")
    } else {
        store_key("screen_off", &alloc::format!("{seconds}"))
    }
}

/// Запомнить высоту заголовка.
pub fn store_title_bar(height: u32) -> Result<(), crate::vfs::VfsError> {
    store_key("titlebar", &alloc::format!("{height}"))
}

/// Запомнить выбор масштаба.
pub fn store_scale(choice: u32) -> Result<(), crate::vfs::VfsError> {
    store_key("scale", mini_ui::theme::scale_choice_tag(choice))
}

/// Запомнить места значков стола (`место=ключ` через `|`).
pub fn store_icon_places(list: &str) -> Result<(), crate::vfs::VfsError> {
    store_key("icon_places", list)
}

/// Запомнить ярлыки, убранные со стола (подписи через `|`).
pub fn store_hidden_shortcuts(list: &str) -> Result<(), crate::vfs::VfsError> {
    store_key("hidden_shortcuts", list)
}

/// Программа для расширения `ext`: выбор человека (с 24.11 — в хранилище
/// настроек, `filetypes/.ext/default` области пользователя), прежняя строка
/// `open_with=` этого файла, затем умолчание системы (OBSOS для `.html`).
#[must_use]
pub fn open_with(ext: &str) -> Option<alloc::string::String> {
    use crate::config::registry::{self, Scope};
    if ext.is_empty() {
        return None;
    }
    let key = sysconf::store::default_key(ext);
    if let Some(command) = registry::get_in(Scope::User, &key).filter(|command| registry::program_exists(command)) {
        return Some(command);
    }
    if let Some(command) = sysconf::filetypes::chosen(&OPEN_WITH.lock(), ext) {
        return Some(alloc::string::String::from(command));
    }
    registry::default_program(ext)
}

/// Выбрал ли программу для `ext` сам человек (а не умолчание системы).
#[must_use]
pub fn chosen_by_person(ext: &str) -> bool {
    use crate::config::registry::{self, Scope};
    registry::get_in(Scope::User, &sysconf::store::default_key(ext)).is_some() || sysconf::filetypes::chosen(&OPEN_WITH.lock(), ext).is_some()
}

/// Запомнить программу для расширения — в хранилище настроек человека
/// (24.11; `desktop.cfg` больше не пишется).
pub fn store_open_with(ext: &str, command: &str) -> Result<(), crate::vfs::VfsError> {
    use crate::config::registry::{self, RegError, Scope};
    match registry::set(Scope::User, &sysconf::store::default_key(ext), Some(command.trim())) {
        Ok(()) => Ok(()),
        Err(RegError::Fs(err)) => Err(err),
        Err(RegError::BadKey) => Err(crate::vfs::VfsError::BadPath),
    }
}

/// Запомнить раскладку мониторов: и в файл, и в память (её же берёт
/// `outputs restore` и автозапуск NVIDIA без перезагрузки).
pub fn store_outputs(list: alloc::vec::Vec<crate::display::outputs::Saved>) -> Result<alloc::string::String, crate::vfs::VfsError> {
    let text = crate::display::outputs::format_saved(&list);
    crate::display::outputs::set_saved(list);
    if let Err(err) = store_key("outputs", &text) {
        // Живая система: корень только для чтения — на флешку (16.11).
        if crate::usb::stick_log::store_layout(&alloc::format!("outputs={text}\n")) {
            crate::kprintln!("  desktop     : monitor layout saved on the stick (\\FREEOS\\LAYOUT.TXT)");
            return Ok(text);
        }
        return Err(err);
    }
    Ok(text)
}

/// Запомнить громкость.
pub fn store_volume(volume: u8) -> Result<(), crate::vfs::VfsError> {
    store_key("volume", &alloc::format!("{volume}"))
}

/// Запомнить, что показывает панель.
pub fn store_panel(pref: super::settings::PanelPref, on: bool) -> Result<(), crate::vfs::VfsError> {
    let key = match pref {
        super::settings::PanelPref::Uptime => "panel_uptime",
        super::settings::PanelPref::Date => "panel_date",
        super::settings::PanelPref::Labels => "panel_labels",
        super::settings::PanelPref::Animations => {
            // Обе анимации «Оформления» (20.11): строка `look=` главнее.
            return super::look::store();
        }
    };
    store_key(key, if on { "on" } else { "off" })
}

/// Записать строку `key=value` (для `deskcfg`: панель и «Пуск»).
pub fn store_line(key: &str, value: &str) -> Result<(), crate::vfs::VfsError> {
    store_key(key, value)
}

/// Правится одна строка файла, остальные остаются как были.
fn store_key(key: &str, value: &str) -> Result<(), crate::vfs::VfsError> {
    let text = config::text(CONFIG, LIMIT);
    let updated = config::replace_key(&text, key, value);
    config::write(CONFIG, &updated)
}

// Разбор строки `theme=` и его тесты — в `sysconf`: ядро под хост не
// собирается, и `#[cfg(test)]` здесь никогда бы не запустился.
