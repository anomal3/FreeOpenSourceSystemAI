//! Окно «Параметры»: то, что человек настраивает, не набирая команд.
//!
//! # Почему это окно, а не набор команд оболочки
//!
//! Все четыре раздела и сейчас доступны из терминала: версию печатает `about`,
//! пакеты — `pkg list`, обновление — `sysupdate`, разрешение задаёт прошивка.
//! Разница не в возможностях, а в том, что человеку не приходится знать имена
//! команд, чтобы посмотреть, сколько памяти в машине и откуда она берёт
//! обновления. Окно ничего не умеет сверх команд — и это осознанно: две дороги к
//! одному действию расходятся ровно в тот день, когда одну из них поправят.
//!
//! # Почему разделы слева, а содержимое справа
//!
//! Потому что так устроены «Параметры» везде, где человек их видел. Раскладка,
//! к которой не надо привыкать, — это раскладка, которую не надо объяснять.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::text::{self, GLYPH_H, GLYPH_W};
use mini_ui::{Rect, Surface};

use super::theme;
use crate::input::KeyCode;
use crate::{arch, config, fs};

/// Разделы окна — порядок сверху вниз.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Что это за машина: версия, процессор, память, экран.
    System,
    /// Экран: какой режим сейчас и какой попросить у прошивки.
    Display,
    /// Что установлено пакетами.
    Programs,
    /// Откуда система берёт обновления и что с ними делать.
    Updates,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::System,
        Section::Display,
        Section::Programs,
        Section::Updates,
    ];

    const fn title(self) -> &'static str {
        match self {
            Section::System => "System",
            Section::Display => "Display",
            Section::Programs => "Programs",
            Section::Updates => "Updates",
        }
    }

    const fn about(self) -> &'static str {
        match self {
            Section::System => "version, processor, memory",
            Section::Display => "screen resolution",
            Section::Programs => "installed packages",
            Section::Updates => "where updates come from",
        }
    }
}

/// Разрешения, которые можно попросить у прошивки.
///
/// Список, а не свободный ввод: прошивка предлагает свой набор режимов, и
/// написанное от руки «1234×567» она всё равно отвергнет. Здесь перечислены те,
/// которые предлагает всякая машина, на которой эта система вообще запускается.
pub const MODES: [(u32, u32); 5] = [
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1600, 900),
    (1920, 1080),
];

/// Одна строка правой половины окна.
enum Line {
    /// Заголовок раздела содержимого.
    Heading(String),
    /// Пара «название — значение»: то, что можно только прочитать.
    Fact(String, String),
    /// Пункт, по которому можно нажать, вместе с тем, что он делает.
    ///
    /// Действие лежит в самой строке, а не выводится из её номера. Номер годился,
    /// пока пункты были перечислением разрешений экрана; список установленных
    /// пакетов меняется от машины к машине, и «пятый пункт означает удалить
    /// пятый пакет» — это два места, обязанные считать одинаково, а разойдутся
    /// они в первый же день.
    Action(Deed, String, String),
    /// Пустая строка.
    Gap,
}

/// Что делает пункт, по которому нажали.
#[derive(Clone, PartialEq, Eq)]
enum Deed {
    /// Попросить у прошивки этот режим экрана.
    Mode(u32, u32),
    /// Запустить `sysupdate`.
    CheckUpdates,
    /// Показать, что можно поставить.
    ChooseFile,
    /// Поставить пакет из этого файла.
    Install(String),
    /// Спросить, точно ли удалять этот пакет.
    AskRemove(String),
    /// Удалить его.
    Remove(String),
    /// Вернуться к списку установленного.
    BackToList,
}

/// Чем занят раздел «Programs».
///
/// Установка и удаление — два разговора, а не два нажатия: у первого надо
/// спросить, какой файл ставить, у второго — точно ли удалять. Оба разговора
/// идут в той же правой половине окна, потому что отдельное окно ради двух
/// вопросов — это ещё один вид окна, который придётся объяснять.
#[derive(Clone, PartialEq, Eq)]
enum Programs {
    /// Список установленного.
    List,
    /// Выбор файла для установки — вместе с тем, что нашлось.
    ///
    /// Список найденного лежит здесь, а не пересчитывается при рисовании, и это
    /// не кеш ради скорости. Строки правой половины считаются заново на каждое
    /// нажатие — и на разбор клавиши, и на перерисовку, — а поиск файлов
    /// пакетов означает обход трёх каталогов и чтение заголовка у каждого
    /// найденного. Внутри обработчика события ввода это те самые сотни
    /// миллисекунд, за которые теряется следующая клавиша.
    Choose(Vec<String>),
    /// Вопрос «точно удалить этот пакет?».
    Confirm(String),
}

