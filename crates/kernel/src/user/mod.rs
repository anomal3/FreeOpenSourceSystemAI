//! Пользовательское пространство: программы, исполняющиеся вне ядра.
//!
//! # Что здесь есть
//!
//! Программа читается с файловой системы, раскладывается по страницам,
//! помеченным доступными из третьего кольца (EL0 на AArch64), и запускается там.
//! Из этого режима нельзя ни прочитать память ядра, ни выполнить его код, ни
//! обратиться к устройству — единственная дверь наружу это системный вызов.
//!
//! С Phase 12c у программы есть чем пользоваться и чьё имя носить: файловые
//! вызовы ([`syscall`], таблица дескрипторов в [`files`]) и личность сеанса
//! ([`session`]), от имени которой проверяются права на каждый открываемый
//! файл. До этого `mode`, `uid` и `gid` лежали на диске и не значили ничего.
//!
//! С Phase 12b у каждого запуска **своё адресное пространство**: корень таблиц
//! страниц копируется с ядерного, окно программы живёт в отдельной записи этого
//! корня (см. [`space`]), а по завершении программы всё окно возвращается в пул
//! кадров. Отсюда три следствия, которых не было раньше:
//!
//! * в таблицах ядра память программы не отображена вовсе — не «отображена, но
//!   недоступна», а отсутствует; проверяется это обходом таблиц при каждом
//!   запуске, а не рассуждением;
//! * память программы не переживает её саму: страницы возвращаются в пул, а не
//!   остаются висеть окном на всю работу системы;
//! * две программы не делят ничего, даже когда живут по одним и тем же
//!   виртуальным адресам.
//!
//! С Phase 13a программа — это **задача планировщика**. У неё свой стек ядра,
//! своё адресное пространство, своя таблица открытых файлов и свой номер, тот
//! же, что показывает `tasks`. Программ поэтому бывает несколько сразу: пока
//! одна считает, вторая печатает, а оболочка отвечает на команды. Всё, что
//! раньше существовало в одном экземпляре на систему, стало состоянием задачи —
//! от таблицы дескрипторов до стека, на который приходит ловушка из третьего
//! кольца (см. [`sched::UserMachine`]).
//!
//! С Phase 13b программу **вытесняет таймер**. Уступать процессор ей больше не
//! обязательно: квант кончается, прерывание приходит прямо в третье кольцо, и
//! задача сменяется на кадре ловушки — том самом, который с Phase 13a лежит на
//! её собственном стеке ядра. Программа с вечным циклом и без единого
//! системного вызова перестала занимать машину; см. `/bin/spin`.
//!
//! С Phase 13c программу можно **снять**: `kill <номер>` в оболочке ставит
//! флаг, а [`check_kill`] на ближайшем возврате в третье кольцо уводит
//! управление в ту же точку, куда его уводит отказ. Дальше работает уже
//! написанная уборка: адресное пространство разбирается, дескрипторы
//! закрываются, задача завершается своим кодом. Из этого следует и граница:
//! снять можно только программу, потому что только у неё есть точка, в которую
//! ядро умеет вернуться, — кадр [`arch::enter_user`] на её стеке.
//!
//! # Чего здесь пока нет
//!
//! **Настоящего входа в систему.** Личность сеанса берётся из `/etc/passwd`
//! ([`session`]) и не проверяется паролем: спросить пароль, не показав его на
//! экране, пока нечем. Права при этом проверяются по-настоящему — см.
//! [`crate::vfs::perm`].
//!
//! # Почему окно ограничено сверху
//!
//! Размер окна фиксирован ([`IMAGE_PAGES`] страниц под образ и [`STACK_PAGES`]
//! под стек), а не выведен из файла программы. Причина — не в простоте
//! реализации: фиксированный размер ограничивает программу сверху известным
//! числом, а сегмент, не поместившийся в окно, отвергается загрузчиком до
//! первой записи в память, а не после.

pub mod elf;
pub mod files;
pub mod session;
pub mod space;
pub mod syscall;

use core::fmt::Write as _;

use alloc::vec::Vec;

