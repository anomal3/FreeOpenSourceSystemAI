//! Оболочка ядра: приглашение, разбор команд, вывод.
//!
//! # Одна оболочка на два экрана
//!
//! Оболочка не знает, есть ли графика. Её вывод уходит в [`Out`], а тот сам
//! решает: если композитор поднят — в окно, если нет — в консоль ядра. Поэтому
//! на машине без фреймбуфера (или с прошивкой, не отдавшей GOP) всё работает
//! точно так же, только текст идёт в серийный порт.
//!
//! Дублирование в serial сохраняется всегда и намеренно: это единственный канал,
//! который можно прочитать снаружи, и именно им проверяется, что ядро делает то,
//! что должно. Оболочка, чей вывод видно только глазами на картинке, была бы
//! непроверяемой.
//!
//! # Почему оболочка — задача, а не цикл в конце загрузки
//!
//! Потому что она обязана уступать процессор: пока пользователь думает,
//! исполняться должно всё остальное — опрос контроллера USB, обновление окна
//! состояния. И потому что она обязана уметь закончиться: на этом заканчивается
//! загрузка, и без этого автоматический прогон висел бы вечно.

use core::fmt::{self, Write};

use alloc::string::String;

use crate::input::line::{Edit, LineEditor};
use crate::input::{self, KeyCode};
use crate::vfs::NodeKind;
use crate::{fs, irq, kprint, mm, sched, ui, usb, user};

/// Приглашение к вводу.
const PROMPT: &str = "freeos> ";

/// Сколько секунд ждать ввода, прежде чем закончить сеанс.
///
/// Предел обязателен, а не удобен: без него запуск в CI (где никто ничего не
/// набирает) висел бы вечно, и «ядро ждёт ввод» стало бы неотличимо от «ядро
/// зависло». Двадцати секунд достаточно, чтобы человек успел напечатать команду,
/// и мало настолько, чтобы автоматический прогон завершался сам.
const IDLE_TIMEOUT_SECONDS: u64 = 20;

/// Как часто обновлять окно состояния.
const STATUS_PERIOD_MS: u64 = 500;

/// Сколько байт файла показывает `cat`.
///
/// Предел не косметический: файл на носителе может быть любого размера, а окно
/// прокручивается символ за символом. Без предела `cat` на образе в сорок
/// мегабайт занял бы машину надолго и вытеснил бы из окна всё остальное.
const CAT_LIMIT: usize = 4096;

/// Приёмник вывода оболочки.
///
/// Реализует [`fmt::Write`], поэтому годится и для `write!`, и для эха редактора
/// строки.
pub struct Out;

impl fmt::Write for Out {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if ui::is_active() {
            // Композитор поднят: на экране окна, и загрузочная консоль экран уже
            // отдала. В serial при этом пишем сами — `kprint!` туда бы написал,
            // но заодно попытался бы рисовать.
            crate::serial::_print(format_args!("{text}"));
            ui::write(text);
        } else {
            kprint!("{text}");
        }
        Ok(())
    }
}

/// Напечатать в оболочку.
macro_rules! sprint {
    ($($arg:tt)*) => {{
        let _ = ::core::write!(&mut $crate::shell::Out, $($arg)*);
    }};
}

/// Напечатать в оболочку с переводом строки.
macro_rules! sprintln {
    () => { sprint!("\n") };
    ($($arg:tt)*) => {{
        sprint!($($arg)*);
        sprint!("\n");
    }};
}

