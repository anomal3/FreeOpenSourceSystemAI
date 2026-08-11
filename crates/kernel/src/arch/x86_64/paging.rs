//! Страничная трансляция x86-64: PML4 → PDPT → PD → PT, страницы только 4 КиБ.
//!
//! # Почему только 4 КиБ
//!
//! Записи уровней PD и PDPT умеют быть листовыми (биты `PS`), давая страницы
//! 2 МиБ и 1 ГиБ. Соблазн велик — таблиц было бы на два порядка меньше, — но
//! W^X работает с гранулярностью отображения: сегменты ELF выровнены на 4 КиБ,
//! и на 2-мегабайтной странице код неизбежно оказался бы в одной странице с
//! данными, а значит либо данные стали бы исполняемыми, либо код — записываемым.
//! Ради этого и строится вся конструкция, поэтому большие страницы не
//! используются нигде, а встреченная чужая большая страница считается ошибкой.
//!
//! # Как выглядит запись таблицы
//!
//! Формат одинаков на всех четырёх уровнях (см. Intel SDM, Vol. 3A, 4.5):
//!
//! ```text
//!   63    62..52   51..12         11..9   8  7  6  5  4   3   2  1  0
//!   NX    ignored  физический     avail   G  PS D  A  PCD PWT US RW P
//!                  адрес (40 бит)
//! ```
//!
//! Права **объединяются по И** вдоль всего пути трансляции: если на PML4 не
//! стоит `R/W`, страница не будет записываемой, что бы ни стояло в PT. Отсюда
//! правило, которого держится этот код: **ограничения живут только в листовой
//! записи**, а промежуточные уровни делаются максимально разрешающими
//! (`P|R/W`, без `NX`). Иначе одна таблица, общая для кода и данных — а на
//! уровне PD такая общая таблица покрывает 2 МиБ и почти наверняка содержит и
//! то и другое, — навязала бы всем своим потомкам самый строгий общий
//! знаменатель. Именно так и возникает классическое «страница почему-то
//! read-only» или «код почему-то не исполняется».
//!
//! Единственное исключение — бит `U/S`: он тоже объединяется по И, поэтому
//! разрешающим значением для него было бы `U/S = 1` на всех промежуточных
//! уровнях, а это открыло бы пользовательскому режиму путь к любой таблице
//! ядра. Поэтому `U/S` на промежуточных уровнях выставляется не заранее, а по
//! требованию: только когда сквозь эту ветку прокладывается отображение с
//! [`PageFlags::USER`].
//!
//! # Доступ к самим таблицам
//!
//! Таблицы адресуются процессором физически, а код ядра исполняется по
//! виртуальным адресам, поэтому «записать в PML4» — это всегда вопрос «по
//! какому виртуальному адресу видна эта физическая страница».
//!
//! До активации собственных таблиц действует identity-отображение прошивки:
//! физический адрес равен виртуальному, смещение нулевое. После активации
//! правильный путь — прямое отображение [`PHYS_MAP_BASE`], которое переживёт
//! снятие identity в Phase 3. Поэтому [`PageTable`] хранит смещение, с которым
//! пересчитывает физический адрес таблицы в виртуальный, и меняет его ровно в
//! момент загрузки `CR3`. Аллокатор кадров устроен так же — иначе он не смог бы
//! обнулять выдаваемые кадры.

use super::{cpuid, rdmsr, wrmsr};
use crate::mm::{
    AddressSpace, FrameAllocator, HEAP_BASE, HEAP_SIZE, MapError, PAGE_SIZE, PHYS_MAP_BASE,
    PageFlags, PhysAddr, STACK_SIZE, STACK_TOP, VirtAddr,
};
use boot_info::{BootInfo, MemoryKind};
use core::arch::asm;
use core::cell::Cell;
use core::fmt;
use core::mem::{align_of, size_of};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

// --- Биты записи таблицы страниц ---------------------------------------------

/// `P` — запись валидна. Без него все остальные биты процессор игнорирует и
/// считает своими (кроме бита 63 при выключенном `EFER.NXE`, см. ниже).
const ENTRY_PRESENT: u64 = 1 << 0;
/// `R/W` — запись разрешена. Сбросить его на промежуточном уровне значит
/// запретить запись во всё поддерево.
const ENTRY_WRITABLE: u64 = 1 << 1;
/// `U/S` — доступ из ring 3.
const ENTRY_USER: u64 = 1 << 2;
/// `PWT` — сквозная запись вместо write-back.
const ENTRY_WRITE_THROUGH: u64 = 1 << 3;
/// `PCD` — кеширование запрещено.
const ENTRY_CACHE_DISABLE: u64 = 1 << 4;
/// `PS` — на уровнях PDPT/PD означает листовую запись (1 ГиБ / 2 МиБ). На
/// уровне PT этот бит означает совсем другое (`PAT`) и обязан быть нулём.
const ENTRY_HUGE: u64 = 1 << 7;
/// `NX` — исполнение запрещено. Работает только при `EFER.NXE = 1`.
const ENTRY_NO_EXECUTE: u64 = 1 << 63;
/// Биты 51..12: физический адрес следующей таблицы или конечного кадра.
const ENTRY_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// Сколько записей в таблице любого уровня: 4096 байт / 8 байт.
const ENTRIES_PER_TABLE: usize = PAGE_SIZE / size_of::<u64>();

/// Номер уровня листовой таблицы (PT) в нумерации [`VirtAddr::table_index`].
const LEVEL_PT: usize = 0;
/// Номер уровня корневой таблицы (PML4).
const LEVEL_PML4: usize = 3;

// --- Регистры процессора ------------------------------------------------------

