//! «Свойства» — отдельное окно (24.11): `files --properties ПУТЬ…`.
//!
//! Роман: свойства «не работают» — прежде это была карточка поверх списка,
//! без имени, которое можно поправить, без «Открывать с помощью → Изменить»,
//! без размера папки и без атрибутов, а у стола — строка в меню. Теперь —
//! окно, как у Проводника:
//!
//! * файл или папка: имя (правится), тип, «Открывать с помощью» и
//!   «Изменить…», расположение, размер (у папки считается в фоне, по куску
//!   за виток цикла), дата изменения, атрибуты «Только чтение» и «Скрытый»;
//!   вкладка «Подробно» — владелец, группа, права;
//! * несколько объектов: сколько файлов и папок, общий тип или «разные»,
//!   общий размер, атрибуты для всех;
//! * диск: метка, тип, файловая система, занято и свободно — числами и
//!   кругом.
//!
//! «Скрытый» в Unix — имя с точки: флажок переименовывает. «Только чтение» —
//! без права записи (`SYS_CHMOD`). «ОК» и «Применить» — как в Windows.
//!
//! Пути приходят аргументами с `%20` вместо пробела (ядро делит строку
//! запуска по пробелам); длинный список — файлом `@/tmp/…` (строка запуска
//! не длиннее 255 байт).

use super::*;
use user_progs::statx;

/// Паспорт окна: имя латиницей — для стенда.
const SPEC: Spec = Spec { name: "fileprops", title: "Properties", period_ms: 1_000 };

/// Размер окна в точках макета.
const SIZE: (u32, u32) = (460, 600);

/// Ширина подписей строк («Открывать с помощью»).
const LABEL_W: u32 = 168;

/// Сколько каталогов обходит подсчёт размера за виток цикла.
const WALK_STEP: usize = 24;

/// Объект свойств.
struct Item {
    path: String,
    name: String,
    directory: bool,
    size: u64,
    mtime: u32,
    /// Last access (24.11); shown when it differs from `mtime`.
    atime: u32,
    mode: u32,
    uid: u32,
    gid: u32,
}

/// Диск (точка монтирования из «Этого компьютера»).
struct Drive {
    point: String,
    fs: String,
    label: String,
    total: u64,
    free: u64,
}

/// Подсчёт размера папок в фоне.
#[derive(Default)]
struct Walk {
    stack: Vec<String>,
    bytes: u64,
    files: u64,
    dirs: u64,
    done: bool,
}

/// Что под клавиатурой.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Name,
    Change,
    ReadOnly,
    Hidden,
    Ok,
    Cancel,
    Apply,
}

const FOCUS_ORDER: [Focus; 7] = [Focus::Name, Focus::Change, Focus::ReadOnly, Focus::Hidden, Focus::Ok, Focus::Cancel, Focus::Apply];

