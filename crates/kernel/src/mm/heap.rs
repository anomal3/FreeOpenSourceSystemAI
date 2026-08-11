//! Куча ядра: глобальный аллокатор для `alloc::vec::Vec`, `Box` и `String`.
//!
//! # Что здесь чужое
//!
//! Сам аллокатор не свой: под кучей лежит [`linked_list_allocator`] — свободные
//! блоки связаны в список, заголовок каждого хранится в самом блоке, поэтому
//! никакого стороннего хранилища крейту не нужно (что здесь принципиально:
//! память под метаданные кучи взять неоткуда — куча и есть то, что мы поднимаем).
//! Писать вместо него собственный первопригодный список — это переизобретать
//! ровно этот крейт вместе с его ошибками слияния соседних блоков и
//! выравнивания.
//!
//! Модуль добавляет к нему то, чего крейт дать не может: диагностику. При отказе
//! выделения ядро печатает, сколько было запрошено и сколько кучи осталось, —
//! иначе единственным следом остаётся `memory allocation of N bytes failed` без
//! единого намёка на состояние кучи.
//!
//! # Кто и когда обязан вызвать [`init`]
//!
//! Диапазон `HEAP_BASE .. HEAP_BASE + HEAP_SIZE` — **виртуальный**, и отобразить
//! его обязан арх-специфичный код подкачки: `HEAP_SIZE / PAGE_SIZE` кадров с
//! правами `READ | WRITE` (и без `EXEC` — на куче лежат данные). [`init`]
//! вызывается уже после того, как таблицы построены **и активированы**: он
//! пишет по этим адресам, а до `activate()` их там просто нет.
//!
//! Чтобы в ядре заработали `Vec`/`Box`/`String`, в корне крейта (`main.rs`)
//! нужна строка `extern crate alloc;`. Одного `#[global_allocator]` для этого
//! мало: он лишь говорит, *через что* аллоцировать.

use crate::kprintln;
use crate::mm::{HEAP_BASE, HEAP_SIZE, PAGE_SIZE};
use crate::sync::SpinLock;
use core::alloc::{GlobalAlloc, Layout};
use core::fmt;
use core::ptr::{self, NonNull};
use linked_list_allocator::Heap;

const _: () = assert!(HEAP_BASE % PAGE_SIZE == 0, "HEAP_BASE must be page-aligned");
const _: () = assert!(HEAP_SIZE % PAGE_SIZE == 0, "HEAP_SIZE must be a whole number of pages");
const _: () = assert!(HEAP_SIZE > 0, "HEAP_SIZE must not be zero");

/// Почему не удалось поднять кучу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapError {
    /// [`init`] уже вызывали. Повторный вызов отдал бы аллокатору память, в
    /// которой уже лежат чужие живые объекты.
    AlreadyInitialised,
    /// Проверка отображения не прошла: страница `page` не хранит то, что в неё
    /// записали. На практике это значит, что несколько виртуальных страниц кучи
    /// отображены в один физический кадр.
    BackingAliased { page: usize },
}

impl fmt::Display for HeapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInitialised => f.write_str("kernel heap is already initialised"),
            Self::BackingAliased { page } => write!(
                f,
                "heap page {page} does not hold what was written to it \
                 (aliased or partially mapped backing)"
            ),
        }
    }
}

/// Занятость кучи в байтах.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeapStats {
    pub size: usize,
    pub used: usize,
    pub free: usize,
}

/// Обёртка над [`Heap`], которую можно положить в `static`.
///
/// Замок настоящий, а не `Racy`: глобальный аллокатор вызывается из
/// произвольной точки ядра, включая обработчик прерывания, а список свободных
/// блоков две одновременные аллокации порвут. [`SpinLock`] запрещает прерывания
/// на время удержания, поэтому обработчик не может вклиниться в середину
/// правки списка и рекурсивно войти в аллокатор.
struct KernelHeap {
    inner: SpinLock<Option<Heap>>,
}

/// Глобальный аллокатор ядра.
#[global_allocator]
static HEAP: KernelHeap = KernelHeap { inner: SpinLock::new(None) };

/// Чем кончилась попытка выделения.
///
/// Отдельное значение нужно ровно затем, чтобы диагностика печаталась уже после
/// освобождения лока кучи. `kprintln!` сегодня не аллоцирует — но печатать
/// из-под удерживаемого аллокатора значит держать наготове зависание
/// `alloc → вывод → alloc`, в котором второй захват ждал бы первого. Стоимость
/// развязки — три `usize`, скопированных на стек.
enum Outcome {
    Block(*mut u8),
    NotInitialised,
    Exhausted(HeapStats),
}

