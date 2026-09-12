//! Проверка договора: каждый вызов, заведённый фазой 44, зовётся отсюда.
//!
//! # Зачем отдельная программа
//!
//! Затем, что вызов, которого никто не зовёт, не проверен ничем. Договор с
//! программами — это то, на что через фазу ляжет libc, и обнаружить в нём
//! ошибку хочется здесь, а не тогда, когда её будет видно как «`fopen` ведёт
//! себя странно».
//!
//! Каждое утверждение печатается **одной** строкой и с числами, которые
//! получены разными путями: `fstat` сверяется со `stat`, сон — с часами,
//! общая позиция у копий дескриптора — двумя чтениями подряд. Строку без чисел
//! стенд проверить не может, а программа, печатающая «всё хорошо», говорит
//! только о том, что дошла до этой строки.
//!
//! Провал печатается как `posix: FAILED <что>` и завершает программу ненулевым
//! кодом: сценарий ищет эту подстроку отдельно, потому что «проверка не
//! напечаталась» и «проверка не прошла» — разные беды.

#![no_std]
#![no_main]

// `Stat` и номер стандартного вывода берутся прямо из договора: обвязка их не
// переэкспортирует, а два имени для одной константы — это два места, где они
// однажды разойдутся.
use user_abi::{FD_STDOUT, Stat};
use user_progs::{
    ABI_VERSION, CLOCK_MONOTONIC, CLOCK_REALTIME, ERR_AGAIN, KIND_FILE, KIND_PIPE, Line, POLL_HUP,
    POLL_IN, PollFd, SEEK_CUR, SEEK_SET, clock, close, dup, exit, fstat, isatty, launch,
    monotonic_ms, nanosleep, open, pipe, poll, read, seek, stat, times, wait,
};

/// Файл, на котором проверяются дескрипторы: он есть в системе всегда и
/// заведомо длиннее восьми байт.
const SAMPLE: &str = "/bin/hello";

/// Сколько миллисекунд просим поспать. Больше тика (10 мс) намеренно: на
/// меньшем округление вверх не отличить от ошибки.
const SLEEP_MS: u64 = 30;

/// Сказать, что проверка не прошла, и закончиться.
fn failed(what: &str) -> ! {
    let mut line = Line::new();
    line.str("posix: FAILED ").str(what);
    line.end();
    exit(1)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let mut line = Line::new();
    line.str("posix: abi version ").num(u64::from(ABI_VERSION));
    line.end();

    check_dup();
    check_fstat();
    check_isatty();
    check_clock();
    check_nanosleep();
    check_times();
    check_poll();

    user_progs::println("posix: all checks passed");
    exit(0)
}

/// Копия дескриптора смотрит на тот же открытый файл — и делит с ним позицию.
///
/// Проверяется это **вторым чтением**, а не тем, что вызов вернул номер: `dup`,
/// отдающий независимую позицию, выглядит работающим ровно до этого места.
fn check_dup() {
    let fd = open(SAMPLE);
    if fd < 0 {
        failed("dup: the sample file does not open");
    }
    let copy = dup(fd);
    if copy < 0 {
        failed("dup: no second descriptor");
    }
    if copy == fd {
        failed("dup: the same number came back");
    }

    // Четыре байта через первый номер, четыре через второй. Если позиция общая,
    // второе чтение возьмёт байты с четвёртого по восьмой, и обе стороны скажут
    // «восемь».
    let mut buffer = [0u8; 4];
    if read(fd, &mut buffer) != 4 {
        failed("dup: the first read came up short");
    }
    if read(copy, &mut buffer) != 4 {
        failed("dup: the second read came up short");
    }

    let through_original = seek(fd, 0, SEEK_CUR);
    let through_copy = seek(copy, 0, SEEK_CUR);
    if through_original != 8 || through_copy != 8 {
        let mut line = Line::new();
        line.str("posix: FAILED dup: positions are ")
            .signed(through_original)
            .str(" and ")
            .signed(through_copy);
        line.end();
        exit(1);
    }

    // Закрытие копии не должно трогать оригинал: файл живёт, пока цел хоть один
    // номер. Проверяется чтением после закрытия — иначе утверждение осталось бы
    // словами.
    close(copy);
    seek(fd, 0, SEEK_SET);
    if read(fd, &mut buffer) != 4 {
        failed("dup: closing the copy took the file with it");
    }
    close(fd);

    let mut line = Line::new();
    line.str("posix: dup shares the position, both say ").num(8);
    line.end();
}

