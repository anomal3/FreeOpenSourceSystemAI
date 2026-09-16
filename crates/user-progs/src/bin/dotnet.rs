//! `/bin/dotnet` — своя среда .NET (веха v0.7c, фаза N2).
//!
//! `dotnet /usr/share/dotnet/samples/hello.dll` запускает сборку, собранную
//! обычным `dotnet build` на Windows, без переделки. Программа — обёртка:
//! прочитать файл, отдать его интерпретатору (`clr-vm`), напечатать итог. Всё
//! исполнение в крейте, и тот же код в `cargo xtask clr-check` сравнивается с
//! настоящим `dotnet` на тех же сборках.
//!
//! # Что пишет в журнал
//!
//! Дескриптор 2: `dotnet: <файл>: N bytes, T type(s), M method(s)` при загрузке
//! и `dotnet: <файл>: Main returned C after K instruction(s), O object(s)` по
//! завершении — по этим строкам стенд и проверяет, что программа не просто
//! что-то напечатала, а дошла до конца. Отказ среды — `dotnet: error: …` с
//! полным именем того, чего не хватило.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use clr_vm::{FileKind, GlyphBitmap, Host, IoError, Vm, WindowEvent, WindowPixels, WindowRect};
use mini_ui::typeface::{self, Face, Role};
use mini_ui::{Color, Rect, Surface};
use user_abi::{CLOCK_MONOTONIC, CLOCK_REALTIME};
use user_progs::{
    Args, Dirent, ERR_AGAIN, KIND_DIRECTORY, SEEK_END, SYSINFO_DARK, WIN_CLOSE, WIN_KEY, WIN_LEAVE, WIN_MOVE, WIN_POINTER, Window, clock, close,
    error, exit, file_size, heap_size, mkdir, monotonic_ms, nanosleep, open, open_write, print, read, readdir_raw, remove,
    rename, seek, sleep_ms, stat, sysinfo, uid, write,
};

/// Сколько ждать графики (фаза N6a) — как `user_progs::app`: стол поднимает ядро,
/// и программа, запущенная при загрузке, может его опередить.
const WAIT_GRAPHICS_MS: u64 = 10_000;

/// Сколько ждать окна у занятого стола — как `user_progs::app`.
const OPEN_WAIT_MS: u64 = 30_000;

/// Пауза между попытками.
const POLL_NS: u32 = 30_000_000;

/// Окно формы WinForms: окно ядра и поверхность поверх его страниц.
struct ProgramWindow {
    window: Window,
    surface: Surface,
    width: u32,
    height: u32,
    title: String,
    frames: u64,
}

/// Куча программы. Сборка, разобранные методы, кадры и все объекты программы
/// живут в ней; мегабайта, который достаётся остальным программам, не хватает.
const HEAP_BYTES: usize = 16 * 1024 * 1024;

/// Базовая библиотека своей среды: на неё разрешаются ссылки программы на
/// `System.Runtime` и `System.Console` (фаза N3a).
const CORELIB: &str = "/usr/share/dotnet/FreeOs.CoreLib.dll";

/// Самая большая сборка, которую программа читает.
const FILE_MAX: usize = 4 * 1024 * 1024;

/// Код выхода, когда среда не довела программу до конца.
///
/// 134 — «прервано», как у процесса, получившего `SIGABRT`: ноль и единица
/// заняты кодами, которые программа вправе вернуть сама.
const RUNTIME_FAILED: i64 = 134;

/// Стандартный вывод программы — терминал, файлы — файловая система FreeOS
/// (фаза N5a), часы — часы ядра (фаза N5b).
///
/// Текущего каталога у задачи FreeOS нет, поэтому у программы он свой:
/// домашний каталог пользователя (см. [`home_directory`]). От него считаются
/// относительные пути, и `File.WriteAllText("notes.txt", …)` пишет туда же, где
/// пользователь хранит свои файлы.
struct Console {
    home: String,
    /// Смещение местного времени из строки `timezone=` настроек, минуты.
    offset_minutes: i32,
    /// Окна форм по номерам, которые видит C#; закрытое — `None`.
    windows: Vec<Option<ProgramWindow>>,
    /// Шрифт форм, когда графика уже приготовлена.
    face: Option<&'static Face>,
    /// Имя программы без `.dll` — заголовок окна, которому C# не дал своего.
    program: String,
}