use crate::mm::{FrameAllocator, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::sync::Mutex;
use crate::vfs::perm::{Access, Credentials};
use crate::{arch, kprintln, sched};

use space::Space;

/// Начало окна программы: 512 ГиБ.
///
/// Тот же адрес записан в компоновочном сценарии `crates/user-progs/user.ld`;
/// разъехаться они не могут незаметно — сегмент за пределами окна отвергается
/// загрузчиком с [`elf::ElfError::OutOfWindow`].
///
/// Выравнено на границу записи корневой таблицы намеренно: вся память программы
/// обязана лежать под одной такой записью, иначе освобождение пространства
/// перестало бы быть обходом одного поддерева (см. [`space::WINDOW_SLOT`]).
pub const WINDOW_BASE: usize = 0x0000_0080_0000_0000;

/// Сколько страниц отведено под образ программы.
///
/// Полтора мегабайта. Было полмегабайта, и этого хватало ровно до фазы 37: с
/// появлением `sshd` в системе завелась программа, которая тянет за собой
/// Curve25519, Ed25519, SHA-256 и ChaCha20 — и в отладочной сборке весит
/// мегабайт. Отказ выглядел при этом обманчиво: загрузчик отвергал не размер, а
/// **сегмент**, потому что ядро дочитывало файл до предела и получало заголовок,
/// обещающий больше байт, чем прочитано.
///
/// Верхняя граница здесь не произвольна: до вершины стека два мегабайта, и
/// образ обязан кончиться раньше, оставив неотображённую полосу между собой и
/// стеком. Настоящее решение — память по запросу (фаза 40), после которой
/// размер программы перестанет быть константой ядра вовсе.
const IMAGE_PAGES: usize = 384;
/// Сколько страниц отведено под стек программы.
const STACK_PAGES: usize = 8;

/// Размер образа в байтах.
const IMAGE_BYTES: usize = IMAGE_PAGES * PAGE_SIZE;

/// Вершина стека программы. Отстоит от образа на два мегабайта — чтобы
/// переполнение стека упиралось в неотображённые страницы, а не в конец образа.
pub const STACK_TOP: usize = WINDOW_BASE + 0x0020_0000;
/// Низ стека.
const STACK_BASE: usize = STACK_TOP - STACK_PAGES * PAGE_SIZE;

/// Сколько байт файла программы ядро согласно прочитать.
///
/// Ровно столько же, сколько отведено под образ: файл больше окна всё равно не
/// разложится, а обрезанное чтение — худший из способов об этом сообщить.
/// Именно оно и произошло, когда `sshd` перерос прежние полмегабайта: ядро
/// молча дочитывало файл до предела, а загрузчик потом жаловался на «сегмент,
/// противоречащий сам себе», уводя расследование к формату ELF вместо размера.
const MAX_FILE: usize = IMAGE_BYTES;

/// Почему программа не запустилась.
#[derive(Debug, Clone, Copy)]
pub enum Error {
    /// Файловой системы нет.
    NoFilesystem,
    /// Файл не прочитался.
    Read(crate::vfs::VfsError),
    /// Файл не является пригодной программой.
    Elf(elf::ElfError),
    /// Не хватило кадров под окно программы.
    OutOfMemory,
    /// Не удалось построить адресное пространство или отобразить страницу.
    Map(crate::mm::MapError),
    /// Страница получилась одновременно записываемой и исполняемой.
    WriteExecute(usize),
    /// Задача уже исполняет программу. Вложенного входа в третье кольцо не
    /// бывает: стек возврата у задачи один, и второй вход затёр бы его.
    AlreadyRunning,
    /// В таблице планировщика нет места под ещё одну задачу.
    TooManyTasks,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoFilesystem => f.write_str("no filesystem is mounted"),
            Self::Read(err) => write!(f, "{err}"),
            Self::Elf(err) => write!(f, "{err}"),
            Self::OutOfMemory => f.write_str("not enough memory for the program window"),
            Self::Map(err) => write!(f, "building the address space failed: {err}"),
            Self::WriteExecute(page) => write!(
                f,
                "page {page} of the image would be writable and executable at once"
            ),
            Self::AlreadyRunning => f.write_str("this task already runs a program"),
            Self::TooManyTasks => f.write_str("the task table is full"),
        }
    }
}

/// Программа, привязанная к задаче планировщика.
///
/// Всё, что раньше существовало в одном экземпляре на систему: адресное
/// пространство, таблица открытых файлов и признак «сейчас исполняется код в
/// третьем кольце». Каждое из них — состояние программы, а программ теперь
/// столько же, сколько задач, которые их запустили.
pub struct Program {
    space: Space,
    /// Открытые файлы. Публично, потому что ими распоряжаются системные вызовы
    /// ([`syscall`]), а прятать таблицу за парой одинаковых методов означало бы
    /// переписать её интерфейс дважды.
    pub files: files::Table,
    /// Исполняется ли прямо сейчас код в третьем кольце.
    ///
    /// Нужно обработчику отказов: исключение «из третьего кольца» снимает
    /// программу, но только если она действительно там. Отказ в момент, когда
    /// программы нет, означает испорченное состояние процессора, и возвращаться
    /// в этом случае некуда.
    running: bool,
    /// Просили ли снять эту программу.
    ///
    /// Флаг, а не немедленное снятие, по той же причине, по которой вытеснение
    /// отложено до конца прерывания: тот, кто просит, исполняется в другой
    /// задаче и о состоянии этой не знает ничего. Разбирает флаг [`check_kill`]
    /// — на границе возврата в третье кольцо, где заведомо нет ни удерживаемого
    /// лока, ни половины начатой работы.
    kill_requested: bool,
    /// От чьего имени исполняется **эта** программа.
    ///
    /// С Phase 33 личность перестала быть свойством системы и стала свойством
    /// программы. Причина конкретная: супервизор служб исполняется от root, а
    /// служба — от того, кто записан в её описании, и общая на всех личность
    /// сеанса сделала бы это описание украшением. Отсюда же следует, что
    /// проверка прав в системном вызове обязана спрашивать **программу**, а не
    /// сеанс, — см. [`credentials`].
    cred: Credentials,
}

/// Программы по слотам задач планировщика.
///
/// Таблица, а не поле задачи, и это разделение обязанностей: планировщику от
/// программы нужны три машинных числа ([`sched::UserMachine`]), которые он
/// обязан переставлять сам, а всё остальное — дело этого модуля, и знать о нём
/// планировщику незачем.
static PROGRAMS: Mutex<[Option<Program>; sched::MAX_TASKS]> =
    Mutex::new([const { None }; sched::MAX_TASKS]);

/// Сделать что-нибудь с программой текущей задачи.
///
/// `None`, если задача программы не исполняет, — например, когда системный
/// вызов пришёл неизвестно откуда.
pub fn with_current<R>(f: impl FnOnce(&mut Program) -> R) -> Option<R> {
    // Слот спрашивается до захвата лока: так два лока никогда не удерживаются
    // одновременно, и порядок их взятия перестаёт быть вопросом.
    let slot = sched::current_slot();
    let mut table = PROGRAMS.lock();
    table.get_mut(slot).and_then(Option::as_mut).map(f)
}

