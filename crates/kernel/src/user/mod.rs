//! Пользовательское пространство: программы, исполняющиеся вне ядра.
//!
//! # Что здесь есть
//!
//! Программа читается с файловой системы, раскладывается по страницам,
//! помеченным доступными из третьего кольца (EL0 на AArch64), и запускается там.
//! Из этого режима нельзя ни прочитать память ядра, ни выполнить его код, ни
//! обратиться к устройству — единственная дверь наружу это системный вызов.
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
//! # Чего здесь пока нет
//!
//! **Настоящего процесса.** Программа исполняется внутри вызова [`run`]:
//! оболочка ждёт её завершения, планировщик о ней ничего не знает, и запустить
//! вторую, не дождавшись первой, нельзя — попытка отвергается
//! [`Error::AlreadyRunning`]. Отсюда же и тонкость с `yield`: уступив процессор
//! системным вызовом, программа отдаёт его задачам ядра, а те продолжают
//! работать, пока в регистре страниц стоит **её** корень. Это безопасно ровно
//! потому, что копия содержит все отображения ядра, а всё, что ядро отображает
//! на ходу, идёт в его собственное дерево (см. `arch::kernel_root`).
//!
//! **Файловых системных вызовов и проверки прав.** Это Phase 12c; `mode`,
//! `uid` и `gid` пишутся установщиком и видны в файловом менеджере, но пока
//! никем не проверяются.
//!
//! # Почему окно ограничено сверху
//!
//! Размер окна фиксирован ([`IMAGE_PAGES`] страниц под образ и [`STACK_PAGES`]
//! под стек), а не выведен из файла программы. Причина — не в простоте
//! реализации: фиксированный размер ограничивает программу сверху известным
//! числом, а сегмент, не поместившийся в окно, отвергается загрузчиком до
//! первой записи в память, а не после.

pub mod elf;
pub mod space;
pub mod syscall;

use alloc::vec::Vec;

use crate::mm::{FrameAllocator, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::sync::SpinLock;
use crate::{arch, kprintln};

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
/// Полмегабайта. Отладочная сборка крошечной программы занимает заметно
/// больше оптимизированной — в ней остаются все проверки предусловий из `core`,
/// — и запас взят под неё, а не под release.
const IMAGE_PAGES: usize = 128;
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
const MAX_FILE: usize = 512 * 1024;

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
    /// Программа уже исполняется.
    AlreadyRunning,
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
            Self::AlreadyRunning => f.write_str("another program is already running"),
        }
    }
}

/// Исполняется ли сейчас пользовательская программа.
///
/// Нужно двоим. Обработчику отказов: исключение из третьего кольца снимает
/// программу, но только если она действительно запущена, — отказ «из третьего
/// кольца» в момент, когда никакой программы нет, означает испорченное
/// состояние процессора, и возвращаться в этом случае некуда. И самому [`run`]:
/// вход в третье кольцо вложенным не бывает, потому что вершина стека ядра
/// запоминается в одном месте на всю систему, и второй вход затёр бы её.
static RUNNING: SpinLock<bool> = SpinLock::new(false);

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

/// Разложить программу по её адресному пространству и вернуть точку входа.
fn load(space: &mut Space, bytes: &[u8]) -> Result<usize, Error> {
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

    for page in 0..STACK_PAGES {
        let frame = take_frame()?;
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

    Ok(image.entry)
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

/// Загрузить программу по пути и исполнить её.
///
/// Возвращает код, с которым программа завершилась.
pub fn run(path: &str) -> Result<i64, Error> {
    if is_running() {
        return Err(Error::AlreadyRunning);
    }

    let bytes = match crate::fs::read(path, MAX_FILE) {
        Some(Ok((bytes, _))) => bytes,
        Some(Err(err)) => return Err(Error::Read(err)),
        None => return Err(Error::NoFilesystem),
    };

    // Пространство живёт до конца функции и разбирается своим `Drop` — на обоих
    // путях выхода, включая тот, которым возвращается снятая отказом программа.
    let mut space = Space::new().map_err(Error::Map)?;
    let entry = load(&mut space, &bytes)?;

    // Вершина стека выравнивается на 16: этого требуют оба соглашения о
    // вызовах, и невыровненный стек ломается не сразу, а на первой же операции
    // с вектором.
    let stack = STACK_TOP - 16;

    kprintln!("  user        : '{path}', entry {entry:#018x}, stack {stack:#018x}");
    report(&space, entry);

    *RUNNING.lock() = true;
    // SAFETY: возврат на таблицы ядра — следующая же строка после выхода из
    // программы, то есть до того, как `space` будет разобрано.
    unsafe { space.activate() };
    // SAFETY: точка входа и стек лежат в окне, только что отображённом
    // доступным из пользовательского режима; `TSS.RSP0` (x86-64) выставлен при
    // инициализации GDT, `SP_EL1` (AArch64) — при настройке векторов.
    let code = unsafe { arch::enter_user(entry, stack) };
    // Сюда возвращаются оба пути: и `exit`, и снятие программы после отказа.
    // SAFETY: таблицы ядра активированы при запуске системы и никуда не делись
    // — пространство программы построено их копией.
    unsafe { Space::leave() };
    *RUNNING.lock() = false;

    Ok(code)
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

/// Исполняется ли сейчас программа.
#[must_use]
pub fn is_running() -> bool {
    *RUNNING.lock()
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
    kprintln!(
        "  user        : killed by {what} at {at:#018x} (address {addr:#018x})",
    );
    *RUNNING.lock() = false;
    // SAFETY: контракт функции — программа запущена, значит `enter_user`
    // действительно исполняется и его кадр на стеке ядра цел.
    unsafe { arch::return_to_kernel(user_abi::EXIT_FAULTED) }
}