/// Строка вкладки «Общие».
enum Row {
    Sep,
    Field(&'static str, String),
    Program,
    Attributes,
    Usage,
}

/// Где что: вкладки, поле имени, «Изменить…», флажки, кнопки, строки.
struct Plan {
    tabs: [Rect; 2],
    head: Rect,
    name: Rect,
    change: Rect,
    readonly: Rect,
    hidden: Rect,
    ok: Rect,
    cancel: Rect,
    apply: Rect,
    rows: Vec<(usize, Rect)>,
}

struct Props {
    items: Vec<Item>,
    drive: Option<Drive>,
    /// 0 — «Общие», 1 — «Подробно».
    tab: u8,
    name: Line,
    /// Атрибуты сейчас (в окне) и как было; `None` — у отмеченных разные.
    readonly: Option<bool>,
    hidden: Option<bool>,
    was_readonly: Option<bool>,
    was_hidden: Option<bool>,
    /// Программа, которой открываются файлы этого типа: подпись и значок.
    program: Option<Prog>,
    chooser: Option<Chooser>,
    walk: Walk,
    focus: Focus,
    note: Option<String>,
    quit: bool,
    icons: IconCache,
    offset: i32,
    pointer: Option<(i32, i32)>,
}

/// Modification and access time of `path` (24.11: `SYS_STATX`; before
/// that — the modification time from the entry of its folder, the only time
/// the kernel gave out).
fn times_of(path: &str) -> (u32, u32) {
    let mut info = user_abi::StatX::default();
    if statx(path, &mut info) < 0 {
        return (0, 0);
    }
    let clip = |seconds: i64| u32::try_from(seconds).unwrap_or(0);
    (clip(info.mtime), clip(info.atime))
}

fn item_of(path: &str) -> Option<Item> {
    let mut info = Stat::default();
    if stat(path, &mut info) < 0 {
        return None;
    }
    let name = if path == "/" { String::from("/") } else { path.rsplit('/').next().unwrap_or(path).to_string() };
    let (mtime, atime) = times_of(path);
    Some(Item {
        path: path.to_string(),
        name,
        directory: info.kind == KIND_DIRECTORY,
        size: info.size,
        mtime,
        atime,
        mode: info.mode,
        uid: info.uid,
        gid: info.gid,
    })
}

/// Окно «Свойства» для путей из аргументов `from..`.
pub fn run(args: &Args, from: usize) -> ! {
    let mut paths: Vec<String> = Vec::new();
    for index in from..args.len() {
        let Some(arg) = args.get(index) else { continue };
        if let Some(file) = arg.strip_prefix('@') {
            if let Some(text) = read_small_limit(file, 256 * 1024) {
                paths.extend(text.lines().filter(|line| line.starts_with('/')).map(String::from));
            }
            remove(file);
        } else if !arg.is_empty() {
            paths.push(unpct(arg));
        }
    }
    let info = app::start(&SPEC);
    let size = app::window_size(&info, SIZE);
    app::run(&SPEC, &info, size, move |_, _| Props::new(paths))
}

impl Props {
    fn new(paths: Vec<String>) -> Self {
        let volumes = list_volumes();
        let drive = match paths.as_slice() {
            [one] => volumes.iter().find(|row| row.name == *one).map(|row| Drive { point: row.name.clone(), fs: row.volume.clone(), label: row.label.clone(), total: row.size, free: row.free }),
            _ => None,
        };
        let items: Vec<Item> = paths.iter().filter_map(|path| item_of(path)).collect();
        let all = |test: &dyn Fn(&Item) -> bool| -> Option<bool> {
            let on = items.iter().filter(|item| test(item)).count();
            if on == items.len() {
                Some(true)
            } else if on == 0 {
                Some(false)
            } else {
                None
            }
        };
        let readonly = all(&|item: &Item| item.mode & 0o200 == 0);
        let hidden = all(&|item: &Item| item.name.starts_with('.'));
        let name = match items.as_slice() {
            [one] => Line::new(&one.name),
            _ => Line::new(""),
        };
        let program = match items.as_slice() {
            [one] if !one.directory && drive.is_none() => {
                // Выбранная (или умолчание); ничего не выбрано — первая
                // подходящая (её и откроет двойной щелчок, если она одна).
                let mut progs = load_progs(&one.path);
                let at = progs.iter().position(|prog| prog.mark == b'd').or_else(|| progs.iter().position(Prog::fits));
                at.map(|at| progs.swap_remove(at))
            }
            _ => None,
        };
        let mut walk = Walk::default();
        for item in items.iter().filter(|_| drive.is_none()) {
            if item.directory {
                walk.stack.push(item.path.clone());
            } else {
                walk.files += 1;
                walk.bytes += item.size;
            }
        }
        walk.done = walk.stack.is_empty();
        let what = match items.as_slice() {
            [one] => format!("'{}'", one.name),
            many => format!("{} item(s)", many.len()),
        };
        println(&format!("files: properties of {what}"));
        let props = Self {
            items,
            drive,
            tab: 0,
            name,
            readonly,
            hidden,
            was_readonly: readonly,
            was_hidden: hidden,
            program,
            chooser: None,
            walk,
            focus: Focus::Ok,
            note: None,
            quit: false,
            icons: IconCache::default(),
            offset: local_offset(),
            pointer: None,
        };
        if let Some(drive) = props.drive.as_ref() {
            println(&format!("fileprops: drive {} {} {} of {} free", drive.point, drive.fs, drive.free, drive.total));
        }
        props
    }