/// Корень таблиц страниц программы текущей задачи.
#[must_use]
pub fn current_space_root() -> Option<PhysAddr> {
    with_current(|program| program.space.root())
}

/// От чьего имени исполняется то, что просит ядро прямо сейчас.
///
/// Личность **программы**, а не сеанса, и это разные вещи с Phase 33: службу
/// запускает супервизор от root, а исполняется она от своего пользователя. Все
/// проверки прав в системных вызовах спрашивают отсюда.
///
/// Сеанс остаётся ответом по умолчанию — для задач ядра, у которых программы
/// нет вовсе: оболочка, чтение `/etc/passwd` при загрузке, файловый менеджер до
/// того, как стал программой. Личность сеанса тоже спрашивается, а не
/// подставляется root: у кода в кольце ноль права всё равно ничего не отнимут,
/// но `echo > /root/x` от имени обычного пользователя обязан получить тот же
/// отказ, что и программа.
#[must_use]
pub fn credentials() -> Credentials {
    with_current(|program| program.cred).unwrap_or_else(session::credentials)
}

/// Виртуальный адрес кадра в прямом отображении — через него ядро пишет в
/// память программы.
///
/// Пишет именно так, а не по адресам самой программы: её окно в таблицах ядра
/// не отображено, и обращение к [`WINDOW_BASE`] из ядра — это отказ страницы, а
/// не запись в образ.
fn frame_bytes(frame: PhysAddr) -> *mut u8 {
    frame.to_direct_map().as_mut_ptr::<u8>()
}

/// Скопировать данные в образ по смещению от [`WINDOW_BASE`].
fn write_image(frames: &[PhysAddr], offset: usize, data: &[u8]) {
    let mut written = 0;
    while written < data.len() {
        let at = offset + written;
        let page = at / PAGE_SIZE;
        let in_page = at % PAGE_SIZE;
        let Some(frame) = frames.get(page) else {
            return;
        };
        let chunk = (PAGE_SIZE - in_page).min(data.len() - written);
        // SAFETY: страница внутри окна (проверено `get`), смещение внутри
        // страницы, длина обрезана по её концу. Источник — срез файла, приёмник
        // — кадр в прямом отображении; пересечься они не могут.
        unsafe {
            core::ptr::copy_nonoverlapping(
                data.as_ptr().add(written),
                frame_bytes(*frame).add(in_page),
                chunk,
            );
        }
        written += chunk;
    }
}

/// Разложить аргументы в верхней странице стека программы.
///
/// Возвращает `(argc, адрес массива argv, новую вершину стека)` — всё в
/// адресах программы.
///
/// # Раскладка
///
/// Сверху вниз: сначала сами строки (каждая с завершающим нулём), под ними
/// массив указателей на них, и всё это — выше новой вершины стека. Программа
/// получает `argc` и адрес массива регистрами, а не находит их на стеке: у двух
/// архитектур это были бы два разных соглашения, а регистры одинаковы —
/// System V и AAPCS64 передают первые два аргумента одинаково по смыслу.
///
/// Строки заканчиваются нулём, потому что иначе их длину пришлось бы передавать
/// отдельным массивом: программе нужен `&str`, а `&str` — это адрес и длина.
/// Ноль в конце — тот же способ, которым это решает C, и здесь он выбран не из
/// уважения к традиции, а потому что второй массив пришлось бы держать в том же
/// стеке и объяснять его формат в договоре.
///
/// # Что если не помещается
///
/// Лишние аргументы отбрасываются, а не роняют запуск. Ограничение — одна
/// страница на всё; аргумент, который в неё не влез, — это командная строка в
/// четыре килобайта, и она встречается там, где кто-то её подставил, а не там,
/// где человек её набрал.
fn place_args(frame: PhysAddr, args: &[&str]) -> (usize, usize, usize) {
    /// Сколько места отдано аргументам: вся верхняя страница стека.
    const AREA: usize = PAGE_SIZE;
    /// Выравнивание вершины стека, которого требуют оба соглашения о вызовах.
    const ALIGN: usize = 16;

    // Смещение внутри страницы; растёт сверху вниз, как и сам стек.
    let mut top = AREA;
    // Кадр принадлежит стеку этой программы и доступен через прямое отображение
    // ядра; писать в него до входа в третье кольцо некому больше.
    let page = frame_bytes(frame);

    let mut pointers = Vec::new();
    if pointers.try_reserve_exact(args.len()).is_err() {
        return (0, 0, STACK_TOP - ALIGN);
    }

    for arg in args {
        let needed = arg.len() + 1;
        // Место под массив указателей резервируется здесь же: без этой проверки
        // строки могли бы занять страницу целиком, и массиву не осталось бы
        // ничего.
        let reserved = (pointers.len() + 1) * size_of::<usize>() + ALIGN;
        if top < needed + reserved {
            break;
        }
        top -= needed;
        // SAFETY: `top` и длина проверены выше — запись целиком внутри страницы.
        unsafe {
            core::ptr::copy_nonoverlapping(arg.as_ptr(), page.add(top), arg.len());
            page.add(top + arg.len()).write(0);
        }
        pointers.push(STACK_TOP - AREA + top);
    }

    // Массив указателей — под строками, выровненный по размеру указателя.
    top &= !(size_of::<usize>() - 1);
    let array_bytes = pointers.len() * size_of::<usize>();
    top -= array_bytes;
    for (index, pointer) in pointers.iter().enumerate() {
        // SAFETY: место под массив зарезервировано в цикле выше.
        unsafe {
            page.add(top + index * size_of::<usize>())
                .cast::<usize>()
                .write_unaligned(*pointer);
        }
    }

    let argv = STACK_TOP - AREA + top;
    let stack = (argv - ALIGN) & !(ALIGN - 1);
    (pointers.len(), argv, stack)
}

