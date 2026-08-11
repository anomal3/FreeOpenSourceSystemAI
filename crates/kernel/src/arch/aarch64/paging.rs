//! Страничная трансляция VMSAv8-64 и переключение стека ядра.
//!
//! # Что здесь реализовано
//!
//! Трансляция с гранулой 4 КиБ и 48-битными адресами: четыре уровня таблиц
//! (ARM называет их L0…L3), только страницы 4 КиБ. Блочные отображения
//! (2 МиБ / 1 ГиБ) сознательно не используются — они бы заставили либо дробить
//! блок при первом же запросе с другими правами, либо огрублять W^X до границы
//! в 2 МиБ, а вся эта машинерия затевается именно ради W^X.
//!
//! # Две половины — два корневых дерева
//!
//! На x86-64 адресное пространство описывается одним корнем (CR3), и верхняя
//! половина — просто старшие записи PML4. На AArch64 это не так: адреса,
//! старшие биты которых нули, транслируются через `TTBR0_EL1`, а те, у которых
//! старшие биты единицы, — через `TTBR1_EL1`. Дерева, следовательно, два, и
//! [`PageTables`] хранит оба корня:
//!
//! * `TTBR0_EL1` — identity-отображение, по которому ядро продолжает
//!   исполняться после переключения (см. раскладку в [`crate::mm`]);
//! * `TTBR1_EL1` — `PHYS_MAP_BASE`, `HEAP_BASE`, `STACK_TOP`.
//!
//! Выбор дерева делается по битам 63:48 виртуального адреса, а индексы уровней
//! берутся из битов 47:12 одинаково для обеих половин.
//!
//! # Нумерация уровней
//!
//! [`VirtAddr::table_index`] нумерует уровни снизу вверх: `level = 0` — таблица
//! листьев. ARM нумерует сверху вниз: L0 — корень. То есть `level = 3` здесь —
//! это L0 в терминах ARM ARM, а `level = 0` — L3. Ниже используется нумерация
//! `mm`, а имена ARM упоминаются только в комментариях.

// TODO(интеграция): снять, как только `arch::mod` реэкспортирует эти имена и
// `main.rs` начнёт их вызывать. Сейчас модуль недостижим по публичным путям,
// и без этого весь его API числится мёртвым кодом.
#![allow(dead_code)]

use crate::mm::{
    AddressSpace, FrameAllocator, HEAP_BASE, HEAP_SIZE, MapError, PAGE_SIZE, PHYS_MAP_BASE,
    PageFlags, PhysAddr, STACK_SIZE, STACK_TOP, VirtAddr,
};
use boot_info::{BootInfo, MemoryKind, MemoryMap};
use core::arch::asm;
use core::mem::{align_of, size_of};
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Формат дескриптора трансляции (ARM ARM, VMSAv8-64, D8.3)
// ---------------------------------------------------------------------------

/// Бит 0: запись валидна. Ноль — Translation fault при первом обращении.
const DESC_VALID: u64 = 1 << 0;

/// Бит 1. На уровнях L0…L2 отличает таблицу (1) от блока (0), на L3 —
/// страницу (1) от зарезервированного значения (0). Мы не создаём блоков,
/// поэтому в наших записях он всегда единица — но на промежуточных уровнях
/// этот же бит нужно ещё и *проверять*, иначе блочная запись будет разобрана
/// как указатель на таблицу.
const DESC_TABLE: u64 = 1 << 1;
/// То же самое значение в роли «это страница уровня L3».
const DESC_PAGE: u64 = 1 << 1;

/// Биты 47:12 — физический адрес следующей таблицы либо самой страницы.
const DESC_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

/// Биты 4:2 — `AttrIndx`, номер байта в `MAIR_EL1`. Тип памяти на AArch64 не
/// кодируется в самом дескрипторе (как PCD/PWT на x86), а выбирается косвенно
/// из восьми предустановленных вариантов.
const DESC_ATTR_INDX_SHIFT: u64 = 2;

/// Биты 7:6 — `AP[2:1]`, права доступа.
const DESC_AP_SHIFT: u64 = 6;

/// Биты 9:8 — `SH[1:0]`, shareability.
const DESC_SH_SHIFT: u64 = 8;

/// Бит 10 — Access Flag.
///
/// Аналога на x86-64 нет, и это самая частая ошибка при первом порте: там бит
/// «accessed» выставляет процессор, здесь — обязана выставить программа. Если
/// оставить AF нулём, первое же обращение к странице даст не «всё работает», а
/// Access Flag fault, и выглядеть это будет как полностью отсутствующее
/// отображение.
const DESC_AF: u64 = 1 << 10;

/// Бит 11 — non-global: запись привязана к текущему ASID. Отображения ядра
/// обязаны быть глобальными, чтобы переживать смену ASID.
const DESC_NG: u64 = 1 << 11;

/// Бит 53 — Privileged eXecute Never.
///
/// Логика инвертирована относительно x86-64: там бит NX *запрещает*
/// исполнение, но и разрешение выражается его отсутствием, а здесь запрещающих
/// бита два, и «страница исполняемая» означает «оба запрещающих бита сняты для
/// нужного уровня привилегий». Отсутствие [`PageFlags::EXEC`] обязано ставить
/// PXN — иначе данные ядра остаются исполняемыми, и W^X превращается в W.
const DESC_PXN: u64 = 1 << 53;

/// Бит 54 — Unprivileged eXecute Never (в однорежимных описаниях просто XN).
const DESC_UXN: u64 = 1 << 54;

