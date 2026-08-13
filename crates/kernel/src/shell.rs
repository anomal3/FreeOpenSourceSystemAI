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
use crate::sync::Mutex;
use crate::vfs::perm::Access;
use crate::vfs::{NodeKind, VfsError};
use crate::{fs, irq, kprint, mm, sched, time, ui, usb, user};

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

/// На сколько оболочка засыпает, не дождавшись ввода.
///
/// Это не период опроса — ввод будит задачу сам (см. [`input::sequence`]). Это
/// предел, после которого она просыпается в любом случае: чтобы шли часы в окне
/// состояния и чтобы сработал предел простоя. Сто миллисекунд — вдвое чаще
/// обновления окна, то есть часы не отстают, а спящая машина просыпается
/// десять раз в секунду вместо ста.
const POLL_PERIOD_MS: u64 = 100;

/// Сколько байт файла показывает `cat`.
///
/// Предел не косметический: файл на носителе может быть любого размера, а окно
/// прокручивается символ за символом. Без предела `cat` на образе в сорок
/// мегабайт занял бы машину надолго и вытеснил бы из окна всё остальное.
const CAT_LIMIT: usize = 4096;

/// Лок вывода оболочки: то, что напечатано одним вызовом, печатается целиком.
///
/// До Phase 13b он был не нужен: задача, начавшая печатать строку, доводила её
/// до конца, потому что процессор у неё никто не отбирал. С вытеснением
/// `write!` из нескольких кусков (а таков любой `write!` с подстановкой) стал
/// разрываться посередине чужим выводом — в журнале появлялись строки вида
/// `freeos> echo shell-count 9: tick 2 of 5`. Читать такой журнал неприятно
/// человеку и невозможно стенду, который ищет в нём подстроки.
///
/// Лок закрывает ровно то, что зависит от ядра. Программа, печатающая строку
/// шестью системными вызовами, по-прежнему может быть вытеснена между ними — и
/// это правильно: в Unix `write` атомарен сам по себе, а не в компании соседних.
static OUT: Mutex<()> = Mutex::new(());

/// Приёмник вывода оболочки.
///
/// Реализует [`fmt::Write`], поэтому годится и для `write!`, и для эха редактора
/// строки. Каждый вызов атомарен; чтобы был атомарен `write!` целиком, есть
/// [`print`].
pub struct Out;

impl fmt::Write for Out {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let _guard = OUT.lock();
        write_raw(text);
        Ok(())
    }
}

/// Напечатать в оболочку одним куском.
///
/// Аргумент — уже собранный `format_args!`, поэтому лок берётся один раз на всю
/// строку, а не на каждую её часть. `SpinLock` не перевходим, поэтому внутри
/// работает [`Raw`], который не запирается повторно.
pub fn print(args: fmt::Arguments<'_>) {
    let _guard = OUT.lock();
    let _ = Raw.write_fmt(args);
}

/// Тот же приёмник, но без лока — для использования под уже взятым [`OUT`].
struct Raw;

impl fmt::Write for Raw {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        write_raw(text);
        Ok(())
    }
}

/// Куда на самом деле уходит вывод оболочки.
fn write_raw(text: &str) {
    if ui::is_active() {
        // Композитор поднят: на экране окна, и загрузочная консоль экран уже
        // отдала. В serial при этом пишем сами — `kprint!` туда бы написал,
        // но заодно попытался бы рисовать.
        crate::serial::_print(format_args!("{text}"));
        ui::write(text);
    } else {
        kprint!("{text}");
    }
}