/// Выделить кадр под страницу программы.
fn take_frame() -> Result<PhysAddr, Error> {
    crate::mm::frame::with(|frames| frames.allocate())
        .flatten()
        .ok_or(Error::OutOfMemory)
}

/// Вернуть в пул кадр, который не попал в таблицы программы.
///
/// Нужно ровно на одном пути: отображение отказало уже после выделения кадра.
/// Такой кадр не принадлежит ни одному дереву, и разбор адресного пространства
/// его не найдёт — вернуть его может только тот, кто его взял.
fn return_frame(frame: PhysAddr) {
    crate::mm::frame::with(|pool| {
        // SAFETY: кадр выдан этим же аллокатором, отображения на него не
        // создано (иначе он сюда не попал бы), и больше его никто не держит.
        unsafe { pool.free(frame) };
    });
}

/// Права страниц образа: объединение прав сегментов, которые их задевают.
///
/// Страница, не задетая ни одним сегментом, остаётся доступной только на
/// чтение: она внутри окна, и оставлять её записываемой незачем.
fn image_flags(image: &elf::Image<'_>) -> Result<[PageFlags; IMAGE_PAGES], Error> {
    let mut flags = [PageFlags::READ.union(PageFlags::USER); IMAGE_PAGES];
    let mut segments = 0usize;

    for segment in image.segments((WINDOW_BASE, WINDOW_BASE + IMAGE_BYTES)) {
        let segment = segment.map_err(Error::Elf)?;
        segments += 1;

        let offset = segment.vaddr - WINDOW_BASE;
        let first = offset / PAGE_SIZE;
        let last = (offset + segment.memsz - 1) / PAGE_SIZE;
        let segment_flags = PageFlags::from_segment_flags(segment.flags).union(PageFlags::USER);
        for page in first..=last.min(IMAGE_PAGES - 1) {
            flags[page] = flags[page].union(segment_flags);
        }
    }

    if segments == 0 {
        return Err(Error::Elf(elf::ElfError::NoSegments));
    }

    // Проверка до первого отображения, а не после: `map` откажет и сам, но
    // отказать на середине окна значит оставить половину страниц с правами, о
    // которых уже никто не спрашивал.
    for (page, page_flags) in flags.iter().enumerate() {
        if page_flags.contains(PageFlags::WRITE) && page_flags.contains(PageFlags::EXEC) {
            return Err(Error::WriteExecute(page));
        }
    }

    Ok(flags)
}

/// Прочитать образ программы целиком.
///
/// Читает **ядро**, а не программа, и права на чтение файла для этого не
/// требуются — так же, как в Unix, где `execve` довольствуется битом `x`.
/// Разница осмысленная: содержимое исполняемого файла не отдаётся тому, кто его
/// запустил, оно отдаётся процессору.
fn read_image(node: &dyn crate::vfs::Node) -> Result<Vec<u8>, Error> {
    let meta = node.metadata();
    if meta.kind != crate::vfs::NodeKind::File {
        return Err(Error::Read(crate::vfs::VfsError::WrongKind));
    }
    let want = (meta.size as usize).min(MAX_FILE);

    let mut bytes = Vec::new();
    // `try_reserve_exact`, а не `vec![]`: размер пришёл с носителя, и отказ
    // аллокатора обязан стать ошибкой, а не паникой.
    bytes.try_reserve_exact(want).map_err(|_| Error::OutOfMemory)?;
    bytes.resize(want, 0);

    let read = node.read_at(0, &mut bytes).map_err(Error::Read)?;
    bytes.truncate(read);
    Ok(bytes)
}

/// Что получилось из разложенного образа: точка входа и верхняя страница стека.
///
/// Кадр стека нужен снаружи, чтобы записать туда аргументы. Писать их через
/// адреса программы было бы нельзя: к моменту, когда пространство активно, и
/// возможно только через прямое отображение — тем же способом, каким сюда
/// попадает содержимое сегментов.
struct Loaded {
    entry: usize,
    stack_top_frame: PhysAddr,
}

/// Разложить программу по её адресному пространству и вернуть точку входа.
fn load(space: &mut Space, bytes: &[u8]) -> Result<Loaded, Error> {
    let image = elf::Image::parse(bytes).map_err(Error::Elf)?;
    let flags = image_flags(&image)?;

    let mut pages = Vec::new();
    pages.try_reserve_exact(IMAGE_PAGES).map_err(|_| Error::OutOfMemory)?;

    // Кадры не обнуляются здесь: аллокатор выдаёт их чистыми по контракту, и это
    // ровно то, что требуется в двух местах сразу — `.bss` программы обязан быть
    // нулевым, а память от чего бы то ни было предыдущего не должна ей достаться.
    for page in 0..IMAGE_PAGES {
        // Кадры, уже попавшие в таблицы, возвращать здесь не надо: они внутри
        // окна, и `Drop` пространства вернёт их вместе с ним.
        let frame = take_frame()?;
        // SAFETY: кадр только что выделен под эту программу и больше никому не
        // принадлежит; при разборе пространства он вернётся в пул.
        let mapped = unsafe {
            space.map(VirtAddr::new(WINDOW_BASE + page * PAGE_SIZE), frame, flags[page])
        };
        if let Err(err) = mapped {
            // Отображения не появилось — значит поддерево окна этот кадр не
            // содержит, и вернуть его надо здесь.
            return_frame(frame);
            return Err(Error::Map(err));
        }
        pages.push(frame);
    }

    let mut stack_top_frame = PhysAddr::new(0);
    for page in 0..STACK_PAGES {
        let frame = take_frame()?;
        // Верхняя страница — та, в которую упирается `STACK_TOP`, и именно в
        // неё лягут аргументы.
        if page == STACK_PAGES - 1 {
            stack_top_frame = frame;
        }
        // SAFETY: см. выше; стек — обычная память программы на чтение и запись.
        let mapped = unsafe {
            space.map(
                VirtAddr::new(STACK_BASE + page * PAGE_SIZE),
                frame,
                PageFlags::READ.union(PageFlags::WRITE).union(PageFlags::USER),
            )
        };
        if let Err(err) = mapped {
            return_frame(frame);
            return Err(Error::Map(err));
        }
    }

    // Содержимое пишется последним и через прямое отображение: права страницы в
    // пространстве программы к этому моменту уже выставлены, и сегмент кода там
    // на запись недоступен.
    for segment in image.segments((WINDOW_BASE, WINDOW_BASE + IMAGE_BYTES)) {
        let segment = segment.map_err(Error::Elf)?;
        let source = &image.bytes()[segment.file_offset..segment.file_offset + segment.filesz];
        write_image(&pages, segment.vaddr - WINDOW_BASE, source);
    }

    Ok(Loaded { entry: image.entry, stack_top_frame })
}

