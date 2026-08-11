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
use crate::sync::Racy;
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
/// `Racy` вместо спинлока — по той же причине, что и во всём ядре на этом
/// этапе: исполнение однопоточное, прерывания выключены, вторичные ядра не
/// стартовали. Как только это перестанет быть верным, обёртка обязана стать
/// настоящим замком, иначе две параллельные аллокации порвут список блоков.
struct KernelHeap {
    inner: Racy<Option<Heap>>,
}

/// Глобальный аллокатор ядра.
#[global_allocator]
static HEAP: KernelHeap = KernelHeap { inner: Racy::new(None) };

// SAFETY: реализация возвращает либо null, либо блок, выделенный `Heap` под
// запрошенные размер и выравнивание и не пересекающийся ни с одним другим
// живым блоком; `dealloc` возвращает блок тому же экземпляру `Heap` с тем же
// `Layout`. Гонок нет: см. комментарий к `KernelHeap`.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: однопоточное невытесняемое исполнение, поэтому второй ссылки
        // на содержимое `inner` в этот момент не существует. Диагностика ниже
        // печатает в serial и во фреймбуфер, а они не аллоцируют — рекурсии
        // через `alloc` не возникает.
        let slot = unsafe { &mut *self.inner.get() };
        let Some(heap) = slot.as_mut() else {
            kprintln!(
                "mm: allocation of {} bytes (align {}) before heap::init",
                layout.size(),
                layout.align()
            );
            return ptr::null_mut();
        };

        match heap.allocate_first_fit(layout) {
            Ok(block) => block.as_ptr(),
            Err(()) => {
                // Возврат null означает OOM, и стандартный обработчик тут же
                // запаникует сообщением вида «memory allocation of N bytes
                // failed», в котором нет ни состояния кучи, ни выравнивания.
                // Поэтому всё, что мы знаем, печатается здесь и сейчас.
                kprintln!("mm: KERNEL HEAP EXHAUSTED");
                kprintln!(
                    "  requested : {} bytes, align {}",
                    layout.size(),
                    layout.align()
                );
                kprintln!(
                    "  heap      : {} KiB total, {} KiB used, {} KiB free",
                    heap.size() / 1024,
                    heap.used() / 1024,
                    heap.free() / 1024
                );
                kprintln!(
                    "  range     : {:#018x}..{:#018x}",
                    HEAP_BASE,
                    HEAP_BASE + HEAP_SIZE
                );
                if heap.free() >= layout.size() {
                    kprintln!("  cause     : free memory is fragmented, no single block fits");
                }
                ptr::null_mut()
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let Some(block) = NonNull::new(ptr) else {
            return;
        };
        // SAFETY: см. `alloc`.
        let slot = unsafe { &mut *self.inner.get() };
        let Some(heap) = slot.as_mut() else {
            kprintln!("mm: deallocation before heap::init, ignored");
            return;
        };
        // SAFETY: контракт `GlobalAlloc::dealloc` обязывает вызывающего передать
        // блок, выданный `alloc` этого же аллокатора, с тем же `Layout`, — а
        // выдавал его именно этот экземпляр `Heap`.
        unsafe { heap.deallocate(block, layout) };
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
    // SAFETY: однопоточное невытесняемое исполнение — эксклюзивность доступа к
    // глобальному состоянию обеспечена структурой запуска ядра.
    let slot = unsafe { &mut *HEAP.inner.get() };
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
    // SAFETY: однопоточное невытесняемое исполнение.
    let slot = unsafe { &*HEAP.inner.get() };
    slot.is_some()
}

/// Занятость кучи. Нули, если [`init`] ещё не вызывали.
#[must_use]
pub fn stats() -> HeapStats {
    // SAFETY: однопоточное невытесняемое исполнение; ссылка только на чтение и
    // не переживает эту функцию.
    let slot = unsafe { &*HEAP.inner.get() };
    match slot.as_ref() {
        Some(heap) => HeapStats { size: heap.size(), used: heap.used(), free: heap.free() },
        None => HeapStats::default(),
    }
}
