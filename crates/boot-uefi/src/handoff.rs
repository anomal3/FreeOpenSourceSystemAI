//! Карта памяти, `ExitBootServices` и передача управления ядру.
//!
//! # Почему карта памяти снимается в последний момент
//!
//! `ExitBootServices` принимает ключ карты памяти и проваливается, если ключ
//! устарел. Устаревает он от любого выделения памяти — в том числе от того,
//! которое прошивка делает внутри собственных сервисов, пока мы печатаем
//! диагностику. Поэтому между «снять карту» и «выйти» не должно быть ничего:
//! в uefi 0.39 это оформлено обёрткой [`uefi::boot::exit_boot_services`],
//! которая сама выделяет буфер, забирает карту и выходит одним вызовом (с
//! одной повторной попыткой, как это делает Linux). Пользоваться связкой
//! `boot::memory_map` + сырым `ExitBootServices` тут незачем.
//!
//! # Почему после выхода нельзя ничего печатать
//!
//! После `ExitBootServices` протоколы прошивки (включая консольный `Output`),
//! пул и таймеры недействительны. `println!`, `stall`, любая аллокация и даже
//! паника, которая попытается напечатать сообщение, обращаются к уже мёртвым
//! указателям. Поэтому всё, что нужно сказать, говорится до выхода, а после —
//! только запись в собственную заранее выделенную память и прыжок.
//!
//! # Где живут `BootInfo`, карта сегментов ядра и массив регионов
//!
//! В одном блоке страниц типа `LOADER_DATA`, выделенном до выхода. Такая память
//! не исчезает при `ExitBootServices` — она просто перестаёт управляться
//! прошивкой, — и в карте помечается как `BootloaderReclaimable`. Класть эти
//! структуры на стек нельзя: ядро переключит стек на свой, и всё, на что
//! `BootInfo` ссылается, будет затёрто раньше, чем ядро успеет прочитать. По
//! той же причине массив [`boot_info::KernelSegment`] не может ехать из
//! `elf::load` по ссылке на локальный буфер — он копируется сюда.

use core::mem;
use core::ptr::NonNull;

use boot_info::{
    BootInfo, KernelEntry, KernelSegment, MemoryKind, MemoryMap as BootMemoryMap, MemoryRegion,
};
use uefi::boot::{self, AllocateType, MemoryType, PAGE_SIZE};
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};
use uefi::println;

use crate::Aborted;

/// Запас регионов сверх того, что показала предварительная карта.
///
/// Между замером и выходом карта успевает измениться: сама обёртка
/// `exit_boot_services` выделяет буфер под карту, а разбиение диапазонов под
/// ядро и фреймбуфер добавляет по паре записей. Множитель в
/// [`Handoff::estimate_capacity`] плюс этот запас покрывают и то, и другое с
/// огромным перебором — переполнить массив уже после выхода означало бы отдать
/// ядру усечённую карту памяти, а сообщить об этом было бы уже некуда.
const CAPACITY_SLACK: usize = 64;

/// Диапазон физической памяти, тип которого мы знаем лучше прошивки.
#[derive(Debug, Clone, Copy)]
pub struct Override {
    start: u64,
    end: u64,
    kind: MemoryKind,
}

impl Override {
    /// Пустой диапазон. На разбиение он не влияет: условия в [`emit_split`]
    /// написаны через строгое `cursor < end`, а `start > cursor` для нулевых
    /// границ не выполняется ни при каком курсоре.
    const NONE: Self = Self { start: 0, end: 0, kind: MemoryKind::Reserved };

    /// Округляет границы наружу до страниц: регионы в [`BootInfo`] обязаны быть
    /// кратны 4 КиБ, а фреймбуфер прошивка вполне может отдать невыровненным.
    ///
    /// Отсутствующий диапазон (нулевая длина — например, headless-загрузка без
    /// фреймбуфера) превращается в [`Override::NONE`].
    #[must_use]
    pub fn new(start: u64, len: u64, kind: MemoryKind) -> Self {
        if len == 0 {
            return Self::NONE;
        }
        let page = PAGE_SIZE as u64;
        match start
            .checked_add(len)
            .and_then(|end| end.checked_next_multiple_of(page))
        {
            Some(end) => Self { start: start & !(page - 1), end, kind },
            None => Self::NONE,
        }
    }
}