/// `IA32_EFER` — регистр расширенных возможностей длинного режима.
const IA32_EFER: u32 = 0xC000_0080;
/// `EFER.NXE`, бит 11: разрешает трактовать бит 63 записи как `NX`.
const EFER_NXE: u64 = 1 << 11;

/// `CR0.WP`, бит 16: заставляет проверять бит `R/W` и для кода ring 0.
const CR0_WP: u64 = 1 << 16;

/// Лист CPUID с расширенными флагами возможностей.
const CPUID_EXT_FEATURES: u32 = 0x8000_0001;
/// Лист CPUID, возвращающий максимальный поддерживаемый расширенный лист.
const CPUID_EXT_MAX: u32 = 0x8000_0000;
/// `CPUID.80000001H:EDX[20]` — поддержка бита `NX`.
const CPUID_EDX_NX: u32 = 1 << 20;

/// Удалось ли включить `EFER.NXE`.
///
/// Пока `NXE` не установлен, бит 63 записи таблицы — **зарезервированный**, и
/// первое же обращение к такой странице даёт #PF с признаком reserved-bit
/// violation вместо ожидаемого доступа. То есть «на всякий случай» ставить `NX`
/// нельзя: либо `NXE` включён и `NX` работает, либо бит обязан остаться нулём.
/// Флаг и хранит этот выбор, чтобы [`leaf_bits`] не строила заведомо ядовитые
/// записи на машине без поддержки NX.
static NX_ENABLED: AtomicBool = AtomicBool::new(false);

/// Нижняя граница стека ядра — самый младший адрес, который ещё отображён.
pub const KERNEL_STACK_BOTTOM: usize = STACK_TOP - STACK_SIZE;

/// Страница-ловушка под стеком: намеренно **не** отображается.
///
/// Стек растёт вниз, и переполнение без ловушки означало бы тихую запись в
/// соседнюю память — ошибку, которая проявится через сотни тысяч инструкций в
/// совершенно постороннем месте. С неотображённой страницей то же переполнение
/// даёт #PF на первом же байте за границей.
pub const KERNEL_STACK_GUARD: usize = KERNEL_STACK_BOTTOM - PAGE_SIZE;

/// Одна запись таблицы страниц.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
struct Entry(u64);

impl Entry {
    const fn is_present(self) -> bool {
        self.0 & ENTRY_PRESENT != 0
    }

    const fn is_huge(self) -> bool {
        self.0 & ENTRY_HUGE != 0
    }

    /// Физический адрес, на который указывает запись.
    const fn addr(self) -> PhysAddr {
        PhysAddr::new(self.0 & ENTRY_ADDR_MASK)
    }
}

/// Адресное пространство x86-64: дерево из четырёх уровней с корнем в PML4.
pub struct PageTable {
    /// Физический адрес PML4 — то, что уезжает в `CR3`.
    root: PhysAddr,
    /// Смещение, прибавив которое к физическому адресу таблицы, получаем
    /// виртуальный адрес, по которому она сейчас доступна ядру. Ноль, пока
    /// работает identity-отображение прошивки; [`PHYS_MAP_BASE`] после того,
    /// как процессор переключён на эти таблицы.
    ///
    /// `Cell`, а не обычное поле: [`AddressSpace::activate`] по контракту
    /// принимает `&self`. Гонки исключены — тип не `Sync` именно из-за `Cell`.
    phys_offset: Cell<usize>,
}

impl PageTable {
    /// Виртуальный адрес записи `index` в таблице, лежащей по физическому
    /// адресу `table`.
    fn entry_ptr(&self, table: PhysAddr, index: usize) -> *mut Entry {
        debug_assert!(index < ENTRIES_PER_TABLE);
        let base = table.as_u64() as usize + self.phys_offset.get();
        (base as *mut Entry).wrapping_add(index)
    }

    /// Спуститься на уровень вниз, создав таблицу, если её ещё нет.
    ///
    /// Возвращает физический адрес таблицы следующего уровня.
    ///
    /// # Safety
    ///
    /// `table` должен быть физическим адресом настоящей таблицы этого дерева,
    /// доступной по текущему `phys_offset`.
    unsafe fn descend(
        &self,
        table: PhysAddr,
        index: usize,
        want_user: bool,
        alloc: &mut impl FrameAllocator,
    ) -> Result<PhysAddr, MapError> {
        // Промежуточная запись — максимально разрешающая: `P|R/W` и без `NX`.
        // Все ограничения ставит листовая запись, см. доккомент модуля.
        let mut wanted = ENTRY_PRESENT | ENTRY_WRITABLE;
        if want_user {
            wanted |= ENTRY_USER;
        }

        let slot = self.entry_ptr(table, index);
        // SAFETY: `slot` указывает внутрь таблицы из 512 записей (индекс
        // проверен в `entry_ptr`), которая отображена по текущему смещению.
        // `volatile` — потому что второй участник этой памяти аппаратный: MMU
        // читает записи и сам дописывает биты A/D, поэтому оптимизировать
        // обращения по правилам обычной памяти нельзя.
        let existing = unsafe { ptr::read_volatile(slot) };

        if existing.is_present() {
            // Большая страница на пути означает, что кто-то (прошивка или
            // ошибка в этом коде) уже отобразил сюда 2 МиБ или 1 ГиБ. Резать её
            // на части ради одной страницы мы не умеем и не хотим.
            if existing.is_huge() {
                return Err(MapError::AlreadyMapped);
            }
            let missing = wanted & !existing.0;
            if missing != 0 {
                // SAFETY: тот же слот той же таблицы; биты только добавляются,
                // адрес следующей таблицы не трогается.
                unsafe { ptr::write_volatile(slot, Entry(existing.0 | missing)) };
            }
            return Ok(existing.addr());
        }

        let frame = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        debug_assert!(frame.is_page_aligned());
        // SAFETY: кадр только что выдан аллокатором, обнулён им же (контракт
        // `FrameAllocator`) и никому больше не принадлежит; слот пуст.
        unsafe { ptr::write_volatile(slot, Entry(frame.as_u64() | wanted)) };
        Ok(frame)
    }
}