pub struct SettingsView {
    section: usize,
    /// Выбранный пункт справа — считается по строкам [`Line::Action`].
    action: usize,
    /// Что ответило последнее действие.
    message: Option<String>,
    /// Размер экрана: окно узнаёт его от стола, само оно экрана не видит.
    screen: (u32, u32),
    /// Фокус на левом списке разделов, а не на пунктах справа.
    on_sections: bool,
    /// Чем занят раздел «Programs».
    programs: Programs,
}

impl SettingsView {
    #[must_use]
    pub fn new(screen: (u32, u32)) -> Self {
        Self {
            section: 0,
            action: 0,
            message: None,
            screen,
            on_sections: true,
            programs: Programs::List,
        }
    }

    /// Перейти на раздел экрана — им открывается меню стола.
    pub fn show_display(&mut self) {
        self.section = 1;
        self.on_sections = true;
        self.action = 0;
        self.message = None;
    }

    fn current(&self) -> Section {
        Section::ALL[self.section.min(Section::ALL.len() - 1)]
    }

    /// Строки правой половины для текущего раздела.
    fn lines(&self) -> Vec<Line> {
        match self.current() {
            Section::System => self.system_lines(),
            Section::Display => self.display_lines(),
            Section::Programs => self.programs_lines(),
            Section::Updates => self.updates_lines(),
        }
    }

    fn system_lines(&self) -> Vec<Line> {
        let frames = crate::mm::frame::stats();
        let uptime = crate::time::uptime_ms() / 1000;
        let mut lines = Vec::new();
        lines.push(Line::Heading(format!("FreeOS {}", crate::VERSION)));
        lines.push(Line::Gap);
        lines.push(Line::Fact("Architecture".to_string(), arch::ARCH_NAME.to_string()));
        lines.push(Line::Fact(
            "Memory".to_string(),
            format!(
                "{} MiB free of {} MiB",
                frames.free_bytes() / (1024 * 1024),
                frames.total_bytes() / (1024 * 1024)
            ),
        ));
        lines.push(Line::Fact(
            "Screen".to_string(),
            format!("{}x{}", self.screen.0, self.screen.1),
        ));
        lines.push(Line::Fact(
            "Uptime".to_string(),
            format!("{} h {:02} min {:02} s", uptime / 3600, (uptime / 60) % 60, uptime % 60),
        ));
        lines.push(Line::Fact("Mounted".to_string(), mounted_text()));
        lines.push(Line::Gap);
        lines.push(Line::Heading("Written from scratch in Rust".to_string()));
        lines
    }

    fn display_lines(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        lines.push(Line::Heading("Screen resolution".to_string()));
        lines.push(Line::Gap);
        lines.push(Line::Fact(
            "Now".to_string(),
            format!("{}x{}", self.screen.0, self.screen.1),
        ));
        lines.push(Line::Gap);
        for (width, height) in MODES {
            let mark = if (width, height) == self.screen { "current" } else { "" };
            lines.push(Line::Action(
                Deed::Mode(width, height),
                format!("{width} x {height}"),
                mark.to_string(),
            ));
        }
        lines.push(Line::Gap);
        lines.push(Line::Heading("Applied at next boot".to_string()));
        lines
    }

    fn programs_lines(&self) -> Vec<Line> {
        match &self.programs {
            Programs::List => self.programs_list(),
            Programs::Choose(found) => Self::programs_choose(found),
            Programs::Confirm(name) => Self::programs_confirm(name),
        }
    }

    fn programs_list(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        lines.push(Line::Heading("Installed packages".to_string()));
        lines.push(Line::Gap);
        match packages() {
            Ok(names) if names.is_empty() => {
                lines.push(Line::Fact(
                    "None".to_string(),
                    "nothing is installed yet".to_string(),
                ));
            }
            Ok(names) => {
                for name in names {
                    lines.push(Line::Action(
                        Deed::AskRemove(name.clone()),
                        name,
                        "remove".to_string(),
                    ));
                }
            }
            // Отказ реестра — не пустой список: «пакетов нет» и «спросить не
            // удалось» человек обязан различать, иначе он поставит второй раз
            // то, что уже стоит.
            Err(err) => lines.push(Line::Fact("Registry".to_string(), err)),
        }
        lines.push(Line::Gap);
        lines.push(Line::Action(
            Deed::ChooseFile,
            "Install a package".to_string(),
            // Без пометки: она встаёт за самим пунктом, а на пункт такой длины
            // от неё остаётся один знак обрезки — то есть ровно ничего, кроме
            // мусора справа.
            String::new(),
        ));
        lines.push(Line::Gap);
        lines.push(Line::Heading("Packages live in /opt".to_string()));
        lines
    }

