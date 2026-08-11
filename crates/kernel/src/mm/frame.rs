//! Аллокатор физических кадров на битовой карте.
//!
//! # Почему битовая карта
//!
//! Альтернатива — список свободных кадров, где заголовок хранится в самом
//! свободном кадре и отдельного хранилища не требует. Он проигрывает здесь по
//! двум причинам. Во-первых, выдача подряд идущих кадров (`allocate_contiguous`)
//! в списке вырождается в перебор с сортировкой, а битовая карта решает её
//! просмотром байтов. Во-вторых, список хранит указатели внутри самих свободных
//! кадров, то есть каждое выделение и освобождение обязано прочитать кадр по
//! физическому адресу — а физическая память до и после включения таблиц ядра
//! адресуется по-разному (см. `phys_offset` ниже), и лишние обращения к ней
//! ровно в момент, когда строятся таблицы страниц, — источник тонких ошибок.
//!
//! Цена битовой карты невелика: один бит на кадр, то есть 1/32768 от объёма
//! памяти. На машине с 512 МиБ это 131072 кадра и ровно 16 КиБ карты; на 4 ГиБ —
//! 128 КиБ. Время выделения предсказуемо, освобождение — сброс одного бита без
//! шанса разрушить структуру данных мусором в освобождаемом кадре.
//!
//! # Где живёт сама карта
//!
//! Кучи в момент инициализации ещё нет (она поднимается позже и поверх этого
//! аллокатора), поэтому положить карту некуда, кроме как в саму физическую
//! память. Инициализация выбирает самый крупный `Usable`-регион, размещает карту
//! в его начале и немедленно помечает занятыми те кадры, которые карта заняла
//! собой, — иначе аллокатор рано или поздно выдал бы кадр из-под собственной
//! структуры данных. Самый крупный, а не первый подходящий, — чтобы не съесть
//! целиком небольшой регион и не раздробить память сильнее необходимого.
//!
//! # Идентичное отображение и прямое
//!
//! [`init`] вызывается до того, как ядро включит свои таблицы страниц: пока
//! действует identity-отображение от прошивки, физический адрес совпадает с
//! виртуальным. После активации таблиц ядра это перестаёт быть общим правилом, и
//! к физической памяти надо обращаться через прямое отображение
//! (`PHYS_MAP_BASE + phys`, см. [`PhysAddr::to_direct_map`]). Аллокатор хранит
//! смещение [`BitmapFrameAllocator::phys_offset`], равное нулю до активации и
//! `PHYS_MAP_BASE` после; переключает его [`BitmapFrameAllocator::use_direct_map`],
//! который обязан вызвать код подкачки сразу после `activate()`.

use crate::kprintln;
use crate::mm::{
    FrameAllocator, FrameStats, PAGE_SHIFT, PAGE_SIZE, PHYS_MAP_BASE, PhysAddr, VirtAddr,
};
use crate::sync::SpinLock;
use boot_info::{BootInfo, MemoryKind};
use core::fmt;
use core::mem::{align_of, size_of};
use core::ptr;

/// Зеркало [`boot_info::MemoryRegion`], у которого все поля скалярные.
///
/// Ровно та же причина, что и у одноимённой структуры в `main.rs`: массив
/// регионов приходит по адресу из-за границы доверия, а у настоящего
/// `MemoryRegion` поле `kind` — `enum`, и восстановление его из повреждённой
/// памяти дало бы значение вне списка вариантов, то есть UB ещё до того, как мы
/// успели бы отвергнуть карту. `u64`/`u32` валидны при любом битовом узоре.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawRegion {
    start: u64,
    len: u64,
    kind: u32,
    _reserved: u32,
}

const _: () = assert!(size_of::<RawRegion>() == size_of::<boot_info::MemoryRegion>());
const _: () = assert!(align_of::<RawRegion>() == align_of::<boot_info::MemoryRegion>());

const KIND_USABLE: u32 = MemoryKind::Usable as u32;

/// Сколько записей карты памяти ядро согласно обойти.
///
/// Реальные карты UEFI — десятки, изредка сотни записей. Абсурдное `len`
/// (повреждённый хэндофф, переполнение у загрузчика) не должно уводить нас
/// читать чужую память на гигабайты.
const MAX_REGIONS: u64 = 1024;