/// Блок памяти, переживающий `ExitBootServices`: [`BootInfo`], карта сегментов
/// ядра и массив [`MemoryRegion`] в одной аллокации.
pub struct Handoff {
    info: NonNull<BootInfo>,
    segments: u64,
    regions: NonNull<MemoryRegion>,
    capacity: usize,
}

impl Handoff {
    /// Оценка нужного числа регионов по текущей карте памяти.
    ///
    /// Карта снимается только ради размера и тут же освобождается: настоящая,
    /// с актуальным ключом, будет получена внутри `ExitBootServices`.
    pub fn estimate_capacity() -> Result<usize, Aborted> {
        let map = match boot::memory_map(MemoryType::LOADER_DATA) {
            Ok(map) => map,
            Err(err) => {
                println!("  [mem] cannot read the memory map ({err:?})");
                return Err(Aborted);
            }
        };
        let len = map.len();
        drop(map);

        println!("  [mem] firmware reports {len} memory descriptors right now");

        Ok(len * 2 + CAPACITY_SLACK)
    }

    /// Выделяет блок под `BootInfo`, карту сегментов ядра и `capacity`
    /// регионов, копирует туда подготовленный `BootInfo` вместе с сегментами и
    /// проставляет в нём указатель на скопированную карту.
    ///
    /// Раскладка блока: `BootInfo`, затем сегменты, затем регионы. Регионы
    /// последние, потому что заполняются уже после `ExitBootServices`, когда
    /// печатать что-либо о переполнении будет некуда.
    pub fn allocate(
        info: &BootInfo,
        capacity: usize,
        segments: &[KernelSegment],
    ) -> Result<Self, Aborted> {
        let segments_offset =
            mem::size_of::<BootInfo>().next_multiple_of(mem::align_of::<KernelSegment>());
        let Some(regions_offset) = segments
            .len()
            .checked_mul(mem::size_of::<KernelSegment>())
            .and_then(|arr| arr.checked_add(segments_offset))
            .map(|end| end.next_multiple_of(mem::align_of::<MemoryRegion>()))
        else {
            println!("  [mem] hand-off block size overflows");
            return Err(Aborted);
        };
        let Some(bytes) = capacity
            .checked_mul(mem::size_of::<MemoryRegion>())
            .and_then(|arr| arr.checked_add(regions_offset))
        else {
            println!("  [mem] hand-off block size overflows");
            return Err(Aborted);
        };
        let pages = bytes.div_ceil(PAGE_SIZE);

        let block = match boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages) {
            Ok(block) => block,
            Err(err) => {
                println!("  [mem] cannot allocate {pages} pages for the hand-off block ({err:?})");
                return Err(Aborted);
            }
        };

        let raw = block.as_ptr();

        // SAFETY: `allocate_pages` вернула `pages` полностью наших страниц,
        // то есть ровно `pages * PAGE_SIZE >= bytes` байт. Обнуляем весь блок,
        // чтобы ядру не достался мусор в хвосте массива регионов.
        unsafe {
            core::ptr::write_bytes(raw, 0, pages * PAGE_SIZE);
        }

        // Начало страницы выровнено на 4 КиБ, поэтому под `BootInfo` (align 8)
        // оно годится, а `regions_offset` кратен align_of::<MemoryRegion>().
        let info_ptr = raw.cast::<BootInfo>();
        // SAFETY: `info_ptr` указывает на начало нашего блока, там достаточно
        // места под `BootInfo`, память выровнена и записывается впервые.
        unsafe {
            info_ptr.write(*info);
        }

        // SAFETY: `segments_offset + segments.len() * size_of::<KernelSegment>()
        // <= regions_offset <= pages * PAGE_SIZE`, значит и указатель, и весь
        // массив остаются внутри аллокации.
        let segments_ptr = unsafe { raw.add(segments_offset) }.cast::<KernelSegment>();

        // Пустая карта передаётся как нулевой указатель: контракт `KernelImage`
        // кодирует отсутствие именно так, а не длиной.
        let segments_addr = if segments.is_empty() {
            0
        } else {
            // SAFETY: приёмник — только что обнулённая память внутри нашего
            // блока, места под `segments.len()` элементов хватает по расчёту
            // выше, выравнивание обеспечено `segments_offset`. Источник — срез
            // в стеке загрузчика, пересечься с блоком он не может.
            unsafe {
                core::ptr::copy_nonoverlapping(segments.as_ptr(), segments_ptr, segments.len());
            }
            segments_ptr as usize as u64
        };

