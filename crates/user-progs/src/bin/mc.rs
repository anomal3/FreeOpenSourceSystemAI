//! Двухпанельный файловый менеджер — первое настоящее интерактивное приложение.
//!
//! # Что эта программа доказывает
//!
//! Что фаз 29 и 20 хватает для приложения, которое читает клавиатуру, рисует
//! весь экран и работает с файлами, — и что для этого не нужно ни строчки в
//! ядре. `mc` здесь обычная программа из `/bin`: те же системные вызовы, что у
//! `hello`, тот же ELF, то же адресное пространство.
//!
//! # Что это не
//!
//! Это **не** Midnight Commander. Настоящий — сто тысяч строк, вложенные
//! просмотрщики, редактор, виртуальные файловые системы и сеть. Здесь
//! двухпанельный менеджер с его раскладкой и привычными клавишами: две панели,
//! ходьба по каталогам, копирование, удаление, создание каталога, просмотр
//! файла, выход по F10. Имя выбрано за раскладку и клавиши, и обещать за ним
//! больше нельзя — поэтому сказано прямо и здесь, и в README.
//!
//! # Почему диагностика идёт в дескриптор 2
//!
//! Потому что дескриптор 1 занят картинкой. Полноэкранная программа рисует
//! экран целиком, и строка «скопировано то-то» посреди панелей была бы мусором
//! поверх рамки. Ядро отправляет второй дескриптор в журнал, не трогая окно, —
//! ровно затем, чтобы у программы с картинкой был канал для слов.
//!
//! # Куча
//!
//! Её нет. Все буферы — массивы на стеке известного размера, и пределы
//! ([`MAX_ENTRIES`], [`NAME_MAX`]) видны в коде. Каталог, который в них не
//! поместился, показывается обрезанным и **говорит** об этом: молчаливое
//! обрезание списка файлов — это потерянные файлы.

#![no_std]
#![no_main]

use user_progs::{
    Args, Dirent, KIND_DIRECTORY, close, error, error_num, exit, mkdir, open, open_write, print,
    println, read, read_key, readdir, remove, rename, set_raw, window_size, write,
};

/// Сколько записей помещается в панель.
const MAX_ENTRIES: usize = 64;
/// Самое длинное имя, которое панель показывает целиком.
const NAME_MAX: usize = 32;
/// Самый длинный путь.
const PATH_MAX: usize = 128;
/// Ширина буфера строки при отрисовке.
const LINE_MAX: usize = 200;
/// Размер куска при копировании.
const CHUNK: usize = 512;

/// Одна панель: путь, содержимое каталога и положение курсора.
struct Panel {
    path: [u8; PATH_MAX],
    path_len: usize,
    names: [[u8; NAME_MAX]; MAX_ENTRIES],
    name_len: [u8; MAX_ENTRIES],
    is_dir: [bool; MAX_ENTRIES],
    size: [u64; MAX_ENTRIES],
    count: usize,
    /// Сколько записей не поместилось.
    dropped: usize,
    cursor: usize,
    /// Первая показанная строка — панель прокручивается вместе с курсором.
    top: usize,
}

impl Panel {
    const fn new() -> Self {
        Self {
            path: [0; PATH_MAX],
            path_len: 0,
            names: [[0; NAME_MAX]; MAX_ENTRIES],
            name_len: [0; MAX_ENTRIES],
            is_dir: [false; MAX_ENTRIES],
            size: [0; MAX_ENTRIES],
            count: 0,
            dropped: 0,
            cursor: 0,
            top: 0,
        }
    }

    fn path(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or("/")
    }

    fn set_path(&mut self, path: &str) -> bool {
        let bytes = path.as_bytes();
        if bytes.len() > PATH_MAX {
            return false;
        }
        self.path[..bytes.len()].copy_from_slice(bytes);
        self.path_len = bytes.len();
        true
    }

    fn name(&self, index: usize) -> &str {
        let len = self.name_len[index] as usize;
        core::str::from_utf8(&self.names[index][..len]).unwrap_or("?")
    }