/// Напечатать то, ради чего фаза затевалась: где лежит программа и чего о ней
/// не знает ядро.
///
/// Строки читает не только человек — их читает стенд (`cargo xtask test`).
/// Утверждение «окно программы в таблицах ядра отсутствует» иначе нечем
/// проверить: снимок экрана его не покажет, а отсутствие отказа доказывает
/// только то, что ядро туда не обращалось.
fn report(space: &Space, entry: usize) {
    match space.translate(VirtAddr::new(entry)) {
        Some((frame, flags)) => kprintln!(
            "  user        : space {:?}, entry maps to {frame:?} {flags:?}",
            space.root()
        ),
        None => kprintln!("  user        : WARNING: the entry point is not mapped in its own space"),
    }

    match space::kernel_maps(VirtAddr::new(entry)) {
        None => kprintln!("  user        : the kernel space maps nothing at {entry:#018x}"),
        Some((frame, flags)) => kprintln!(
            "  user        : WARNING: the kernel space maps {entry:#018x} to {frame:?} {flags:?}"
        ),
    }
}

/// Загрузить программу по пути и исполнить её в текущей задаче.
///
/// Возвращает код, с которым программа завершилась.
fn run(path: &str, args: &[&str], cred: Credentials) -> Result<i64, Error> {
    // Право исполнить спрашивается до чтения файла, и спрашивается от имени
    // **той личности, с которой программа будет исполняться**. Это тот же
    // вопрос, который в Unix задаёт `execve`, и задавать его обязано ядро:
    // программа, которой не дали бы прочитать файл, не должна получать его в
    // виде исполняемого кода. Отсюда же и [`Access::SEARCH`] на самом файле —
    // бит `x`, а не `r`.
    //
    // Порядок здесь имеет значение: спросить от имени запускающего, а
    // исполнить от имени другого — это ровно та дыра, ради закрытия которой
    // права запуска и стали свойством программы.
    let node = match crate::fs::resolve_as(cred, path, Access::SEARCH) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return Err(Error::Read(err)),
        None => return Err(Error::NoFilesystem),
    };
    let bytes = read_image(&*node)?;

    let mut space = Space::new().map_err(Error::Map)?;
    let loaded = load(&mut space, &bytes)?;
    let entry = loaded.entry;
    let root = space.root();

    // Аргументы кладутся в стек программы до входа в третье кольцо. Вершина
    // стека при этом опускается под них и выравнивается на 16: этого требуют
    // оба соглашения о вызовах, и невыровненный стек ломается не сразу, а на
    // первой же операции с вектором.
    let (argc, argv, stack) = place_args(loaded.stack_top_frame, args);

    let id = sched::current();
    kprintln!(
        "  user        : {id} '{path}' as {cred}, entry {entry:#018x}, stack {stack:#018x}"
    );
    report(&space, entry);

    let slot = sched::current_slot();
    {
        let mut table = PROGRAMS.lock();
        let Some(entry) = table.get_mut(slot) else {
            return Err(Error::AlreadyRunning);
        };
        if entry.is_some() {
            // Задача уже исполняет программу. Вложенного входа в третье кольцо
            // не бывает: стек возврата у задачи один, и второй вход затёр бы
            // его.
            return Err(Error::AlreadyRunning);
        }
        *entry = Some(Program {
            space,
            files: files::Table::new(),
            running: true,
            kill_requested: false,
            cred,
        });
    }

    // Планировщик узнаёт о пространстве до того, как процессор на него
    // переключится, — и с Phase 13b этот порядок стал обязательным, а не
    // предусмотрительным: между двумя строками задачу могут вытеснить. Знание,
    // записанное первым, переживает вытеснение (вернувшись, планировщик сам
    // поставит нужный корень); действие, сделанное первым, — нет.
    sched::set_current_space(Some(root));
    // SAFETY: корень построен копией ядерного и содержит все его отображения;
    // возврат на таблицы ядра — сразу после выхода из программы, до того как
    // пространство будет разобрано.
    unsafe { arch::activate_space(root) };

    // SAFETY: точка входа и стек лежат в окне, только что отображённом
    // доступным из пользовательского режима; стек ловушки выставит сам вход в
    // третье кольцо.
    let code = unsafe { arch::enter_user(entry, stack, argc, argv) };

    // Сюда возвращаются оба пути: и `exit`, и снятие программы после отказа.
    //
    // Порядок здесь обратный запуску и по той же причине: сначала знание, потом
    // действие. Скажи мы процессору раньше, чем планировщику, — вытеснение между
    // двумя строками вернуло бы задачу с корнем **программы** в регистре, и
    // разбор пространства несколькими строками ниже отдал бы в пул кадры,
    // по которым процессор в этот момент ходит.
    sched::set_current_space(None);
    // SAFETY: таблицы ядра активированы при запуске системы и никуда не делись
    // — пространство программы построено их копией.
    unsafe { arch::activate_kernel_space() };

    // Программа забирается из таблицы и уничтожается здесь: `Drop` её
    // пространства возвращает окно в пул кадров, а таблица дескрипторов
    // закрывает всё, что программа не закрыла сама. Снятая отказом закрыть их и
    // не могла.
    let leaked = {
        let mut table = PROGRAMS.lock();
        table.get_mut(slot).and_then(Option::take)
    }
    .map_or(0, |program| program.files.open_count());
    if leaked > 0 {
        kprintln!("  user        : closed {leaked} file(s) the program left open");
    }

    // Сокеты закрываются здесь же и по той же причине: программа, снятая по
    // `kill` или отказавшая, ничего не закрывает сама, а незакрытый сокет
    // держит за собой порт — и следующая попытка запустить ту же службу
    // упёрлась бы в «порт занят» тем, кого уже нет.
    let sockets = crate::net::close_owner(sched::current());
    if sockets > 0 {
        kprintln!("  user        : closed {sockets} socket(s) the program left open");
    }

    Ok(code)
}