impl Console {
    /// Приготовить рисование при первом окне: формат точки, тема и шрифт.
    /// `None` — графики у машины нет.
    fn graphics(&mut self) -> Option<&'static Face> {
        if let Some(face) = self.face {
            return Some(face);
        }
        let deadline = monotonic_ms() + WAIT_GRAPHICS_MS;
        let info = loop {
            match sysinfo() {
                Some(info) if info.pixel_format != 0 && info.screen_w != 0 => break info,
                Some(_) if monotonic_ms() < deadline => {
                    nanosleep(0, POLL_NS);
                }
                _ => return None,
            }
        };
        mini_ui::use_raw_format(info.pixel_format);
        mini_ui::theme::set_dark(info.flags & SYSINFO_DARK != 0);
        let tier = mini_ui::paint::Ctx::scaled(mini_ui::theme::geometry_scale(
            info.screen_w,
            info.screen_h,
        ))
        .tier;
        let face = typeface::face(Role::Body, tier);
        self.face = Some(face);
        Some(face)
    }

    fn window_mut(&mut self, window: u32) -> Option<&mut ProgramWindow> {
        self.windows.get_mut(window as usize).and_then(Option::as_mut)
    }
}

impl Host for Console {
    fn write_out(&mut self, text: &str) {
        print(text);
    }