    /// Перечитать каталог. `false` — каталог не открылся.
    fn reload(&mut self) -> bool {
        self.count = 0;
        self.dropped = 0;
        self.cursor = 0;
        self.top = 0;

        let fd = open(self.path());
        if fd < 0 {
            return false;
        }

        // Первой строкой всегда «..», даже в корне: подъём из корня никуда не
        // ведёт, но строка на месте, и палец не ищет её каждый раз заново.
        self.push("..", true, 0);

        let mut entry = Dirent::default();
        while readdir(fd, &mut entry) {
            let Some(name) = entry.name() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            self.push(name, entry.kind == KIND_DIRECTORY, entry.size);
        }
        close(fd);
        true
    }

    fn push(&mut self, name: &str, directory: bool, size: u64) {
        if self.count == MAX_ENTRIES {
            self.dropped += 1;
            return;
        }
        let bytes = name.as_bytes();
        let len = bytes.len().min(NAME_MAX);
        self.names[self.count][..len].copy_from_slice(&bytes[..len]);
        self.name_len[self.count] = len as u8;
        self.is_dir[self.count] = directory;
        self.size[self.count] = size;
        self.count += 1;
    }

    /// Полный путь к записи под курсором. Возвращает длину, записанную в `out`.
    fn selected_path(&self, out: &mut [u8; PATH_MAX]) -> usize {
        join(self.path(), self.name(self.cursor), out)
    }

    fn move_cursor(&mut self, delta: i32, rows: usize) {
        if self.count == 0 {
            return;
        }
        let last = self.count - 1;
        let next = self.cursor as i32 + delta;
        self.cursor = next.clamp(0, last as i32) as usize;
        // Окно прокрутки едет за курсором, а не курсор за окном: так список не
        // «прыгает» при движении внутри видимой части.
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + rows {
            self.top = self.cursor + 1 - rows;
        }
    }
}

/// Склеить каталог и имя в путь. Возвращает длину.
fn join(dir: &str, name: &str, out: &mut [u8; PATH_MAX]) -> usize {
    let mut len = 0;
    for byte in dir.bytes() {
        if len < PATH_MAX {
            out[len] = byte;
            len += 1;
        }
    }
    // Корень уже кончается разделителем — второй сделал бы `//`, а это другой
    // путь для всякого, кто сравнивает строки.
    if len > 0 && out[len - 1] != b'/' && len < PATH_MAX {
        out[len] = b'/';
        len += 1;
    }
    for byte in name.bytes() {
        if len < PATH_MAX {
            out[len] = byte;
            len += 1;
        }
    }
    len
}

/// Родительский каталог пути. Возвращает длину, записанную в `out`.
fn parent(path: &str, out: &mut [u8; PATH_MAX]) -> usize {
    let bytes = path.as_bytes();
    let mut cut = bytes.len();
    while cut > 0 && bytes[cut - 1] != b'/' {
        cut -= 1;
    }
    // Убрать сам разделитель, кроме случая корня: у `/bin` родитель `/`, а не
    // пустая строка.
    let len = if cut <= 1 { 1 } else { cut - 1 };
    out[..len].copy_from_slice(&bytes[..len]);
    if len == 1 {
        out[0] = b'/';
    }
    len
}

/// Что нажали. Разбор escape-последовательностей — здесь, потому что терминал
/// присылает клавиши ровно так же, как их прислал бы любой другой терминал.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Enter,
    Tab,
    View,
    Copy,
    Rename,
    MakeDir,
    Delete,
    Quit,
    Other,
    /// Ввод кончился — терминал закрылся или программу читать больше нечем.
    End,
}

