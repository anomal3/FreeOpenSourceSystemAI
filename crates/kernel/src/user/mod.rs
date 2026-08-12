//! Пользовательское пространство: программы, исполняющиеся вне ядра.
//!
//! # Что здесь появилось
//!
//! До этой фазы всё, что делала система, исполнялось в самом привилегированном
//! режиме процессора: оболочка, файловый менеджер, драйверы — один уровень
//! доверия на всё. Теперь есть второй: программа читается с файловой системы,
//! раскладывается по страницам, помеченным доступными из третьего кольца
//! (EL0 на AArch64), и запускается там. Из этого режима нельзя ни прочитать
//! память ядра, ни выполнить его код, ни обратиться к устройству — единственная
//! дверь наружу это системный вызов.
//!
//! # Чего здесь пока нет
//!
//! **Отдельного адресного пространства.** Программа живёт в тех же таблицах
//! страниц, что и ядро, просто в своей их части: младшая половина уже занята —
//! образ ядра исполняется identity-отображённым, рядом лежит отображение всей
//! физической памяти, — и всё это помещается в первую запись корневой таблицы.
//! Программа занимает вторую, начиная с 512 ГиБ.
//!
//! Что это даёт и чего не даёт, стоит сказать прямо. Даёт: программа физически
//! не может дотянуться до ядра — записи её страниц помечены пользовательскими,
//! а ядерные нет, и проверку делает блок управления памятью, а не код. Не даёт:
//! две программы, запущенные подряд, живут по одним адресам, и разделения между
//! ними нет — потому что одновременно их не бывает. Своё пространство на
//! программу — следующая фаза, и она про переключение `CR3`/`TTBR0`, а не про
//! привилегии.
//!
//! # Почему окно памяти постоянное
//!
//! Кадры под программу выделяются один раз и дальше переиспользуются. Причина
//! прозаическая: снимать отображение таблицы страниц не умеют — функции
//! `unmap` в них нет, — и каждый запуск, выделяющий новые кадры, утекал бы
//! памятью. Окно фиксированного размера с обнулением перед загрузкой честнее:
//! оно ограничивает программу сверху и не течёт.

pub mod elf;
pub mod syscall;

use alloc::vec::Vec;

use crate::mm::{FrameAllocator, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::sync::SpinLock;
use crate::{arch, kprintln};

/// Начало окна программы: 512 ГиБ.
///
/// Тот же адрес записан в компоновочном сценарии `crates/user-progs/user.ld`;
/// разъехаться они не могут незаметно — сегмент за пределами окна отвергается
/// загрузчиком с [`elf::ElfError::OutOfWindow`].
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
    /// Не удалось отобразить страницу окна.
    Map(crate::mm::MapError),
    /// Страница получилась одновременно записываемой и исполняемой.
    WriteExecute(usize),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoFilesystem => f.write_str("no filesystem is mounted"),
            Self::Read(err) => write!(f, "{err}"),
            Self::Elf(err) => write!(f, "{err}"),
            Self::OutOfMemory => f.write_str("not enough memory for the program window"),
            Self::Map(err) => write!(f, "mapping the program window failed: {err}"),
            Self::WriteExecute(page) => write!(
                f,
                "page {page} of the image would be writable and executable at once"
            ),
        }
    }
}

/// Кадры окна программы. Выделяются при первом запуске и живут дальше.
struct Window {
    image: Vec<PhysAddr>,
    stack: Vec<PhysAddr>,
}

static WINDOW: SpinLock<Option<Window>> = SpinLock::new(None);

/// Исполняется ли сейчас пользовательская программа.
///
/// Нужно обработчику отказов: исключение из третьего кольца снимает программу,
/// но только если она действительно запущена. Отказ «из третьего кольца» в
/// момент, когда никакой программы нет, означает испорченное состояние
/// процессора, и возвращаться в этом случае некуда.
static RUNNING: SpinLock<bool> = SpinLock::new(false);

/// Выделить кадры окна, если их ещё нет.
fn ensure_window() -> Result<(), Error> {
    let mut guard = WINDOW.lock();
    if guard.is_some() {
        return Ok(());
    }

    let mut image = Vec::new();
    let mut stack = Vec::new();
    image.try_reserve_exact(IMAGE_PAGES).map_err(|_| Error::OutOfMemory)?;
    stack.try_reserve_exact(STACK_PAGES).map_err(|_| Error::OutOfMemory)?;

    let filled = crate::mm::frame::with(|alloc| {
        for _ in 0..IMAGE_PAGES {
            match alloc.allocate() {
                Some(frame) => image.push(frame),
                None => return false,
            }
        }
        for _ in 0..STACK_PAGES {
            match alloc.allocate() {
                Some(frame) => stack.push(frame),
                None => return false,
            }
        }
        true
    });

    // Кадры, успевшие выделиться до отказа, не возвращаются: аллокатор их
    // отдаст обратно только по `free`, а вызывать его на половине списка —
    // отдельный путь ради случая «памяти нет вовсе», в котором система всё
    // равно доживает последние секунды.
    if filled != Some(true) {
        return Err(Error::OutOfMemory);
    }

    *guard = Some(Window { image, stack });
    Ok(())
}

/// Виртуальный адрес кадра в прямом отображении — через него ядро пишет в
/// память программы.
fn frame_bytes(frame: PhysAddr) -> *mut u8 {
    frame.to_direct_map().as_mut_ptr::<u8>()
}