    fn programs_choose(found: &[String]) -> Vec<Line> {
        let mut lines = Vec::new();
        lines.push(Line::Heading("Install a package".to_string()));
        lines.push(Line::Gap);
        if found.is_empty() {
            lines.push(Line::Fact(
                "No files".to_string(),
                "looked in /media and home".to_string(),
            ));
        }
        for path in found {
            let name = short_name(path);
            lines.push(Line::Action(
                Deed::Install(path.clone()),
                name,
                String::new(),
            ));
        }
        lines.push(Line::Gap);
        lines.push(Line::Action(
            Deed::BackToList,
            "Back".to_string(),
            String::new(),
        ));
        lines
    }

    fn programs_confirm(name: &str) -> Vec<Line> {
        let mut lines = Vec::new();
        lines.push(Line::Heading(format!("Remove {name}?")));
        lines.push(Line::Gap);
        lines.push(Line::Fact(
            "Files".to_string(),
            format!("/opt/{name} goes away"),
        ));
        lines.push(Line::Gap);
        lines.push(Line::Action(
            Deed::Remove(name.to_string()),
            "Yes, remove it".to_string(),
            "runs pkg remove".to_string(),
        ));
        lines.push(Line::Action(
            Deed::BackToList,
            "No, keep it".to_string(),
            String::new(),
        ));
        lines
    }

    fn updates_lines(&self) -> Vec<Line> {
        let mut lines = Vec::new();
        lines.push(Line::Heading("System updates".to_string()));
        lines.push(Line::Gap);
        lines.push(Line::Fact("Installed".to_string(), crate::VERSION.to_string()));
        for server in update_servers() {
            lines.push(Line::Fact("Server".to_string(), server));
        }
        lines.push(Line::Gap);
        lines.push(Line::Action(
            Deed::CheckUpdates,
            "Check for updates".to_string(),
            "runs sysupdate".to_string(),
        ));
        lines.push(Line::Gap);
        lines.push(Line::Heading("Updates must be signed".to_string()));
        lines
    }

    /// Сколько пунктов, по которым можно нажать, в текущем разделе.
    fn actions(&self) -> usize {
        self.lines()
            .iter()
            .filter(|line| matches!(line, Line::Action(..)))
            .count()
    }