    fn single(&self) -> Option<&Item> {
        match self.items.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// Можно ли править имя и атрибуты.
    fn editable(&self) -> bool {
        self.drive.is_none() && !self.items.is_empty() && self.items.iter().all(|item| item.path != "/")
    }

    fn has_details(&self) -> bool {
        self.drive.is_none() && self.single().is_some()
    }

    /// Что поменяли и ещё не применили.
    fn dirty(&self) -> bool {
        let renamed = self.single().is_some_and(|item| self.name.text.trim() != item.name);
        renamed || self.readonly != self.was_readonly || self.hidden != self.was_hidden
    }

    /// Строки вкладки «Общие».
    fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        if let Some(drive) = self.drive.as_ref() {
            let kind = if drive.point == "/" {
                "Системный диск"
            } else if drive.point.starts_with("/usb") {
                "USB-накопитель"
            } else {
                "Локальный диск"
            };
            out.push(Row::Field("Тип", String::from(kind)));
            out.push(Row::Field("Файловая система", drive.fs.clone()));
            out.push(Row::Field("Путь", drive.point.clone()));
            out.push(Row::Sep);
            if drive.total > 0 {
                let used = drive.total.saturating_sub(drive.free);
                out.push(Row::Field("Занято", format!("{}  ({} байт)", human_size(used), group_digits(used))));
                out.push(Row::Field("Свободно", format!("{}  ({} байт)", human_size(drive.free), group_digits(drive.free))));
                out.push(Row::Sep);
                out.push(Row::Field("Ёмкость", format!("{}  ({} байт)", human_size(drive.total), group_digits(drive.total))));
                out.push(Row::Usage);
            } else {
                out.push(Row::Field("Место", String::from("эта файловая система его не считает")));
            }
            return out;
        }
        match self.items.as_slice() {
            [] => out.push(Row::Field("Ошибка", String::from("объект не найден"))),
            [one] => {
                let ext = sysconf::filetypes::extension(&one.name);
                let kind = file_kind(&one.name, one.directory).1;
                let shown = if one.directory || ext.is_empty() { String::from(kind) } else { format!("{kind} (.{ext})") };
                out.push(Row::Field("Тип", shown));
                if !one.directory {
                    out.push(Row::Program);
                }
                out.push(Row::Sep);
                out.push(Row::Field("Расположение", parent_of(&one.path)));
                out.push(Row::Field("Размер", self.size_text()));
                if one.directory {
                    out.push(Row::Field("Содержит", self.contains_text()));
                }
                out.push(Row::Sep);
                if one.mtime != 0 {
                    out.push(Row::Field("Изменён", long_date(one.mtime, self.offset)));
                }
                if one.atime != 0 && one.atime != one.mtime {
                    out.push(Row::Field("Открыт", long_date(one.atime, self.offset)));
                }
                out.push(Row::Sep);
                out.push(Row::Attributes);
            }
            many => {
                let first = file_kind(&many[0].name, many[0].directory).1;
                let same = many.iter().all(|item| file_kind(&item.name, item.directory).1 == first);
                out.push(Row::Field("Тип", if same { format!("Все типа «{first}»") } else { String::from("Разных типов") }));
                let parent = parent_of(&many[0].path);
                let together = many.iter().all(|item| parent_of(&item.path) == parent);
                out.push(Row::Field("Расположение", if together { format!("Все в {parent}") } else { String::from("В разных папках") }));
                out.push(Row::Sep);
                out.push(Row::Field("Размер", self.size_text()));
                out.push(Row::Field("Содержит", self.contains_text()));
                out.push(Row::Sep);
                out.push(Row::Attributes);
            }
        }
        out
    }

    fn size_text(&self) -> String {
        let walk = &self.walk;
        let tail = if walk.done { "" } else { " — считается…" };
        format!("{} ({} байт){tail}", human_size(walk.bytes), group_digits(walk.bytes))
    }

    fn contains_text(&self) -> String {
        let walk = &self.walk;
        let tail = if walk.done { "" } else { "…" };
        format!("Файлов: {}; папок: {}{tail}", group_digits(walk.files), group_digits(walk.dirs))
    }

    /// Раскладка окна; строки «Общих» — в порядке [`Self::rows`].
    fn plan(&self, ctx: Ctx, area: Rect) -> Plan {
        let pad = ctx.px(16) as i32;
        let face = ctx.face(Role::Body);
        let tab_y = area.y + ctx.px(10) as i32;
        let general = Rect::new(area.x + pad, tab_y, face.width("Общие") + ctx.px(24), ctx.px(32));
        let details = Rect::new(general.right() + ctx.px(4) as i32, tab_y, face.width("Подробно") + ctx.px(24), ctx.px(32));
        let x = area.x + pad;
        let w = area.w.saturating_sub(pad as u32 * 2);
        let head = Rect::new(x, general.bottom() + ctx.px(14) as i32, w, ctx.px(40));
        let name = Rect::new(head.x + ctx.px(48) as i32, head.y + ctx.px(4) as i32, w.saturating_sub(ctx.px(48)), ctx.px(32));
        let face_l = ctx.face(Role::Label);
        let bw = |label: &str| (face_l.width(label) + ctx.px(32)).max(ctx.px(96));
        let by = area.bottom() - pad - ctx.px(32) as i32;
        let apply = Rect::new(area.right() - pad - bw("Применить") as i32, by, bw("Применить"), ctx.px(32));
        let cancel = Rect::new(apply.x - ctx.px(8) as i32 - bw("Отмена") as i32, by, bw("Отмена"), ctx.px(32));
        let ok = Rect::new(cancel.x - ctx.px(8) as i32 - bw("ОК") as i32, by, bw("ОК"), ctx.px(32));
        let mut rows = Vec::new();
        let mut change = Rect::EMPTY;
        let (mut readonly, mut hidden) = (Rect::EMPTY, Rect::EMPTY);
        let mut y = head.bottom() + ctx.px(10) as i32;
        let row_h = ctx.px(30);
        for (index, row) in self.rows().iter().enumerate() {
            let h = match row {
                Row::Sep => ctx.px(14),
                Row::Usage => ctx.px(170),
                Row::Attributes => ctx.px(64),
                _ => row_h,
            };
            let rect = Rect::new(x, y, w, h);
            match row {
                Row::Program => change = Rect::new(rect.right() - bw("Изменить…") as i32, rect.y, bw("Изменить…"), ctx.px(28)),
                Row::Attributes => {
                    let lx = x + ctx.px(LABEL_W) as i32;
                    readonly = Rect::new(lx, rect.y, (rect.right() - lx).max(0) as u32, ctx.px(28));
                    hidden = Rect::new(lx, rect.y + ctx.px(30) as i32, (rect.right() - lx).max(0) as u32, ctx.px(28));
                }
                _ => {}
            }
            rows.push((index, rect));
            y += h as i32;
        }
        Plan { tabs: [general, details], head, name, change, readonly, hidden, ok, cancel, apply, rows }
    }