        // SAFETY: `info_ptr` указывает на записанный выше `BootInfo` внутри
        // нашего блока; поле дописывается по месту, потому что адрес карты
        // становится известен только сейчас.
        unsafe {
            (*info_ptr).kernel.segments_ptr = segments_addr;
            (*info_ptr).kernel.segments_len = segments.len() as u64;
        }

        // SAFETY: `regions_offset + capacity * size_of::<MemoryRegion>() <=
        // pages * PAGE_SIZE`, значит указатель остаётся внутри аллокации.
        let regions_ptr = unsafe { raw.add(regions_offset) }.cast::<MemoryRegion>();

        println!(
            "  [mem] hand-off block: {pages} page(s) at {:#018x}, room for {capacity} regions",
            raw as usize as u64
        );

        Ok(Self {
            // SAFETY: `allocate_pages` не возвращает нулевой адрес (крейт
            // отдельно это гарантирует), а смещение не выводит за аллокацию.
            info: unsafe { NonNull::new_unchecked(info_ptr) },
            segments: segments_addr,
            regions: unsafe { NonNull::new_unchecked(regions_ptr) },
            capacity,
        })
    }

    /// Адрес `BootInfo` в памяти, переживающей выход из boot services.
    pub fn info_address(&self) -> u64 {
        self.info.as_ptr() as usize as u64
    }

    /// Адрес скопированной карты сегментов, либо `0`, если она пуста.
    pub fn segments_address(&self) -> u64 {
        self.segments
    }
}

/// Выходит из boot services и передаёт управление ядру. Не возвращается.
///
/// Всё, что печатается, должно быть напечатано до вызова: внутри консоли уже
/// нет. `overrides` описывает диапазоны, тип которых прошивка не знает (образ
/// ядра, фреймбуфер), и живёт на стеке — это допустимо, потому что стек
/// загрузчика остаётся нашим до самого прыжка; в ядро уходит только адрес
/// блока `Handoff`.
pub fn exit_and_jump(handoff: Handoff, entry: u64, overrides: &[Override]) -> ! {
    let entry_ptr = entry as usize as *const ();
    // SAFETY: `entry` вычислен из `e_entry` загруженного образа и проверен на
    // принадлежность ему. Контракт [`boot_info::KernelEntry`] обязывает ядро
    // экспортировать точку входа именно как `extern "C" fn(*const BootInfo) -> !`,
    // так что ABI совпадает. Указатель на функцию имеет тот же размер, что и
    // `*const ()`, и заведомо не нулевой.
    let kernel_main = unsafe { mem::transmute::<*const (), KernelEntry>(entry_ptr) };

    let info = handoff.info.as_ptr();
    let regions = handoff.regions.as_ptr();
    let capacity = handoff.capacity;

    // ─────────────── Точка невозврата ───────────────
    // SAFETY: к этому моменту все протоколы закрыты (GOP уронен в
    // `graphics`, файловые хендлы — в `kernel_image`), пул из-под образа ядра
    // освобождён, а `Handoff` держит только сырые указатели на страницы,
    // которые прошивка нам отдала насовсем. Обёртка сама снимает карту
    // непосредственно перед выходом, поэтому ключ гарантированно свежий.
    let map = unsafe { boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) };

    // Ниже — только работа с собственной памятью. Ни одного вызова boot
    // services, ни одного println!, ни одной аллокации.
    let len = build_regions(&map, regions, capacity, overrides);

    // SAFETY: `info` указывает на инициализированный `BootInfo` внутри блока,
    // выделенного до выхода; эта память не исчезает при `ExitBootServices`.
    unsafe {
        (*info).memory_map = BootMemoryMap { ptr: regions as usize as u64, len: len as u64 };
    }

    kernel_main(info)
}