    /// Разобрать клавишу. `true` — окно её использовало.
    pub fn handle(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Up => {
                if self.on_sections {
                    self.section = self.section.saturating_sub(1);
                    self.programs = Programs::List;
                } else {
                    self.action = self.action.saturating_sub(1);
                }
                self.message = None;
                true
            }
            KeyCode::Down => {
                if self.on_sections {
                    self.section = (self.section + 1).min(Section::ALL.len() - 1);
                    // Уход из раздела возвращает его в исходное состояние:
                    // вопрос «точно удалить?», оставленный без ответа, при
                    // возврате означал бы вопрос о том, чего человек уже не
                    // помнит.
                    self.programs = Programs::List;
                } else {
                    self.action = (self.action + 1).min(self.actions().saturating_sub(1));
                }
                self.message = None;
                true
            }
            // Вправо — перейти к пунктам раздела, влево — вернуться к списку
            // разделов. Клавиатурой окно проходится целиком: мышь на этой
            // машине есть не всегда.
            KeyCode::Right | KeyCode::Tab => {
                if self.actions() > 0 {
                    self.on_sections = false;
                    self.action = self.action.min(self.actions() - 1);
                }
                true
            }
            KeyCode::Left => {
                self.on_sections = true;
                true
            }
            KeyCode::Enter => {
                if self.on_sections {
                    if self.actions() > 0 {
                        self.on_sections = false;
                        self.action = 0;
                    }
                } else {
                    self.activate();
                }
                true
            }
            _ => false,
        }
    }

    /// Щелчок по окну: координаты внутри области содержимого.
    ///
    /// Возвращает `true`, если щелчок что-то изменил и окно надо перерисовать.
    pub fn click(&mut self, area: Rect, scale: u32, x: i32, y: i32) -> bool {
        let step = (GLYPH_H * scale + 4 * scale) as i32;
        let split = area.x + (area.w as i32) / 3;
        if x < split {
            let index = ((y - area.y - step / 2) / (step * 2)).max(0) as usize;
            if index < Section::ALL.len() {
                self.section = index;
                self.on_sections = true;
                self.action = 0;
                self.message = None;
                self.programs = Programs::List;
                return true;
            }
            return false;
        }

        // Справа считаем строки подряд и ищем, на какую попали: у пунктов свой
        // счёт, потому что нажимать можно только по ним.
        let row = ((y - area.y) / step).max(0) as usize;
        let mut action_index = 0;
        for (index, line) in self.lines().iter().enumerate() {
            if let Line::Action(..) = line {
                if index == row {
                    self.on_sections = false;
                    self.action = action_index;
                    self.activate();
                    return true;
                }
                action_index += 1;
            }
        }
        false
    }

    /// Выполнить выбранный пункт.
    ///
    /// Что именно делать, спрашивается у самой строки: список пунктов зависит
    /// от того, что установлено на этой машине, и вывести действие из номера
    /// строки означало бы считать список дважды.
    fn activate(&mut self) {
        let deed = self
            .lines()
            .into_iter()
            .filter_map(|line| match line {
                Line::Action(deed, _, _) => Some(deed),
                _ => None,
            })
            .nth(self.action);
        let Some(deed) = deed else {
            return;
        };
        match deed {
            Deed::Mode(width, height) => {
                self.message = Some(match crate::slot::request_screen_mode(width, height) {
                    Ok(()) => format!("{width}x{height} will be used from the next start"),
                    Err(err) => format!("cannot save the choice: {err}"),
                });
            }
            // Окно не качает обновление само и не ждёт его: `sysupdate` —
            // программа третьего кольца, у неё сеть, TLS и запись в раздел.
            // Окно только запускает её, а разговаривает она с человеком в
            // терминале — там же, где отвечала бы, набери он её имя руками.
            // Ждать её здесь нельзя: этот код работает внутри разбора события
            // ввода. То же самое и ниже, с `pkg`.
            Deed::CheckUpdates => self.message = Some(run("/bin/sysupdate", "sysupdate")),
            Deed::ChooseFile => {
                let found = available();
                let empty = found.is_empty();
                self.programs = Programs::Choose(found);
                self.action = 0;
                self.message = empty.then(|| {
                    "no .fpk package was found in /media or in your home".to_string()
                });
            }
            Deed::Install(path) => {
                let line = format!("/bin/pkg install {path}");
                self.message = Some(run(&line, "pkg install"));
                self.programs = Programs::List;
                self.action = 0;
            }
            Deed::AskRemove(name) => {
                self.programs = Programs::Confirm(name);
                self.action = 0;
                self.message = None;
            }
            Deed::Remove(name) => {
                let line = format!("/bin/pkg remove {name}");
                self.message = Some(run(&line, "pkg remove"));
                self.programs = Programs::List;
                self.action = 0;
            }
            Deed::BackToList => {
                self.programs = Programs::List;
                self.action = 0;
                self.message = None;
            }
        }
    }

    /// Нарисовать окно целиком.
    pub fn draw(&self, surface: &mut Surface, area: Rect, scale: u32) {
        surface.fill(area, theme::WINDOW_BG);
        let step = (GLYPH_H * scale + 4 * scale) as i32;
        let split = area.x + (area.w as i32) / 3;

        // Левая колонка: разделы. Каждый занимает две строки — название и
        // пояснение под ним, как в меню запуска.
        for (index, section) in Section::ALL.iter().enumerate() {
            let top = area.y + index as i32 * step * 2;
            let row = Rect::new(
                area.x,
                top,
                (split - area.x).max(0) as u32,
                (step * 2) as u32,
            );
            let chosen = index == self.section;
            if chosen {
                surface.fill(row, if self.on_sections { theme::SELECT_BG } else { theme::inactive(theme::SELECT_BG) });
            }
            text::draw_text(
                surface,
                (area.x + 6 * scale as i32) as u32,
                (top + 2 * scale as i32) as u32,
                section.title(),
                scale,
                if chosen { theme::TEXT } else { theme::DIM },
                None,
            );
            // Подпись раздела обрезается по ширине колонки: она пояснение, а
            // не заголовок, и заезжать на содержимое справа ей нельзя.
            let small = scale.saturating_sub(1).max(1);
            let room = (((split - area.x - 12 * scale as i32).max(0) as u32) / (GLYPH_W * small)) as usize;
            text::draw_text(
                surface,
                (area.x + 6 * scale as i32) as u32,
                (top + step) as u32,
                &clip_text(section.about(), room),
                small,
                theme::DIM,
                None,
            );
        }

        // Разделительная черта: без неё две колонки читаются как одна с рваными
        // отступами.
        surface.fill(
            Rect::new(split, area.y, 1.max(scale / 2), area.h),
            theme::FRAME,
        );

        // Правая колонка: содержимое раздела. Ширина колонки значений
        // считается от места, а не задана числом: у узкого окна значение,
        // отодвинутое на четырнадцать знаков, уезжало за край и обрывалось на
        // полуслове.
        let text_x = (split + 10 * scale as i32) as u32;
        let right_w = area.right().saturating_sub(text_x as i32).max(0) as u32;
        let cell = GLYPH_W * scale;
        let columns = (right_w / cell).max(1);
        let name_width = columns.saturating_sub(4) / 2;
        let value_x = text_x + cell * name_width.min(14).max(1);
        let value_room = ((area.right() - value_x as i32).max(0) as u32 / cell) as usize;
        let mut action_index = 0;
        for (index, line) in self.lines().iter().enumerate() {
            let top = area.y + index as i32 * step;
            if top + step > area.bottom() {
                break;
            }
            match line {
                Line::Heading(title) => {
                    let room = (right_w / cell) as usize;
                    text::draw_text(
                        surface,
                        text_x,
                        top as u32,
                        &clip_text(title, room),
                        scale,
                        theme::ACCENT,
                        None,
                    );
                }
                Line::Fact(name, value) => {
                    text::draw_text(surface, text_x, top as u32, name, scale, theme::DIM, None);
                    text::draw_text(
                        surface,
                        value_x,
                        top as u32,
                        &clip_text(value, value_room),
                        scale,
                        theme::TEXT,
                        None,
                    );
                }
                Line::Action(_, name, note) => {
                    let chosen = !self.on_sections && action_index == self.action;
                    if chosen {
                        surface.fill(
                            Rect::new(split + 4, top, area.right().saturating_sub(split + 8).max(0) as u32, step as u32),
                            theme::SELECT_BG,
                        );
                    }
                    text::draw_text(
                        surface,
                        text_x,
                        top as u32,
                        name,
                        scale,
                        if chosen { theme::TEXT } else { theme::DIM },
                        None,
                    );
                    if !note.is_empty() {
                        // Пометка идёт за самим пунктом, а не в колонке
                        // значений: пункт короткий, и разнесённые по разным
                        // колонкам «1280 x 720» и «current» читались как две
                        // отдельные строки, а на узком окне ещё и налезали
                        // друг на друга.
                        let after = text_x + cell * (name.chars().count() as u32 + 2);
                        let room = ((area.right() - after as i32).max(0) as u32 / cell) as usize;
                        text::draw_text(
                            surface,
                            after,
                            top as u32,
                            &clip_text(note, room),
                            scale,
                            theme::DIRECTORY,
                            None,
                        );
                    }
                    action_index += 1;
                }
                Line::Gap => {}
            }
        }

        // Ответ последнего действия — внизу окна, отдельной полосой: он
        // относится ко всему окну, а не к строке, по которой нажали.
        if let Some(message) = &self.message {
            let bar = Rect::new(area.x, area.bottom() - step, area.w, step as u32);
            surface.fill(bar, theme::SELECT_BG);
            text::draw_text(
                surface,
                (area.x + 6 * scale as i32) as u32,
                (bar.y + 2) as u32,
                message,
                scale,
                theme::TEXT,
                None,
            );
        }
    }
}