    fn current_dir(&mut self) -> String {
        self.home.clone()
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, IoError> {
        if self.stat(path)?.0 == FileKind::Directory {
            return Err(IoError::WrongKind);
        }
        let fd = open(path);
        if fd < 0 {
            return Err(io_error(fd));
        }
        let size = file_size(fd);
        if size < 0 {
            close(fd);
            return Err(io_error(size));
        }
        let mut data = Vec::new();
        if data.try_reserve_exact(size as usize).is_err() {
            close(fd);
            return Err(IoError::Other);
        }
        data.resize(size as usize, 0);
        let mut filled = 0;
        while filled < data.len() {
            let got = read(fd, &mut data[filled..]);
            if got < 0 {
                close(fd);
                return Err(io_error(got));
            }
            if got == 0 {
                break;
            }
            filled += got as usize;
        }
        close(fd);
        data.truncate(filled);
        Ok(data)
    }

    fn write_file(&mut self, path: &str, data: &[u8], append: bool) -> Result<(), IoError> {
        if let Ok((FileKind::Directory, _)) = self.stat(path) {
            return Err(IoError::WrongKind);
        }
        let fd = open_write(path, true, !append);
        if fd < 0 {
            return Err(io_error(fd));
        }
        if append {
            let end = seek(fd, 0, SEEK_END);
            if end < 0 {
                close(fd);
                return Err(io_error(end));
            }
        }
        let mut written = 0;
        while written < data.len() {
            let count = write(fd, &data[written..]);
            if count <= 0 {
                close(fd);
                return Err(if count < 0 { io_error(count) } else { IoError::NoSpace });
            }
            written += count as usize;
        }
        let closed = close(fd);
        if closed < 0 { Err(io_error(closed)) } else { Ok(()) }
    }

    fn remove_file(&mut self, path: &str) -> Result<(), IoError> {
        if self.stat(path)?.0 == FileKind::Directory {
            return Err(IoError::WrongKind);
        }
        code(remove(path))
    }

    fn create_dir(&mut self, path: &str) -> Result<(), IoError> {
        code(mkdir(path, 0o755))
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), IoError> {
        if self.stat(path)?.0 == FileKind::File {
            return Err(IoError::WrongKind);
        }
        code(remove(path))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), IoError> {
        code(rename(from, to))
    }

    fn stat(&mut self, path: &str) -> Result<(FileKind, u64), IoError> {
        let mut info = user_abi::Stat::default();
        code(stat(path, &mut info))?;
        Ok((if info.kind == KIND_DIRECTORY { FileKind::Directory } else { FileKind::File }, info.size))
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, IoError> {
        let fd = open(path);
        if fd < 0 {
            return Err(io_error(fd));
        }
        let mut names = Vec::new();
        loop {
            let mut entry = Dirent::default();
            let got = readdir_raw(fd, &mut entry);
            if got < 0 {
                close(fd);
                return Err(io_error(got));
            }
            if got == 0 {
                break;
            }
            let length = (entry.name_len as usize).min(entry.name.len());
            let name = String::from_utf8_lossy(&entry.name[..length]).into_owned();
            if name != "." && name != ".." {
                names.push(name);
            }
        }
        close(fd);
        Ok(names)
    }

    fn utc_now(&mut self) -> i64 {
        clock(CLOCK_REALTIME).map_or(0, |time| time.seconds as i64 * 10_000_000 + i64::from(time.nanos / 100))
    }

    fn local_offset_minutes(&mut self) -> i32 {
        self.offset_minutes
    }

    fn monotonic_nanos(&mut self) -> u64 {
        clock(CLOCK_MONOTONIC).map_or(0, |time| time.seconds * 1_000_000_000 + u64::from(time.nanos))
    }

    fn sleep(&mut self, milliseconds: u32) {
        sleep_ms(u64::from(milliseconds));
    }

    fn processor_count(&mut self) -> u32 {
        sysinfo().map_or(1, |info| info.cpus.max(1))
    }

    fn window_open(&mut self, title: &str, width: u32, height: u32) -> Option<u32> {
        self.graphics()?;
        // Пустой заголовок ядро не принимает: у кнопки на панели задач должно быть
        // имя. Окно без заголовка — например, окно сообщения — называется
        // именем программы.
        let program = self.program.clone();
        let title = if title.is_empty() { program.as_str() } else { title };
        let deadline = monotonic_ms() + OPEN_WAIT_MS;
        let mut window = loop {
            match Window::open(title, width, height) {
                Ok(window) => break window,
                Err(code) if code != ERR_AGAIN || monotonic_ms() >= deadline => {
                    error(&format!("dotnet: window '{title}' was refused (code {code})\n"));
                    return None;
                }
                Err(_) => {
                    nanosleep(0, POLL_NS);
                }
            }
        };
        let base = window.pixels().as_mut_ptr();
        // SAFETY: страницы окна отображены ядром, пока живо `window`, а
        // поверхность лежит в той же структуре и умирает вместе с ним.
        let Some(surface) = (unsafe { Surface::from_raw(base, width, height) }) else {
            window.close();
            return None;
        };
        error(&format!("dotnet: window '{title}' opened, {width}x{height}\n"));
        self.windows.push(Some(ProgramWindow { window, surface, width, height, title: String::from(title), frames: 0 }));
        u32::try_from(self.windows.len() - 1).ok()
    }

    // Прозрачность не смешивается: форма N6a рисует непрозрачными кистями.
    fn window_fill(&mut self, window: u32, area: WindowRect, argb: u32) {
        let Some(target) = self.window_mut(window) else { return };
        let color = Color::rgb((argb >> 16) as u8, (argb >> 8) as u8, argb as u8);
        target.surface.fill(Rect::new(area.x, area.y, area.width, area.height), color);
    }

    // Отсечения у шрифта нет, поэтому оно честное, но снаружи: точки строки
    // вне элемента запоминаются до рисования и возвращаются после (фаза N7a —
    // текст, уехавший за край поля ввода, не ложится на соседей).
    fn window_text(&mut self, window: u32, x: i32, y: i32, text: &str, argb: u32, clip: WindowRect) {
        let Some(face) = self.graphics() else { return };
        let Some(target) = self.window_mut(window) else { return };
        let width = face.width(text) as i32;
        let height = i32::from(face.line);
        let clip_right = clip.x + clip.width as i32;
        let clip_bottom = clip.y + clip.height as i32;
        if x >= clip_right || y >= clip_bottom || x + width <= clip.x || y + height <= clip.y {
            return;
        }
        let mut saved = Vec::new();
        for py in y.max(0)..(y + height).min(target.height as i32) {
            for px in x.max(0)..(x + width).min(target.width as i32) {
                if px < clip.x || px >= clip_right || py < clip.y || py >= clip_bottom {
                    saved.push((px as u32, py as u32, target.surface.get(px as u32, py as u32)));
                }
            }
        }
        let color = Color::rgb((argb >> 16) as u8, (argb >> 8) as u8, argb as u8);
        typeface::draw(&mut target.surface, face, x, y, text, color, 255);
        for (px, py, pixel) in saved {
            target.surface.put(px, py, pixel);
        }
    }

    // Глиф шрифта форм четырьмя битами на точку (фаза N9c) — развёрнутый в
    // байты: растеризатору `System.Drawing` нужна маска, а не рисование в окно.
    fn glyph(&mut self, ch: char) -> Option<GlyphBitmap> {
        let face = self.graphics()?;
        let glyph = face.glyph(ch)?;
        let (width, height) = (u32::from(glyph.w), u32::from(glyph.h));
        let stride = usize::from(glyph.w).div_ceil(2);
        let mut coverage = Vec::new();
        coverage.try_reserve_exact(width as usize * height as usize).ok()?;
        for row in 0..usize::from(glyph.h) {
            for column in 0..usize::from(glyph.w) {
                let byte = typeface::data::COVERAGE.get(glyph.off as usize + row * stride + column / 2).copied().unwrap_or(0);
                let level = if column % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                coverage.push(level * 17);
            }
        }
        Some(GlyphBitmap {
            advance: u32::from(glyph.adv),
            left: i32::from(glyph.left),
            top: i32::from(glyph.top),
            width,
            height,
            coverage,
        })
    }

    fn text_width(&mut self, text: &str) -> u32 {
        self.graphics().map_or(text.chars().count() as u32 * 8, |face| face.width(text))
    }

    fn text_height(&mut self) -> u32 {
        self.graphics().map_or(16, |face| u32::from(face.line))
    }

    fn window_present(&mut self, window: u32) {
        let Some(target) = self.window_mut(window) else { return };
        // Отказ занятого стола — не сбой: следующая перерисовка покажет то же.
        if target.window.commit() >= 0 {
            target.frames += 1;
        }
    }

    fn window_event(&mut self, window: u32) -> Option<WindowEvent> {
        let event = self.window_mut(window)?.window.next_event()?;
        match event.kind {
            WIN_KEY => Some(WindowEvent::Key { symbol: event.code, mods: event.x as u32, latin: event.y as u32 }),
            WIN_POINTER => Some(WindowEvent::Pointer { x: event.x, y: event.y, buttons: event.code }),
            WIN_MOVE => Some(WindowEvent::Move { x: event.x, y: event.y, buttons: event.code }),
            WIN_LEAVE => Some(WindowEvent::Leave),
            WIN_CLOSE => Some(WindowEvent::Close),
            _ => None,
        }
    }

    fn window_close(&mut self, window: u32) {
        let Some(slot) = self.windows.get_mut(window as usize) else { return };
        if let Some(ProgramWindow { window, surface, title, frames, .. }) = slot.take() {
            drop(surface);
            window.close();
            error(&format!("dotnet: window '{title}' closed after {frames} frame(s)\n"));
        }
    }

    // Точки окна для `System.Drawing` (фаза N9). Порядок байтов — тот, что
    // ядро назвало при первом окне (`graphics`); у окна без известного формата
    // точек не даём вовсе: растеризатор перепутал бы красный с синим.
    fn window_pixels(&mut self, window: u32) -> Option<WindowPixels<'_>> {
        let red_low = match mini_ui::format_code() {
            mini_ui::PIXEL_RGB => true,
            mini_ui::PIXEL_BGR => false,
            _ => return None,
        };
        let target = self.window_mut(window)?;
        let (width, height) = (target.width, target.height);
        Some(WindowPixels { pixels: target.window.pixels(), width, height, red_low })
    }

    fn window_resize(&mut self, window: u32, width: u32, height: u32) -> bool {
        let Some(target) = self.window_mut(window) else { return false };
        if target.width == width && target.height == height {
            return true;
        }
        if let Err(code) = target.window.resize(width, height) {
            error(&format!("dotnet: window '{}' kept its size, the resize was refused (code {code})\n", target.title));
            return false;
        }
        let base = target.window.pixels().as_mut_ptr();
        // SAFETY: как при открытии — страницы новой поверхности отображены ядром,
        // пока живо окно; прежняя поверхность заменяется, не будучи прочитана.
        let Some(surface) = (unsafe { Surface::from_raw(base, width, height) }) else { return false };
        target.surface = surface;
        target.width = width;
        target.height = height;
        error(&format!("dotnet: window '{}' resized to {width}x{height}\n", target.title));
        true
    }
}

/// Отказ ядра как отказ файловой операции среды.
fn io_error(code: i64) -> IoError {
    match code {
        user_abi::ERR_NOT_FOUND => IoError::NotFound,
        user_abi::ERR_EXISTS => IoError::Exists,
        user_abi::ERR_NOT_EMPTY => IoError::NotEmpty,
        user_abi::ERR_PERMISSION => IoError::Denied,
        user_abi::ERR_NO_SPACE => IoError::NoSpace,
        _ => IoError::Other,
    }
}

fn code(result: i64) -> Result<(), IoError> {
    if result < 0 { Err(io_error(result)) } else { Ok(()) }
}

/// Открытые настройки системы (`/etc/system.cfg`) текстом; нет файла — пусто.
fn system_settings() -> String {
    let Some(config) = user_progs::config_path("system.cfg") else { return String::new() };
    let Ok(data) = load(config.as_str()) else { return String::new() };
    String::from_utf8_lossy(&data).into_owned()
}

/// Домашний каталог того, от чьего имени запущена программа.
///
/// У `root` — `/root`. У пользователя — строка `home=` из настроек:
/// `/etc/passwd` закрыт от всех, кроме root (в нём отпечаток пароля), и
/// установщик нарочно дублирует имя и домашний каталог в открытых настройках.
/// Нет строки — корень.
fn home_directory(text: &str) -> String {
    if uid() == 0 {
        return String::from("/root");
    }
    for line in text.lines() {
        if let Some(home) = line.trim().strip_prefix("home=") {
            let home = home.trim();
            if home.starts_with('/') {
                return String::from(home);
            }
        }
    }
    String::from("/")
}

/// Стек среды: мегабайт вместо обычных 64 КиБ.
///
/// Загрузка типа в `clr-vm` рекурсивна по цепочке баз и полей-структур, по
/// 3–5 КиБ на уровень, и образец `objects` фазы N3a съедал около 90 КиБ —
/// `/bin/dotnet` снимался ядром на первой же сборке. Иерархии WinForms
/// (`Form` → … → `Object`, семь уровней) глубже. Мегабайт — с запасом на них и
/// на кадры AArch64, а тест `samples_fit_in_the_user_stack` в `clr-vm` держит
/// образцы в четверти этого.
const DOTNET_STACK_BYTES: usize = 1024 * 1024;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // Первым делом, до любого выделения: и размер кучи, и место стека зависят
    // от того, что ещё ничего не выделено (см. `run_on_own_stack`).
    heap_size(HEAP_BYTES);
    let start = (argc, argv);
    let code = user_progs::run_on_own_stack(DOTNET_STACK_BYTES, run, core::ptr::from_ref(&start) as usize);
    error(&format!("dotnet: error: no memory for a {DOTNET_STACK_BYTES}-byte stack (code {code})\n"));
    exit(1)
}