/// Конвертирует дескрипторы UEFI в [`MemoryRegion`], сортирует и сливает
/// соседей. Возвращает число получившихся регионов.
///
/// Вызывается уже после `ExitBootServices`, поэтому не имеет права ни
/// выделять память, ни печатать, ни паниковать: всё делается на месте, в
/// заранее выделенном массиве.
fn build_regions(
    map: &MemoryMapOwned,
    regions: *mut MemoryRegion,
    capacity: usize,
    overrides: &[Override],
) -> usize {
    let mut sink = Sink { ptr: regions, capacity, len: 0 };

    for desc in map.entries() {
        let len = desc.page_count.saturating_mul(PAGE_SIZE as u64);
        if len == 0 {
            continue;
        }
        let Some(end) = desc.phys_start.checked_add(len) else {
            continue;
        };
        emit_split(&mut sink, desc.phys_start, end, kind_of(desc.ty), overrides);
    }

    if sink.len == 0 {
        return 0;
    }

    // SAFETY: `sink.len` первых элементов массива записаны через `Sink::push`,
    // то есть инициализированы; массив выровнен и принадлежит нам целиком.
    let slice = unsafe { core::slice::from_raw_parts_mut(regions, sink.len) };

    // Сортировка на месте: `sort_unstable_by_key` ничего не выделяет, что
    // после выхода из boot services принципиально.
    slice.sort_unstable_by_key(|region| region.start);

    // Слияние соседей одного типа: ядру проще работать с короткой картой, а
    // прошивки любят дробить свободную память на десятки одинаковых кусков.
    let mut write = 0usize;
    for read in 1..slice.len() {
        let current = slice[read];
        if slice[write].kind == current.kind && slice[write].end() == current.start {
            slice[write].len += current.len;
        } else {
            write += 1;
            slice[write] = current;
        }
    }

    write + 1
}

/// Разрезает диапазон `[start, end)` по границам `overrides` и складывает куски
/// в `sink`, подменяя тип там, где мы знаем его точнее прошивки.
fn emit_split(
    sink: &mut Sink,
    start: u64,
    end: u64,
    default_kind: MemoryKind,
    overrides: &[Override],
) {
    let mut cursor = start;

    while cursor < end {
        // Кусок, накрытый override'ом, начинающимся не позже курсора.
        if let Some(ov) = overrides
            .iter()
            .find(|ov| ov.start <= cursor && cursor < ov.end)
        {
            let piece_end = end.min(ov.end);
            sink.push(cursor, piece_end, ov.kind);
            cursor = piece_end;
            continue;
        }

        // Иначе кусок до ближайшего override'а, начинающегося дальше.
        let next = overrides
            .iter()
            .filter(|ov| ov.start > cursor && ov.start < end)
            .map(|ov| ov.start)
            .min()
            .unwrap_or(end);
        sink.push(cursor, next, default_kind);
        cursor = next;
    }
}

/// Отображение типов памяти UEFI в словарь ядра.
fn kind_of(ty: MemoryType) -> MemoryKind {
    match ty {
        // BOOT_SERVICES_* освобождается ровно в момент ExitBootServices, так
        // что для ядра эта память уже свободна.
        MemoryType::CONVENTIONAL
        | MemoryType::BOOT_SERVICES_CODE
        | MemoryType::BOOT_SERVICES_DATA => MemoryKind::Usable,
        // Здесь лежат сам загрузчик, блок hand-off и массив регионов: ядро
        // может забрать эту память, скопировав всё нужное.
        MemoryType::LOADER_CODE | MemoryType::LOADER_DATA => {
            MemoryKind::BootloaderReclaimable
        }
        MemoryType::ACPI_RECLAIM => MemoryKind::AcpiReclaimable,
        MemoryType::ACPI_NON_VOLATILE => MemoryKind::AcpiNvs,
        // RUNTIME_SERVICES_*, MMIO, UNUSABLE, PAL_CODE, PERSISTENT_MEMORY,
        // UNACCEPTED и вендорские типы — всё, из чего нельзя выделять.
        _ => MemoryKind::Reserved,
    }
}

/// Пишущий курсор по заранее выделенному массиву регионов.
struct Sink {
    ptr: *mut MemoryRegion,
    capacity: usize,
    len: usize,
}

impl Sink {
    fn push(&mut self, start: u64, end: u64, kind: MemoryKind) {
        if end <= start || self.len == self.capacity {
            return;
        }
        // SAFETY: `self.len < self.capacity`, значит слот лежит внутри массива,
        // выделенного под `capacity` элементов и выровненного под `MemoryRegion`.
        unsafe {
            self.ptr.add(self.len).write(MemoryRegion::new(start, end - start, kind));
        }
        self.len += 1;
    }
}