/// Обрезать строку по числу знаков, поставив многоточие.
///
/// Обрезать, а не переносить: строка «Параметров» — это одно значение, и
/// половина его на следующей строке читается как другое значение.
fn clip_text(text: &str, room: usize) -> String {
    if room == 0 {
        return String::new();
    }
    if text.chars().count() <= room {
        return text.to_string();
    }
    let keep = room.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('~');
    out
}

/// Что и куда смонтировано — одной строкой.
fn mounted_text() -> String {
    let mounts = fs::mounted();
    if mounts.is_empty() {
        return "nothing".to_string();
    }
    let mut text = String::new();
    for (index, (prefix, kind)) in mounts.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        text.push_str(prefix);
        text.push_str(" ");
        text.push_str(kind);
    }
    text
}

/// Имена установленных пакетов из реестра `/var/lib/pkg`.
///
/// Реестр — то же место, куда пишет `pkg`: два списка установленного
/// разошлись бы в первый же день.
fn packages() -> Result<Vec<String>, String> {
    const REGISTRY: &str = "/var/lib/pkg";
    let listing = fs::list(REGISTRY).ok_or_else(|| "no filesystem".to_string())?;
    let entries = listing.map_err(|err| format!("{err:?}"))?;
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.name.ends_with(".pkg"))
        .map(|entry| entry.name.trim_end_matches(".pkg").to_string())
        .collect();
    names.sort();
    Ok(names)
}