/// Тело задачи-оболочки.
///
/// Возвращается, когда сеанс закончен: по команде `exit`, по Ctrl+D или по
/// истечении времени без ввода.
pub fn task() {
    // Дать демонстрационным задачам договорить: их вывод и набираемая строка
    // идут в одну консоль, и перемешивать их незачем. Условие — «жива только эта
    // задача», то есть счётчик равен единице.
    while sched::alive() > 1 {
        sched::yield_now();
    }

    banner();
    ui::set_cursor(true);
    sprint!("{PROMPT}");

    let mut editor = LineEditor::new();
    let mut idle_since = irq::uptime_ms();
    let mut status_at = 0u64;

    loop {
        // Опрос контроллера USB живёт здесь, а не в обработчике таймера, и это
        // осознанно: обработчик обязан быть коротким и не имеет права ждать
        // занятый лок, а разбор кольца событий делает и то, и другое. Задача же
        // и без того просыпается на каждом витке планировщика.
        usb::xhci::service();

        let now = irq::uptime_ms();
        if now.saturating_sub(status_at) >= STATUS_PERIOD_MS {
            status_at = now;
            update_status();
        }

        // Отчёты мыши разбираются перед клавишами: они меняют, какое окно
        // активно, и разобрать их после значило бы отдать нажатие тому окну,
        // которое было активным до щелчка.
        while let Some(event) = input::next_pointer() {
            idle_since = irq::uptime_ms();
            ui::dispatch_pointer(event);
        }

        while let Some(event) = input::next_event() {
            idle_since = irq::uptime_ms();
            // Рабочий стол смотрит на событие первым: меню, переключение и
            // перемещение окон, а также ввод в окно другой программы — всё это
            // до оболочки не доходит. Оболочка — одна из программ, а не хозяин
            // клавиатуры, и без стола (машина без фреймбуфера) события просто
            // проходят насквозь.
            let Some(event) = ui::dispatch(event) else {
                continue;
            };
            match editor.handle(event, &mut Out) {
                Edit::Submitted => {
                    let done = run_command(editor.as_str());
                    editor.clear();
                    if done {
                        ui::set_cursor(false);
                        return;
                    }
                    sprint!("{PROMPT}");
                }
                Edit::Cancelled => sprint!("{PROMPT}"),
                Edit::EndOfInput => {
                    sprintln!("  end of input");
                    ui::set_cursor(false);
                    return;
                }
                Edit::Full => {
                    sprintln!();
                    sprintln!("  the line is full ({} bytes)", input::line::MAX_LINE);
                    sprint!("{PROMPT}{}", editor.as_str());
                }
                Edit::Unhandled(code) => {
                    handle_key(code, &editor);
                }
                Edit::Ignored | Edit::Inserted | Edit::Erased => {}
            }
        }

        if irq::uptime_ms().saturating_sub(idle_since) >= IDLE_TIMEOUT_SECONDS * 1000 {
            sprintln!();
            sprintln!("  no input for {IDLE_TIMEOUT_SECONDS} s, finishing the session");
            ui::set_cursor(false);
            return;
        }

        // Уступаем, а не крутимся: пока пользователь думает, процессор должен
        // достаться остальным задачам.
        sched::yield_now();
    }
}

/// Приветствие: что здесь есть и куда нажимать.
///
/// Строки короткие не случайно: окно оболочки — это 52–72 символа в зависимости
/// от экрана, а перенос длинной фразы посередине слова читается как сбой вывода,
/// хотя это правильное поведение.
fn banner() {
    sprintln!("FreeOS shell. 'help' for commands, 'exit' to finish.");
    if ui::is_active() {
        let (cols, rows) = ui::shell_size();
        sprintln!("Window {cols}x{rows}. Tab raises the window below.");
    }
    // Перечисляем только то, что действительно поднялось. Обещать клавиатуру на
    // машине, где её нет, — это заставить человека искать неисправность там, где
    // её не бывает.
    let sources = input::sources();
    match (sources.keyboard, sources.serial) {
        (true, true) => sprintln!("Input: keyboard and serial line."),
        (true, false) => sprintln!("Input: keyboard."),
        (false, true) => sprintln!("Input: serial line."),
        (false, false) => sprintln!("Input: none available."),
    }
    if sources.mouse {
        sprintln!("Mouse: click to focus, drag the title bar to move.");
    }
    sprintln!();
}

/// Клавиши, которые не относятся к правке строки.
///
/// Сочетаний рабочего стола здесь нет и быть не должно: их разобрал
/// [`ui::dispatch`] до того, как событие дошло до оболочки.
fn handle_key(code: KeyCode, editor: &LineEditor) {
    sprintln!();
    sprintln!("  key: {}", code.name());
    sprint!("{PROMPT}{}", editor.as_str());
}