/// `fstat` говорит о файле то же, что `stat`, а о канале — что он канал.
fn check_fstat() {
    let fd = open(SAMPLE);
    if fd < 0 {
        failed("fstat: the sample file does not open");
    }

    let mut by_name = Stat::default();
    if stat(SAMPLE, &mut by_name) != 0 {
        failed("fstat: stat by name refused");
    }
    let mut by_fd = Stat::default();
    if fstat(fd, &mut by_fd) != 0 {
        failed("fstat: refused on an open file");
    }
    close(fd);

    if by_fd.size != by_name.size || by_fd.kind != by_name.kind {
        failed("fstat: the two answers disagree");
    }
    if by_fd.kind != KIND_FILE {
        failed("fstat: a regular file did not call itself one");
    }

    let mut line = Line::new();
    line.str("posix: fstat matches stat, size ")
        .num(by_fd.size)
        .str(" kind ")
        .num(u64::from(by_fd.kind));
    line.end();
}

/// Терминал отличается от файла, и это видно вызовом.
fn check_isatty() {
    let fd = open(SAMPLE);
    if fd < 0 {
        failed("isatty: the sample file does not open");
    }
    let on_file = isatty(fd);
    close(fd);
    if on_file {
        failed("isatty: a regular file called itself a terminal");
    }

    // Вывод этой программы стенд читает через окно оболочки, то есть терминал;
    // запущенная через канал, она напечатает ноль — и это тоже правильный
    // ответ, поэтому число печатается, а не проверяется.
    let mut line = Line::new();
    line.str("posix: isatty stdout ")
        .num(u64::from(isatty(FD_STDOUT as i64)))
        .str(", file 0");
    line.end();
}

/// Монотонные часы идут вперёд, и идут вместе со сном.
fn check_clock() {
    let Some(first) = clock(CLOCK_MONOTONIC) else {
        failed("clock: monotonic refused");
    };
    if clock(CLOCK_REALTIME).is_none() {
        failed("clock: realtime refused");
    }
    if first.nanos >= 1_000_000_000 {
        failed("clock: nanos are not below a second");
    }

    let before = monotonic_ms();
    nanosleep(0, 50_000_000);
    let after = monotonic_ms();
    if after < before {
        failed("clock: monotonic went backwards");
    }

    let mut line = Line::new();
    line.str("posix: monotonic advanced ").num(after - before).str(" ms over 50");
    line.end();
}

/// Сон длится **не меньше** запрошенного.
///
/// Проверяется по часам, а не по слову вызова: `nanosleep`, округлявший вниз,
/// вернулся бы раньше срока и выглядел бы исправным.
fn check_nanosleep() {
    let before = monotonic_ms();
    if nanosleep(0, (SLEEP_MS * 1_000_000) as u32) != 0 {
        failed("nanosleep: refused");
    }
    let slept = monotonic_ms() - before;
    if slept < SLEEP_MS {
        let mut line = Line::new();
        line.str("posix: FAILED nanosleep: asked ")
            .num(SLEEP_MS)
            .str(" ms, slept ")
            .num(slept);
        line.end();
        exit(1);
    }

    // Наносекунды сверх секунды — не «почти секунда», а ошибка в счёте, и вызов
    // обязан отказать, а не проспать лишнее.
    if nanosleep(0, 1_000_000_000) == 0 {
        failed("nanosleep: accepted more than a second of nanoseconds");
    }

    let mut line = Line::new();
    line.str("posix: nanosleep asked ").num(SLEEP_MS).str(" ms, slept ").num(slept);
    line.end();
}

/// Истраченное время не превышает прожитого.
///
/// Это и есть то единственное, что можно утверждать о нём наверняка: сколько
/// именно достанется программе, решает планировщик.
fn check_times() {
    let Some(spent) = times() else {
        failed("times: refused");
    };
    if spent.uptime_ms == 0 {
        failed("times: the system claims to have just started");
    }
    if spent.cpu_ms > spent.uptime_ms {
        failed("times: more cpu time than the system has been up");
    }

    let mut line = Line::new();
    line.str("posix: times cpu ")
        .num(spent.cpu_ms)
        .str(" ms of uptime ")
        .num(spent.uptime_ms);
    line.end();
}