    /// Применить: имя, «Скрытый» (точка в начале имени), «Только чтение».
    fn apply(&mut self) -> bool {
        if !self.editable() {
            return true;
        }
        let mut ok = true;
        // Имя (один объект).
        if let Some(index) = (self.items.len() == 1).then_some(0) {
            let wanted = self.name.text.trim().to_string();
            let mut new = if wanted.is_empty() || wanted.contains('/') { self.items[index].name.clone() } else { wanted };
            if let Some(hidden) = self.hidden.filter(|_| self.hidden != self.was_hidden) {
                new = if hidden { if new.starts_with('.') { new } else { format!(".{new}") } } else { new.trim_start_matches('.').to_string() };
            }
            ok &= self.rename_item(index, &new);
            self.name = Line::new(&self.items[index].name);
        } else if let Some(hidden) = self.hidden.filter(|_| self.hidden != self.was_hidden) {
            for index in 0..self.items.len() {
                let name = self.items[index].name.clone();
                let new = if hidden { if name.starts_with('.') { name } else { format!(".{name}") } } else { name.trim_start_matches('.').to_string() };
                ok &= self.rename_item(index, &new);
            }
        }
        if let Some(readonly) = self.readonly.filter(|_| self.readonly != self.was_readonly) {
            for item in &mut self.items {
                let mode = if readonly { item.mode & !0o222 } else { item.mode | 0o200 } & 0o7777;
                let code = user_progs::chmod(&item.path, mode);
                if code < 0 {
                    println(&format!("fileprops: chmod '{}' failed: {code}", item.path));
                    self.note = Some(format!("«{}»: {}", item.name, error_text(code)));
                    ok = false;
                } else {
                    item.mode = (item.mode & !0o7777) | mode;
                    println(&format!("fileprops: '{}' read-only {}", item.name, if readonly { "on" } else { "off" }));
                }
            }
        }
        self.was_hidden = self.hidden;
        self.was_readonly = self.readonly;
        if ok {
            self.note = None;
        }
        ok
    }

    /// Переименовать объект `index` в `new` (в той же папке).
    fn rename_item(&mut self, index: usize, new: &str) -> bool {
        let item = &self.items[index];
        if new.is_empty() || new == item.name {
            return true;
        }
        let target = join(&parent_of(&item.path), new);
        if exists(&target) {
            self.note = Some(format!("«{new}» уже есть в этой папке"));
            return false;
        }
        let code = rename(&item.path, &target);
        if code < 0 {
            println(&format!("fileprops: rename of '{}' failed: {code}", item.name));
            self.note = Some(format!("Не удалось переименовать: {}", error_text(code)));
            return false;
        }
        println(&format!("fileprops: renamed '{}' to '{new}'", item.name));
        let item = &mut self.items[index];
        item.path = target;
        item.name = new.to_string();
        true
    }

    /// «Изменить…»: окно выбора программы для этого типа.
    fn open_chooser(&mut self) {
        let Some(item) = self.single().filter(|item| !item.directory) else { return };
        let progs = load_progs(&item.path);
        println(&format!("fileprops: choose the program for '{}' ({} listed)", item.name, progs.len()));
        self.chooser = Some(Chooser { progs, selected: 0, first: 0, always: true });
    }

    /// Выбор в окне программ: запомнить для расширения.
    fn finish_chooser(&mut self) {
        let Some(chooser) = self.chooser.take() else { return };
        let Some(prog) = chooser.progs.into_iter().nth(chooser.selected) else { return };
        let Some(item) = self.single() else { return };
        let ext = sysconf::filetypes::extension(&item.name);
        if ext.is_empty() {
            return;
        }
        if user_progs::open_with_remember(&ext, &prog.command) {
            println(&format!("fileprops: .{ext} files open with '{}'", prog.command));
            self.program = Some(Prog { mark: b'd', ..prog });
        } else {
            self.note = Some(String::from("Не удалось запомнить программу"));
        }
    }

    fn press(&mut self, focus: Focus) -> bool {
        self.focus = focus;
        match focus {
            Focus::Name => {}
            Focus::Change => self.open_chooser(),
            Focus::ReadOnly if self.editable() => self.readonly = Some(!self.readonly.unwrap_or(false)),
            Focus::Hidden if self.editable() => self.hidden = Some(!self.hidden.unwrap_or(false)),
            Focus::ReadOnly | Focus::Hidden => {}
            Focus::Ok => {
                if self.apply() {
                    self.quit = true;
                }
            }
            Focus::Cancel => self.quit = true,
            Focus::Apply => {
                self.apply();
            }
        }
        true
    }