/// Всё остальное — уже на стеке среды.
extern "C" fn run(start: usize) -> ! {
    // SAFETY: `start` — адрес пары в кадре `_start`, который не вернётся
    // никогда, так что пара жива до конца программы.
    let (argc, argv) = unsafe { *(start as *const (usize, *const *const u8)) };

    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    let Some(path) = args.get(1) else {
        error("usage: dotnet <assembly.dll> [arguments]\n");
        exit(2)
    };
    let name = path.rsplit('/').next().unwrap_or(path);

    let data = match load(path) {
        Ok(data) => data,
        Err(text) => {
            error(&format!("dotnet: error: {path}: {text}\n"));
            exit(1)
        }
    };

    let mut program_args = Vec::new();
    let mut index = 2;
    while let Some(arg) = args.get(index) {
        program_args.push(arg);
        index += 1;
    }

    let corelib = match load(CORELIB) {
        Ok(corelib) => corelib,
        Err(text) => {
            error(&format!("dotnet: error: {CORELIB}: {text}\n"));
            exit(1)
        }
    };

    // Пояс — тот же, что показывают часы стола: строка `timezone=` настроек.
    let settings = system_settings();
    let console = Console {
        home: home_directory(&settings),
        offset_minutes: sysconf::timezone_minutes(&settings).unwrap_or(0),
        windows: Vec::new(),
        face: None,
        program: String::from(name.strip_suffix(".dll").unwrap_or(name)),
    };
    let mut vm = match Vm::new(&data, &corelib, console) {
        Ok(vm) => vm,
        Err(failure) => {
            error(&format!("dotnet: error: {name}: {failure}\n"));
            exit(1)
        }
    };
    error(&format!(
        "dotnet: {name}: {} bytes, {} type(s), {} method(s)\n",
        data.len(),
        vm.type_count(),
        vm.method_count()
    ));

    vm.set_program_path(path);
    match vm.run_main(&program_args) {
        Ok(code) => {
            // `Environment.Exit` — не возврат из `Main`, и журнал это различает.
            let how = if vm.exited() { format!("Environment.Exit({code})") } else { format!("Main returned {code}") };
            error(&format!(
                "dotnet: {name}: {how} after {} instruction(s), {} object(s), {} collection(s)\n",
                vm.instructions,
                vm.object_count(),
                vm.collections()
            ));
            exit(i64::from(code))
        }
        Err(failure) => {
            error(&format!("dotnet: error: {name}: {failure}\n"));
            exit(RUNTIME_FAILED)
        }
    }
}

/// Прочитать сборку целиком.
fn load(path: &str) -> Result<Vec<u8>, String> {
    let fd = open(path);
    if fd < 0 {
        return Err(format!("cannot open (code {fd})"));
    }
    let size = file_size(fd);
    if size < 0 {
        close(fd);
        return Err(format!("cannot read the size (code {size})"));
    }
    let size = size as usize;
    if size > FILE_MAX {
        close(fd);
        return Err(format!("{size} bytes is more than the {FILE_MAX} this runtime reads"));
    }
    let mut data = Vec::new();
    if data.try_reserve_exact(size).is_err() {
        close(fd);
        return Err(String::from("out of memory"));
    }
    data.resize(size, 0);
    let mut filled = 0;
    while filled < size {
        let got = read(fd, &mut data[filled..]);
        if got <= 0 {
            break;
        }
        filled += got as usize;
    }
    close(fd);
    if filled != size {
        return Err(format!("read {filled} of {size} bytes"));
    }
    Ok(data)
}