/// Заполнить окно образа нулями.
///
/// Обязательно перед каждой загрузкой, и по двум причинам сразу: `.bss`
/// программы обязан быть нулевым, а память от предыдущей программы не должна
/// доставаться следующей.
fn zero_image(window: &Window) {
    for frame in &window.image {
        // SAFETY: кадр выделен аллокатором и отображён в прямое отображение
        // ядра; пишем ровно страницу от его начала.
        unsafe { core::ptr::write_bytes(frame_bytes(*frame), 0, PAGE_SIZE) };
    }
}

/// Скопировать данные в окно образа по смещению от [`WINDOW_BASE`].
fn write_image(window: &Window, offset: usize, data: &[u8]) {
    let mut written = 0;
    while written < data.len() {
        let at = offset + written;
        let page = at / PAGE_SIZE;
        let in_page = at % PAGE_SIZE;
        let Some(frame) = window.image.get(page) else {
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

/// Разложить программу по окну и вернуть точку входа.
fn load(bytes: &[u8]) -> Result<usize, Error> {
    let image = elf::Image::parse(bytes).map_err(Error::Elf)?;

    ensure_window()?;
    let guard = WINDOW.lock();
    let Some(window) = guard.as_ref() else {
        return Err(Error::OutOfMemory);
    };

    zero_image(window);

    // Права страницы — объединение прав сегментов, которые её задевают.
    // Страница, не задетая ни одним сегментом, остаётся доступной только на
    // чтение: она внутри окна, и оставлять её записываемой незачем.
    let mut flags = [PageFlags::READ.union(PageFlags::USER); IMAGE_PAGES];
    let mut segments = 0usize;

    for segment in image.segments((WINDOW_BASE, WINDOW_BASE + IMAGE_BYTES)) {
        let segment = segment.map_err(Error::Elf)?;
        segments += 1;

        let offset = segment.vaddr - WINDOW_BASE;
        let source = &image.bytes()[segment.file_offset..segment.file_offset + segment.filesz];
        write_image(window, offset, source);

        let first = offset / PAGE_SIZE;
        let last = (offset + segment.memsz - 1) / PAGE_SIZE;
        let segment_flags =
            PageFlags::from_segment_flags(segment.flags).union(PageFlags::USER);
        for page in first..=last.min(IMAGE_PAGES - 1) {
            flags[page] = flags[page].union(segment_flags);
        }
    }

    if segments == 0 {
        return Err(Error::Elf(elf::ElfError::NoSegments));
    }

    for (page, frame) in window.image.iter().enumerate() {
        let page_flags = flags[page];
        if page_flags.contains(PageFlags::WRITE) && page_flags.contains(PageFlags::EXEC) {
            return Err(Error::WriteExecute(page));
        }
        // SAFETY: адрес внутри окна программы, которое ядро ни под что другое
        // не использует; кадр принадлежит окну. Переотображение того же кадра с
        // другими правами разрешено — именно так окно переиспользуется между
        // запусками.
        unsafe {
            arch::map_active(
                VirtAddr::new(WINDOW_BASE + page * PAGE_SIZE),
                *frame,
                PAGE_SIZE,
                page_flags,
            )
        }
        .map_err(Error::Map)?;
    }

    for (page, frame) in window.stack.iter().enumerate() {
        // SAFETY: см. выше; стек — обычная память программы на чтение и запись.
        unsafe {
            arch::map_active(
                VirtAddr::new(STACK_BASE + page * PAGE_SIZE),
                *frame,
                PAGE_SIZE,
                PageFlags::READ
                    .union(PageFlags::WRITE)
                    .union(PageFlags::USER),
            )
        }
        .map_err(Error::Map)?;
    }

    Ok(image.entry)
}

/// Загрузить программу по пути и исполнить её.
///
/// Возвращает код, с которым программа завершилась.
pub fn run(path: &str) -> Result<i64, Error> {
    let bytes = match crate::fs::read(path, MAX_FILE) {
        Some(Ok((bytes, _))) => bytes,
        Some(Err(err)) => return Err(Error::Read(err)),
        None => return Err(Error::NoFilesystem),
    };

    let entry = load(&bytes)?;

    // Вершина стека выравнивается на 16: этого требуют оба соглашения о
    // вызовах, и невыровненный стек ломается не сразу, а на первой же операции
    // с вектором.
    let stack = STACK_TOP - 16;

    kprintln!(
        "  user        : '{path}', entry {entry:#018x}, stack {stack:#018x}",
    );

    *RUNNING.lock() = true;
    // SAFETY: точка входа и стек лежат в окне, только что отображённом
    // доступным из пользовательского режима; `TSS.RSP0` (x86-64) выставлен при
    // инициализации GDT, `SP_EL1` (AArch64) — при настройке векторов.
    let code = unsafe { arch::enter_user(entry, stack) };
    *RUNNING.lock() = false;

    Ok(code)
}

/// Границы памяти, доступной программе.
///
/// Нужны системным вызовам: указатель, пришедший из третьего кольца, обязан
/// быть проверен. Проверка не про безопасность программы, а про безопасность
/// ядра — иначе `write` с адресом ядерной структуры заставил бы ядро самому
/// прочитать то, до чего программа не дотянулась бы.
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
/// была запущена.
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