/// Верхняя граница физического адреса, который аллокатор берётся описывать.
///
/// Защита от повреждённой карты: регион с мусорным `start` мог бы потребовать
/// карту на терабайты. 1 ТиБ — это 256 Ми кадров и 32 МиБ битовой карты, заведомо
/// больше любой машины, на которую ядро сейчас рассчитано; всё, что окажется
/// выше, просто не будет управляться.
const MAX_TRACKED_PHYS: u64 = 1 << 40;

/// Почему не удалось поднять аллокатор кадров.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameInitError {
    /// Загрузчик передал пустую карту памяти.
    EmptyMemoryMap,
    /// Массив регионов лежит по невыровненному адресу.
    MisalignedMemoryMap,
    /// В карте нет ни одного `Usable`-региона.
    NoUsableMemory,
    /// Ни один `Usable`-регион не вмещает битовую карту.
    BitmapDoesNotFit { needed: usize },
}

impl fmt::Display for FrameInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyMemoryMap => f.write_str("bootloader passed an empty memory map"),
            Self::MisalignedMemoryMap => f.write_str("memory region array is misaligned"),
            Self::NoUsableMemory => f.write_str("memory map describes no usable memory"),
            Self::BitmapDoesNotFit { needed } => {
                write!(f, "no usable region large enough for a {needed}-byte frame bitmap")
            }
        }
    }
}

/// Аллокатор физических кадров.
///
/// Бит на кадр: `1` — кадр занят или недоступен, `0` — свободен. Карта начинается
/// со всех единиц, и нули в ней появляются только там, где карта памяти явно
/// сказала [`MemoryKind::Usable`]. То есть неописанные дыры в физическом
/// адресном пространстве недоступны по умолчанию, а не по недосмотру.
///
/// # Инварианты
///
/// * `bitmap` указывает на `bitmap_bytes` байт физической памяти, доступных по
///   адресу `bitmap + phys_offset`;
/// * кадры, занятые самой картой, помечены в ней занятыми;
/// * `bitmap_bytes == frames.div_ceil(8)`.
pub struct BitmapFrameAllocator {
    /// Физический адрес битовой карты.
    bitmap: PhysAddr,
    bitmap_bytes: usize,
    /// Диапазон кадров, которые карта вообще описывает: `[0, frames)`.
    frames: usize,
    /// Первый кадр битовой карты и число занятых ею кадров — чтобы отличать
    /// попытку освободить служебную память от честного `free`.
    bitmap_first_frame: usize,
    bitmap_frames: usize,
    /// Сколько кадров попало в пул (все `Usable`), и сколько из них свободно.
    total: usize,
    free: usize,
    /// Смещение, по которому физическая память видна процессору прямо сейчас:
    /// `0` при identity-отображении от прошивки и `PHYS_MAP_BASE` после
    /// активации таблиц ядра. См. доккомментарий модуля.
    phys_offset: usize,
    /// С какого кадра начинать следующий поиск. Чисто оптимизация: без него
    /// каждое выделение начинало бы просмотр с нулевого кадра.
    cursor: usize,
}