/// Обновить окно состояния и панель задач.
fn update_status() {
    if !ui::is_active() {
        return;
    }
    // Часы и память на панели — то же обновление, что и окно состояния, и по
    // тому же таймеру: заводить столу отдельный источник времени значило бы
    // рисовать из обработчика прерывания.
    ui::tick();
    let frames = mm::frame::stats();
    let heap = mm::heap::stats();
    let (dma_used, dma_total) = mm::dma::stats();
    let events = input::stats();
    let (composed, rects, windows) = ui::stats();

    let mut text = String::new();
    let _ = write!(
        text,
        "uptime  {} ms\n\
         ticks   {}\n\
         frames  {composed} composed, {rects} rects\n\
         windows {windows}\n\
         keys    {} posted, {} dropped\n",
        irq::uptime_ms(),
        irq::ticks(),
        events.posted,
        events.dropped,
    );
    if let Some(usb) = usb::xhci::summary() {
        let _ = write!(
            text,
            "usb     {} devices, {} reports, {} err\n",
            usb.devices, usb.reports, usb.errors
        );
    }
    let (moves, merged) = input::pointer_stats();
    if moves > 0 {
        let _ = write!(text, "pointer {moves} reports, {merged} merged\n");
    }
    let _ = write!(
        text,
        "memory  {} MiB free of {} MiB\n\
         heap    {} KiB free\n\
         dma     {} of {} KiB\n\
         tasks   {} alive",
        frames.free_bytes() / (1024 * 1024),
        frames.total_bytes() / (1024 * 1024),
        heap.free / 1024,
        dma_used / 1024,
        dma_total / 1024,
        sched::alive(),
    );
    ui::set_status(&text);
}

/// Выполнить команду. Возвращает `true`, если сеанс пора закончить.
fn run_command(line: &str) -> bool {
    let line = line.trim();
    let (command, argument) = match line.split_once(' ') {
        Some((command, rest)) => (command, rest.trim()),
        None => (line, ""),
    };

    match command {
        "" => {}
        "help" => help(),
        "uptime" => sprintln!("  {} ms, {} timer ticks", irq::uptime_ms(), irq::ticks()),
        "mem" => memory(),
        "input" => {
            let stats = input::stats();
            sprintln!(
                "  events   {} posted, {} dropped, {} queued; modifiers {:?}",
                stats.posted,
                stats.dropped,
                stats.queued,
                input::modifiers()
            );
        }
        "usb" => usb_status(),
        "ui" => ui_status(),
        "tasks" => tasks(),
        "clear" => {
            ui::clear_shell();
            if !ui::is_active() {
                sprintln!("  (no window to clear; output goes to the serial console)");
            }
        }
        "ls" => list(if argument.is_empty() { "/" } else { argument }),
        "cat" => {
            if argument.is_empty() {
                sprintln!("  usage: cat <path>");
            } else {
                show(argument);
            }
        }
        "echo" => sprintln!("  {argument}"),
        "run" => {
            if argument.is_empty() {
                sprintln!("  usage: run <path>");
            } else {
                run_program(argument);
            }
        }
        "exit" | "quit" => {
            sprintln!("  finishing the session");
            return true;
        }
        other => sprintln!("  unknown command '{other}'; try 'help'"),
    }
    false
}

fn help() {
    sprintln!("  help          this list");
    sprintln!("  uptime        time since the timer started");
    sprintln!("  mem           physical frames, heap and DMA window");
    sprintln!("  input         key event counters");
    sprintln!("  usb           xHCI controller state");
    sprintln!("  ui            compositor state");
    sprintln!("  tasks         scheduler state");
    sprintln!("  ls [path]     list a directory of the mounted filesystem");
    sprintln!("  cat <path>    print a file, up to {CAT_LIMIT} bytes");
    sprintln!("  echo <text>   print the text back");
    sprintln!("  run <path>    load an ELF and run it outside the kernel");
    sprintln!("  clear         clear the window");
    sprintln!("  exit          finish the boot and halt");
}

fn memory() {
    let frames = mm::frame::stats();
    sprintln!(
        "  frames   {} of {} used, {} MiB free",
        frames.used(),
        frames.total,
        frames.free_bytes() / (1024 * 1024)
    );
    let heap = mm::heap::stats();
    sprintln!("  heap     {} bytes free of {}", heap.free, mm::HEAP_SIZE);
    let (used, total) = mm::dma::stats();
    sprintln!("  dma      {used} of {total} bytes in use");
}