impl AddressSpace for PageTable {
    fn new(alloc: &mut impl FrameAllocator) -> Result<Self, MapError> {
        // Строить записи с битом `NX` можно только после того, как он разрешён
        // в EFER, а `map` начнёт их строить сразу же, — поэтому включаем здесь,
        // до появления первой записи, а не перед `activate`.
        enable_nx();
        let root = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        Ok(Self { root, phys_offset: Cell::new(0) })
    }

    unsafe fn map(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        alloc: &mut impl FrameAllocator,
    ) -> Result<(), MapError> {
        if !virt.is_page_aligned() || !phys.is_page_aligned() {
            return Err(MapError::Misaligned);
        }
        // Контракт трейта: такая страница не должна появиться даже по ошибке.
        if flags.contains(PageFlags::WRITE) && flags.contains(PageFlags::EXEC) {
            return Err(MapError::WriteExecute);
        }

        let want_user = flags.contains(PageFlags::USER);
        let mut table = self.root;
        // От PML4 вниз до PD включительно: (3, 2, 1). Уровень 0 — листовой.
        for level in (LEVEL_PT + 1..=LEVEL_PML4).rev() {
            // SAFETY: на первой итерации `table` — корень этого дерева, дальше
            // — то, что вернула предыдущая `descend`, то есть таблица этого же
            // дерева, доступная по текущему смещению.
            table = unsafe { self.descend(table, virt.table_index(level), want_user, alloc)? };
        }

        let slot = self.entry_ptr(table, virt.table_index(LEVEL_PT));
        // SAFETY: `slot` — запись листовой таблицы, полученной спуском выше.
        let existing = unsafe { ptr::read_volatile(slot) };
        if existing.is_present() && existing.addr() != phys {
            // Тот же виртуальный адрес указывал бы на другой кадр — это почти
            // наверняка ошибка в раскладке, а не намерение. А вот повторное
            // отображение того же кадра с другими правами разрешено: именно так
            // общее identity-отображение уточняется до W^X по сегментам ядра.
            return Err(MapError::AlreadyMapped);
        }

        // SAFETY: слот принадлежит нашей таблице; записываем корректно
        // сформированную запись на тот же (или новый, если слот был пуст) кадр.
        unsafe { ptr::write_volatile(slot, Entry(leaf_bits(phys, flags))) };

        // Кеш трансляций не знает, что запись изменилась, — сбрасываем её явно.
        // Делаем это и для ранее пустого слота: дёшево, а рассуждение «x86 не
        // кеширует отсутствующие трансляции» верно ровно до первой ошибки в
        // рассуждении. Если это дерево ещё не активно, `invlpg` просто выкинет
        // запись текущего (прошивочного) пространства — безвредно.
        // SAFETY: `invlpg` не обращается к памяти и не может отказать.
        unsafe { invlpg(virt) };
        Ok(())
    }

    fn root(&self) -> PhysAddr {
        self.root
    }

    unsafe fn activate(&self) {
        // `CR0.WP` — не про W^X напрямую, но про то же самое: без него ring 0
        // игнорирует бит `R/W` и спокойно пишет в страницы, размеченные как
        // read-only, из-за чего защита `.rodata` и `.text` от записи была бы
        // чисто декоративной. Ставим до загрузки CR3: таблицы прошивки всё
        // равно отображают память ядра как записываемую, так что момент
        // включения безразличен, а забыть его — нет.
        // SAFETY: чтение и запись CR0 с единственным добавленным битом; ядро
        // не пишет в собственные read-only сегменты, поэтому включение WP не
        // ломает уже работающий код.
        unsafe { write_cr0(read_cr0() | CR0_WP) };

        // SAFETY: контракт трейта требует от вызывающего, чтобы это дерево уже
        // отображало текущий код, стек и данные. Запись в CR3 попутно
        // сбрасывает весь TLB (глобальных страниц мы не заводим — см.
        // `leaf_bits`), поэтому отдельная инвалидация не нужна.
        unsafe { write_cr3(self.root) };

        // С этого момента таблицы правятся через прямое отображение: identity
        // сейчас ещё работает, но исчезнет в Phase 3, а direct map — нет.
        //
        // Аллокатор кадров хранит такое же смещение отдельно и о переключении
        // не знает: сразу после `activate` вызывающий обязан сообщить ему о нём
        // (`mm::frame::use_direct_map`), иначе битмап продолжит адресоваться по
        // identity и переживёт его снятие ровно до первого обращения.
        self.phys_offset.set(PHYS_MAP_BASE);
    }
}