impl BitmapFrameAllocator {
    /// Построить аллокатор по карте памяти загрузчика.
    ///
    /// # Safety
    ///
    /// * `info` должен быть проверенным хэндоффом (magic/revision совпали), а его
    ///   `memory_map.ptr` — указывать на `memory_map.len` записей;
    /// * вызывать только пока действует identity-отображение прошивки: и массив
    ///   регионов, и выбранная под битовую карту память адресуются здесь
    ///   физическими адресами напрямую;
    /// * вызывать один раз: конструктор пишет в физическую память, ещё никому не
    ///   принадлежащую, и второй экземпляр раздавал бы те же самые кадры.
    pub unsafe fn new(info: &BootInfo) -> Result<Self, FrameInitError> {
        let map = &info.memory_map;
        if map.ptr == 0 || map.len == 0 {
            return Err(FrameInitError::EmptyMemoryMap);
        }
        if map.ptr % align_of::<RawRegion>() as u64 != 0 {
            return Err(FrameInitError::MisalignedMemoryMap);
        }

        let count = map.len.min(MAX_REGIONS) as usize;
        let base = map.ptr as *const RawRegion;

        // Проход 1: верхняя граница физической памяти и самый крупный
        // Usable-регион. Граница считается по ВСЕМ регионам, а не только по
        // usable: карта должна описывать и дыры тоже, иначе кадр, случайно
        // выданный из неописанной области, не с чем будет сверить.
        let mut top: u64 = 0;
        let mut host_start: u64 = 0;
        let mut host_len: u64 = 0;
        for index in 0..count {
            // SAFETY: загрузчик заявил `len` записей по адресу `ptr`, индекс
            // ограничен `count <= len`, выравнивание проверено выше, память
            // BootloaderReclaimable ещё никем не переиспользована. Читаем
            // `RawRegion`, а не `MemoryRegion`, чтобы не создать enum вне
            // списка вариантов.
            let region = unsafe { ptr::read(base.add(index)) };
            // checked_add: `start` и `len` пришли снаружи, и повреждённая карта
            // не должна ронять ядро паникой переполнения внутри инициализации.
            let Some(end) = region.start.checked_add(region.len) else {
                continue;
            };
            top = top.max(end.min(MAX_TRACKED_PHYS));
            if region.kind == KIND_USABLE && region.len > host_len {
                host_start = region.start;
                host_len = region.len;
            }
        }

        if host_len == 0 {
            return Err(FrameInitError::NoUsableMemory);
        }
        let frames = (top >> PAGE_SHIFT) as usize;
        if frames == 0 {
            return Err(FrameInitError::EmptyMemoryMap);
        }

        let bitmap_bytes = frames.div_ceil(8);
        let bitmap_span = bitmap_bytes.next_multiple_of(PAGE_SIZE);

        // Карта кладётся в начало выбранного региона. Нулевой кадр пропускается
        // намеренно: физический адрес 0 служит в хэндоффе признаком «значения
        // нет», и выдавать его наружу — напрашиваться на путаницу.
        let bitmap_phys = align_up_u64(host_start, PAGE_SIZE as u64).max(PAGE_SIZE as u64);
        let tail = host_start.saturating_add(host_len).saturating_sub(bitmap_phys);
        if tail < bitmap_span as u64 || bitmap_phys >= MAX_TRACKED_PHYS {
            return Err(FrameInitError::BitmapDoesNotFit { needed: bitmap_span });
        }

        let mut this = Self {
            bitmap: PhysAddr::new(bitmap_phys),
            bitmap_bytes,
            frames,
            bitmap_first_frame: (bitmap_phys >> PAGE_SHIFT) as usize,
            bitmap_frames: bitmap_span / PAGE_SIZE,
            total: 0,
            free: 0,
            // Конструктор работает при identity-отображении — смещения нет.
            phys_offset: 0,
            cursor: 0,
        };

        // Всё занято, пока карта памяти не докажет обратное.
        // SAFETY: `bitmap_phys .. + bitmap_span` целиком лежит внутри
        // Usable-региона (проверено выше), а при identity-отображении этот
        // физический диапазон читается и пишется по тому же адресу. Память
        // никому не принадлежит: аллокатора до этой строки не существовало.
        unsafe { ptr::write_bytes(this.bitmap_ptr(), 0xFF, bitmap_bytes) };

        // Проход 2: раздать нули кадрам из Usable-регионов.
        for index in 0..count {
            // SAFETY: см. проход 1.
            let region = unsafe { ptr::read(base.add(index)) };
            if region.kind != KIND_USABLE {
                // Всё остальное аллокатор не выдаёт никогда. Отдельно про
                // BootloaderReclaimable: несмотря на название, эта память занята
                // прямо сейчас — в ней лежат сам BootInfo, массив регионов,
                // который мы читаем в этом цикле, и массив сегментов ядра.
                // Выдать оттуда кадр — значит затереть карту памяти под собой.
                //
                // TODO(Phase 3): вернуть BootloaderReclaimable в пул после того,
                // как ядро скопирует BootInfo, регионы и сегменты в свои
                // структуры (в кучу), и убедится, что указателей в ту память не
                // осталось.
                continue;
            }
            this.release_region(region.start, region.len);
        }

        if this.total == 0 {
            return Err(FrameInitError::NoUsableMemory);
        }

        // Кадры, занятые самой битовой картой. Пометить их надо именно здесь,
        // сразу после раздачи нулей: до этой строки они выглядят свободными.
        for offset in 0..this.bitmap_frames {
            this.reserve(this.bitmap_first_frame + offset);
        }
        // Нулевой кадр: см. выбор `bitmap_phys` выше.
        this.reserve(0);

        Ok(this)
    }

