//! Программа, у которой есть собственное окно.
//!
//! # Что именно она доказывает
//!
//! Что окно перестало быть частью ядра. До фазы 47a нарисовать своё окно можно
//! было единственным способом — стать модулем ядра; здесь программа просит окно
//! системным вызовом, получает страницы с пикселями и пишет в них как в обычную
//! память, без вызова на точку.
//!
//! Проверок три, и каждая ловит своё.
//!
//! 1. **Поверхность — настоящая память программы.** Узор пишется и читается
//!    обратно, точка за точкой. Отображение, в котором две страницы ведут на
//!    один кадр, — самый вероятный способ ошибиться в этом коде, и константа
//!    его бы не поймала: область, залитая одним значением, после такой ошибки
//!    читается идеально. Узор зависит от координат и ломается сразу.
//! 2. **`munmap` на поверхность отвечает отказом.** Это названный предел
//!    договора, а не случайность: кадры под поверхностью принадлежат окну, и
//!    вернуть их в пул по просьбе программы значило бы отдать их второму
//!    владельцу, пока композитор продолжает из них рисовать. Предел, который
//!    никто не проверяет, — это предел, который однажды перестанет работать.
//! 3. **События доходят.** Программа ждёт их и печатает, что пришло. Клавиша
//!    приезжает символом, а не кодом: так сказано в договоре у `WinEvent::code`.
//!
//! # Аргументы: `winshow [leak]`
//!
//! Слово `leak` означает «уйти, не закрыв окно». Это не небрежность, а вторая
//! половина проверки: снять окно за программой и вернуть его кадры обязано
//! ядро, причём **до** разбора её адресного пространства. Доказывает это строка
//! ядра о возвращённых кадрах — программа о них сказать ничего не может, её к
//! этому моменту уже нет.

#![no_std]
#![no_main]

use user_progs::{
    Line, WIN_CLOSE, WIN_KEY, WIN_POINTER, Window, exit, monotonic_ms, munmap, nanosleep, println,
};

/// Размер окна в точках. Небольшое намеренно: 320×200 — это 63 страницы
/// поверхности, то есть проверка обходит их все за разумное время, а кадров под
/// неё хватает на любой машине, где вообще есть графика.
const WIDTH: u32 = 320;
const HEIGHT: u32 = 200;

/// Сколько всего ждать событий, прежде чем закрыть окно самой.
///
/// Программа не вправе ждать вечно: если событий не будет вовсе, окно обязано
/// закрыться само, иначе прогон стенда упрётся в свой срок и скажет «программа
/// зависла» вместо «событий не пришло».
const WAIT_MS: u64 = 20_000;

/// Пауза между опросами очереди.
///
/// Вызов событий **не ждёт** — так сказано в договоре, — и решать, сколько
/// спать, обязана сама программа. Пятьдесят миллисекунд: двадцать опросов в
/// секунду, чего с избытком хватает человеку, и в двести раз меньше работы, чем
/// у цикла без сна.
const POLL_MS: u32 = 50;

/// Клавиша, которой человек закрывает это окно. Символ, а не код: договор
/// присылает именно его.
const QUIT: u32 = 'q' as u32;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let mut window = match open() {
        Some(window) => window,
        None => exit(1),
    };

    let mut line = Line::new();
    line.str("winshow: window ").num(u64::from(WIDTH));
    line.str("x").num(u64::from(HEIGHT));
    line.str(" opened, surface at ");
    line.num(window.pixels().as_ptr() as u64);
    line.end();

    paint(&mut window);
    if !verify(&mut window) {
        exit(1);
    }

    if window.commit() < 0 {
        println("winshow: FAILED committing the window was refused");
        exit(1);
    }
    println("winshow: committed the whole window");

    check_munmap(&mut window);

    wait_for_events(&window);

    // Уходим, не прибравшись, и делаем это **всегда**. Сказать об этом вслух
    // обязательно: окно остаётся на столе, и снять его должно ядро — до того,
    // как разберёт адресное пространство программы. Вернулись ли кадры, скажет
    // оно само; программы к тому времени уже не будет.
    //
    // Почему не двумя режимами, как было сначала. Аргумент означал вторую
    // команду в сценарии, а вторая команда в серийную линию теряется, пока
    // система много печатает (открытый дефект приёмника UART) — прогон на
    // медленной машине падал именно так, и чем длиннее команда, тем вернее.
    // Проверку «программа закрывает окно сама» делает `sysmon` в сценариях с
    // мышью: там крестик просит, а программа соглашается.
    println("winshow: leaving the window to the teardown");
    exit(0)
}