/// Напечатать в оболочку.
macro_rules! sprint {
    ($($arg:tt)*) => {{
        $crate::shell::print(::core::format_args!($($arg)*));
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
    // задача», то есть счётчик равен единице; служебные задачи в него не входят.
    while sched::alive() > 1 {
        sched::sleep_ms(POLL_PERIOD_MS);
    }

    banner();
    ui::set_cursor(true);
    sprint!("{PROMPT}");

    let mut editor = LineEditor::new();
    let mut idle_since = time::uptime_ms();
    let mut status_at = 0u64;

    loop {
        // Значение счётчика читается **до** разбора очередей: всё, что придёт
        // после этой строки, изменит его и не даст задаче уснуть в конце витка.
        // Прочитать его после разбора значило бы оставить окно, в котором
        // событие уже пришло, а спящий его проспал.
        let seen = input::sequence();

        let now = time::uptime_ms();
        if now.saturating_sub(status_at) >= STATUS_PERIOD_MS {
            status_at = now;
            update_status();
        }

        // Отчёты мыши разбираются перед клавишами: они меняют, какое окно
        // активно, и разобрать их после значило бы отдать нажатие тому окну,
        // которое было активным до щелчка.
        while let Some(event) = input::next_pointer() {
            idle_since = time::uptime_ms();
            ui::dispatch_pointer(event);
        }

        while let Some(event) = input::next_event() {
            idle_since = time::uptime_ms();
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

        if time::uptime_ms().saturating_sub(idle_since) >= IDLE_TIMEOUT_SECONDS * 1000 {
            sprintln!();
            sprintln!("  no input for {IDLE_TIMEOUT_SECONDS} s, finishing the session");
            ui::set_cursor(false);
            return;
        }

        // Спим, а не уступаем: пока пользователь думает, процессору положено
        // стоять. Уступка вернула бы управление сюда же на ближайшем витке
        // планировщика, и оболочка — задача, которая девяносто девять процентов
        // времени ничего не делает, — не давала бы машине простаивать ни такта.
        //
        // Срок сна — до ближайшего обновления окна состояния: часы в нём должны
        // идти и тогда, когда никто ничего не набирает.
        let deadline = irq::ticks() + POLL_PERIOD_MS * u64::from(irq::TIMER_HZ) / 1000;
        sched::block_on_input(deadline, || input::sequence() != seen);
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
    // Чем ядро сочло каждое устройство на шине — здесь, а не только в журнале.
    //
    // Причина конкретная: на машине без последовательного порта журнала нет
    // вовсе, а экранная консоль к этому моменту уже закрыта рабочим столом. На
    // VirtualBox ARM это выглядело так: «Input: none available» при двух
    // поднятых USB-устройствах, и понять, чем они оказались, было нечем — ни
    // одной строки диагностики человек в этот момент не видит.
    if let Some(usb) = usb::xhci::summary() {
        if usb.occupied > 0 {
            sprintln!(
                "USB: {} of {} port(s) brought up, {} keyboard(s), {} pointer(s).",
                usb.devices,
                usb.occupied,
                usb.keyboards,
                usb.mice
            );
            // Причина отказа — тут же. «Порт занят, а устройства нет» без неё
            // означает ровно одно: садиться разбираться заново, уже с журналом,
            // которого на этой машине не бывает.
            for device in usb.attached.iter().filter(|device| device.port != 0) {
                sprintln!(
                    "     port {}: {:04x}:{:04x} {} on interface {} of {}, {}",
                    device.port,
                    device.vendor,
                    device.product,
                    device.kind,
                    device.interface,
                    device.interfaces,
                    match device.descriptor {
                        0 => "boot protocol",
                        _ => "own report descriptor",
                    }
                );
            }
            if let Some((port, stage, err)) = usb.last_error {
                sprintln!("     port {port} stopped while {stage}: {err}");
            }
        }
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
        time::uptime_ms(),
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
        "uptime" => sprintln!("  {} ms, {} timer ticks", time::uptime_ms(), irq::ticks()),
        "date" => date(),
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
        "stat" => {
            if argument.is_empty() {
                sprintln!("  usage: stat <path>");
            } else {
                stat(argument);
            }
        }
        "cat" => {
            if argument.is_empty() {
                sprintln!("  usage: cat <path>");
            } else {
                show(argument);
            }
        }
        // `echo текст > путь` — единственное перенаправление, какое здесь
        // есть, и оно живёт внутри команды, а не в разборе строки. Настоящее
        // перенаправление означает, что вывод команды — это дескриптор, который
        // оболочка вправе подменить; дескрипторов у команд оболочки нет, они
        // печатают напрямую. Обещать `>` для всех команд, сделав его для одной,
        // было бы хуже, чем не обещать вовсе.
        "echo" => match argument.split_once('>') {
            Some((text, path)) => save(path.trim(), text.trim_end()),
            None => sprintln!("  {argument}"),
        },
        "mkdir" => {
            if argument.is_empty() {
                sprintln!("  usage: mkdir <path>");
            } else {
                match fs::mkdir_as(user::session::credentials(), argument, 0o755) {
                    Some(Ok(())) => sprintln!("  created {argument}"),
                    Some(Err(err)) => sprintln!("  mkdir {argument}: {err}"),
                    None => sprintln!("  no filesystem is mounted"),
                }
            }
        }
        "rm" => {
            if argument.is_empty() {
                sprintln!("  usage: rm <path>");
            } else {
                match fs::remove_as(user::session::credentials(), argument) {
                    Some(Ok(())) => sprintln!("  removed {argument}"),
                    Some(Err(err)) => sprintln!("  rm {argument}: {err}"),
                    None => sprintln!("  no filesystem is mounted"),
                }
            }
        }
        "whoami" => whoami(),
        "run" => {
            if argument.is_empty() {
                sprintln!("  usage: run [-b] <path> [args]");
            } else {
                run_program(argument);
            }
        }
        "kill" => {
            if argument.is_empty() {
                sprintln!("  usage: kill <task>");
            } else {
                kill(argument);
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
    sprintln!("  date          the wall clock, local and UTC");
    sprintln!("  mem           physical frames, heap and DMA window");
    sprintln!("  input         key event counters");
    sprintln!("  usb           xHCI controller state");
    sprintln!("  ui            compositor state");
    sprintln!("  tasks         scheduler state");
    sprintln!("  ls [path]     list a directory of the mounted filesystem");
    sprintln!("  stat <path>   size, mode, owner and modification time");
    sprintln!("  cat <path>    print a file, up to {CAT_LIMIT} bytes");
    sprintln!("  echo <text>   print the text back; 'echo t > path' writes a file");
    sprintln!("  mkdir <path>  create a directory");
    sprintln!("  rm <path>     delete a file or an empty directory");
    sprintln!("  whoami        the identity programs are run with");
    sprintln!("  run [-b] <p>  run a program with arguments; -b does not wait");
    sprintln!("  kill <task>   stop a running program by its task number");
    sprintln!("  clear         clear the window");
    sprintln!("  exit          finish the boot and halt");
}

/// Который час.
///
/// Печатается и местное время, и UTC с числом секунд эпохи. Первое нужно
/// человеку, второе — тому, кто сверяет часы системы с внешними: строка со
/// смещением и строка без него позволяют отличить неверно выставленные часы от
/// неверно прочитанного часового пояса, а число секунд не зависит ни от того,
/// ни от другого.
fn date() {
    match time::now_local() {
        Some(local) => {
            let utc = time::now_utc().unwrap_or(local);
            sprintln!("  local  {local} UTC{}", time::offset_text());
            sprintln!("  utc    {utc}");
            sprintln!("  epoch  {} s", time::now_unix().unwrap_or(0));
        }
        // Часы прошивки не ответили. Выдумывать дату здесь нечем и незачем:
        // система работает и без неё, а файлы честно помечаются нулём.
        None => sprintln!("  the wall clock is unknown: the firmware had no clock to read"),
    }
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
            // Печатается только когда случилось: строка «floods 0» ничего не
            // сообщает, а вот отличное от нуля значение означает, что контроллер
            // отдаёт события быстрее, чем драйвер успевает их забирать, — или
            // что кольцо испорчено и разбор не сходится сам.
            if usb.event_floods != 0 {
                sprintln!("  events   {} drains hit the per-pass limit", usb.event_floods);
            }
            // Ноль здесь при работающей клавиатуре означает опрос: контроллер
            // либо не объявил MSI-X, либо на этой машине их некуда направить.
            if usb.interrupts == 0 {
                sprintln!("  irqs     none, events are polled on a timer");
            } else {
                sprintln!("  irqs     {} delivered", usb.interrupts);
            }
            // Пробуждения — цена опроса, выраженная числом. При опросе их сто в
            // секунду, независимо от того, происходит ли что-нибудь; по
            // прерываниям — ровно столько, сколько было событий.
            sprintln!("  wakeups  {} of the service task", usb.services);
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

/// От чьего имени система запускает программы.
///
/// Оболочка при этом исполняется в кольце ноль, и её собственные `cat` и `ls`
/// никаких прав не спрашивают. Так и должно быть: проверять код, который в
/// любом случае может прочитать диск сектор за сектором, значит изображать
/// границу там, где её нет. Настоящая граница — системный вызов, и она видна по
/// тому, что `run /bin/perms` получает отказы там, где `cat` их не получает.
fn whoami() {
    let cred = user::session::credentials();
    user::session::with_name(|name| {
        if name.is_empty() {
            sprintln!("  root ({cred}); no account was read from /etc/passwd");
        } else {
            sprintln!("  {name} ({cred})");
        }
    });
    sprintln!("  programs run with these credentials; the shell itself runs in the kernel");
}

/// Запустить программу вне ядра.
///
/// Программа исполняется отдельной задачей, поэтому «в фоне» — это не режим, а
/// просто отсутствие ожидания: с `-b` оболочка возвращает приглашение сразу и
/// продолжает отвечать, пока программа считает. Без `-b` она ждёт завершения,
/// уступая процессор, — то есть ждёт так же, как ждала бы любая другая задача.
///
/// Строку об окончании печатает сама задача программы, а не оболочка: у
/// фоновой программы к тому моменту никакой оболочки может уже и не быть.
fn run_program(argument: &str) {
    let (line, background) = match argument.strip_prefix("-b ") {
        Some(rest) => (rest.trim(), true),
        None => (argument, false),
    };
    if line.is_empty() {
        sprintln!("  usage: run [-b] <path> [args]");
        return;
    }
    // Всё, что после пути, уезжает программе как аргументы; разбирает строку
    // сама задача, которой предстоит их получить. Оболочка здесь ничего не
    // разбирает намеренно: путь ей нужен только для сообщения об ошибке.
    let path = line.split_whitespace().next().unwrap_or(line);

    match user::spawn(line) {
        Ok(id) => {
            if background {
                sprintln!("  {path}: started as {id}");
            } else {
                sched::wait(id);
            }
        }
        Err(err) => sprintln!("  {path}: {err}"),
    }
}

/// Записать строку в файл, создав его или заменив содержимое.
///
/// Права проверяются как у программы — от имени сеанса, а не от имени ядра.
/// Оболочка исполняется в кольце ноль и могла бы писать мимо проверок; делать
/// так значило бы, что `echo > /root/x` от обычного пользователя проходит там,
/// где `run /bin/save` получает отказ.
fn save(path: &str, text: &str) {
    if path.is_empty() {
        sprintln!("  usage: echo <text> > <path>");
        return;
    }
    let cred = user::session::credentials();

    // Существующий файл открывается и обрезается, отсутствующий создаётся.
    // Порядок именно такой: `create` на существующем имени — это отказ
    // «занято», и подменять им «перезаписать» значило бы врать про причину.
    let node = match fs::resolve_as(cred, path, Access::WRITE) {
        Some(Ok(node)) => Some(node),
        Some(Err(VfsError::NotFound)) => None,
        Some(Err(err)) => {
            sprintln!("  {path}: {err}");
            return;
        }
        None => {
            sprintln!("  no filesystem is mounted");
            return;
        }
    };

    let node = match node {
        Some(node) => match node.truncate(0) {
            Ok(()) => node,
            Err(err) => {
                sprintln!("  {path}: {err}");
                return;
            }
        },
        None => match fs::create_as(cred, path, 0o644) {
            Some(Ok(node)) => node,
            Some(Err(err)) => {
                sprintln!("  {path}: {err}");
                return;
            }
            None => {
                sprintln!("  no filesystem is mounted");
                return;
            }
        },
    };

    // Перевод строки дописывается: файл без него — это строка, которую всякая
    // читающая программа склеит со следующей.
    let mut line = String::from(text);
    line.push('\n');
    match node.write_at(0, line.as_bytes()) {
        Ok(written) => sprintln!("  wrote {written} bytes to {path}"),
        Err(err) => sprintln!("  {path}: {err}"),
    }
}

/// Снять программу по номеру задачи.
///
/// Номер принимается и с решёткой, и без: `tasks` печатает `#5`, и требовать от
/// человека стирать символ, который система сама же и показала, — придирка.
///
/// Оболочка сообщает только о том, что просьба принята. О том, что программа
/// снята, скажет ядро, и скажет тогда, когда это действительно произойдёт: между
/// просьбой и снятием — возврат снимаемой задачи в третье кольцо, то есть
/// событие, которого оболочка не ждёт.
fn kill(argument: &str) {
    let text = argument.strip_prefix('#').unwrap_or(argument);
    let Ok(raw) = text.parse::<u32>() else {
        sprintln!("  kill: '{argument}' is not a task number");
        return;
    };

    let id = sched::TaskId::new(raw);
    match user::request_kill(id) {
        Ok(()) => sprintln!("  kill: {id} asked to stop"),
        Err(err) => sprintln!("  kill: task {id} {err}"),
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
                    "  {:04o} {:>4}:{:<4} {:>9}  {}  {}",
                    entry.mode,
                    entry.uid,
                    entry.gid,
                    entry.size,
                    time::stamp_text(entry.mtime),
                    name,
                );
            }
        }
        Some(Err(err)) => sprintln!("  {path}: {err}"),
        None => sprintln!("  no filesystem is mounted"),
    }
}

/// Напечатать файл.
/// Метаданные одного файла — то же, что показывает `ls`, но по одному имени и
/// с временем в обоих видах.
///
/// Секунды эпохи печатаются рядом с датой не для человека: дату можно
/// напечатать правдоподобной, не зная времени, а число сверяется с внешними
/// часами. Ровно это и делает стенд с файлом, созданным секунду назад.
fn stat(path: &str) {
    let node = match fs::resolve_as(user::session::credentials(), path, Access::NONE) {
        Some(Ok(node)) => node,
        Some(Err(err)) => {
            sprintln!("  {path}: {err}");
            return;
        }
        None => {
            sprintln!("  no filesystem is mounted");
            return;
        }
    };

    let meta = node.metadata();
    let kind = match meta.kind {
        NodeKind::Directory => "directory",
        NodeKind::File => "file",
    };
    sprintln!("  path   {path}");
    sprintln!("  kind   {kind}, {} bytes", meta.size);
    sprintln!("  mode   {:04o}, uid {}, gid {}", meta.mode, meta.uid, meta.gid);
    if meta.mtime == 0 {
        // Ноль — не 1970 год. Так помечены файлы на ФС, которая времени не
        // хранит, и файлы, созданные системой, у которой не было часов.
        sprintln!("  mtime  unknown");
    } else {
        sprintln!("  mtime  {} s = {}", meta.mtime, time::stamp_text(meta.mtime));
        // Возраст — то же самое, сказанное так, чтобы не зависеть от внешних
        // часов. Именно он отвечает на вопрос «файл помечен временем своего
        // создания или временем, доставшимся от установщика»: у только что
        // созданного файла здесь ноль, а у файла с диска, помеченного меткой
        // тома, — минуты. Часы хоста для такого сравнения не нужны.
        if let Some(now) = time::now_unix() {
            sprintln!("  age    {} s", now.saturating_sub(u64::from(meta.mtime)));
        }
    }
}

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