/// Прочитать одну клавишу, собрав её из байтов.
fn key() -> Key {
    let byte = match read_key() {
        Some(byte) => byte,
        None => return Key::End,
    };

    match byte {
        b'\t' => return Key::Tab,
        b'\n' | b'\r' => return Key::Enter,
        b'q' | b'Q' => return Key::Quit,
        0x1b => {}
        _ => return Key::Other,
    }

    // `ESC` в одиночку — это тоже выход: так ведёт себя всякая полноэкранная
    // программа, и человеку, который не помнит, что здесь F10, нужна дорога
    // наружу.
    let Some(second) = read_key() else {
        return Key::End;
    };
    match second {
        // `ESC O P..S` — F1..F4 из VT100.
        b'O' => match read_key() {
            Some(b'R') => Key::View,
            Some(_) => Key::Other,
            None => Key::End,
        },
        b'[' => csi(),
        _ => Key::Other,
    }
}

/// Разобрать хвост `ESC [ …`.
fn csi() -> Key {
    let mut number = 0u32;
    loop {
        let Some(byte) = read_key() else {
            return Key::End;
        };
        match byte {
            b'0'..=b'9' => number = (number * 10 + u32::from(byte - b'0')).min(999),
            b'A' => return Key::Up,
            b'B' => return Key::Down,
            b'~' => {
                return match number {
                    15 => Key::Copy,
                    17 => Key::Rename,
                    18 => Key::MakeDir,
                    19 => Key::Delete,
                    21 => Key::Quit,
                    _ => Key::Other,
                };
            }
            // Всё прочее (стрелки вбок, модификаторы, `;`) панелям не нужно, но
            // последовательность надо доесть до финального байта, иначе её
            // хвост будет разобран как отдельные клавиши.
            b';' => number = 0,
            _ => return Key::Other,
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };

    let mut left = Panel::new();
    let mut right = Panel::new();
    left.set_path(args.get(1).unwrap_or("/"));
    right.set_path(args.get(2).unwrap_or("/"));

    if !left.reload() {
        error("mc: cannot open the left directory\n");
        exit(1);
    }
    if !right.reload() {
        // Правая панель не критична: система, где есть только один читаемый
        // каталог, — это всё ещё система, в которой можно работать.
        right.set_path("/");
        right.reload();
    }

    let (cols, rows) = window_size();
    // Полей у панели три: рамка сверху, рамка снизу и строка подсказки.
    let list_rows = (rows.saturating_sub(4) as usize).max(1);
    let width = (cols as usize).clamp(40, LINE_MAX - 1);

    // Прямой режим: клавиши приезжают немедленно и без эха. Без него `mc`
    // получал бы строки по Enter, а стрелки — четырьмя видимыми символами.
    set_raw(true);
    error("mc: started\n");

    let mut active_left = true;
    loop {
        draw(&left, &right, active_left, width, list_rows);

        match key() {
            Key::Quit | Key::End => break,
            Key::Tab => active_left = !active_left,
            Key::Up => panel(&mut left, &mut right, active_left).move_cursor(-1, list_rows),
            Key::Down => panel(&mut left, &mut right, active_left).move_cursor(1, list_rows),
            Key::Enter => enter(panel(&mut left, &mut right, active_left)),
            Key::View => view(panel(&mut left, &mut right, active_left)),
            Key::Rename => rename_selected(panel(&mut left, &mut right, active_left)),
            Key::MakeDir => make_dir(panel(&mut left, &mut right, active_left)),
            Key::Delete => delete(panel(&mut left, &mut right, active_left)),
            Key::Copy => {
                // Копирование — единственное действие, которому нужны обе
                // панели сразу: источник под курсором активной, приёмник —
                // каталог другой.
                let (source, target) = if active_left {
                    (&left, &right)
                } else {
                    (&right, &left)
                };
                let ok = copy(source, target);
                if ok {
                    let other = if active_left { &mut right } else { &mut left };
                    other.reload();
                }
            }
            Key::Other => {}
        }
    }

    // Экран возвращается оболочке в том виде, в каком она его оставила бы сама:
    // очищенным, с курсором в углу и без нашего цвета. Программа, ушедшая, не
    // прибрав за собой, ломает приглашение того, кто её запустил.
    set_raw(false);
    print("\x1b[0m\x1b[2J\x1b[1;1H");
    error("mc: quit\n");
    println("mc: done");
    exit(0)
}

/// Панель, на которой стоит фокус.
fn panel<'a>(left: &'a mut Panel, right: &'a mut Panel, active_left: bool) -> &'a mut Panel {
    if active_left { left } else { right }
}

/// Войти в каталог под курсором или подняться на уровень выше.
fn enter(panel: &mut Panel) {
    let mut buffer = [0u8; PATH_MAX];
    let len = if panel.name(panel.cursor) == ".." {
        parent(panel.path(), &mut buffer)
    } else if panel.is_dir[panel.cursor] {
        panel.selected_path(&mut buffer)
    } else {
        // На файле Enter не делает ничего: запускать чужой код по нажатию
        // клавиши, которой ходят по каталогам, — не то, чего ждёт человек.
        return;
    };

    let Ok(path) = core::str::from_utf8(&buffer[..len]) else {
        return;
    };
    let saved_len = panel.path_len;
    let mut saved = [0u8; PATH_MAX];
    saved[..saved_len].copy_from_slice(&panel.path[..saved_len]);

    if !panel.set_path(path) || !panel.reload() {
        // Каталог не открылся: возвращаемся туда, где были, а не остаёмся с
        // пустой панелью и путём, которого нет.
        panel.path[..saved_len].copy_from_slice(&saved[..saved_len]);
        panel.path_len = saved_len;
        panel.reload();
        error("mc: cannot enter the directory\n");
        return;
    }

    error("mc: entered ");
    error(panel.path());
    error("\n");
}

/// Показать первые строки файла.
fn view(panel: &Panel) {
    if panel.is_dir[panel.cursor] {
        return;
    }
    let mut buffer = [0u8; PATH_MAX];
    let len = panel.selected_path(&mut buffer);
    let Ok(path) = core::str::from_utf8(&buffer[..len]) else {
        return;
    };

    let fd = open(path);
    if fd < 0 {
        error("mc: cannot open the file\n");
        return;
    }

    print("\x1b[2J\x1b[1;1H");
    print("--- ");
    print(path);
    print(" ---\n");

    let mut chunk = [0u8; CHUNK];
    let mut shown = 0u64;
    loop {
        let read_bytes = read(fd, &mut chunk);
        if read_bytes <= 0 {
            break;
        }
        let read_bytes = read_bytes as usize;
        // Байты выводятся как есть только пока это текст: двоичный файл, попав
        // на экран целиком, испортил бы сетку символов управляющими байтами.
        for byte in &mut chunk[..read_bytes] {
            if *byte != b'\n' && *byte != b'\t' && !(0x20..0x7f).contains(byte) {
                *byte = b'.';
            }
        }
        write(1, &chunk[..read_bytes]);
        shown += read_bytes as u64;
        // Один экран за раз: прокрутки у просмотрщика нет, и лить в терминал
        // мегабайт значило бы занять машину надолго без всякой пользы.
        if shown >= 1024 {
            break;
        }
    }
    close(fd);

    error("mc: viewed ");
    error(path);
    error("\n");

    print("\n--- any key ---");
    let _ = read_key();
}

/// Скопировать файл под курсором в каталог другой панели.
fn copy(source: &Panel, target: &Panel) -> bool {
    if source.count == 0 || source.is_dir[source.cursor] {
        error("mc: only files are copied\n");
        return false;
    }

    let mut from = [0u8; PATH_MAX];
    let from_len = source.selected_path(&mut from);
    let mut to = [0u8; PATH_MAX];
    let to_len = join(target.path(), source.name(source.cursor), &mut to);

    let (Ok(from_path), Ok(to_path)) = (
        core::str::from_utf8(&from[..from_len]),
        core::str::from_utf8(&to[..to_len]),
    ) else {
        return false;
    };

    let input = open(from_path);
    if input < 0 {
        error("mc: cannot read the source\n");
        return false;
    }
    let output = open_write(to_path, true, true);
    if output < 0 {
        close(input);
        error("mc: cannot create ");
        error(to_path);
        error(": ");
        error_num(output);
        error("\n");
        return false;
    }

    let mut chunk = [0u8; CHUNK];
    let mut copied = 0u64;
    let mut failed = false;
    loop {
        let read_bytes = read(input, &mut chunk);
        if read_bytes < 0 {
            failed = true;
            break;
        }
        if read_bytes == 0 {
            break;
        }
        let written = write(output, &chunk[..read_bytes as usize]);
        if written != read_bytes {
            failed = true;
            break;
        }
        copied += written as u64;
    }
    close(input);
    close(output);

    if failed {
        error("mc: copy failed\n");
        return false;
    }

    // Строка, ради которой всё это проверяется снаружи: она называет обе
    // стороны, поэтому «скопировалось не то» и «скопировалось» выглядят
    // по-разному.
    error("mc: copied ");
    error(from_path);
    error(" -> ");
    error(to_path);
    error("\n");
    let _ = copied;
    true
}

/// Переименовать то, что под курсором.
///
/// Именно переименовать, а не «скопировать и удалить»: содержимое не читается
/// вовсе, меняется запись каталога. Разница видна на файле в гигабайт — и на
/// файле, который в этот момент кто-то читает.
fn rename_selected(panel: &mut Panel) {
    if panel.count == 0 || panel.name(panel.cursor) == ".." {
        return;
    }

    let mut from = [0u8; PATH_MAX];
    let from_len = panel.selected_path(&mut from);

    let Some(name) = prompt("mc: rename to: ") else {
        return;
    };
    if name.is_empty() {
        return;
    }

    let mut to = [0u8; PATH_MAX];
    let to_len = join(panel.path(), name, &mut to);

    let (Ok(from_path), Ok(to_path)) = (
        core::str::from_utf8(&from[..from_len]),
        core::str::from_utf8(&to[..to_len]),
    ) else {
        return;
    };

    let result = rename(from_path, to_path);
    if result < 0 {
        error("mc: rename ");
        error(from_path);
        error(": ");
        error_num(result);
        error("
");
        return;
    }
    error("mc: renamed ");
    error(from_path);
    error(" -> ");
    error(to_path);
    error("
");
    panel.reload();
}

/// Создать каталог с именем, набранным человеком.
fn make_dir(panel: &mut Panel) {
    let Some(name) = prompt("mc: new directory name: ") else {
        return;
    };
    if name.is_empty() {
        return;
    }

    let mut buffer = [0u8; PATH_MAX];
    let len = join(panel.path(), name, &mut buffer);
    let Ok(path) = core::str::from_utf8(&buffer[..len]) else {
        return;
    };

    let result = mkdir(path, 0o755);
    if result < 0 {
        error("mc: mkdir ");
        error(path);
        error(": ");
        error_num(result);
        error("\n");
        return;
    }
    error("mc: created ");
    error(path);
    error("\n");
    panel.reload();
}

/// Удалить то, что под курсором.
fn delete(panel: &mut Panel) {
    if panel.count == 0 || panel.name(panel.cursor) == ".." {
        return;
    }
    let mut buffer = [0u8; PATH_MAX];
    let len = panel.selected_path(&mut buffer);
    let Ok(path) = core::str::from_utf8(&buffer[..len]) else {
        return;
    };

    let result = remove(path);
    if result < 0 {
        error("mc: remove ");
        error(path);
        error(": ");
        error_num(result);
        error("\n");
        return;
    }
    error("mc: removed ");
    error(path);
    error("\n");
    panel.reload();
}

/// Спросить строку в нижней части экрана.
///
/// Возвращает `None`, если ввод кончился. Строка живёт в статическом буфере
/// функции — кучи у программы нет, а возвращать ссылку на свой стек нельзя.
fn prompt(question: &str) -> Option<&'static str> {
    /// Буфер ответа. `static mut` здесь безопасен по построению: программа
    /// однопоточна, и ссылка на буфер не переживает следующего вызова —
    /// вызывающий копирует из неё путь сразу же.
    static mut ANSWER: [u8; NAME_MAX] = [0; NAME_MAX];

    print("\x1b[0m");
    print(question);

    let mut len = 0;
    loop {
        let byte = read_key()?;
        match byte {
            b'\n' | b'\r' => break,
            // Backspace приезжает как 0x7F — так его присылает всякий терминал.
            0x7f | 0x08 => {
                if len > 0 {
                    len -= 1;
                    // Возврат-пробел-возврат: сам возврат каретки символ не
                    // стирает.
                    print("\u{8} \u{8}");
                }
            }
            0x1b => return None,
            _ if (0x20..0x7f).contains(&byte) && len < NAME_MAX => {
                // SAFETY: программа однопоточна, и другой ссылки на буфер в
                // этот момент не существует.
                unsafe {
                    (&raw mut ANSWER).cast::<u8>().add(len).write(byte);
                }
                len += 1;
                // Эха в прямом режиме нет — его делает программа, и делает
                // ровно там, где ждёт ввод.
                let echo = [byte];
                write(1, &echo);
            }
            _ => {}
        }
    }

    // SAFETY: та же однопоточность; срез не переживает следующего вызова.
    let bytes = unsafe { core::slice::from_raw_parts((&raw const ANSWER).cast::<u8>(), len) };
    core::str::from_utf8(bytes).ok()
}

/// Нарисовать обе панели целиком.
fn draw(left: &Panel, right: &Panel, active_left: bool, width: usize, rows: usize) {
    print("\x1b[H");

    let half = width / 2;

    // Заголовки: путь каждой панели, активная — цветом.
    header(left, active_left, half);
    header(right, !active_left, width - half);
    print("\n");

    for row in 0..rows {
        cell(left, row, active_left, half);
        cell(right, row, !active_left, width - half);
        print("\x1b[0m\x1b[K\n");
    }

    // Подсказка: те же клавиши, что у настоящего менеджера, в том же порядке.
    print("\x1b[0m\x1b[K F3 View  F5 Copy  F7 Mkdir  F8 Delete  Tab Other  F10 Quit");
    // Обрезанный список **говорит** об этом. Молчаливое обрезание — это файлы,
    // которых человек не увидел и о которых не узнал.
    if left.dropped > 0 || right.dropped > 0 {
        print("  (list truncated)");
    }
    print("\x1b[K");
}

/// Полоса с путём панели.
fn header(panel: &Panel, active: bool, width: usize) {
    if active {
        // Инверсия — единственный способ показать активную панель в шрифте, у
        // которого нет ни жирного, ни курсива.
        print("\x1b[7m");
    } else {
        print("\x1b[0m");
    }
    let path = panel.path();
    print(" ");
    print_fixed(path, width.saturating_sub(1));
    print("\x1b[0m");
}

/// Одна строка списка.
fn cell(panel: &Panel, row: usize, active: bool, width: usize) {
    let index = panel.top + row;
    if index >= panel.count {
        print("\x1b[0m");
        print_fixed("", width);
        return;
    }

    let selected = active && index == panel.cursor;
    if selected {
        print("\x1b[7m");
    } else if panel.is_dir[index] {
        // Каталог другого цвета — так его видно без значков, которых в шрифте
        // 8×8 всё равно не нарисовать.
        print("\x1b[36m");
    } else {
        print("\x1b[0m");
    }

    let name = panel.name(index);
    // Каталог помечается косой чертой: тип записи приехал от ядра, а не
    // угадывается по имени, и показать его — половина смысла панели.
    let mark = if panel.is_dir[index] { "/" } else { " " };
    print(" ");
    print(mark);
    print_fixed(name, width.saturating_sub(2));
    print("\x1b[0m");
}

/// Напечатать строку, дополнив её пробелами или обрезав до ширины.
fn print_fixed(text: &str, width: usize) {
    let mut written = 0;
    for ch in text.chars() {
        if written == width {
            return;
        }
        let mut utf8 = [0u8; 4];
        print(ch.encode_utf8(&mut utf8));
        written += 1;
    }
    while written < width {
        print(" ");
        written += 1;
    }
}