// SAFETY: реализация возвращает либо null, либо блок, выделенный `Heap` под
// запрошенные размер и выравнивание и не пересекающийся ни с одним другим
// живым блоком; `dealloc` возвращает блок тому же экземпляру `Heap` с тем же
// `Layout`. Гонок нет: доступ к `Heap` возможен только через `SpinLock`.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let outcome = {
            let mut slot = self.inner.lock();
            match slot.as_mut() {
                None => Outcome::NotInitialised,
                Some(heap) => match heap.allocate_first_fit(layout) {
                    Ok(block) => Outcome::Block(block.as_ptr()),
                    Err(()) => Outcome::Exhausted(HeapStats {
                        size: heap.size(),
                        used: heap.used(),
                        free: heap.free(),
                    }),
                },
            }
        };

        // Ниже лок уже отпущен, и печатать можно свободно.
        let stats = match outcome {
            Outcome::Block(block) => return block,
            Outcome::NotInitialised => {
                kprintln!(
                    "mm: allocation of {} bytes (align {}) before heap::init",
                    layout.size(),
                    layout.align()
                );
                return ptr::null_mut();
            }
            Outcome::Exhausted(stats) => stats,
        };

        // Возврат null означает OOM, и стандартный обработчик тут же
        // запаникует сообщением вида «memory allocation of N bytes failed», в
        // котором нет ни состояния кучи, ни выравнивания. Поэтому всё, что мы
        // знаем, печатается здесь и сейчас.
        kprintln!("mm: KERNEL HEAP EXHAUSTED");
        kprintln!("  requested : {} bytes, align {}", layout.size(), layout.align());
        kprintln!(
            "  heap      : {} KiB total, {} KiB used, {} KiB free",
            stats.size / 1024,
            stats.used / 1024,
            stats.free / 1024
        );
        kprintln!("  range     : {:#018x}..{:#018x}", HEAP_BASE, HEAP_BASE + HEAP_SIZE);
        if stats.free >= layout.size() {
            kprintln!("  cause     : free memory is fragmented, no single block fits");
        }
        ptr::null_mut()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(block) = NonNull::new(ptr) else {
            return;
        };
        {
            let mut slot = self.inner.lock();
            if let Some(heap) = slot.as_mut() {
                // SAFETY: контракт `GlobalAlloc::dealloc` обязывает вызывающего
                // передать блок, выданный `alloc` этого же аллокатора, с тем же
                // `Layout`, — а выдавал его именно этот экземпляр `Heap`.
                unsafe { heap.deallocate(block, layout) };
                return;
            }
        }
        // Печать — уже без лока, по той же причине, что и в `alloc`.
        kprintln!("mm: deallocation before heap::init, ignored");
    }
}

/// Отдать куче диапазон `HEAP_BASE .. HEAP_BASE + HEAP_SIZE`.
///
/// # Safety
///
/// К моменту вызова весь этот диапазон обязан быть отображён на физические
/// кадры с правами чтения и записи, а таблицы, содержащие это отображение, —
/// активны. Память должна принадлежать только куче: `init` немедленно начинает
/// писать по этим адресам.
pub unsafe fn init() -> Result<HeapStats, HeapError> {
    // Лок держится и через `verify_backing`: проверка обязана застать диапазон
    // ровно в том состоянии, в котором его получит `Heap::new`, а чужая
    // аллокация между ними означала бы, что мы затираем узором уже выданную
    // память. Обходится это дорого — тысячи записей с запрещёнными
    // прерываниями, — но `init` вызывается один раз и до запуска таймера.
    let mut slot = HEAP.inner.lock();
    if slot.is_some() {
        return Err(HeapError::AlreadyInitialised);
    }

    // SAFETY: диапазон отображён и активен по контракту функции.
    unsafe { verify_backing()? };

    // SAFETY: тот же контракт: диапазон отображён, доступен на запись и не
    // используется ничем другим.
    let heap = unsafe { Heap::new(HEAP_BASE as *mut u8, HEAP_SIZE) };
    let stats = HeapStats { size: heap.size(), used: heap.used(), free: heap.free() };
    *slot = Some(heap);
    Ok(stats)
}

/// Проверить, что за кучей действительно стоят разные физические кадры.
///
/// Ошибка, ради которой это написано, выглядит так: код подкачки строит
/// отображение кучи в цикле и по недосмотру подставляет один и тот же кадр всем
/// страницам. Куча при этом «работает» — до первого момента, когда одна
/// аллокация молча затрёт другую, и разбираться придётся уже по последствиям,
/// за тысячи инструкций от причины. Один проход записи и один проход чтения
/// ловят это сразу.
///
/// # Safety
///
/// Диапазон кучи должен быть отображён и доступен на запись.
unsafe fn verify_backing() -> Result<(), HeapError> {
    /// Произвольная константа: важно лишь, чтобы её не оказалось в свежем
    /// (обнулённом) кадре случайно.
    const PATTERN: u64 = 0xA5A5_1DEA_0BAD_C0DE;

    let base = HEAP_BASE as *mut u64;
    let pages = HEAP_SIZE / PAGE_SIZE;
    let stride = PAGE_SIZE / size_of::<u64>();

    for page in 0..pages {
        // volatile: иначе компилятор вправе выбросить и запись, и чтение как
        // очевидно избыточную пару — а проверяем мы как раз то, о чём он не
        // знает, то есть поведение MMU.
        //
        // SAFETY: `page < pages`, поэтому адрес лежит внутри диапазона кучи,
        // отображённого и доступного на запись по контракту функции;
        // выравнивание на 8 байт следует из выравнивания `HEAP_BASE` на страницу.
        unsafe { ptr::write_volatile(base.add(page * stride), PATTERN ^ page as u64) };
    }
    for page in 0..pages {
        // SAFETY: см. цикл записи.
        let value = unsafe { ptr::read_volatile(base.add(page * stride)) };
        if value != PATTERN ^ page as u64 {
            return Err(HeapError::BackingAliased { page });
        }
    }
    Ok(())
}

/// Поднята ли куча.
#[must_use]
pub fn is_ready() -> bool {
    HEAP.inner.lock().is_some()
}

/// Занятость кучи. Нули, если [`init`] ещё не вызывали.
#[must_use]
pub fn stats() -> HeapStats {
    let slot = HEAP.inner.lock();
    match slot.as_ref() {
        Some(heap) => HeapStats { size: heap.size(), used: heap.used(), free: heap.free() },
        None => HeapStats::default(),
    }
}