    /// Пометить свободными полные кадры, попадающие в `[start, start + len)`.
    fn release_region(&mut self, start: u64, len: u64) {
        let Some(end) = start.checked_add(len) else {
            return;
        };
        // Частичные кадры по краям не отдаём: регион, начавшийся в середине
        // страницы, делит её с чем-то, что usable не является.
        let first = align_up_u64(start, PAGE_SIZE as u64) >> PAGE_SHIFT;
        let last = (end >> PAGE_SHIFT).min(self.frames as u64);
        if first >= last {
            return;
        }
        for index in (first as usize)..(last as usize) {
            // Проверка `is_used` не только оптимизация: она делает функцию
            // идемпотентной, так что перекрывающиеся регионы в кривой карте не
            // раздуют счётчик свободных кадров.
            if self.is_used(index) {
                self.set_free(index);
                self.total += 1;
                self.free += 1;
            }
        }
    }

    /// Изъять кадр из пула навсегда (служебные структуры, нулевой кадр).
    fn reserve(&mut self, index: usize) {
        if index < self.frames && !self.is_used(index) {
            self.set_used(index);
            self.free -= 1;
        }
    }

    /// Переключить аллокатор на прямое отображение физической памяти.
    ///
    /// Вызывается один раз, сразу после `AddressSpace::activate()`: с этого
    /// момента физический адрес больше не равен виртуальному, и битовая карта (а
    /// также обнуляемые кадры) доступны по `phys + PHYS_MAP_BASE`.
    ///
    /// # Safety
    ///
    /// К моменту вызова прямое отображение обязано существовать и покрывать всю
    /// физическую память, которую описывает карта. Вызов раньше времени сделает
    /// первое же обращение к битовой карте обращением по неотображённому адресу.
    pub unsafe fn use_direct_map(&mut self) {
        self.phys_offset = PHYS_MAP_BASE;
    }

    /// Текущее смещение доступа к физической памяти.
    #[must_use]
    pub fn phys_offset(&self) -> usize {
        self.phys_offset
    }

    /// Виртуальный адрес, по которому физическая страница доступна прямо сейчас.
    ///
    /// Нужен коду подкачки: выделив кадр под таблицу страниц, в него надо что-то
    /// записать, а способ до него дотянуться зависит от того, включены ли уже
    /// таблицы ядра.
    #[must_use]
    pub fn access(&self, phys: PhysAddr) -> VirtAddr {
        VirtAddr::new((phys.as_u64() as usize).wrapping_add(self.phys_offset))
    }

    /// Выделить `count` подряд идущих обнулённых кадров, вернув адрес первого.
    ///
    /// На битовой карте это стоит того же просмотра, что и одиночное выделение,
    /// поэтому отдельная структура данных не нужна. Пригодится под DMA-буферы и
    /// под всё, что железо адресует физически непрерывно.
    pub fn allocate_contiguous(&mut self, count: usize) -> Option<PhysAddr> {
        let first = self.find_run(count)?;
        for index in first..(first + count) {
            self.set_used(index);
        }
        self.free -= count;
        self.cursor = first + count;
        let base = frame_addr(first);
        for offset in 0..count {
            self.zero_frame(frame_addr(first + offset));
        }
        Some(base)
    }

    /// Вернуть в пул `count` подряд идущих кадров, выданных
    /// [`Self::allocate_contiguous`].
    ///
    /// # Safety
    ///
    /// Те же условия, что у [`FrameAllocator::free`], для каждого кадра
    /// диапазона.
    pub unsafe fn free_contiguous(&mut self, base: PhysAddr, count: usize) {
        for offset in 0..count {
            let addr = PhysAddr::new(base.as_u64().wrapping_add((offset * PAGE_SIZE) as u64));
            // SAFETY: условия делегированы вызывающему контрактом метода.
            unsafe { self.free(addr) };
        }
    }