    /// Следующая доступная цель Tab.
    fn next_focus(&self, back: bool) -> Focus {
        let usable = |focus: Focus| match focus {
            Focus::Name => self.editable() && self.single().is_some() && self.tab == 0,
            Focus::Change => self.tab == 0 && self.single().is_some_and(|item| !item.directory) && self.drive.is_none(),
            Focus::ReadOnly | Focus::Hidden => self.tab == 0 && self.editable(),
            _ => true,
        };
        let at = FOCUS_ORDER.iter().position(|focus| *focus == self.focus).unwrap_or(0);
        for step in 1..=FOCUS_ORDER.len() {
            let index = if back { (at + FOCUS_ORDER.len() * 2 - step) % FOCUS_ORDER.len() } else { (at + step) % FOCUS_ORDER.len() };
            if usable(FOCUS_ORDER[index]) {
                return FOCUS_ORDER[index];
            }
        }
        Focus::Ok
    }

    fn draw_general(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        // Шапка: значок и имя.
        let side = ctx.px(36);
        let icon = Rect::new(plan.head.x, plan.head.y + (plan.head.h as i32 - side as i32) / 2, side, side);
        let art = match (&self.drive, self.items.as_slice()) {
            (Some(drive), _) => {
                if drive.point == "/" {
                    "place-system-disk"
                } else if drive.point.starts_with("/usb") {
                    "place-usb-stick"
                } else {
                    "place-local-disk"
                }
            }
            (None, [one]) => file_kind(&one.name, one.directory).0,
            (None, _) => "doc-unknown",
        };
        draw_design_icon(&self.icons, s, icon, art);
        let name_ink = p.ink;
        if let Some(drive) = self.drive.as_ref() {
            let title = if drive.label.is_empty() { volume_title(&drive.point, "") } else { format!("Метка: {}", drive.label) };
            paint::text_clipped(ctx, s, Role::Strong, plan.name.x, paint::baseline(ctx, Role::Strong, plan.name), plan.name.w, &title, name_ink);
        } else if self.single().is_some() && self.editable() {
            paint::sunk(ctx, s, plan.name, ctx.px(theme::R_ROW));
            let focused = self.focus == Focus::Name;
            if focused {
                draw::rounded_stroke(s, plan.name, ctx.px(theme::R_ROW), p.acc, 255);
            }
            let text = Rect::new(plan.name.x + ctx.px(8) as i32, plan.name.y, plan.name.w.saturating_sub(ctx.px(16)), plan.name.h);
            self.name.draw(ctx, s, text, focused, name_ink);
        } else {
            let title = match self.items.as_slice() {
                [one] => one.name.clone(),
                many => {
                    let dirs = many.iter().filter(|item| item.directory).count();
                    format!("Объектов: {} (файлов {}, папок {})", many.len(), many.len() - dirs, dirs)
                }
            };
            paint::text_clipped(ctx, s, Role::Strong, plan.name.x, paint::baseline(ctx, Role::Strong, plan.name), plan.name.w, &title, name_ink);
        }
        let label_w = ctx.px(LABEL_W);
        let rows = self.rows();
        for (index, rect) in &plan.rows {
            let Some(row) = rows.get(*index) else { continue };
            if rect.bottom() > plan.ok.y - ctx.px(8) as i32 {
                break;
            }
            let value_x = rect.x + label_w as i32;
            let value_w = rect.w.saturating_sub(label_w);
            match row {
                Row::Sep => paint::separator(ctx, s, rect.x, rect.y + rect.h as i32 / 2, rect.w),
                Row::Field(label, value) => {
                    paint::text_clipped(ctx, s, Role::Body, rect.x, paint::baseline(ctx, Role::Body, *rect), label_w, label, p.ink3);
                    paint::text_clipped(ctx, s, Role::Body, value_x, paint::baseline(ctx, Role::Body, *rect), value_w, value, p.ink);
                }
                Row::Program => {
                    paint::text_clipped(ctx, s, Role::Body, rect.x, paint::baseline(ctx, Role::Body, *rect), label_w, "Открывать с помощью", p.ink3);
                    let room = (plan.change.x - value_x - ctx.px(8) as i32).max(0) as u32;
                    match self.program.as_ref() {
                        Some(prog) => {
                            let side = ctx.px(20);
                            let icon = Rect::new(value_x, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
                            draw_prog_icon(ctx, s, icon, prog);
                            let tx = icon.right() + ctx.px(8) as i32;
                            paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, *rect), room.saturating_sub(side + ctx.px(8)), &prog.caption, p.ink);
                        }
                        None => {
                            paint::text_clipped(ctx, s, Role::Body, value_x, paint::baseline(ctx, Role::Body, *rect), room, "не выбрано", p.ink4);
                        }
                    }
                    paint::button(ctx, s, plan.change, Weight::Normal, "Изменить…", false);
                    if self.focus == Focus::Change {
                        draw::rounded_stroke(s, plan.change, ctx.px(theme::R_ROW), p.acc, 255);
                    }
                }
                Row::Attributes => {
                    paint::text_clipped(ctx, s, Role::Body, rect.x, paint::baseline(ctx, Role::Body, plan.readonly), label_w, "Атрибуты", p.ink3);
                    for (check, value, caption, focus) in [(plan.readonly, self.readonly, "Только чтение", Focus::ReadOnly), (plan.hidden, self.hidden, "Скрытый", Focus::Hidden)] {
                        let side = ctx.px(16);
                        let bx = Rect::new(check.x, check.y + (check.h as i32 - side as i32) / 2, side, side);
                        match value {
                            Some(true) => {
                                draw::rounded(s, bx, ctx.px(3), p.acc, 255);
                                glyphicon::draw(s, Icon::Check, bx.x, bx.y, bx.w, p.acc_ink, 255);
                            }
                            // У отмеченных по-разному — чёрточка, как в Windows.
                            None => {
                                draw::rounded_stroke(s, bx, ctx.px(3), p.ink4, 255);
                                s.fill(Rect::new(bx.x + ctx.px(4) as i32, bx.y + side as i32 / 2 - 1, side.saturating_sub(ctx.px(8)), ctx.px(2)), p.acc);
                            }
                            Some(false) => draw::rounded_stroke(s, bx, ctx.px(3), p.ink4, 255),
                        }
                        let ink = if self.editable() { p.ink } else { p.ink4 };
                        paint::text_clipped(ctx, s, Role::Body, bx.right() + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, check), check.w.saturating_sub(side + ctx.px(10)), caption, ink);
                        if self.focus == focus {
                            draw::rounded_stroke(s, Rect::new(check.x - ctx.px(4) as i32, check.y, (ctx.face(Role::Body).width(caption) + side + ctx.px(18)).min(check.w), check.h), ctx.px(4), p.acc, 255);
                        }
                    }
                }
                Row::Usage => self.draw_usage(s, ctx, *rect),
            }
        }
    }

    /// Круг «занято / свободно» диска с подписями.
    fn draw_usage(&self, s: &mut Surface, ctx: Ctx, rect: Rect) {
        let Some(drive) = self.drive.as_ref().filter(|drive| drive.total > 0) else { return };
        let p = ctx.palette;
        let used = drive.total.saturating_sub(drive.free);
        let fraction = used as f32 / drive.total as f32;
        let low = drive.free.saturating_mul(10) < drive.total;
        let used_ink = if low { p.bad } else { p.acc };
        let free_ink = p.ink5;
        let r = (rect.h.min(rect.w / 2) as i32 / 2 - ctx.px(10) as i32).max(8);
        let cx = rect.x + r + ctx.px(20) as i32;
        let cy = rect.y + rect.h as i32 / 2;
        pie(s, cx, cy, r, fraction, used_ink, free_ink);
        draw::circle_outline(s, cx, cy, r as u32, 1, p.ink4, 255);
        // Подписи справа от круга.
        let lx = cx + r + ctx.px(28) as i32;
        let room = (rect.right() - lx).max(0) as u32;
        let mut y = cy - ctx.px(30) as i32;
        for (ink, label, bytes) in [(used_ink, "Занято", used), (free_ink, "Свободно", drive.free)] {
            let side = ctx.px(12);
            s.fill(Rect::new(lx, y + ctx.px(4) as i32, side, side), ink);
            draw::rounded_stroke(s, Rect::new(lx, y + ctx.px(4) as i32, side, side), 0, p.line.color, p.line.alpha);
            paint::text_clipped(ctx, s, Role::Body, lx + ctx.px(20) as i32, y, room.saturating_sub(ctx.px(20)), &format!("{label}: {}", human_size(bytes)), p.ink2);
            y += ctx.px(30) as i32;
        }
        let percent = (used * 100 / drive.total.max(1)).min(100);
        paint::text_clipped(ctx, s, Role::Caption, lx, y, room, &format!("Занято {percent} %{}", if low { " — места почти нет" } else { "" }), if low { p.bad_ink } else { p.ink4 });
    }

    fn draw_details(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let Some(item) = self.single() else { return };
        let x = plan.head.x;
        let mut y = plan.head.y;
        let label_w = ctx.px(LABEL_W) as i32;
        let step = i32::from(ctx.face(Role::Body).line) + ctx.px(12) as i32;
        paint::text(ctx, s, Role::Strong, x, y, "Владелец и права доступа", p.ink2);
        y += step + ctx.px(4) as i32;
        for (label, value) in [
            ("Владелец", user_name(item.uid)),
            ("Группа", group_name(item.gid)),
            ("Права", format!("{}  ({:04o})", rwx(item.mode, item.directory), item.mode & 0o7777)),
            ("Путь Windows", sysconf::winpath::to_windows(&item.path)),
        ] {
            paint::text_clipped(ctx, s, Role::Body, x, y, label_w as u32, label, p.ink3);
            paint::text_clipped(ctx, s, Role::Body, x + label_w, y, plan.head.w.saturating_sub(label_w as u32), &value, p.ink);
            y += step;
        }
        y += ctx.px(8) as i32;
        let col_w = ctx.px(86) as i32;
        for (index, head) in ["Чтение", "Запись", "Запуск"].iter().enumerate() {
            paint::text(ctx, s, Role::Caption, x + label_w + index as i32 * col_w, y, head, p.ink4);
        }
        y += step;
        for (row, who) in ["Владелец", "Группа", "Остальные"].iter().enumerate() {
            paint::text(ctx, s, Role::Body, x, y, who, p.ink2);
            for column in 0..3 {
                let bit = 1u32 << (8 - (row * 3 + column));
                draw_check(ctx, s, Rect::new(x + label_w + column as i32 * col_w, y, ctx.px(16), u32::from(ctx.face(Role::Body).line)), item.mode & bit != 0);
            }
            y += step;
        }
    }
}