/// Сколько стека ядра отводится задаче, исполняющей программу.
///
/// Вдвое больше обычной задачи, и не про запас. На этом стеке лежит всё сразу:
/// разбор ELF, кадр входа в третье кольцо, кадр ловушки от каждого системного
/// вызова и весь путь вывода — от `write` до перерисовки окна оболочки. Раньше
/// последние два жили на отдельном общем стеке ловушек; с появлением второй
/// программы общий стек перестал быть возможен (см. [`sched::UserMachine`]), и
/// глубина переехала сюда.
const PROGRAM_STACK_SIZE: usize = 64 * 1024;

/// Командная строка в том виде, в каком её получает новая задача.
///
/// Массив, а не `String`: аргумент задачи — одно машинное слово, значит строка
/// уезжает через кучу, а выделение под неё обязано уметь отказать вместо паники.
/// `String::into_boxed_str` этого не умеет.
///
/// Хранится целиком, вместе с аргументами, и разбирается уже в новой задаче.
/// Разобрать её здесь и передать `&[&str]` было бы нельзя: срезы указывали бы в
/// память вызывающего, а он к моменту запуска давно вернулся из `spawn`.
#[repr(C)]
struct Request {
    line: [u8; MAX_LINE],
    len: usize,
    /// От чьего имени исполнять. Едет вместе со строкой, а не спрашивается в
    /// новой задаче: спросить там значило бы спросить у **её** программы,
    /// которой ещё нет, и получить личность сеанса вместо заказанной.
    cred: Credentials,
    /// Считать ли новую задачу служебной — см. [`spawn_with`].
    daemon: bool,
}

/// Самая длинная командная строка, которую принимает [`spawn`].
const MAX_LINE: usize = 255;

/// Сколько аргументов программа получает самое большее.
///
/// Восемь — не свойство программы, а предел на разбор: аргументы разбираются в
/// массив на стеке ядра, и брать его длину из того, что набрал человек, значило
/// бы отдать ему глубину этого стека.
const MAX_ARGS: usize = 8;

/// Запустить программу отдельной задачей.
///
/// Возвращает идентификатор задачи — по нему её можно дождаться
/// ([`sched::wait`]) или просто оставить работать. Именно здесь программа
/// перестала быть вызовом внутри оболочки: у неё своя задача, свой стек ядра,
/// своё адресное пространство и свои открытые файлы, и пока она считает,
/// оболочка отвечает.
pub fn spawn(line: &str, cred: Credentials) -> Result<sched::TaskId, Error> {
    spawn_with(line, cred, false)
}

/// То же, но задача объявляется служебной.
///
/// Служебная задача не считается [`sched::alive`] — то есть не удерживает
/// систему от остановки и не заставляет оболочку ждать. Ровно это и есть
/// служба: она работает, пока работает система, и сама по себе поводом ей
/// работать не является.
///
/// Без этого различия первая же служба сломала бы две вещи сразу: оболочка
/// ждёт, пока договорят остальные задачи, прежде чем напечатать приглашение, а
/// `exit` останавливает машину, когда живых задач не осталось. Служба,
/// работающая вечно, отменила бы и то и другое.
pub fn spawn_with(line: &str, cred: Credentials, daemon: bool) -> Result<sched::TaskId, Error> {
    if line.len() > MAX_LINE {
        return Err(Error::Read(crate::vfs::VfsError::BadPath));
    }

    let layout = core::alloc::Layout::new::<Request>();
    // SAFETY: `Request` непустой, поэтому размер положителен. Глобальный
    // аллокатор ядра при нехватке памяти возвращает null, а не паникует.
    let raw = unsafe { alloc::alloc::alloc(layout) }.cast::<Request>();
    if raw.is_null() {
        return Err(Error::OutOfMemory);
    }
    let mut request = Request { line: [0; MAX_LINE], len: line.len(), cred, daemon };
    request.line[..line.len()].copy_from_slice(line.as_bytes());
    // SAFETY: блок только что выделен под `Request` с нужными размером и
    // выравниванием и никому больше не принадлежит.
    unsafe { core::ptr::write(raw, request) };

    match sched::spawn_raw("program", PROGRAM_STACK_SIZE, program_entry, raw as usize) {
        Ok(id) => {
            if daemon {
                sched::mark_daemon(id);
            }
            Ok(id)
        }
        Err(err) => {
            // Задачи не будет — значит некому и разобрать запрос.
            // SAFETY: блок выделен этим же аллокатором и этим же `Layout`,
            // ссылок на него больше нет.
            unsafe { alloc::alloc::dealloc(raw.cast::<u8>(), layout) };
            Err(match err {
                sched::SpawnError::OutOfMemory => Error::OutOfMemory,
                sched::SpawnError::TooManyTasks => Error::TooManyTasks,
            })
        }
    }
}