/// Собрать листовую запись PT для кадра `phys` с правами `flags`.
fn leaf_bits(phys: PhysAddr, flags: PageFlags) -> u64 {
    // Бит `PS` (он же `PAT` на уровне PT) остаётся нулём: единица здесь выбрала
    // бы запись PAT, о которой мы ничего не сообщали процессору.
    //
    // Бит `G` тоже намеренно не ставится, хотя отображения ядра глобальны по
    // смыслу: глобальные записи переживают перезагрузку CR3, то есть ровно тот
    // сброс TLB, на который здесь всё и рассчитано.
    let mut bits = (phys.as_u64() & ENTRY_ADDR_MASK) | ENTRY_PRESENT;
    if flags.contains(PageFlags::WRITE) {
        bits |= ENTRY_WRITABLE;
    }
    if flags.contains(PageFlags::USER) {
        bits |= ENTRY_USER;
    }
    if flags.contains(PageFlags::DEVICE) {
        // MMIO без `PCD` — это запись, которая может осесть в кеше и не дойти
        // до регистра устройства (а чтение — вернуть давно устаревшее
        // значение). `PWT` добавлен на случай, если строка всё же окажется
        // кешируемой из-за MTRR: тогда она хотя бы будет сквозной.
        bits |= ENTRY_CACHE_DISABLE | ENTRY_WRITE_THROUGH;
    }
    // `PageFlags::DMA` здесь намеренно ни во что не превращается, и это не
    // упущение. Когерентность DMA на x86-64 обеспечивает сама шина: устройство,
    // читающее память, получает данные из кеша процессора (snooping), а запись
    // устройства инвалидирует строку. Выставить `PCD` было бы не осторожностью,
    // а замедлением каждого обращения к кольцу дескрипторов без всякой пользы.
    // На AArch64 тот же флаг делает настоящую работу — там обещания нет.
    if !flags.contains(PageFlags::EXEC) && NX_ENABLED.load(Ordering::Relaxed) {
        bits |= ENTRY_NO_EXECUTE;
    }
    bits
}

// --- Работа с регистрами ------------------------------------------------------

fn read_cr0() -> u64 {
    let value: u64;
    // SAFETY: чтение CR0 в ring 0 всегда разрешено и не имеет побочных
    // эффектов. `preserves_flags` не заявляем: SDM объявляет флаги после
    // `mov ... , cr0` неопределёнными.
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack)) };
    value
}

/// # Safety
///
/// CR0 управляет режимом работы процессора: сброс `PG`, `PE` или `WP` меняет
/// смысл уже исполняющегося кода. Допустимо менять только те биты, последствия
/// которых вызывающий продумал.
unsafe fn write_cr0(value: u64) {
    // SAFETY: см. контракт функции.
    unsafe { asm!("mov cr0, {}", in(reg) value, options(nostack)) };
}

/// # Safety
///
/// `root` обязан быть физическим адресом валидной PML4, отображающей как
/// минимум текущий код, стек и данные. Иначе следующая же выборка инструкции
/// уйдёт по неотображённому адресу и превратится в тройную ошибку.
unsafe fn write_cr3(root: PhysAddr) {
    // SAFETY: см. контракт функции. `nomem` здесь был бы ложью: инструкция
    // меняет смысл вообще всех обращений к памяти.
    unsafe { asm!("mov cr3, {}", in(reg) root.as_u64(), options(nostack)) };
}

/// Физический адрес корневой таблицы, на которой процессор работает сейчас.
fn read_cr3() -> PhysAddr {
    let value: u64;
    // SAFETY: чтение CR3 в ring 0 разрешено и побочных эффектов не имеет.
    // `preserves_flags` не заявляем: SDM объявляет флаги после `mov ..., cr3`
    // неопределёнными.
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack)) };
    // Младшие биты CR3 — это PCD/PWT (или PCID при CR4.PCIDE), а не часть
    // адреса.
    PhysAddr::new(value & ENTRY_ADDR_MASK)
}

/// Взять в работу уже активное дерево таблиц, прочитав его корень из `CR3`.
///
/// Нужно тем частям ядра, которые доотображают что-то уже после запуска —
/// например окно MMIO локального APIC, которого нет в карте памяти прошивки.
/// Экземпляр, построенный [`build_kernel_address_space`], до них не доживает:
/// он локален для инициализации памяти, а хранить его глобально означало бы
/// заводить ещё один изменяемый синглтон ради двух записей в таблицу.
///
/// Возвращаемый [`PageTable`] сразу настроен на прямое отображение: функция по
/// контракту вызывается только когда собственные таблицы ядра уже активны, а
/// значит `PHYS_MAP_BASE` работает. Владения дерево не получает — `PageTable`
/// не реализует `Drop` и при уничтожении ничего не освобождает.
///
/// # Safety
///
/// * процессор должен исполняться на таблицах, построенных этим модулем
///   (у чужих таблиц прошивки нет прямого отображения по [`PHYS_MAP_BASE`],
///   и первое же обращение к записи ушло бы в никуда);
/// * пока полученный экземпляр жив, никто другой не должен править то же
///   дерево: два `&mut` на одни и те же таблицы дадут гонку записей.
pub unsafe fn active_address_space() -> PageTable {
    PageTable { root: read_cr3(), phys_offset: Cell::new(PHYS_MAP_BASE) }
}

/// Добавить отображение в **уже активное** адресное пространство ядра.
///
/// Нужно всему, что появляется после инициализации памяти: окнам регистров
/// устройств, буферам DMA, диапазонам конфигурационного пространства PCI. От
/// [`AddressSpace::map_range`] отличается только тем, что не требует держать
/// экземпляр [`PageTable`]: тот локален для запуска и до этих потребителей не
/// доживает.
///
/// # Safety
///
/// Те же требования, что у [`active_address_space`]: собственные таблицы ядра
/// должны быть активны, и никто другой не должен править их в это время.
/// Отображаемый диапазон не должен пересекаться с тем, по чему ядро сейчас
/// исполняется.
pub unsafe fn map_active(
    virt: VirtAddr,
    phys: PhysAddr,
    len: usize,
    flags: PageFlags,
) -> Result<(), MapError> {
    // SAFETY: условия делегированы вызывающему контрактом этой функции.
    let mut space = unsafe { active_address_space() };
    // SAFETY: см. выше; `map_range` сам проверяет выравнивание и W^X.
    let result =
        crate::mm::frame::with(|frames| unsafe { space.map_range(virt, phys, len, flags, frames) });
    match result {
        Some(result) => result,
        // Аллокатора кадров нет — значит взять кадр под промежуточную таблицу
        // неоткуда, что для вызывающего неотличимо от исчерпания пула.
        None => Err(MapError::OutOfFrames),
    }
}