    /// Найти `count` подряд идущих свободных кадров.
    fn find_run(&self, count: usize) -> Option<usize> {
        if count == 0 || count > self.free {
            return None;
        }
        // Два прохода: от курсора до конца карты, затем с самого начала. Пробег
        // через границу оборота не продолжается — кадров за концом карты нет.
        for start in [self.cursor.min(self.frames), 0] {
            let mut run = 0usize;
            let mut index = start;
            while index < self.frames {
                // Байт из одних единиц — восемь занятых кадров подряд; на
                // заполненной памяти это основной случай, и перебирать его по
                // биту незачем.
                if index & 7 == 0 && index + 8 <= self.frames && self.byte(index >> 3) == 0xFF {
                    index += 8;
                    run = 0;
                    continue;
                }
                if self.is_used(index) {
                    run = 0;
                } else {
                    run += 1;
                    if run == count {
                        return Some(index + 1 - count);
                    }
                }
                index += 1;
            }
            if start == 0 {
                break;
            }
        }
        None
    }

    /// Обнулить кадр.
    ///
    /// Контракт [`FrameAllocator`] требует выдавать кадры чистыми, и это не
    /// перестраховка: из этих кадров строятся таблицы страниц, а любой мусор в
    /// записи таблицы процессор прочтёт как валидное отображение — с битом
    /// присутствия, произвольным физическим адресом и произвольными правами.
    fn zero_frame(&self, frame: PhysAddr) {
        let ptr = self.access(frame).as_mut_ptr::<u8>();
        // SAFETY: кадр только что помечен занятым в карте, то есть больше никому
        // не принадлежит, и лежит внутри Usable-региона. `access` даёт адрес,
        // по которому физическая память видна при текущем режиме отображения.
        unsafe { ptr::write_bytes(ptr, 0, PAGE_SIZE) };
    }

    /// Адрес битовой карты в текущем режиме отображения.
    fn bitmap_ptr(&self) -> *mut u8 {
        self.access(self.bitmap).as_mut_ptr::<u8>()
    }

    fn byte(&self, index: usize) -> u8 {
        debug_assert!(index < self.bitmap_bytes);
        // SAFETY: инвариант типа — `bitmap` указывает на `bitmap_bytes` байт,
        // доступных по `bitmap + phys_offset`; индекс внутри этого диапазона,
        // потому что все вызывающие получают его из номера кадра `< frames`, а
        // `bitmap_bytes == frames.div_ceil(8)`.
        unsafe { ptr::read(self.bitmap_ptr().add(index)) }
    }

    fn set_byte(&mut self, index: usize, value: u8) {
        debug_assert!(index < self.bitmap_bytes);
        // SAFETY: см. `byte`.
        unsafe { ptr::write(self.bitmap_ptr().add(index), value) };
    }

    fn is_used(&self, index: usize) -> bool {
        self.byte(index >> 3) & (1 << (index & 7)) != 0
    }

    fn set_used(&mut self, index: usize) {
        let byte = index >> 3;
        self.set_byte(byte, self.byte(byte) | (1 << (index & 7)));
    }

    fn set_free(&mut self, index: usize) {
        let byte = index >> 3;
        self.set_byte(byte, self.byte(byte) & !(1 << (index & 7)));
    }
}

impl FrameAllocator for BitmapFrameAllocator {
    fn allocate(&mut self) -> Option<PhysAddr> {
        let index = self.find_run(1)?;
        self.set_used(index);
        self.free -= 1;
        self.cursor = index + 1;
        let frame = frame_addr(index);
        self.zero_frame(frame);
        Some(frame)
    }

    unsafe fn free(&mut self, frame: PhysAddr) {
        if !frame.is_page_aligned() {
            kprintln!("mm: refusing to free unaligned frame {:?}", frame);
            return;
        }
        let index = (frame.as_u64() >> PAGE_SHIFT) as usize;
        if index >= self.frames {
            kprintln!("mm: refusing to free frame {:?} outside the bitmap", frame);
            return;
        }
        // Освобождение служебной памяти — не «двойное освобождение», а нечто
        // худшее: битовая карта живёт в этих кадрах, и выдав их наружу,
        // аллокатор затрёт сам себя.
        if index.wrapping_sub(self.bitmap_first_frame) < self.bitmap_frames {
            kprintln!("mm: refusing to free frame {:?} holding the frame bitmap", frame);
            return;
        }
        if !self.is_used(index) {
            kprintln!("mm: double free of frame {:?}, ignored", frame);
            return;
        }
        self.set_free(index);
        self.free += 1;
        // Курсор отводится назад: освобождённый кадр горячий в кеше, и следующее
        // выделение разумнее начать искать именно с него.
        self.cursor = self.cursor.min(index);
    }