/// Сектор круга: доля `fraction` от 12 часов по часовой стрелке — `used`,
/// остальное — `free`.
fn pie(s: &mut Surface, cx: i32, cy: i32, r: i32, fraction: f32, used: mini_ui::Color, free: mini_ui::Color) {
    let abs = |v: f32| if v < 0.0 { -v } else { v };
    // atan2 приближением (до тысячной радиана — для круга довольно).
    let atan2 = |y: f32, x: f32| -> f32 {
        let (ax, ay) = (abs(x), abs(y));
        if ax == 0.0 && ay == 0.0 {
            return 0.0;
        }
        let a = if ax > ay { ay / ax } else { ax / ay };
        let q = a * a;
        let mut t = ((-0.046_496_47 * q + 0.159_314_22) * q - 0.327_622_76) * q * a + a;
        if ay > ax {
            t = core::f32::consts::FRAC_PI_2 - t;
        }
        if x < 0.0 {
            t = core::f32::consts::PI - t;
        }
        if y < 0.0 { -t } else { t }
    };
    let r2 = r * r;
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy > r2 {
                continue;
            }
            // Угол от верха по часовой стрелке, в долях круга.
            let mut turn = atan2(dx as f32, -dy as f32) / core::f32::consts::TAU;
            if turn < 0.0 {
                turn += 1.0;
            }
            let ink = if turn < fraction { used } else { free };
            draw::blend_pixel(s, cx + dx, cy + dy, ink, 255);
        }
    }
}