/// Доотобразить одну страницу регистров устройства и вернуть её виртуальный
/// адрес в прямом отображении.
///
/// Нужно всем контроллерам, окон которых нет в карте памяти прошивки: и
/// локальный APIC, и I/O APIC «висят» вне шины, поэтому UEFI их не описывает, и
/// рассчитывать на случайное попадание в уже отображённый регион нельзя —
/// первое же обращение к незамапленному регистру дало бы #PF.
///
/// Адрес берётся из прямого отображения, а не identity: identity исчезнет вместе
/// с переездом ядра в верхнюю половину, а прямое отображение — нет.
///
/// # Safety
///
/// Те же требования, что у [`active_address_space`]: собственные таблицы ядра
/// должны быть активны, и никто другой не должен править их в это время.
/// Дополнительно `phys` обязан быть адресом регистров устройства — отображение
/// получает семантику Device-памяти, непригодную для обычной оперативной.
pub unsafe fn map_device_page(phys: PhysAddr) -> Result<usize, MapError> {
    if !phys.is_page_aligned() {
        return Err(MapError::Misaligned);
    }
    let virt = phys.to_direct_map();
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;

    // SAFETY: условия делегированы вызывающему; страница по этому виртуальному
    // адресу либо ещё не отображена, либо отображена на тот же самый физический
    // кадр — прямое отображение по построению взаимно однозначно, поэтому
    // запись не может увести из-под ног работающий код. `map` сам откажет с
    // `AlreadyMapped`, если это не так.
    unsafe { map_active(virt, phys, PAGE_SIZE, flags) }?;
    Ok(virt.as_usize())
}

/// Убрать из TLB трансляцию одной страницы.
///
/// # Safety
///
/// Безопасна при любых аргументах (для неотображённого адреса — просто ничего
/// не делает), но помечена `unsafe` как часть арх-специфичного слоя.
unsafe fn invlpg(virt: VirtAddr) {
    // SAFETY: `invlpg` не читает и не пишет память по этому адресу — только
    // выбрасывает кешированную трансляцию. Флаги не меняет.
    unsafe {
        asm!("invlpg [{}]", in(reg) virt.as_usize(), options(nostack, preserves_flags));
    }
}

/// Разрешить бит `NX`, если процессор его поддерживает.
///
/// Идемпотентна. Пока `EFER.NXE = 0`, бит 63 записи таблицы зарезервирован, и
/// установленный в ней `NX` приводит не к запрету исполнения, а к #PF с
/// признаком reserved-bit violation при **любом** обращении к странице —
/// включая чтение данных. Поэтому порядок обязателен: сначала `NXE`, потом
/// первая запись с `NX`, и только потом `CR3`.
fn enable_nx() {
    if NX_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    // Записывать `NXE` вслепую нельзя: на процессоре без поддержки NX бит в
    // EFER зарезервирован и `wrmsr` даст #GP — то есть тройную ошибку, ведь
    // обработчиков исключений ещё нет.
    let max_leaf = cpuid(CPUID_EXT_MAX, 0).eax;
    let supported =
        max_leaf >= CPUID_EXT_FEATURES && cpuid(CPUID_EXT_FEATURES, 0).edx & CPUID_EDX_NX != 0;
    if !supported {
        crate::kprintln!("WARNING: CPU reports no NX support; W^X will not be enforced");
        return;
    }

    // SAFETY: поддержка NX подтверждена через CPUID, поэтому бит 11 в EFER
    // допустим; остальные биты сохраняются как есть, режим трансляции не
    // меняется. Собственные таблицы ещё не активированы, а в таблицах прошивки
    // бит 63 нулевой, так что включение NXE не меняет смысла ни одной
    // действующей записи.
    unsafe { wrmsr(IA32_EFER, rdmsr(IA32_EFER) | EFER_NXE) };
    NX_ENABLED.store(true, Ordering::Relaxed);
}

// --- Построение адресного пространства ядра -----------------------------------

/// Почему не удалось построить адресное пространство ядра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// Отказало конкретное отображение.
    Map(MapError),
    /// Загрузчик не передал карту памяти. Строить по ней нечего, а без
    /// identity-отображения переключаться некуда: первая же инструкция после
    /// загрузки CR3 окажется по неотображённому адресу.
    NoMemoryMap,
}

impl From<MapError> for BuildError {
    fn from(error: MapError) -> Self {
        Self::Map(error)
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Map(error) => write!(f, "{error}"),
            Self::NoMemoryMap => f.write_str("bootloader passed no memory map"),
        }
    }
}

/// Зеркало [`boot_info::MemoryRegion`], у которого все поля скалярные.
///
/// Причина та же, что у `RawRegion` в `main.rs`: массив регионов приезжает
/// из-за границы доверия, а `MemoryRegion::kind` — `enum`, и восстановление
/// значения с недопустимым дискриминантом было бы UB ещё до всякой проверки.
/// Совпадение раскладки проверяется статически.
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
const KIND_RESERVED: u32 = MemoryKind::Reserved as u32;

/// Начиная с какого размера окно устройства не отображается целиком.
const LARGE_DEVICE_WINDOW: u64 = 64 * 1024 * 1024;
const KIND_ACPI_NVS: u32 = MemoryKind::AcpiNvs as u32;
const KIND_FRAMEBUFFER: u32 = MemoryKind::Framebuffer as u32;