/// Попросить окно.
///
/// Занятый стол пережидает сама обвязка — там это свойство договора, общее для
/// всех программ, а не забота этой одной.
fn open() -> Option<Window> {
    match Window::open("winshow", WIDTH, HEIGHT) {
        Ok(window) => Some(window),
        Err(code) => {
            let mut line = Line::new();
            line.str("winshow: FAILED opening the window: ").signed(code);
            line.end();
            None
        }
    }
}

/// Цвет точки. Зависит от обеих координат — в этом весь смысл проверки.
///
/// Старший байт — непрозрачность: поверхность копируется на экран как есть, и
/// нули в нём означали бы окно, сквозь которое видно обои.
const fn color(x: u32, y: u32) -> u32 {
    let red = x * 255 / WIDTH;
    let green = y * 255 / HEIGHT;
    0xff00_0000 | (red << 16) | (green << 8) | 0x80
}

/// Нарисовать узор.
fn paint(window: &mut Window) {
    let pixels = window.pixels();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            pixels[(y * WIDTH + x) as usize] = color(x, y);
        }
    }
}

/// Прочитать узор обратно и сверить.
fn verify(window: &mut Window) -> bool {
    let pixels = window.pixels();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let got = pixels[(y * WIDTH + x) as usize];
            let want = color(x, y);
            if got != want {
                let mut line = Line::new();
                line.str("winshow: FAILED point ").num(u64::from(x));
                line.str(",").num(u64::from(y));
                line.str(": wrote ").num(u64::from(want));
                line.str(", read ").num(u64::from(got));
                line.end();
                return false;
            }
        }
    }
    let mut line = Line::new();
    line.str("winshow: wrote and read back ");
    line.num(u64::from(WIDTH) * u64::from(HEIGHT));
    line.str(" points, no mismatches");
    line.end();
    true
}

/// Убедиться, что поверхность нельзя вернуть системе по частям или целиком.
///
/// Отказ здесь — правильный ответ, и программа требует именно его. Успех
/// означал бы, что кадры окна уехали в пул из-под композитора.
fn check_munmap(window: &mut Window) {
    let base = window.pixels().as_ptr() as usize;
    let len = (WIDTH * HEIGHT * 4) as usize;
    let answer = munmap(base, len);
    if answer < 0 {
        let mut line = Line::new();
        line.str("winshow: munmap refused the surface, as promised: ").signed(answer);
        line.end();
        return;
    }
    println("winshow: FAILED munmap took the surface away");
    exit(1);
}

/// Подождать событий и рассказать, что пришло.
fn wait_for_events(window: &Window) {
    let started = monotonic_ms();
    let mut seen = 0u64;

    loop {
        while let Some(event) = window.next_event() {
            seen += 1;
            match event.kind {
                WIN_KEY => {
                    let mut line = Line::new();
                    line.str("winshow: key ");
                    // Символ печатается как число: собрать из него строку
                    // нечем — разбора UTF-8 в обвязке нет, а врать про букву,
                    // которой не видел, программа не станет.
                    line.num(u64::from(event.code));
                    line.end();
                    // `q` заканчивает ожидание. Ветка нужна не для удобства: она
                    // доказывает, что программа клавиши **различает**, а не
                    // просто считает события. Одной напечатанной строки на
                    // любое нажатие для этого мало.
                    if event.code == QUIT {
                        report(seen, monotonic_ms().saturating_sub(started));
                        return;
                    }
                }
                WIN_POINTER => {
                    let mut line = Line::new();
                    line.str("winshow: click at ").signed(i64::from(event.x));
                    line.str(",").signed(i64::from(event.y));
                    line.end();
                }
                WIN_CLOSE => {
                    // Просьба, а не приказ: закрывает окно программа, и здесь
                    // она соглашается сразу — спрашивать ей не о чем.
                    println("winshow: close requested");
                    report(seen, monotonic_ms().saturating_sub(started));
                    return;
                }
                _ => println("winshow: an event of a kind this program does not know"),
            }
        }

        if monotonic_ms().saturating_sub(started) >= WAIT_MS {
            report(seen, monotonic_ms().saturating_sub(started));
            return;
        }
        nanosleep(0, POLL_MS * 1_000_000);
    }
}

/// Сказать, сколько ждали и сколько дождались.
fn report(seen: u64, waited: u64) {
    let mut line = Line::new();
    line.str("winshow: waited ").num(waited);
    line.str(" ms, ").num(seen);
    line.str(" event(s)");
    line.end();
}