/// Два процесса, соединённые каналом: `poll` просыпается на чужом выводе.
///
/// Это и есть проверка, ради которой фаза заводила `poll`: до неё программа
/// умела ждать ровно один источник — тот, который назвала в `read`.
fn check_poll() {
    let Ok((reader, writer)) = pipe() else {
        failed("poll: no pipe");
    };

    // Запускаем чужую программу, отдав ей пишущий конец под стандартный вывод.
    let task = launch("/bin/hello", None, user_progs::KEEP, writer);
    if task < 0 {
        failed("poll: the child did not start");
    }
    // Свою копию пишущего конца закрываем **немедленно**: пока она жива, конца
    // файла на другом конце не наступит никогда — мы сами и есть тот писатель,
    // которого ждут.
    close(writer);

    let mut fds = [PollFd { fd: reader, wanted: POLL_IN, ready: 0 }];
    let ready = poll(&mut fds, 30_000);
    if ready <= 0 {
        failed("poll: nothing became readable");
    }
    if fds[0].ready & POLL_IN == 0 {
        failed("poll: woke up without anything to read");
    }

    let mut line = Line::new();
    line.str("posix: poll saw the pipe readable, ").signed(ready).str(" ready");
    line.end();

    // Канал на той стороне — это канал и с этой: `fstat` обязан сказать о нём
    // то, чего не говорит ни об одном файле.
    let mut about = Stat::default();
    if fstat(reader, &mut about) != 0 || about.kind != KIND_PIPE {
        failed("poll: fstat did not recognise the pipe");
    }

    // Дочитываем до конца — и вот здесь начинается настоящая работа `poll`.
    //
    // Чтение канала **через дескриптор не ждёт**: пустой канал с живым
    // писателем отвечает `ERR_AGAIN`, а не останавливает программу. Так решено
    // в ядре намеренно — ждать, держа таблицу дескрипторов под локом, значит
    // остановить всех, кто в неё заглянет, включая того, кто этот канал
    // наполняет. Значит ждать обязан тот, кому надо, и ждать ему нечем, кроме
    // этого вызова.
    //
    // Первая версия этой проверки считала `ERR_AGAIN` отказом и падала на
    // втором чтении: `hello` успевает написать не всё разом. Ошибка стоит того,
    // чтобы остаться записанной, — она ровно та, которую сделает всякий, кто
    // прочтёт «read вернул отрицательное» как «read сломался».
    //
    // Ноль означает конец: писателей не осталось, то есть программа на том
    // конце закончилась.
    let mut buffer = [0u8; 128];
    let mut total = 0u64;
    let mut waits = 0u64;
    loop {
        let got = read(reader, &mut buffer);
        if got == ERR_AGAIN {
            // Канал пуст, но писатель жив. Засыпаем на нём: `poll` вернётся и
            // когда появятся байты, и когда писатель уйдёт, — второе сообщается
            // отдельным признаком, и именно поэтому цикл не может зависнуть.
            let mut quiet = [PollFd { fd: reader, wanted: POLL_IN, ready: 0 }];
            if poll(&mut quiet, 30_000) <= 0 {
                failed("poll: the pipe went quiet with a writer still alive");
            }
            waits += 1;
            continue;
        }
        if got < 0 {
            failed("poll: reading the pipe failed");
        }
        if got == 0 {
            break;
        }
        total += got as u64;
    }
    let mut line = Line::new();
    line.str("posix: poll waited on the pipe ").num(waits).str(" time(s)");
    line.end();
    if total == 0 {
        failed("poll: the child sent nothing");
    }

    // Писателей нет — и `poll` обязан сказать об этом сам, не дожидаясь срока.
    // Без этого программа, ждущая данных на закрытом канале, ждала бы вечно.
    let mut hung = [PollFd { fd: reader, wanted: POLL_IN, ready: 0 }];
    if poll(&mut hung, 0) <= 0 || hung[0].ready & POLL_HUP == 0 {
        failed("poll: the hangup went unreported");
    }
    close(reader);

    // Задача обязана уже закончиться: конец файла наступил, значит её конец
    // канала закрылся, а закрывается он вместе с ней.
    let code = wait(task);
    let mut line = Line::new();
    line.str("posix: poll saw the hangup after ")
        .num(total)
        .str(" bytes, child exited with ")
        .signed(code);
    line.end();
}