/// Столько регионов ядро согласно обойти; всё сверх — признак повреждённого
/// хэндоффа, а не машины с очень подробной картой.
const MAX_REGIONS: u64 = 1024;

/// Сколько физической памяти помещается в окно прямого отображения:
/// от [`PHYS_MAP_BASE`] до начала кучи. Физический адрес выше этой границы
/// отобразить по `phys + PHYS_MAP_BASE` уже нельзя — он налез бы на кучу.
const PHYS_MAP_LIMIT: u64 = (HEAP_BASE - PHYS_MAP_BASE) as u64;

/// Округление вверх до границы страницы; `None` при переполнении.
///
/// Все длины и адреса здесь приходят из хэндоффа, то есть из-за границы
/// доверия. Обычное округление на повреждённом значении дало бы панику прямо
/// внутри построения таблиц — там, где ядро ещё не в состоянии ничего о себе
/// рассказать. Поэтому переполнение обрабатывается как «этот регион пропускаем».
fn page_align_up(addr: u64) -> Option<u64> {
    addr.checked_next_multiple_of(PAGE_SIZE as u64)
}

/// Собрать рабочее адресное пространство ядра.
///
/// Порядок шагов не случаен: сначала грубое отображение всей физической памяти,
/// затем уточнения поверх него (сегменты ядра, фреймбуфер), затем новые области
/// (куча, стек). Уточнение работает потому, что [`PageTable::map`] разрешает
/// переотображение того же кадра с другими правами.
pub fn build_kernel_address_space(
    info: &BootInfo,
    alloc: &mut impl FrameAllocator,
) -> Result<PageTable, BuildError> {
    let mut space = PageTable::new(alloc)?;
    map_physical_memory(&mut space, info, alloc)?;
    map_kernel_image(&mut space, info, alloc)?;
    map_framebuffer(&mut space, info, alloc)?;
    map_heap(&mut space, alloc)?;
    map_stack(&mut space, alloc)?;
    Ok(space)
}

/// Identity- и прямое отображение всей физической памяти из карты.
///
/// Identity нужно, чтобы ядро продолжило исполняться после загрузки CR3: оно
/// собрано PIE и слинковано под тот адрес, куда его положил загрузчик.
/// Прямое отображение по [`PHYS_MAP_BASE`] нужно, чтобы после переключения
/// оставался способ дотянуться до самих таблиц.
fn map_physical_memory(
    space: &mut PageTable,
    info: &BootInfo,
    alloc: &mut impl FrameAllocator,
) -> Result<(), BuildError> {
    let map = &info.memory_map;
    if map.ptr == 0 || map.len == 0 {
        return Err(BuildError::NoMemoryMap);
    }
    if map.ptr % align_of::<RawRegion>() as u64 != 0 {
        return Err(BuildError::NoMemoryMap);
    }

    let count = map.len.min(MAX_REGIONS);
    let base = map.ptr as *const RawRegion;
    let mut mapped_pages = 0usize;

    for index in 0..count as usize {
        // SAFETY: загрузчик заявил `len` записей по адресу `ptr`; индекс
        // ограничен `count <= len`, выравнивание проверено выше, память
        // BootloaderReclaimable ещё никем не переиспользована — ядро на этом
        // шаге не отдало наружу ни одного кадра из неё. `read` вместо ссылки на
        // срез — чтобы не строить `MemoryRegion` с непроверенным `kind`.
        let region = unsafe { ptr::read(base.add(index)) };

        let start = region.start & !(PAGE_SIZE as u64 - 1);
        let Some(end) = page_align_up(region.start.saturating_add(region.len)) else {
            continue;
        };
        if end <= start {
            continue;
        }
        if end > PHYS_MAP_LIMIT {
            crate::kprintln!(
                "WARNING: physical region {start:#018x}..{end:#018x} \
                 does not fit the direct map window; skipped"
            );
            continue;
        }

        // Огромные окна устройств отображать постранично разорительно и
        // бессмысленно. На типичной машине карта описывает десятки гигабайт
        // адресного пространства (PCIe ECAM и прочие MMIO-дыры) при считанных
        // сотнях мегабайт настоящей памяти: каждый гигабайт таких окон стоит
        // 262144 вызовов `map` и около 2 МиБ таблиц, а обращаться туда ядро всё
        // равно не станет. То, к чему обращение действительно нужно —
        // фреймбуфер, — отображается отдельно и явно.
        //
        // Порог намеренно щедрый: настоящая RAM отдельными регионами такого
        // размера в карте не описывается, а мелкие окна прошивки (ACPI, LAPIC,
        // HPET) под него не подпадают и остаются на месте.
        if region.kind != KIND_USABLE && end - start > LARGE_DEVICE_WINDOW {
            crate::kprintln!(
                "paging: skipping {} MiB device window at {start:#018x}",
                (end - start) / (1024 * 1024)
            );
            continue;
        }

        // Reserved и ACPI NVS — это в том числе MMIO прошивки, и кешировать их
        // нельзя. Обычная память идёт как RW без исполнения: код ядра получит
        // свои права отдельно, поверх этого отображения.
        let mut flags = PageFlags::READ | PageFlags::WRITE;
        if matches!(region.kind, KIND_RESERVED | KIND_ACPI_NVS | KIND_FRAMEBUFFER) {
            flags |= PageFlags::DEVICE;
        }

        let len = (end - start) as usize;
        let phys = PhysAddr::new(start);
        // Нулевую страницу в identity сознательно пропускаем: отобразив её, мы
        // бы сделали разыменование нулевого указателя тихо успешным вместо #PF.
        // В прямом отображении она есть — физический кадр 0 ничем не хуже
        // прочих, и обращаться к нему через direct map никто случайно не станет.
        let ident_start = start.max(PAGE_SIZE as u64);
        // SAFETY: отображение строится в ещё не активном дереве, поэтому
        // затронуть исполняющийся сейчас код оно не может. Identity ставит
        // виртуальный адрес равным физическому — ровно то, по чему ядро
        // работает прямо сейчас, так что после переключения ничего не
        // сдвинется.
        unsafe {
            if ident_start < end {
                space.map_range(
                    VirtAddr::new(ident_start as usize),
                    PhysAddr::new(ident_start),
                    (end - ident_start) as usize,
                    flags,
                    alloc,
                )?;
            }
            space.map_range(phys.to_direct_map(), phys, len, flags, alloc)?;
        }
        mapped_pages += len / PAGE_SIZE;
    }

    if mapped_pages == 0 {
        return Err(BuildError::NoMemoryMap);
    }
    crate::kprintln!(
        "paging: identity + direct map for {} MiB of physical memory",
        mapped_pages * PAGE_SIZE / (1024 * 1024)
    );
    Ok(())
}

