//! Окно «Открыть» / «Сохранить как» / «Выбор папки» отдельной программой
//! (20.11).
//!
//! # Зачем отдельной программой
//!
//! Роман (20.11): окно выбора файла в Блокноте рисовалось внутри его окна —
//! карточкой поверх затемнённого текста, — и его нельзя было ни сдвинуть, ни
//! растянуть. В Windows это настоящее окно: свой заголовок, края, оно над
//! окном программы, а программа под ним ждёт. Здесь так же: окно — своя
//! программа со своим окном стола, а стол знает, что оно модально к окну
//! программы, которая его открыла (`SYS_WINOWNER` с чужим владельцем):
//! владелец ввода не принимает, окно встаёт посередине него, кнопка на панели
//! задач одна. Окном пользуется всякая программа — Блокнот, `dotnet`
//! (`OpenFileDialog`, `SaveFileDialog`, `FolderBrowserDialog`), — через
//! `user_progs::filedlg::Dialog`.
//!
//! # Договор
//!
//! `filedlg --stdin`: запрос — строки `ключ\tзначение` в стандартный ввод
//! (`user_progs::filedlg::request_text`), ответ — строка
//! `filedlg::ANSWER` + `ok\t<путь>` или `cancel` в стандартный вывод.
//! Для проверки руками: `filedlg open|save|folder [папка] [имя]` — ответ
//! печатается так же.
//!
//! Пока окно открыто, папка, которую оно показывает, наблюдается
//! (`SYS_FSWATCH`): файл, созданный в ней кем-то ещё, появляется в списке сам.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::paint::Ctx;
use mini_ui::{Rect, Surface};
use user_progs::app::{self, App, Spec};
use user_progs::filedlg::{self, ANSWER, FileDialog, Mode, Options, Outcome, SysHost};
use user_progs::{Args, println, read_stdin};

const SPEC: Spec = Spec { name: "filedlg", title: "File dialog", period_ms: 5_000 };

/// Размер окна в точках макета — как у «Сохранить как» в Windows, чуть
/// меньше Блокнота (900×600): владелец под окном виден по краям.
const SIZE: (u32, u32) = (820, 540);

/// Самый длинный запрос.
const MAX_REQUEST: usize = 16 * 1024;

struct Picker {
    host: SysHost,
    dialog: FileDialog,
    /// Ответ отдан — окно закрывается.
    done: bool,
    /// Запрос пришёл со стандартного ввода: ответ — служебной строкой.
    machine: bool,
    /// Очередь наблюдения за папкой и что наблюдается: путь и номер.
    watch: i64,
    watched: Option<(String, i64)>,
}

impl Picker {
    fn answer(&mut self, outcome: &Outcome) {
        if self.done {
            return;
        }
        if *outcome == Outcome::Pending {
            return;
        }
        // Служебная строка — только программе, открывшей окно (`--stdin`);
        // в терминале её знак-разделитель был бы мусором.
        if self.machine {
            match outcome {
                Outcome::Chosen(path) => println(&format!("{ANSWER}ok\t{path}")),
                _ => println(&format!("{ANSWER}cancel")),
            }
        }
        // Строка — и для журнала стенда: ответ сам по себе служебный.
        match outcome {
            Outcome::Chosen(path) => println(&format!("filedlg: answered {path}")),
            _ => println("filedlg: cancelled"),
        }
        self.done = true;
    }

    fn handle(&mut self, outcome: Outcome) -> bool {
        if outcome != Outcome::Pending {
            self.answer(&outcome);
        }
        true
    }

    /// Следить за папкой, которую показывает список.
    fn follow_dir(&mut self) {
        let want = self.dialog.watched_dir().map(ToString::to_string);
        if want.as_deref() == self.watched.as_ref().map(|(path, _)| path.as_str()) {
            return;
        }
        if let Some((_, wd)) = self.watched.take() {
            user_progs::fswatch_remove(self.watch, wd);
        }
        if let Some(path) = want {
            let wd = user_progs::fswatch_add(self.watch, &path, 0);
            if wd >= 0 {
                self.watched = Some((path, wd));
            }
        }
    }
}

impl App for Picker {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        self.dialog.draw(&self.host, s, ctx, area);
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let outcome = self.dialog.key(&self.host, code, mods, latin);
        self.handle(outcome)
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let outcome = self.dialog.click(&self.host, ctx, area, x, y);
        self.handle(outcome)
    }

    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        self.dialog.drag(ctx, area, x, y)
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        self.dialog.release();
        true
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        self.dialog.wheel(clicks);
        true
    }

    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        self.dialog.hover(ctx, area, x, y)
    }

    fn leave(&mut self) -> bool {
        self.dialog.leave()
    }

    fn close_requested(&mut self) -> bool {
        self.answer(&Outcome::Cancelled);
        true
    }

    fn wants_quit(&self) -> bool {
        self.done
    }

    /// `q` — буква в поле имени, а не «закрыть».
    fn quits_on_q(&self) -> bool {
        false
    }

    fn step(&mut self) -> bool {
        self.follow_dir();
        let changes = user_progs::fswatch_read(self.watch);
        if changes.is_empty() {
            return false;
        }
        self.dialog.refresh(&self.host)
    }

    fn title(&self) -> Option<String> {
        Some(format!("{}\x1f{}", SPEC.title, self.dialog.title()))
    }

    /// Только крестик (как у окна выбора файла Windows), но края тянутся.
    fn window_style(&self) -> u32 {
        user_abi::WIN_STYLE_DIALOG
    }
}

/// Запрос со стандартного ввода — до конца ввода.
fn read_request() -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 1024];
    loop {
        let got = read_stdin(&mut buffer);
        if got <= 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..got as usize]);
        if bytes.len() >= MAX_REQUEST {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в `_start`.
    let args = unsafe { Args::new(argc, argv) };
    let machine = args.get(1) == Some("--stdin");
    let (options, owner, tag) = match args.get(1) {
        Some("--stdin") => filedlg::parse_request(&read_request()),
        mode => {
            let mut options = match mode {
                Some("save") => Options::save(args.get(3).unwrap_or("Без имени.txt")),
                Some("folder") => Options::folder(),
                _ => Options::open(),
            };
            if let Some(dir) = args.get(2) {
                options = options.dir(dir);
            }
            (options, 0, String::from("filedlg"))
        }
    };
    let options = options.windowed();
    let mode = options.mode;
    let info = app::start(&SPEC);
    let size = app::window_size(&info, SIZE);
    app::run(&SPEC, &info, size, move |_, _| {
        // Журнал окна — от имени программы, которая его открыла
        // (`notepad: filedlg: at /data`), как было, пока окно жило в ней.
        let host = SysHost::new(alloc::boxed::Box::leak(tag.clone().into_boxed_str()));
        let me = app::window_id();
        if owner != 0 {
            let flags = user_abi::WIN_OWNER_MODAL | user_abi::WIN_OWNER_CENTER;
            let code = user_progs::set_window_owner(me, owner, flags);
            if code < 0 {
                println(&format!("filedlg: owner window {owner} refused: {code}"));
            } else {
                println(&format!("filedlg: modal over the window of {tag}"));
            }
        }
        let dialog = FileDialog::new(&host, options);
        let kind = match mode {
            Mode::Open => "open",
            Mode::Save => "save",
            Mode::Folder => "folder",
        };
        println(&format!("filedlg: {kind} window for {tag}"));
        let watch = user_progs::fswatch_open();
        let mut picker = Picker { host, dialog, done: false, machine, watch, watched: None };
        picker.follow_dir();
        picker
    })
}