/// Точка входа задачи, исполняющей программу.
extern "C" fn program_entry(arg: usize) -> ! {
    // Задача начинает исполняться с запрещёнными прерываниями: их запретил
    // планировщик перед переключением, и вернуть «как было» здесь некому. Тот
    // же инвариант, что у обычного батута задач.
    arch::interrupts::enable();

    // SAFETY: `arg` — указатель, выделенный в `spawn` и переданный ровно один
    // раз ровно этой задаче.
    let request = unsafe { alloc::boxed::Box::from_raw(arg as *mut Request) };
    let cred = request.cred;
    // Задача помечает себя служебной сама, хотя это же сделал и [`spawn_with`].
    // Дублирование не лишнее: пометка снаружи успевает не всегда — между
    // возвратом `spawn_raw` и ней задачу могут вытеснить, — а всё, что эта
    // задача запустит дальше, наследует признак по **её** состоянию. Пометка
    // изнутри стоит одного взятия лока и закрывает эту щель целиком.
    if request.daemon {
        sched::mark_daemon(sched::current());
    }
    let line = core::str::from_utf8(&request.line[..request.len]).unwrap_or("");

    // Разбор здесь, а не в `spawn`: срезы указывают внутрь `request`, который
    // живёт ровно столько, сколько эта задача. Первое слово — путь, остальные —
    // аргументы программы; кавычек и экранирования нет, и обещать их, сделав
    // разбиение по пробелам, было бы хуже, чем не обещать.
    let mut words = line.split_whitespace();
    let path = words.next().unwrap_or("");
    let mut args: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut argc = 0;
    // Нулевым аргументом идёт сам путь — так его видит всякая программа в Unix,
    // и программе, печатающей своё имя в сообщении об ошибке, взять его больше
    // неоткуда.
    args[argc] = path;
    argc += 1;
    for word in words {
        if argc == MAX_ARGS {
            break;
        }
        args[argc] = word;
        argc += 1;
    }

    let code = match run(path, &args[..argc], cred) {
        Ok(code) => {
            if code == user_abi::EXIT_KILLED {
                report_line(path, "killed by request");
            } else if code == user_abi::EXIT_FAULTED {
                report_line(path, "killed by the kernel, see the serial log");
            } else {
                let text = CodeText::new(code);
                report_line(path, text.as_str());
            }
            code
        }
        Err(err) => {
            crate::shell::print(format_args!("  {} {path}: {err}\n", sched::current()));
            user_abi::EXIT_FAULTED
        }
    };

    sched::exit_current_with(code)
}

/// Напечатать в окно оболочки строку об окончании программы.
///
/// `shell::print`, а не `write!` по частям: с вытеснением строка, собираемая из
/// кусков, разрывается чужим выводом посередине.
fn report_line(path: &str, what: &str) {
    crate::shell::print(format_args!("  {} {path}: {what}\n", sched::current()));
}

/// Текст «exited with code N» без выделения памяти.
///
/// Печатать код возврата приходится из задачи программы, а форматирование в
/// `String` требовало бы кучи ровно там, где программа только что могла её
/// исчерпать.
struct CodeText {
    buffer: [u8; 32],
    len: usize,
}

impl CodeText {
    fn new(code: i64) -> Self {
        let mut this = Self { buffer: [0; 32], len: 0 };
        let mut out = CodeWriter { text: &mut this };
        let _ = write!(&mut out, "exited with code {code}");
        this
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buffer[..self.len]).unwrap_or("exited")
    }
}

/// Приёмник для `write!`, складывающий байты в [`CodeText`].
struct CodeWriter<'a> {
    text: &'a mut CodeText,
}

impl core::fmt::Write for CodeWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            if self.text.len == self.text.buffer.len() {
                return Err(core::fmt::Error);
            }
            let at = self.text.len;
            self.text.buffer[at] = byte;
            self.text.len += 1;
        }
        Ok(())
    }
}

/// Границы памяти, доступной программе.
///
/// Нужны системным вызовам: указатель, пришедший из третьего кольца, обязан
/// быть проверен. Проверка не про безопасность программы, а про безопасность
/// ядра — иначе `write` с адресом ядерной структуры заставил бы ядро самому
/// прочитать то, до чего программа не дотянулась бы.
///
/// Диапазоны заданы константами, а не спрошены у таблиц: обращение к таблицам
/// на каждом системном вызове стоило бы обхода четырёх уровней, а окно у всех
/// программ одно и то же по построению.
#[must_use]
pub fn owns(ptr: usize, len: usize) -> bool {
    let Some(end) = ptr.checked_add(len) else {
        return false;
    };
    let in_image = ptr >= WINDOW_BASE && end <= WINDOW_BASE + IMAGE_BYTES;
    let in_stack = ptr >= STACK_BASE && end <= STACK_TOP;
    in_image || in_stack
}

/// Исполняет ли текущая задача код в третьем кольце.
#[must_use]
pub fn is_running() -> bool {
    with_current(|program| program.running).unwrap_or(false)
}