/// W^X для образа ядра: каждый сегмент — со своими правами.
fn map_kernel_image(
    space: &mut PageTable,
    info: &BootInfo,
    alloc: &mut impl FrameAllocator,
) -> Result<(), BuildError> {
    let image = &info.kernel;
    // SAFETY: массив сегментов лежит там же, где и остальной хэндофф, — в
    // BootloaderReclaimable-памяти, которая ещё не переиспользована и всё ещё
    // identity-отображена прошивкой. Все поля `KernelSegment` скалярные,
    // поэтому построение среза не может дать невалидное значение.
    let segments = unsafe { image.segments() };

    if segments.is_empty() {
        if image.base == 0 || image.size == 0 {
            crate::kprintln!("WARNING: bootloader described no kernel image; W^X not applied");
            return Ok(());
        }
        // Падать не за что: ядро уже отображено identity как RW, и без прав на
        // исполнение оно бы просто не работало. Даём RW-X одним куском и честно
        // сообщаем, что защиты нет.
        crate::kprintln!(
            "WARNING: bootloader passed no kernel segments; mapping image RW- without W^X"
        );
        let base = image.base & !(PAGE_SIZE as u64 - 1);
        let Some(end) = page_align_up(image.base.saturating_add(image.size)) else {
            return Ok(());
        };
        // SAFETY: дерево ещё не активно; identity сохраняет текущие адреса.
        unsafe {
            space.map_range(
                VirtAddr::new(base as usize),
                PhysAddr::new(base),
                (end - base) as usize,
                PageFlags::READ | PageFlags::WRITE,
                alloc,
            )?;
        }
        return Ok(());
    }

    // Самый младший адрес, ниже которого страницы уже размечены предыдущими
    // сегментами. Сегменты приходят упорядоченными по возрастанию, поэтому
    // пересечение проявится как `base < prev_end` — и это не мелочь: две разные
    // разметки одной страницы означают, что победит последняя, а значит W^X на
    // ней держится на случайности порядка, а не на решении.
    let mut prev_end = 0u64;

    for segment in segments {
        if segment.len == 0 {
            continue;
        }
        let mut flags = PageFlags::from_segment_flags(segment.flags) | PageFlags::READ;
        if flags.contains(PageFlags::WRITE) && flags.contains(PageFlags::EXEC) {
            // ELF такое допускает, `map` — нет. Снять EXEC безопаснее, чем снять
            // WRITE: неисполняемые данные приведут к честному #PF, а
            // неписуемые данные — к молчаливому нарушению логики.
            crate::kprintln!(
                "WARNING: kernel segment at {:#018x} is both writable and executable; \
                 dropping EXEC",
                segment.base
            );
            flags = PageFlags::READ | PageFlags::WRITE;
        }

        let base = segment.base & !(PAGE_SIZE as u64 - 1);
        let Some(end) = page_align_up(segment.base.saturating_add(segment.len)) else {
            continue;
        };
        if base < prev_end {
            crate::kprintln!(
                "WARNING: kernel segments share the page at {base:#018x}; \
                 W^X on it is decided by segment order"
            );
        }
        prev_end = prev_end.max(end);

        let len = (end - base) as usize;
        // SAFETY: дерево ещё не активно. Кадр тот же, что и в
        // identity-отображении из `map_physical_memory`, поэтому запись просто
        // уточняет права, а не переносит страницу.
        unsafe {
            space.map_range(VirtAddr::new(base as usize), PhysAddr::new(base), len, flags, alloc)?;
        }
        crate::kprintln!("paging: kernel segment {base:#018x}..{end:#018x} {flags:?}");
    }
    Ok(())
}

/// Фреймбуфер: без него ядро потеряет экран сразу после загрузки CR3.
fn map_framebuffer(
    space: &mut PageTable,
    info: &BootInfo,
    alloc: &mut impl FrameAllocator,
) -> Result<(), BuildError> {
    let fb = &info.framebuffer;
    if !fb.is_present() || fb.size == 0 {
        return Ok(());
    }
    let base = fb.base & !(PAGE_SIZE as u64 - 1);
    let Some(end) = page_align_up(fb.base.saturating_add(fb.size)) else {
        return Ok(());
    };
    // SAFETY: дерево ещё не активно; консоль обращается к фреймбуферу по
    // физическому адресу, который identity-отображение сохраняет.
    unsafe {
        space.map_range(
            VirtAddr::new(base as usize),
            PhysAddr::new(base),
            (end - base) as usize,
            PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE,
            alloc,
        )?;
    }
    Ok(())
}