impl App for Props {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let p = ctx.palette;
        let plan = self.plan(ctx, area);
        for (index, (rect, label)) in plan.tabs.iter().zip(["Общие", "Подробно"]).enumerate() {
            if index == 1 && !self.has_details() {
                continue;
            }
            let active = self.tab as usize == index;
            paint::text(ctx, s, Role::Body, rect.x + ctx.px(12) as i32, paint::baseline(ctx, Role::Body, *rect), label, if active { p.ink } else { p.ink3 });
            if active {
                s.fill(Rect::new(rect.x + ctx.px(8) as i32, rect.bottom() - ctx.px(2) as i32, rect.w - ctx.px(16), ctx.px(2)), p.acc);
            }
        }
        draw::hline(s, plan.head.x, plan.tabs[0].bottom(), plan.head.w, p.line.color, p.line.alpha);
        if self.tab == 0 {
            self.draw_general(s, ctx, &plan);
        } else {
            self.draw_details(s, ctx, &plan);
        }
        if let Some(note) = self.note.as_ref() {
            paint::text_clipped(ctx, s, Role::Caption, plan.head.x, plan.ok.y - ctx.px(22) as i32, plan.head.w, note, p.bad_ink);
        }
        paint::separator(ctx, s, area.x, plan.ok.y - ctx.px(10) as i32, area.w);
        for (rect, label, weight, focus) in [
            (plan.ok, "ОК", Weight::Primary, Focus::Ok),
            (plan.cancel, "Отмена", Weight::Normal, Focus::Cancel),
            (plan.apply, "Применить", if self.dirty() { Weight::Normal } else { Weight::Ghost }, Focus::Apply),
        ] {
            paint::button(ctx, s, rect, weight, label, self.pointer.is_some_and(|(x, y)| rect.contains(x, y)));
            if self.focus == focus {
                draw::rounded_stroke(s, Rect::new(rect.x - 2, rect.y - 2, rect.w + 4, rect.h + 4), ctx.px(6), p.acc, 255);
            }
        }
        if let (Some(chooser), Some(item)) = (self.chooser.as_ref(), self.single()) {
            draw_chooser_card(ctx, s, area, chooser, &item.name, true);
        }
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        if matches!(code, user_abi::WIN_KEY_SHIFT | user_abi::WIN_KEY_CTRL | user_abi::WIN_KEY_ALT) {
            return false;
        }
        // Окно выбора программы.
        if let Some(chooser) = self.chooser.as_mut() {
            match code {
                WIN_KEY_UP => {
                    chooser.selected = chooser.selected.saturating_sub(1);
                    chooser.follow();
                }
                WIN_KEY_DOWN => {
                    if chooser.selected + 1 < chooser.progs.len() {
                        chooser.selected += 1;
                    }
                    chooser.follow();
                }
                ENTER => self.finish_chooser(),
                ESCAPE => self.chooser = None,
                _ => {}
            }
            return true;
        }
        // Ctrl+Tab — соседняя вкладка.
        if code == 0x09 && mods & WIN_MOD_CTRL != 0 {
            if self.has_details() {
                self.tab = 1 - self.tab.min(1);
            }
            return true;
        }
        match code {
            0x09 => {
                self.focus = self.next_focus(mods & WIN_MOD_SHIFT != 0);
                return true;
            }
            ESCAPE => {
                self.quit = true;
                return true;
            }
            ENTER => return self.press(if matches!(self.focus, Focus::Cancel | Focus::Apply | Focus::Change) { self.focus } else { Focus::Ok }),
            _ => {}
        }
        if self.focus == Focus::Name && self.tab == 0 {
            if code == 0x2F {
                return true;
            }
            return self.name.key(code, mods, latin).is_some();
        }
        match code {
            WIN_KEY_LEFT | WIN_KEY_RIGHT if self.has_details() => {
                self.tab = 1 - self.tab.min(1);
                true
            }
            0x20 => self.press(self.focus),
            _ => false,
        }
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if let Some(chooser) = self.chooser.as_mut() {
            let cp = chooser_plan(ctx, area, chooser.progs.len());
            if cp.ok.contains(x, y) {
                self.finish_chooser();
            } else if cp.cancel.contains(x, y) {
                self.chooser = None;
            } else if let Some(at) = cp.rows.iter().position(|row| row.contains(x, y)) {
                let index = chooser.first + at;
                if index < chooser.progs.len() {
                    if index == chooser.selected {
                        self.finish_chooser();
                    } else {
                        chooser.selected = index;
                    }
                }
            }
            return true;
        }
        let plan = self.plan(ctx, area);
        if let Some(index) = plan.tabs.iter().position(|tab| tab.contains(x, y)) {
            if index == 0 || self.has_details() {
                self.tab = index as u8;
            }
            return true;
        }
        for (rect, focus) in [(plan.ok, Focus::Ok), (plan.cancel, Focus::Cancel), (plan.apply, Focus::Apply)] {
            if rect.contains(x, y) {
                return self.press(focus);
            }
        }
        if self.tab != 0 {
            return false;
        }
        if plan.name.contains(x, y) && self.editable() && self.single().is_some() {
            self.focus = Focus::Name;
            let text = Rect::new(plan.name.x + ctx.px(8) as i32, plan.name.y, plan.name.w.saturating_sub(ctx.px(16)), plan.name.h);
            self.name.click(ctx.face(Role::Body), text.w, x - text.x);
            return true;
        }
        for (rect, focus) in [(plan.change, Focus::Change), (plan.readonly, Focus::ReadOnly), (plan.hidden, Focus::Hidden)] {
            if rect.contains(x, y) {
                return self.press(focus);
            }
        }
        false
    }

    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, _y: i32) -> bool {
        if self.focus != Focus::Name || !self.name.held() {
            return false;
        }
        let plan = self.plan(ctx, area);
        let text = Rect::new(plan.name.x + ctx.px(8) as i32, plan.name.y, plan.name.w.saturating_sub(ctx.px(16)), plan.name.h);
        self.name.drag(ctx.face(Role::Body), text.w, x - text.x)
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        self.name.release();
        false
    }

    fn wheel_at(&mut self, _area: Rect, _ctx: Ctx, clicks: i32) -> bool {
        let Some(chooser) = self.chooser.as_mut() else { return false };
        let last = chooser.progs.len().saturating_sub(CHOOSER_ROWS);
        let step = clicks.unsigned_abs() as usize;
        chooser.first = if clicks > 0 { chooser.first.saturating_sub(step) } else { (chooser.first + step).min(last) };
        true
    }

    fn hover(&mut self, _area: Rect, _ctx: Ctx, x: i32, y: i32) -> bool {
        self.pointer = Some((x, y));
        true
    }

    fn leave(&mut self) -> bool {
        self.pointer = None;
        true
    }

    /// Размер папок — кусками.
    fn step(&mut self) -> bool {
        if self.walk.done {
            return false;
        }
        for _ in 0..WALK_STEP {
            let Some(dir) = self.walk.stack.pop() else { break };
            self.walk.dirs += u64::from(!self.items.iter().any(|item| item.path == dir));
            for (name, directory, size) in dir_entries(&dir).unwrap_or_default() {
                if directory {
                    self.walk.stack.push(join(&dir, &name));
                } else {
                    self.walk.files += 1;
                    self.walk.bytes += size;
                }
            }
        }
        if self.walk.stack.is_empty() {
            self.walk.done = true;
            println(&format!("fileprops: size {} byte(s), {} file(s), {} folder(s)", self.walk.bytes, self.walk.files, self.walk.dirs));
        }
        true
    }

    fn title(&self) -> Option<String> {
        let what = match (&self.drive, self.items.as_slice()) {
            (Some(drive), _) => volume_title(&drive.point, &drive.label),
            (None, [one]) => one.name.clone(),
            (None, many) => format!("объектов {}", many.len()),
        };
        Some(format!("Properties\x1fСвойства: {what}"))
    }

    fn wants_quit(&self) -> bool {
        self.quit
    }

    fn quits_on_q(&self) -> bool {
        false
    }

    fn resizable(&self) -> bool {
        false
    }

    fn window_style(&self) -> u32 {
        user_abi::WIN_STYLE_DIALOG
    }
}