/// Запустить программу и рассказать человеку, чем это кончилось.
///
/// Окно не ждёт её конца и не может ждать: этот код работает внутри разбора
/// события ввода, а `pkg` читает файл, распаковывает его и пишет на диск. Всё,
/// что окно вправе сообщить, — что программа **начала** работу; говорит она
/// сама, в терминале.
fn run(line: &str, what: &str) -> String {
    match crate::user::spawn(line, crate::user::session::credentials()) {
        Ok(id) => {
            // В журнал — чтобы снаружи было видно, что программу запустило
            // **окно**, а не человек в терминале. Без этой строки проверить
            // кнопку нечем: вывод самой программы одинаков в обоих случаях.
            crate::kprintln!("  settings    : started '{line}' as {id}");
            format!("{what} started as {id}; see the terminal")
        }
        Err(err) => {
            crate::kprintln!("  settings    : cannot start '{line}': {err}");
            format!("cannot start {what}: {err}")
        }
    }
}

/// Файлы пакетов, которые видно с этой машины.
///
/// Смотрим в носитель, в домашний каталог и на стол — три места, куда пакет
/// попадает: с установочного носителя, из загрузки и рукой человека. Обходить
/// весь корень нельзя: это чтение каждого каталога тома внутри обработчика
/// события ввода.
fn available() -> Vec<String> {
    const MEDIA: &str = "/media";
    let home = super::context::home_dir();
    let desktop = super::context::desktop_dir();
    let mut found = Vec::new();
    for place in [MEDIA, home.as_str(), desktop.as_str()] {
        let Some(Ok(entries)) = fs::list(place) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .into_iter()
            .filter(|entry| entry.name.ends_with(".fpk"))
            .map(|entry| entry.name)
            .collect();
        names.sort();
        for name in names {
            if found.len() == MAX_FILES {
                return found;
            }
            let path = format!("{place}/{name}");
            if is_package(&path) {
                found.push(path);
            }
        }
    }
    found
}

/// Пакет ли это — или образ системы под тем же расширением.
///
/// Различать обязательно: в `/media` рядом с пакетами лежат контейнеры
/// обновления, у них то же расширение и тот же формат, но ставит их не `pkg`, а
/// `sysupdate`. Предложить образ системы в списке «что установить» значило бы
/// предложить действие, которое кончится отказом, — и человек решил бы, что
/// сломан он или файл.
///
/// Читается только заголовок: манифест с именем и версией лежит за ним и стоил
/// бы ещё одного чтения на каждый файл, а этот код работает внутри разбора
/// события ввода.
fn is_package(path: &str) -> bool {
    let Some(Ok((bytes, _))) = fs::read(path, fpk::HEADER_SIZE) else {
        return false;
    };
    matches!(fpk::Header::parse(&bytes), Ok(header) if header.kind == fpk::Kind::Package)
}

/// Сколько файлов показывается в списке установки.
///
/// Предел не косметический: список рисуется в окне, а имена приходят с
/// носителя. Каталог с сотней файлов означал бы сто строк, из которых видно
/// восемь, и прокрутки у этого списка нет.
const MAX_FILES: usize = 8;

/// Имя файла без каталога.
fn short_name(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[index + 1..].to_string(),
        None => path.to_string(),
    }
}

/// Серверы обновлений из `update.cfg` — в том порядке, в каком их пробует
/// `sysupdate`.
fn update_servers() -> Vec<String> {
    let Some((bytes, _)) = config::read("update.cfg", 4096) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes).to_string();
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("server="))
        .map(|value| value.to_string())
        .take(4)
        .collect()
}