// `AP[2:1]` (биты 7:6). Кодировка неинтуитивна: ноль — это не «нет доступа», а
// «чтение и запись из EL1». Таблица из ARM ARM D8.4.1:
//
//   AP[2:1] | EL0             | EL1
//   00      | нет доступа     | чтение+запись
//   01      | чтение+запись   | чтение+запись
//   10      | нет доступа     | только чтение
//   11      | только чтение   | только чтение
//
/// Память ядра, доступная на запись.
const AP_EL1_RW: u64 = 0b00;
/// Память ядра, доступная только на чтение.
const AP_EL1_RO: u64 = 0b10;
/// Разделяемая с пользователем страница, доступная на запись.
const AP_EL1_RW_EL0_RW: u64 = 0b01;
/// Разделяемая с пользователем страница, только чтение.
const AP_EL1_RO_EL0_RO: u64 = 0b11;

/// `SH[1:0]` = 00. Для Device-памяти поле игнорируется (она всегда трактуется
/// как outer shareable), поэтому туда пишем ноль.
const SH_NON_SHAREABLE: u64 = 0b00;

/// `SH[1:0]` = 11, inner shareable.
///
/// Обязательно для Normal-памяти: без этого когерентность кешей между ядрами
/// одного кластера архитектурой не гарантируется, и на многоядерной машине
/// таблицы страниц, записанные одним ядром, другое может не увидеть. Ошибка
/// при этом проявляется не сразу и не воспроизводится на одном ядре.
const SH_INNER_SHAREABLE: u64 = 0b11;

/// Сколько уровней в дереве при 4 КиБ и 48 битах.
const LEVEL_COUNT: usize = 4;
/// Уровень листьев в нумерации [`VirtAddr::table_index`] (ARM: L3).
const LEAF_LEVEL: usize = 0;
/// Корневой уровень в той же нумерации (ARM: L0).
const ROOT_LEVEL: usize = LEVEL_COUNT - 1;
/// Записей в одной таблице.
const ENTRIES_PER_TABLE: usize = PAGE_SIZE / size_of::<u64>();

// ---------------------------------------------------------------------------
// MAIR_EL1: словарь типов памяти
// ---------------------------------------------------------------------------

/// Normal memory, Inner/Outer Write-Back, non-transient, Read/Write-Allocate.
const MAIR_ATTR_NORMAL_WB: u64 = 0xFF;
/// Device-nGnRnE: без gathering, reordering и раннего подтверждения записи.
/// Самый строгий вариант; держим его в словаре для устройств, которым важен
/// точный момент записи.
const MAIR_ATTR_DEVICE_NGNRNE: u64 = 0x00;
/// Device-nGnRE: то же, но с ранним подтверждением. Штатный режим для MMIO —
/// именно он используется для PL011 и фреймбуфера.
const MAIR_ATTR_DEVICE_NGNRE: u64 = 0x04;

const ATTR_IDX_NORMAL: u64 = 0;
const ATTR_IDX_DEVICE_NGNRNE: u64 = 1;
const ATTR_IDX_DEVICE_NGNRE: u64 = 2;

/// Готовое значение `MAIR_EL1`: восемь байт, по байту на индекс атрибута.
const MAIR_EL1_VALUE: u64 = (MAIR_ATTR_NORMAL_WB << (ATTR_IDX_NORMAL * 8))
    | (MAIR_ATTR_DEVICE_NGNRNE << (ATTR_IDX_DEVICE_NGNRNE * 8))
    | (MAIR_ATTR_DEVICE_NGNRE << (ATTR_IDX_DEVICE_NGNRE * 8));

// ---------------------------------------------------------------------------
// TCR_EL1: геометрия трансляции
// ---------------------------------------------------------------------------

const TCR_T0SZ_SHIFT: u64 = 0;
const TCR_IRGN0_SHIFT: u64 = 8;
const TCR_ORGN0_SHIFT: u64 = 10;
const TCR_SH0_SHIFT: u64 = 12;
const TCR_TG0_SHIFT: u64 = 14;
const TCR_T1SZ_SHIFT: u64 = 16;
const TCR_IRGN1_SHIFT: u64 = 24;
const TCR_ORGN1_SHIFT: u64 = 26;
const TCR_SH1_SHIFT: u64 = 28;
const TCR_TG1_SHIFT: u64 = 30;
const TCR_IPS_SHIFT: u64 = 32;

/// `TG0` = 00 означает 4 КиБ.
const TCR_TG0_4KIB: u64 = 0b00;
/// `TG1` = **10** означает те же 4 КиБ.
///
/// Кодировки `TG0` и `TG1` разные, и это классическая ловушка: скопированное
/// из `TG0` нулевое значение в `TG1` — зарезервированная комбинация, после
/// которой верхняя половина транслируется мусором. Ради этой пары строк стоит
/// сверяться с таблицей в ARM ARM (D19.2.139), а не с памятью.
const TCR_TG1_4KIB: u64 = 0b10;

/// `IRGNx`/`ORGNx` = 01: обращения table walker'а к таблицам кешируемые,
/// Write-Back Read/Write-Allocate. Должно соответствовать тому, как та же
/// память отображена в самих таблицах, иначе запись ядра и чтение walker'а
/// пойдут через разные представления одной строки кеша.
const TCR_RGN_WB_WA: u64 = 0b01;
/// `SHx` = 11: обращения walker'а inner shareable, парно к [`SH_INNER_SHAREABLE`].
const TCR_SH_INNER: u64 = 0b11;
/// `T0SZ`/`T1SZ` = 64 - 48: обе половины по 48 бит.
const TCR_TXSZ_48BIT: u64 = 64 - 48;

/// `IPS` = 101: физические адреса до 48 бит. Больше требует FEAT_LPA, который
/// с гранулой 4 КиБ и обычными дескрипторами всё равно не задействовать.
const TCR_IPS_48BIT: u64 = 0b101;

const ID_AA64MMFR0_PARANGE_MASK: u64 = 0xF;
const ID_AA64MMFR0_TGRAN4_SHIFT: u64 = 28;
const ID_AA64MMFR0_TGRAN4_MASK: u64 = 0xF;
/// `TGran4` = 1111 — гранула 4 КиБ реализацией не поддерживается.
const ID_AA64MMFR0_TGRAN4_NONE: u64 = 0b1111;