/// Куча ядра: [`HEAP_SIZE`] байт свежих кадров по [`HEAP_BASE`].
fn map_heap(space: &mut PageTable, alloc: &mut impl FrameAllocator) -> Result<(), BuildError> {
    for page in 0..HEAP_SIZE / PAGE_SIZE {
        let frame = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        // SAFETY: дерево ещё не активно, а диапазон кучи в нём до сих пор пуст —
        // ничего работающего это отображение не задевает. Кадр только что выдан
        // аллокатором и никому больше не принадлежит.
        unsafe {
            space.map(
                VirtAddr::new(HEAP_BASE + page * PAGE_SIZE),
                frame,
                PageFlags::READ | PageFlags::WRITE,
                alloc,
            )?;
        }
    }
    Ok(())
}

/// Стек ядра: [`STACK_SIZE`] байт так, чтобы вершина легла на [`STACK_TOP`],
/// плюс неотображённая страница-ловушка снизу.
fn map_stack(space: &mut PageTable, alloc: &mut impl FrameAllocator) -> Result<(), BuildError> {
    for page in 0..STACK_SIZE / PAGE_SIZE {
        let frame = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        // SAFETY: те же условия, что и у кучи. Ядро в этот момент работает на
        // стеке загрузчика, поэтому новый стек ничего не пересекает.
        unsafe {
            space.map(
                VirtAddr::new(KERNEL_STACK_BOTTOM + page * PAGE_SIZE),
                frame,
                PageFlags::READ | PageFlags::WRITE,
                alloc,
            )?;
        }
    }
    // Страница по адресу KERNEL_STACK_GUARD сознательно не отображается —
    // отдельного действия для этого не требуется, но требуется, чтобы её никто
    // не отобразил позже.
    crate::kprintln!(
        "paging: kernel stack {:#018x}..{:#018x}, guard page at {:#018x}",
        KERNEL_STACK_BOTTOM,
        STACK_TOP,
        KERNEL_STACK_GUARD
    );
    Ok(())
}

// --- Переключение стека -------------------------------------------------------

/// Перейти на собственный стек ядра и передать управление `entry`.
///
/// # Зачем
///
/// До этого момента ядро работает на стеке, который оставил загрузчик, а память
/// под ним размечена как `BootloaderReclaimable` — то есть рано или поздно
/// будет роздана аллокатором кадров под что-нибудь ещё. Сохранять этот стек
/// незачем: переехать на свой нужно ровно один раз, как можно раньше.
///
/// # Почему возврата нет
///
/// Смена `RSP` мгновенно делает недействительным текущий кадр стека: локальные
/// переменные, сохранённые регистры и адрес возврата этой функции остались на
/// старом стеке, а `RSP` смотрит уже на новый. Выполнить `ret` значило бы снять
/// со стека что угодно, только не адрес возврата. Поэтому и сама функция, и
/// продолжение расходящиеся, а переход сделан `jmp` внутри `asm!` с
/// `options(noreturn)` — компилятор даже не станет генерировать эпилог.
///
/// # ABI
///
/// Продолжение обязано быть `extern "sysv64"`: в inline-ассемблере аргумент
/// кладётся в `RDI` вручную, а у обычного `fn` соглашение о вызове не
/// зафиксировано и Rust вправе передавать аргументы как угодно.
///
/// Выравнивание: System V требует, чтобы в точке `call` значение `RSP` было
/// кратно 16, то есть на входе в функцию — сравнимо с 8 по модулю 16 (адрес
/// возврата уже уложен). Мы не вызываем, а прыгаем, поэтому кладём на стек
/// фиктивный нулевой «адрес возврата» сами: это и восстанавливает нужное
/// выравнивание, и обрывает раскрутку стека (нулевые `RBP` и адрес возврата —
/// общепринятый признак самого нижнего кадра).
///
/// # Safety
///
/// * `stack_top` должен указывать на конец отображённой, доступной для записи
///   области размером не меньше того, что потребуется `entry` и всему, что она
///   вызовет, и быть выровнен на 16 байт;
/// * `entry` и `arg` должны оставаться валидными в том адресном пространстве,
///   которое активно на момент вызова, — в частности, `arg` не может указывать
///   на старый стек: он перестанет существовать сразу после `mov rsp`;
/// * всё, что ядро ещё хочет получить из старого стека, должно быть скопировано
///   заранее.
pub unsafe fn switch_stack<T>(
    stack_top: VirtAddr,
    entry: extern "sysv64" fn(*mut T) -> !,
    arg: *mut T,
) -> ! {
    assert!(
        stack_top.as_usize() % 16 == 0,
        "kernel stack top must be 16-byte aligned for the System V ABI"
    );

    // SAFETY: контракт функции требует от вызывающего отображённый и
    // выровненный `stack_top` и валидное в текущем адресном пространстве
    // продолжение. Блок не возвращает управление (`jmp` в расходящуюся
    // функцию), поэтому потеря старого кадра стека ни на что не влияет и
    // сохранять регистры не требуется — `options(noreturn)` сообщает об этом
    // компилятору.
    unsafe {
        asm!(
            "mov rsp, {top}",
            "xor ebp, ebp",   // конец цепочки кадров: раскрутка остановится здесь
            "push 0",         // фиктивный адрес возврата + выравнивание под ABI
            "jmp {entry}",
            top = in(reg) stack_top.as_usize(),
            entry = in(reg) entry as usize,
            in("rdi") arg,    // первый аргумент System V
            options(noreturn),
        )
    }
}