/// Почему задачу не удалось снять.
#[derive(Debug, Clone, Copy)]
pub enum KillError {
    /// Задачи с таким номером нет — или уже нет.
    NoSuchTask,
    /// Задача уже завершилась сама.
    AlreadyFinished,
    /// Задача есть, но программы не исполняет.
    ///
    /// Снять можно только программу, и это не половина работы, а граница
    /// возможного: у программы есть точка, в которую ядро умеет вернуться —
    /// кадр `enter_user` на её стеке. У задачи ядра такой точки нет, и «снять»
    /// её означало бы бросить её стек, её локи и её незаконченную работу
    /// неизвестно в каком состоянии.
    NotAProgram,
}

impl core::fmt::Display for KillError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // Формулировки продолжают фразу «task #5 ...»: сообщение об ошибке
            // собирается там, где известен номер, а не здесь.
            Self::NoSuchTask => f.write_str("does not exist"),
            Self::AlreadyFinished => f.write_str("has already finished"),
            Self::NotAProgram => f.write_str("is not running a program; only programs can be stopped"),
        }
    }
}

/// Попросить снять программу задачи `id`.
///
/// Возврат `Ok` означает «просьба принята», а не «программа снята»: снимет её
/// [`check_kill`] на ближайшем возврате в третье кольцо — то есть не позже
/// следующего тика таймера, если программа исполняет свой код. Программа,
/// застрявшая **внутри** ядра, не снимется до тех пор, пока оттуда не выйдет; в
/// этом ядре системные вызовы не блокируются, поэтому выйдет она сразу, но
/// обещать это на будущее нельзя.
pub fn request_kill(id: sched::TaskId) -> Result<(), KillError> {
    let (slot, state) = sched::lookup(id).ok_or(KillError::NoSuchTask)?;
    if state == sched::TaskState::Finished {
        return Err(KillError::AlreadyFinished);
    }

    let mut table = PROGRAMS.lock();
    match table.get_mut(slot).and_then(Option::as_mut) {
        Some(program) => {
            program.kill_requested = true;
            // Программа могла спать, а снятие происходит на возврате в третье
            // кольцо — то есть спящая не снялась бы, пока не проснётся сама.
            // Пробуждение здесь и делает `kill` работающим на программе,
            // которая ничего не делает.
            //
            // Двух вызовов, а не одного: `wake_input` будит **всех**, кто ждёт
            // ввода, — это нужно оболочке, чтобы она разобрала очередь и
            // заметила снятие. `wake` будит именно снимаемую, чем бы она ни
            // была занята: `sleep_ms` в службе, отзывающейся раз в полминуты,
            // иначе откладывал бы снятие на полминуты, и `kill` выглядел бы как
            // команда, которая не работает. Ровно так это и выглядело.
            sched::wake_input();
            sched::wake(id);
            Ok(())
        }
        None => Err(KillError::NotAProgram),
    }
}

/// Просили ли снять программу текущей задачи.
///
/// Спрашивает [`syscall`] в тех вызовах, которые умеют ждать долго: снятие
/// произойдёт на возврате в третье кольцо, но до возврата надо ещё дойти, а
/// ожидание ввода само по себе не кончается никогда.
#[must_use]
pub fn kill_pending() -> bool {
    with_current(|program| program.kill_requested).unwrap_or(false)
}

/// Снять текущую программу, если её просили снять.
///
/// Вызывается арх-слоем на возврате из ловушки в третье кольцо (см.
/// [`crate::irq::on_trap_return`]) и не возвращается, если снятие состоялось.
///
/// Почему именно это место: здесь ядро заведомо не держит ни одного лока и не
/// оставляет незаконченной работы — вся она закончилась вместе с обработчиком.
/// Снять программу в произвольной точке ядра было бы нельзя: `return_to_kernel`
/// бросает стек обработчика целиком, и вместе с ним пропал бы любой охранник
/// лока, который на нём лежал.
///
/// # Safety
///
/// Вызывать только тогда, когда прерванный код исполнялся в третьем кольце: это
/// и есть доказательство того, что кадр [`arch::enter_user`] на стеке этой
/// задачи цел.
pub unsafe fn check_kill() {
    if !with_current(|program| program.running && program.kill_requested).unwrap_or(false) {
        return;
    }

    kprintln!("  user        : killed by request, task {}", sched::current());
    with_current(|program| program.running = false);
    // SAFETY: контракт функции плюс проверенный `running` — программа
    // действительно исполняется, значит `enter_user` на стеке и вернуться есть
    // куда.
    unsafe { arch::return_to_kernel(user_abi::EXIT_KILLED) }
}

/// Снять программу после отказа.
///
/// Вызывается обработчиком исключений, когда отказ пришёл из пользовательского
/// режима. Не возвращается: управление уходит в точку, из которой программа
/// была запущена, — то есть в [`run`], который вернёт процессор на таблицы ядра
/// и разберёт её адресное пространство.
///
/// # Safety
///
/// Вызывать только тогда, когда [`is_running`] отвечает `true`.
pub unsafe fn faulted(what: &str, at: usize, addr: usize) -> ! {
    // Номер задачи стоит в конце строки, а не в начале, и это не вкус: начало
    // строки читает стенд, и сдвинуть его значит переписать проверки, которые
    // про номер задачи ничего не спрашивают.
    kprintln!(
        "  user        : killed by {what} at {at:#018x} (address {addr:#018x}), task {}",
        sched::current()
    );
    with_current(|program| program.running = false);
    // SAFETY: контракт функции — программа запущена, значит `enter_user`
    // действительно исполняется и его кадр на стеке ядра цел.
    unsafe { arch::return_to_kernel(user_abi::EXIT_FAULTED) }
}