// SCTLR_EL1
const SCTLR_M: u64 = 1 << 0; // MMU включён
const SCTLR_C: u64 = 1 << 2; // кеш данных включён
const SCTLR_I: u64 = 1 << 12; // кеш инструкций включён

// ---------------------------------------------------------------------------
// Примитивы синхронизации трансляции
// ---------------------------------------------------------------------------

/// Дождаться, пока все ранее выполненные *записи* станут видны разделяемому
/// домену — в частности, table walker'у.
///
/// Без этого барьера процессор вправе начать ходить по таблицам раньше, чем
/// записи в них покинут буфер записи, и увидеть там прежнее содержимое. Отказ
/// при этом выглядит как «отображение не подействовало», хотя в памяти оно уже
/// есть.
#[inline]
fn dsb_ishst() {
    // SAFETY: барьер не читает и не пишет память и не трогает стек, он лишь
    // упорядочивает уже выполненные обращения.
    unsafe { asm!("dsb ishst", options(nostack, preserves_flags)) };
}

/// Полный барьер по разделяемому домену: дождаться и записей, и чтений (в том
/// числе завершения `tlbi`).
#[inline]
fn dsb_ish() {
    // SAFETY: см. [`dsb_ishst`].
    unsafe { asm!("dsb ish", options(nostack, preserves_flags)) };
}

/// Контекстная синхронизация: сбросить конвейер, чтобы уже выбранные
/// инструкции не выполнялись по старым системным регистрам и старым
/// трансляциям.
#[inline]
fn isb() {
    // SAFETY: инструкция не имеет операндов и не обращается к памяти.
    unsafe { asm!("isb", options(nostack, preserves_flags)) };
}

/// Выбросить из TLB трансляцию одной страницы во всех ASID.
///
/// `vaae1is`: VA, All ASID, EL1, Inner Shareable. Аргумент — не сам адрес, а
/// его старшие биты (`VA[55:12]`).
///
/// # Safety
///
/// Вызывающий отвечает за то, что новая запись для этого адреса уже находится
/// в памяти и видна walker'у (то есть [`dsb_ishst`] уже выполнен).
#[inline]
unsafe fn invalidate_page(virt: VirtAddr) {
    let operand = (virt.as_usize() >> 12) as u64;
    // SAFETY: `tlbi` меняет только состояние кеша трансляций; корректность
    // момента вызова — на вызывающем, см. контракт функции.
    unsafe { asm!("tlbi vaae1is, {}", in(reg) operand, options(nostack, preserves_flags)) };
    dsb_ish();
    isb();
}