    fn stats(&self) -> FrameStats {
        FrameStats { total: self.total, free: self.free }
    }
}

/// Физический адрес кадра по его номеру.
fn frame_addr(index: usize) -> PhysAddr {
    PhysAddr::new((index as u64) << PAGE_SHIFT)
}

/// Округление вверх до кратного `align` (степень двойки) без переполнения.
fn align_up_u64(value: u64, align: u64) -> u64 {
    match value.checked_add(align - 1) {
        Some(sum) => sum & !(align - 1),
        // Значение у самого потолка u64 — карта заведомо повреждена. Вернуть
        // невыровненный максимум безопасно: любой диапазон, начинающийся здесь,
        // отсечётся проверкой `first >= last` у вызывающего.
        None => value,
    }
}

/// Глобальный аллокатор кадров. `None`, пока не вызван [`init`].
static FRAMES: SpinLock<Option<BitmapFrameAllocator>> = SpinLock::new(None);

/// Поднять глобальный аллокатор кадров по карте памяти загрузчика.
///
/// Возвращает состояние пула сразу после инициализации — удобно напечатать.
///
/// # Safety
///
/// Требования [`BitmapFrameAllocator::new`]: проверенный `info`, действующее
/// identity-отображение, ровно один вызов за время жизни ядра.
pub unsafe fn init(info: &BootInfo) -> Result<FrameStats, FrameInitError> {
    // SAFETY: см. контракт функции.
    let allocator = unsafe { BitmapFrameAllocator::new(info) }?;
    let stats = allocator.stats();
    *FRAMES.lock() = Some(allocator);
    Ok(stats)
}

/// Выполнить операцию над глобальным аллокатором кадров.
///
/// Возвращает `None`, если [`init`] ещё не вызывался. Паникует при попытке
/// войти повторно из вложенного вызова: выдать второй `&mut` на то же состояние
/// нельзя, а тихо вернуть `None` — значит превратить ошибку в загадочную
/// нехватку памяти где-то дальше.
pub fn with<R>(f: impl FnOnce(&mut BitmapFrameAllocator) -> R) -> Option<R> {
    // `try_lock`, а не `lock`, и это не оптимизация. Пока процессор один, а
    // прерывания на время удержания запрещены, занятый лок означает ровно одно:
    // мы уже внутри `with` и пришли сюда из `f`. Обычный `lock` в этом случае
    // стал бы ждать сам себя — не паника, а вечное молчание, что заметно хуже
    // прежнего флага повторного входа. `try_lock` возвращает ту же внятную
    // диагностику, что была до перехода на локи.
    //
    // TODO(SMP): на нескольких ядрах занятый лок станет обычной конкуренцией.
    // Тогда ждать придётся честным `lock`, а рекурсию отличать по отметке
    // владельца (номеру ядра) внутри самого лока.
    let Some(mut frames) = FRAMES.try_lock() else {
        panic!("mm::frame::with is not re-entrant");
    };
    // `f` вправе печатать: вывод берёт свои локи (serial, консоль) и никогда не
    // просит кадров, поэтому цикла ожидания через диагностику здесь не выходит.
    frames.as_mut().map(f)
}

/// Состояние пула кадров. Нули, если аллокатор ещё не поднят.
#[must_use]
pub fn stats() -> FrameStats {
    with(|allocator| allocator.stats()).unwrap_or_default()
}

/// Перевести глобальный аллокатор на прямое отображение физической памяти.
///
/// # Safety
///
/// Условия [`BitmapFrameAllocator::use_direct_map`]: прямое отображение уже
/// построено и активно.
pub unsafe fn use_direct_map() {
    // SAFETY: условие делегировано вызывающему.
    let switched = with(|allocator| unsafe { allocator.use_direct_map() });
    if switched.is_none() {
        kprintln!("mm: frame allocator is not initialised, direct map switch ignored");
    }
}