fn usb_status() {
    match usb::xhci::summary() {
        Some(usb) => {
            sprintln!(
                "  xhci     {} devices, first slot {} on port {}",
                usb.devices,
                usb.slot,
                usb.port
            );
            sprintln!("  reports  {} parsed", usb.reports);
            sprintln!("  events   {} seen, {} transfer errors", usb.events, usb.errors);
        }
        None => sprintln!("  xhci     no controller"),
    }
}

fn ui_status() {
    if !ui::is_active() {
        sprintln!("  ui       no framebuffer; the shell runs on the serial console");
        return;
    }
    let (composed, rects, windows) = ui::stats();
    let (cols, rows) = ui::shell_size();
    sprintln!("  ui       {windows} windows, {composed} frames, {rects} rects");
    sprintln!("  shell    {cols}x{rows} characters");
    if let Some((x, y, visible)) = ui::pointer_state() {
        let (moves, merged) = input::pointer_stats();
        sprintln!(
            "  pointer  {x},{y} {}, {moves} reports, {merged} merged",
            if visible { "visible" } else { "hidden" }
        );
    }
}

fn tasks() {
    // Планировщик печатает сам, через `kprintln!`, то есть в serial и (пока
    // композитор не поднят) на экран. Дублировать его вывод в окно здесь нечем:
    // формат он держит внутри себя, а отдавать его строками наружу ради одной
    // команды — менять контракт планировщика под оболочку.
    sprintln!("  (task list goes to the serial console)");
    sched::dump();
}

/// Загрузить и запустить программу вне ядра.
///
/// Оболочка на время работы программы стоит: планировщик кооперативный, и
/// «запустить в фоне» означало бы сначала завести настоящий процесс со своим
/// состоянием. Программа при этом уступает процессор системным вызовом, так что
/// остальные задачи не останавливаются.
fn run_program(path: &str) {
    match user::run(path) {
        Ok(code) => {
            if code == user_abi::EXIT_FAULTED {
                sprintln!("  {path}: killed by the kernel, see the serial log");
            } else {
                sprintln!("  {path}: exited with code {code}");
            }
        }
        Err(err) => sprintln!("  {path}: {err}"),
    }
}

/// Перечислить каталог.
fn list(path: &str) {
    match fs::list(path) {
        Some(Ok(entries)) => {
            if entries.is_empty() {
                sprintln!("  {path}: empty");
                return;
            }
            // Права и владелец печатаются всегда, а не только там, где они
            // настоящие: на FAT32 они выдуманы значениями по умолчанию, и
            // одинаковые числа во всех строках — это и есть видимая разница
            // между двумя файловыми системами.
            for entry in entries {
                let name = match entry.kind {
                    NodeKind::Directory => alloc::format!("{}/", entry.name),
                    NodeKind::File => entry.name.clone(),
                };
                sprintln!(
                    "  {:04o} {:>4}:{:<4} {:>9}  {}",
                    entry.mode,
                    entry.uid,
                    entry.gid,
                    entry.size,
                    name,
                );
            }
        }
        Some(Err(err)) => sprintln!("  {path}: {err}"),
        None => sprintln!("  no filesystem is mounted"),
    }
}

/// Напечатать файл.
fn show(path: &str) {
    match fs::read(path, CAT_LIMIT) {
        Some(Ok((bytes, total))) => {
            match core::str::from_utf8(&bytes) {
                Ok(text) => sprint!("{text}"),
                // Двоичный файл в окно выводить нельзя: управляющие байты
                // испортят и сетку символов, и терминал на другом конце линии.
                Err(_) => sprintln!("  {path}: not valid UTF-8, {} bytes", bytes.len()),
            }
            if total > bytes.len() as u64 {
                sprintln!();
                sprintln!("  ... {} of {total} bytes shown", bytes.len());
            }
        }
        Some(Err(err)) => sprintln!("  {path}: {err}"),
        None => sprintln!("  no filesystem is mounted"),
    }
}