/// Прочитать `ID_AA64MMFR0_EL1` — какие возможности трансляции есть у железа.
fn id_aa64mmfr0() -> u64 {
    let value: u64;
    // SAFETY: регистр только для чтения, побочных эффектов нет.
    unsafe {
        asm!("mrs {}, id_aa64mmfr0_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

/// Реально поддерживаемый диапазон физических адресов в кодировке `TCR.IPS`.
///
/// Ставить в `IPS` значение больше того, что заявляет `PARange`, нельзя:
/// поведение не определено. Поэтому берём минимум из заявленного железом и
/// того, что умеем сами.
fn supported_ips() -> u64 {
    (id_aa64mmfr0() & ID_AA64MMFR0_PARANGE_MASK).min(TCR_IPS_48BIT)
}

/// Поддерживает ли реализация гранулу 4 КиБ на первом уровне трансляции.
fn granule_4kib_supported() -> bool {
    let field = (id_aa64mmfr0() >> ID_AA64MMFR0_TGRAN4_SHIFT) & ID_AA64MMFR0_TGRAN4_MASK;
    field != ID_AA64MMFR0_TGRAN4_NONE
}

/// Текущий уровень исключений (0…3).
fn current_el() -> u64 {
    let value: u64;
    // SAFETY: чтение `CurrentEL` не имеет побочных эффектов.
    unsafe {
        asm!("mrs {}, currentel", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    (value >> 2) & 0b11
}

/// Собрать значение `TCR_EL1` для 48-битной трансляции обеих половин.
fn tcr_el1_value(ips: u64) -> u64 {
    // EPD0 (бит 7) и EPD1 (бит 23) остаются нулями — именно это и включает обе
    // половины: единица в EPD запретила бы обход таблиц соответствующего TTBR
    // и превратила бы любое обращение в Translation fault.
    (TCR_TXSZ_48BIT << TCR_T0SZ_SHIFT)
        | (TCR_RGN_WB_WA << TCR_IRGN0_SHIFT)
        | (TCR_RGN_WB_WA << TCR_ORGN0_SHIFT)
        | (TCR_SH_INNER << TCR_SH0_SHIFT)
        | (TCR_TG0_4KIB << TCR_TG0_SHIFT)
        | (TCR_TXSZ_48BIT << TCR_T1SZ_SHIFT)
        | (TCR_RGN_WB_WA << TCR_IRGN1_SHIFT)
        | (TCR_RGN_WB_WA << TCR_ORGN1_SHIFT)
        | (TCR_SH_INNER << TCR_SH1_SHIFT)
        | (TCR_TG1_4KIB << TCR_TG1_SHIFT)
        | (ips << TCR_IPS_SHIFT)
}

/// Собрать дескриптор страницы уровня L3.
fn leaf_descriptor(phys: PhysAddr, flags: PageFlags) -> u64 {
    let device = flags.contains(PageFlags::DEVICE);
    let user = flags.contains(PageFlags::USER);
    let write = flags.contains(PageFlags::WRITE);
    // Device-память архитектурно execute-never, поэтому запрашивать для неё
    // исполнение бессмысленно: снятый PXN всё равно не сделает её исполняемой,
    // а в дескрипторе создаст видимость разрешения.
    let exec = flags.contains(PageFlags::EXEC) && !device;

    let attr_index = if device { ATTR_IDX_DEVICE_NGNRE } else { ATTR_IDX_NORMAL };
    let shareability = if device { SH_NON_SHAREABLE } else { SH_INNER_SHAREABLE };
    // Отдельного «запрета чтения» на AArch64 нет: любая валидная запись
    // читаема, и `PageFlags::READ` влияет только на выбор AP через отсутствие
    // записи.
    let ap = match (user, write) {
        (false, true) => AP_EL1_RW,
        (false, false) => AP_EL1_RO,
        (true, true) => AP_EL1_RW_EL0_RW,
        (true, false) => AP_EL1_RO_EL0_RO,
    };

    let mut desc = (phys.as_u64() & DESC_ADDR_MASK)
        | DESC_VALID
        | DESC_PAGE
        | DESC_AF
        | (attr_index << DESC_ATTR_INDX_SHIFT)
        | (ap << DESC_AP_SHIFT)
        | (shareability << DESC_SH_SHIFT);

    if user {
        desc |= DESC_NG;
    }

    if exec {
        // Исполнение разрешается снятием запрета ровно на одном уровне
        // привилегий: код ядра не должен быть исполняем из EL0, код
        // пользователя — из EL1.
        desc |= if user { DESC_PXN } else { DESC_UXN };
    } else {
        desc |= DESC_PXN | DESC_UXN;
    }

    desc
}

// ---------------------------------------------------------------------------
// Адресное пространство
// ---------------------------------------------------------------------------

/// Дерево таблиц страниц AArch64: два корня, нижний и верхний.
pub struct PageTables {
    /// Корень `TTBR0_EL1` — нижняя половина (identity).
    low: PhysAddr,
    /// Корень `TTBR1_EL1` — верхняя половина (direct map, куча, стек).
    high: PhysAddr,
    /// Смещение, которое надо прибавить к физическому адресу таблицы, чтобы
    /// получить адрес, по которому её можно прочитать и записать.
    ///
    /// Таблицы адресуются процессором физически, а ядро исполняется по
    /// виртуальным адресам, поэтому «как добраться до таблицы» зависит от
    /// того, какая трансляция сейчас действует:
    ///
    /// * до [`AddressSpace::activate`] работает отображение от прошивки, а
    ///   UEFI на AArch64 оставляет identity — смещение нулевое;
    /// * после активации identity-отображение прошивки исчезает, и таблицы
    ///   доступны через прямое отображение, то есть по `PHYS_MAP_BASE + phys`
    ///   (ровно то, что даёт [`PhysAddr::to_direct_map`]).
    ///
    /// Аллокатор кадров устроен так же и по той же причине.
    ///
    /// Атомик, а не обычное поле, потому что `activate` в контракте трейта
    /// принимает `&self`.
    access_offset: AtomicUsize,
}

impl PageTables {
    /// Указатель, по которому можно прочитать/записать запись `index` таблицы,
    /// физически расположенной по `table`.
    fn entry_ptr(&self, table: PhysAddr, index: usize) -> *mut u64 {
        debug_assert!(index < ENTRIES_PER_TABLE);
        let base = table.as_u64() as usize + self.access_offset.load(Ordering::Acquire);
        (base + index * size_of::<u64>()) as *mut u64
    }

    /// Физический адрес корня, через который транслируется `virt`.
    fn root_for(&self, virt: VirtAddr) -> Result<PhysAddr, MapError> {
        match virt.as_usize() >> 48 {
            0x0000 => Ok(self.low),
            0xFFFF => Ok(self.high),
            // Адрес неканонический: биты 63:48 не повторяют бит 47. В `MapError`
            // отдельного варианта нет, а `Misaligned` — ближайший по смыслу
            // («адрес непригоден для трансляции»).
            _ => Err(MapError::Misaligned),
        }
    }

    /// Спуститься от корня до записи уровня L3, создавая недостающие таблицы.
    ///
    /// # Safety
    ///
    /// Дерево должно принадлежать этому объекту, а `access_offset` —
    /// соответствовать действующей трансляции.
    unsafe fn walk_to_leaf(
        &self,
        virt: VirtAddr,
        alloc: &mut impl FrameAllocator,
    ) -> Result<*mut u64, MapError> {
        let mut table = self.root_for(virt)?;

        let mut level = ROOT_LEVEL;
        while level > LEAF_LEVEL {
            let entry_ptr = self.entry_ptr(table, virt.table_index(level));
            // SAFETY: `entry_ptr` указывает внутрь таблицы, выделенной как
            // целый кадр и доступной по действующей трансляции; чтение
            // выровненного `u64` в её пределах корректно. `volatile` — потому
            // что то же место читает и правит table walker.
            let entry = unsafe { ptr::read_volatile(entry_ptr) };

            table = if entry & DESC_VALID == 0 {
                let frame = alloc.allocate().ok_or(MapError::OutOfFrames)?;
                let desc = (frame.as_u64() & DESC_ADDR_MASK) | DESC_VALID | DESC_TABLE;
                // Кадр приходит от аллокатора обнулённым, но эти нули ещё могут
                // сидеть в буфере записи. Они обязаны стать видны walker'у
                // раньше ссылки на таблицу — иначе он пройдёт по свежей ссылке
                // в недописанную память и примет мусор за валидные записи.
                dsb_ishst();
                // SAFETY: тот же указатель, что и при чтении выше.
                unsafe { ptr::write_volatile(entry_ptr, desc) };
                // А теперь сама ссылка — до того, как по ней спустятся ниже.
                dsb_ishst();
                frame
            } else if entry & DESC_TABLE == 0 {
                // Блочное отображение на промежуточном уровне. Сами мы их не
                // создаём, но разбирать чужую запись как указатель на таблицу
                // нельзя — получился бы обход по адресу данных.
                return Err(MapError::AlreadyMapped);
            } else {
                PhysAddr::new(entry & DESC_ADDR_MASK)
            };

            level -= 1;
        }

        Ok(self.entry_ptr(table, virt.table_index(LEAF_LEVEL)))
    }

    /// Общая реализация отображения. `allow_write_exec` снимает запрет W^X и
    /// доступен только внутри модуля — см. [`PageTables::map_image_fallback`].
    ///
    /// # Safety
    ///
    /// Те же условия, что у [`AddressSpace::map`].
    unsafe fn map_inner(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        alloc: &mut impl FrameAllocator,
        allow_write_exec: bool,
    ) -> Result<(), MapError> {
        if !virt.is_page_aligned() || !phys.is_page_aligned() {
            return Err(MapError::Misaligned);
        }
        if !allow_write_exec
            && flags.contains(PageFlags::WRITE)
            && flags.contains(PageFlags::EXEC)
        {
            return Err(MapError::WriteExecute);
        }

        // SAFETY: дерево принадлежит `self`, смещение доступа хранится в нём же.
        let entry_ptr = unsafe { self.walk_to_leaf(virt, alloc)? };
        // SAFETY: `walk_to_leaf` вернул указатель внутрь таблицы L3.
        let existing = unsafe { ptr::read_volatile(entry_ptr) };

        let replacing = existing & DESC_VALID != 0;
        if replacing && existing & DESC_ADDR_MASK != phys.as_u64() {
            return Err(MapError::AlreadyMapped);
        }

        let desc = leaf_descriptor(phys, flags);
        // SAFETY: тот же указатель; запись валидного дескриптора страницы.
        unsafe { ptr::write_volatile(entry_ptr, desc) };
        dsb_ishst();

        if replacing {
            // Прежняя трансляция того же адреса могла осесть в TLB: смена
            // только прав (например, RW → R-X для сегмента кода) без сброса
            // оставила бы страницу записываемой до ближайшего вытеснения.
            // SAFETY: новая запись уже в памяти и видна walker'у (`dsb ishst`
            // выше).
            unsafe { invalidate_page(virt) };
        }

        Ok(())
    }

    /// Отобразить диапазон, минуя проверку W^X.
    ///
    /// Единственный законный потребитель — аварийный путь для образа ядра, у
    /// которого загрузчик не передал сегменты: без исполняемости ядро не
    /// переживёт активацию таблиц, а без записываемости — первое же обращение
    /// к своим данным.
    ///
    /// # Safety
    ///
    /// Те же условия, что у [`AddressSpace::map`].
    unsafe fn map_image_fallback(
        &mut self,
        base: PhysAddr,
        len: u64,
        alloc: &mut impl FrameAllocator,
    ) -> Result<(), MapError> {
        let pages = (len as usize).div_ceil(PAGE_SIZE);
        for index in 0..pages {
            let offset = index * PAGE_SIZE;
            // SAFETY: условия делегированы вызывающему.
            unsafe {
                self.map_inner(
                    VirtAddr::new(base.as_u64() as usize + offset),
                    PhysAddr::new(base.as_u64() + offset as u64),
                    KERNEL_IMAGE_FALLBACK,
                    alloc,
                    true,
                )?;
            }
        }
        Ok(())
    }

    /// Физический адрес корня нижней половины (`TTBR0_EL1`).
    #[must_use]
    pub fn ttbr0(&self) -> PhysAddr {
        self.low
    }

    /// Физический адрес корня верхней половины (`TTBR1_EL1`).
    #[must_use]
    pub fn ttbr1(&self) -> PhysAddr {
        self.high
    }

    /// Работают ли таблицы уже через прямое отображение.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.access_offset.load(Ordering::Acquire) != 0
    }
}

impl AddressSpace for PageTables {
    fn new(alloc: &mut impl FrameAllocator) -> Result<Self, MapError> {
        let low = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        let high = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        // Кадры приходят обнулёнными по контракту `FrameAllocator`, поэтому обе
        // корневые таблицы уже состоят из невалидных записей.
        Ok(Self { low, high, access_offset: AtomicUsize::new(0) })
    }

    unsafe fn map(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        alloc: &mut impl FrameAllocator,
    ) -> Result<(), MapError> {
        // SAFETY: условия контракта трейта делегированы вызывающему.
        unsafe { self.map_inner(virt, phys, flags, alloc, false) }
    }

    /// Корень нижней половины: именно он транслирует адреса, по которым ядро
    /// сейчас исполняется. Верхний корень доступен отдельно — см.
    /// [`PageTables::ttbr1`]; одним значением, как CR3 на x86-64, здесь не
    /// обойтись.
    fn root(&self) -> PhysAddr {
        self.low
    }

    unsafe fn activate(&self) {
        let tcr = tcr_el1_value(supported_ips());

        // Все четыре системных регистра пишутся подряд, и `isb` стоит только
        // после последнего. Это не экономия: запись в системный регистр
        // вступает в силу лишь после контекстной синхронизации, поэтому один
        // общий `isb` переводит трансляцию из старого состояния в новое
        // целиком, не задерживаясь в промежуточном (новый TCR со старыми
        // TTBR — это геометрия одного дерева, применённая к другому).
        //
        // SAFETY: обе половины уже описаны полностью — нижняя identity-отображает
        // код, данные и стек, по которым исполняется эта самая функция, а
        // верхняя содержит прямое отображение. Ответственность за это лежит на
        // вызывающем по контракту трейта.
        unsafe {
            asm!(
                // Записи в таблицы должны быть видны walker'у до того, как он
                // пойдёт по ним.
                "dsb ishst",
                "msr mair_el1, {mair}",
                "msr tcr_el1, {tcr}",
                "msr ttbr0_el1, {ttbr0}",
                "msr ttbr1_el1, {ttbr1}",
                "isb",
                // TLB хранит трансляции от прошивки; без сброса процессор
                // продолжит пользоваться ими, и новые таблицы вступят в силу
                // непредсказуемо поздно.
                "tlbi vmalle1",
                "dsb ish",
                "isb",
                mair = in(reg) MAIR_EL1_VALUE,
                tcr = in(reg) tcr,
                ttbr0 = in(reg) self.low.as_u64(),
                ttbr1 = in(reg) self.high.as_u64(),
                options(nostack, preserves_flags),
            );
        }

        // UEFI передаёт управление с уже включённым MMU, так что обычно это
        // no-op. Но на прошивке, которая MMU выключила, без этой части ядро
        // продолжило бы работать по физическим адресам, а верхняя половина
        // (куча, стек, прямое отображение) осталась бы недоступной.
        let mut sctlr: u64;
        // SAFETY: чтение `SCTLR_EL1` побочных эффектов не имеет.
        unsafe {
            asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
        }
        sctlr |= SCTLR_M | SCTLR_C | SCTLR_I;
        // SAFETY: выставляются только биты включения MMU и кешей, остальные
        // сохраняются как есть; отображение, необходимое для продолжения
        // исполнения, уже установлено выше.
        unsafe {
            asm!(
                "msr sctlr_el1, {}",
                "isb",
                in(reg) sctlr,
                options(nostack, preserves_flags),
            );
        }

        // С этого момента identity-отображения прошивки больше нет, и до
        // таблиц надо добираться через прямое отображение.
        self.access_offset.store(PHYS_MAP_BASE, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Построение адресного пространства ядра
// ---------------------------------------------------------------------------

/// Обычная память ядра: чтение и запись, исполнение запрещено.
const KERNEL_DATA: PageFlags = PageFlags::READ.union(PageFlags::WRITE);
/// MMIO и фреймбуфер.
const KERNEL_DEVICE: PageFlags =
    PageFlags::READ.union(PageFlags::WRITE).union(PageFlags::DEVICE);
/// Права образа ядра, когда загрузчик не сообщил сегменты, — то есть W^X не
/// применён.
const KERNEL_IMAGE_FALLBACK: PageFlags =
    PageFlags::READ.union(PageFlags::WRITE).union(PageFlags::EXEC);

/// Верхняя граница на число регионов, которые ядро согласно обойти, — та же
/// защита от абсурдного `len`, что и в диагностике `main.rs`.
const MAX_REGIONS: u64 = 1024;

/// Regions типа `Reserved` крупнее этого не отображаются.
///
/// Туда попадают окна MMIO (например, PCIe ECAM), которые бывают на десятки и
/// сотни гигабайт. Отображать их постранично значило бы потратить гигабайты на
/// сами таблицы ради памяти, к которой ядро на этой фазе не обращается.
const MMIO_SPAN_LIMIT: u64 = 1 << 30;

/// Физическая память выше этой границы не помещается в 48-битное
/// identity-отображение.
const MAX_IDENTITY_PHYS: u64 = 1 << 48;

/// Сколько физической памяти помещается в прямое отображение до `HEAP_BASE`.
const DIRECT_MAP_SPAN: u64 = (HEAP_BASE - PHYS_MAP_BASE) as u64;

/// Зеркало [`boot_info::MemoryRegion`] со всеми полями скалярного типа.
///
/// Нужно по той же причине, что и в `main.rs`: массив регионов приходит
/// из-за границы доверия, а у `MemoryRegion` поле `kind` — `enum`, и
/// восстановление его из мусора было бы UB ещё до всякой проверки.
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

const KIND_RESERVED: u32 = MemoryKind::Reserved as u32;
const KIND_FRAMEBUFFER: u32 = MemoryKind::Framebuffer as u32;

/// Собрать рабочее адресное пространство ядра.
///
/// Что попадает внутрь:
///
/// * identity-отображение всей физической памяти из карты (`TTBR0_EL1`) — по
///   нему ядро продолжит исполняться сразу после переключения;
/// * прямое отображение той же памяти по `PHYS_MAP_BASE` (`TTBR1_EL1`) — без
///   него после активации нечем будет править сами таблицы;
/// * образ ядра посегментно, с правами из ELF (W^X);
/// * фреймбуфер и UART как Device-память;
/// * куча и стек ядра со страницей-ловушкой.
///
/// Возвращённое пространство ещё не активировано: это отдельный шаг
/// [`AddressSpace::activate`].
pub fn build_kernel_address_space(
    info: &BootInfo,
    alloc: &mut impl FrameAllocator,
) -> Result<PageTables, MapError> {
    if !granule_4kib_supported() {
        crate::kprintln!(
            "WARNING: ID_AA64MMFR0_EL1 reports no 4 KiB granule support; mapping anyway"
        );
    }
    let el = current_el();
    if el != 1 {
        // Регистры EL1 доступны и с более высокого уровня, но исполнение при
        // этом транслируется через свой набор таблиц, и переключение TTBRx_EL1
        // просто ничего не изменит.
        crate::kprintln!("WARNING: running at EL{el}, kernel page tables assume EL1");
    }

    let mut space = PageTables::new(alloc)?;

    map_physical_memory(&mut space, &info.memory_map, alloc)?;
    map_kernel_image(&mut space, info, alloc)?;
    map_devices(&mut space, info, alloc)?;
    map_heap(&mut space, alloc)?;
    map_stack(&mut space, alloc)?;

    Ok(space)
}

/// Identity- и прямое отображение всей физической памяти из карты.
fn map_physical_memory(
    space: &mut PageTables,
    map: &MemoryMap,
    alloc: &mut impl FrameAllocator,
) -> Result<(), MapError> {
    if map.ptr == 0 || map.len == 0 {
        crate::kprintln!("WARNING: empty memory map, nothing to identity-map");
        return Ok(());
    }
    if map.ptr % align_of::<RawRegion>() as u64 != 0 {
        crate::kprintln!("WARNING: region array at {:#018x} is misaligned, skipping", map.ptr);
        return Ok(());
    }

    let count = map.len.min(MAX_REGIONS);
    let base = map.ptr as *const RawRegion;

    for index in 0..count {
        // SAFETY: загрузчик заявил `len` записей по адресу `ptr`, индекс
        // ограничен `count <= len`, выравнивание проверено выше. Память типа
        // BootloaderReclaimable ещё никем не переиспользована: аллокатор кадров
        // раздаёт только Usable. Читаем через `RawRegion`, чтобы не собирать
        // `enum` из непроверенных байтов.
        let region = unsafe { ptr::read(base.add(index as usize)) };

        if region.len == 0 {
            continue;
        }
        if region.kind == KIND_RESERVED && region.len > MMIO_SPAN_LIMIT {
            crate::kprintln!(
                "  skipping reserved MMIO window {:#014x}+{} MiB (too large to page-map)",
                region.start,
                region.len / (1024 * 1024)
            );
            continue;
        }
        let end = region.start.saturating_add(region.len);
        if end > MAX_IDENTITY_PHYS || end > DIRECT_MAP_SPAN {
            crate::kprintln!(
                "  skipping region {:#014x}+{:#x}: beyond the mappable physical range",
                region.start,
                region.len
            );
            continue;
        }

        // Reserved — это прошивка и MMIO. Normal-память процессору позволено
        // читать спекулятивно, а спекулятивное чтение регистра устройства
        // имеет побочные эффекты; Device-семантика такое чтение запрещает.
        let flags = if region.kind == KIND_RESERVED || region.kind == KIND_FRAMEBUFFER {
            KERNEL_DEVICE
        } else {
            KERNEL_DATA
        };

        let phys = PhysAddr::new(region.start);
        let len = region.len as usize;

        // SAFETY: обе цели ещё не отображены (пространство строится с нуля), а
        // текущая трансляция принадлежит прошивке и этими записями не
        // затрагивается — пространство не активировано.
        unsafe {
            space.map_range(VirtAddr::new(region.start as usize), phys, len, flags, alloc)?;
            space.map_range(phys.to_direct_map(), phys, len, flags, alloc)?;
        }
    }

    Ok(())
}

/// Образ ядра посегментно: код — `R-X`, данные — `RW-`.
fn map_kernel_image(
    space: &mut PageTables,
    info: &BootInfo,
    alloc: &mut impl FrameAllocator,
) -> Result<(), MapError> {
    // SAFETY: массив сегментов лежит в BootloaderReclaimable-памяти, которую
    // ядро ещё не переиспользовало, и состоит только из скалярных полей —
    // невалидных дискриминантов здесь взяться неоткуда.
    let segments = unsafe { info.kernel.segments() };

    if segments.is_empty() {
        crate::kprintln!(
            "WARNING: bootloader passed no kernel segments; W^X NOT applied to the image"
        );
        if info.kernel.base == 0 || info.kernel.size == 0 {
            crate::kprintln!("WARNING: kernel image bounds unknown either; relying on identity map");
            return Ok(());
        }
        // Аварийный путь сознательно даёт RWX. Права «RW без X» здесь не
        // вариант: сразу после активации таблиц процессор выбирает следующую
        // инструкцию по identity-адресу, и PXN на странице кода означает не
        // «нарушение W^X», а мгновенный отказ без единого сообщения.
        // SAFETY: диапазон совпадает с уже построенным identity-отображением
        // того же физического адреса, поэтому переписываются права, а не цель.
        let base = PhysAddr::new(info.kernel.base).page_align_down();
        // Хвост, срезанный выравниванием базы вниз, надо вернуть в длину —
        // иначе последняя страница образа останется без нужных прав.
        let len = info.kernel.base - base.as_u64() + info.kernel.size;
        unsafe { space.map_image_fallback(base, len, alloc)? };
        return Ok(());
    }

    for segment in segments {
        let flags = PageFlags::from_segment_flags(segment.flags);
        if segment.is_writable() && segment.is_executable() {
            // Отказ ниже иначе выглядел бы как «ядро не смогло построить
            // таблицы» без единого намёка на то, что виноват линкер образа.
            crate::kprintln!(
                "FATAL: kernel segment {:#014x}+{:#x} asks for write+execute",
                segment.base,
                segment.len
            );
        }
        let phys = PhysAddr::new(segment.base);
        // Ядро исполняется по адресам, по которым его разместил загрузчик, то
        // есть виртуальный адрес сегмента равен физическому.
        // SAFETY: перезаписываются права уже существующего identity-отображения
        // на тот же кадр; активной эта таблица ещё не является.
        unsafe {
            space.map_range(
                VirtAddr::new(segment.base as usize),
                phys,
                segment.len as usize,
                flags,
                alloc,
            )?;
        }
    }

    Ok(())
}

/// Фреймбуфер и UART.
fn map_devices(
    space: &mut PageTables,
    info: &BootInfo,
    alloc: &mut impl FrameAllocator,
) -> Result<(), MapError> {
    // UART отображается явно и до всего остального, что может пойти не так.
    // В карте памяти UEFI его может не быть вовсе, а без него ядро теряет
    // единственный канал диагностики ровно в момент переключения таблиц —
    // отказ выглядит как «ядро молча умерло на activate».
    let uart = PhysAddr::new(super::QEMU_VIRT_PL011 as u64);
    // SAFETY: пространство не активировано; страница регистров PL011 не
    // пересекается с кодом или стеком ядра.
    unsafe {
        space.map_range(
            VirtAddr::new(super::QEMU_VIRT_PL011),
            uart,
            PAGE_SIZE,
            KERNEL_DEVICE,
            alloc,
        )?;
        space.map_range(uart.to_direct_map(), uart, PAGE_SIZE, KERNEL_DEVICE, alloc)?;
    }

    if info.framebuffer.is_present() {
        let fb = PhysAddr::new(info.framebuffer.base).page_align_down();
        let len = (info.framebuffer.base - fb.as_u64() + info.framebuffer.size) as usize;
        // SAFETY: те же условия, что и для UART.
        unsafe {
            space.map_range(
                VirtAddr::new(fb.as_u64() as usize),
                fb,
                len,
                KERNEL_DEVICE,
                alloc,
            )?;
            space.map_range(fb.to_direct_map(), fb, len, KERNEL_DEVICE, alloc)?;
        }
    }

    Ok(())
}

/// Куча ядра: `HEAP_SIZE` байт по `HEAP_BASE`, обычная память на запись.
fn map_heap(space: &mut PageTables, alloc: &mut impl FrameAllocator) -> Result<(), MapError> {
    for index in 0..HEAP_SIZE / PAGE_SIZE {
        let frame = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        // SAFETY: `HEAP_BASE` лежит в верхней половине, где до сих пор не было
        // ни одного отображения, — ничего работающего перекрыть невозможно.
        unsafe {
            space.map(
                VirtAddr::new(HEAP_BASE + index * PAGE_SIZE),
                frame,
                KERNEL_DATA,
                alloc,
            )?;
        }
    }
    Ok(())
}

/// Стек ядра: `STACK_SIZE` байт так, чтобы вершина пришлась на `STACK_TOP`.
fn map_stack(space: &mut PageTables, alloc: &mut impl FrameAllocator) -> Result<(), MapError> {
    let bottom = STACK_TOP - STACK_SIZE;

    for index in 0..STACK_SIZE / PAGE_SIZE {
        let frame = alloc.allocate().ok_or(MapError::OutOfFrames)?;
        // SAFETY: верхняя половина в этом диапазоне пуста; ядро в момент вызова
        // работает на стеке загрузчика, который лежит в нижней половине.
        unsafe {
            space.map(VirtAddr::new(bottom + index * PAGE_SIZE), frame, KERNEL_DATA, alloc)?;
        }
    }

    // Страница по адресу `bottom - PAGE_SIZE` намеренно остаётся
    // неотображённой. Стек растёт вниз, и при переполнении следующая же запись
    // попадёт в эту дыру и даст Translation fault — то есть отказ в точке
    // ошибки. Без ловушки переполнение молча затирало бы то, что окажется под
    // стеком, и проявлялось бы позже и в другом месте.
    Ok(())
}

// ---------------------------------------------------------------------------
// Переключение стека
// ---------------------------------------------------------------------------

/// Продолжение, которому [`switch_stack`] передаёт управление.
///
/// `extern "C"` и один аргумент-слово — чтобы соглашение было зафиксировано
/// явно: по AAPCS64 первый целочисленный аргумент передаётся в `x0`, что и
/// делает переход тремя инструкциями.
pub type StackEntry = extern "C" fn(arg: usize) -> !;

/// Требование к выравниванию `SP` на AArch64.
///
/// Это не рекомендация ABI, а требование железа: при `SCTLR_EL1.SA` (взведён по
/// умолчанию) любое обращение к памяти относительно невыровненного `SP`
/// возбуждает SP alignment fault.
const STACK_ALIGN: usize = 16;

/// Перейти на собственный стек ядра и передать управление в `entry`.
///
/// Ядро стартует на стеке загрузчика, а память под ним размечена как
/// [`MemoryKind::BootloaderReclaimable`] — рано или поздно аллокатор кадров
/// раздаст её кому-нибудь ещё. Поэтому до первого же выделения стек надо
/// сменить.
///
/// Функция расходящаяся и не имеет права возвращаться: как только `SP` указал
/// на новый стек, текущий кадр (сохранённый `x29`/`x30`, локальные переменные,
/// адрес возврата) остаётся на старом, и любая попытка вернуться прочитает
/// произвольные данные. Отсюда и `-> !` у самой функции, и `-> !` у
/// продолжения, и `options(noreturn)` в ассемблерном блоке — три независимых
/// способа сказать компилятору одно и то же.
///
/// # Safety
///
/// * `stack_top` обязан быть верхней границей диапазона, отображённого на
///   запись, и под ним должно быть не меньше `STACK_SIZE` полезных байт;
/// * `entry` обязана быть валидной точкой входа в отображённом на исполнение
///   коде;
/// * после вызова прежний стек считается недействительным — всё, что нужно
///   сохранить, должно быть передано через `arg` или лежать в статической
///   памяти.
pub unsafe fn switch_stack(stack_top: VirtAddr, entry: StackEntry, arg: usize) -> ! {
    // Округление вниз, а не проверка: получить `SP` невыровненным по недосмотру
    // вызывающего хуже, чем потерять до 15 байт стека.
    let sp = stack_top.as_usize() & !(STACK_ALIGN - 1);
    let entry_addr = entry as usize;

    // SAFETY: контракт функции требует от вызывающего отображённый на запись
    // стек и исполняемую точку входа. `in("x0") arg` занимает регистр первого
    // аргумента AAPCS64 явно, поэтому распределитель регистров не сможет
    // отдать `x0` под `sp` или `entry` и затереть аргумент до перехода.
    // `options(noreturn)` подтверждает, что управление сюда не вернётся, —
    // именно это позволяет безнаказанно менять `SP`, `x29` и `x30`.
    unsafe {
        asm!(
            "mov sp, {sp}",
            // Обнуление цепочки кадров: новый стек начинается с чистого листа,
            // а нулевой `x30` превращает ошибочный возврат из `entry` в
            // немедленный отказ вместо ухода по случайному адресу.
            "mov x29, xzr",
            "mov x30, xzr",
            "br {entry}",
            sp = in(reg) sp,
            entry = in(reg) entry_addr,
            in("x0") arg,
            options(noreturn),
        );
    }
}

/// Верхняя граница стека, который построил [`build_kernel_address_space`].
#[must_use]
pub const fn kernel_stack_top() -> VirtAddr {
    VirtAddr::new(STACK_TOP)
}
