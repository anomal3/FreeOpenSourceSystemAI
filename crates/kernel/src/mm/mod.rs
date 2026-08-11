//! Управление памятью: физические кадры, таблицы страниц, куча.
//!
//! # Раскладка виртуального адресного пространства
//!
//! Ядро на этом этапе **не переехало** в верхнюю половину и продолжает
//! исполняться по тем же адресам, по которым его разместил загрузчик. Причина
//! не в лени: образ собран как PIE, и релокации уже применены с физической
//! базой. Честный higher-half потребовал бы либо считать релокации от
//! виртуальной базы (и тогда таблицы обязан строить загрузчик, до прыжка),
//! либо переезжать самостоятельно, повторно обрабатывая `.rela.dyn` через
//! символы линкера. И то и другое — отдельная работа, а не побочный эффект
//! включения страничной трансляции.
//!
//! Что делается вместо этого — прямое отображение всей физической памяти в
//! верхнюю половину (в Linux это `PAGE_OFFSET`, direct map):
//!
//! ```text
//!   0x0000_0000_0000_0000 .. остаток нижней половины
//!       identity: код и данные ядра там, где их положил загрузчик,
//!       плюс MMIO и фреймбуфер. Здесь релокации остаются валидными.
//!
//!   0xFFFF_8000_0000_0000 .. PHYS_MAP_BASE
//!       вся физическая память, доступная как phys + PHYS_MAP_BASE.
//!       Нужна, чтобы править таблицы страниц после переключения на них:
//!       identity рано или поздно исчезнет, а таблицы адресуются физически.
//!
//!   0xFFFF_C000_0000_0000 .. HEAP_BASE
//!       куча ядра.
//!
//!   0xFFFF_E000_0000_0000 .. STACK_TOP
//!       стек ядра, растущий вниз, со страницей-ловушкой снизу.
//! ```
//!
//! Все константы канонические для 48-битной трансляции: биты 63:48 повторяют
//! бит 47. На x86-64 неканонический адрес даёт #GP при первом же обращении, на
//! AArch64 такие адреса уходят в TTBR1 — что здесь и требуется.
//!
//! TODO(Phase 3): перевести исполнение ядра в верхнюю половину и снять
//! identity-отображение целиком.

// У части контракта `mm` пока нет вызывающих, и это ожидаемо: освобождение
// кадров понадобится при завершении процессов, подряд идущие кадры — под
// DMA-буферы USB-стека, доступ к битам флагов — арх-коду новых платформ.
// Это спроектированный API, а не забытый код; удалить его сейчас означает
// написать заново через фазу. Глушим предупреждение здесь, чтобы шум не
// скрывал по-настоящему новые.
#![allow(dead_code)]

pub mod frame;
pub mod heap;

use core::fmt;
use core::ops::{BitOr, BitOrAssign};

/// Размер страницы. Ядро сознательно работает только со страницами 4 КиБ:
/// большие страницы усложняют защиту W^X, ради которой всё и затевается.
pub const PAGE_SIZE: usize = 4096;
pub const PAGE_SHIFT: usize = 12;

/// Начало прямого отображения физической памяти.
pub const PHYS_MAP_BASE: usize = 0xFFFF_8000_0000_0000;

/// Начало кучи ядра.
pub const HEAP_BASE: usize = 0xFFFF_C000_0000_0000;

/// Размер кучи. 16 МиБ с запасом хватает на всё, что ядро аллоцирует до
/// появления пользовательских процессов, и при этом требует лишь несколько
/// таблиц страниц.
pub const HEAP_SIZE: usize = 16 * 1024 * 1024;

/// Верхняя граница стека ядра. Стек растёт вниз от этого адреса.
pub const STACK_TOP: usize = 0xFFFF_E000_0000_0000;

/// Размер стека ядра.
pub const STACK_SIZE: usize = 64 * 1024;

/// Физический адрес. Отдельный тип от [`VirtAddr`] специально: перепутать их —
/// самая частая ошибка в коде подкачки, и пусть её ловит компилятор.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysAddr(pub u64);

/// Виртуальный адрес.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtAddr(pub usize);

impl PhysAddr {
    #[must_use]
    pub const fn new(addr: u64) -> Self {
        Self(addr)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn is_page_aligned(self) -> bool {
        self.0 % PAGE_SIZE as u64 == 0
    }

    /// Округление вниз до границы страницы.
    #[must_use]
    pub const fn page_align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE as u64 - 1))
    }

    /// Адрес в прямом отображении, по которому эта физическая страница доступна
    /// после активации таблиц ядра.
    #[must_use]
    pub const fn to_direct_map(self) -> VirtAddr {
        VirtAddr(PHYS_MAP_BASE + self.0 as usize)
    }
}

impl VirtAddr {
    #[must_use]
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0
    }

    #[must_use]
    pub const fn as_mut_ptr<T>(self) -> *mut T {
        self.0 as *mut T
    }

    #[must_use]
    pub const fn is_page_aligned(self) -> bool {
        self.0 % PAGE_SIZE == 0
    }

    #[must_use]
    pub const fn page_align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE - 1))
    }

    /// Индекс уровня таблицы страниц, от `level = 0` (младший) и выше.
    /// Одинаково устроено на обеих архитектурах: по 9 бит на уровень.
    #[must_use]
    pub const fn table_index(self, level: usize) -> usize {
        (self.0 >> (PAGE_SHIFT + level * 9)) & 0x1FF
    }
}

impl fmt::Debug for PhysAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "phys:{:#018x}", self.0)
    }
}

impl fmt::Debug for VirtAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "virt:{:#018x}", self.0)
    }
}

/// Права и свойства отображения — в терминах, не зависящих от архитектуры.
/// Конкретные биты записей таблиц собирает арх-специфичный код.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(u32);

impl PageFlags {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXEC: Self = Self(1 << 2);
    /// Доступно из пользовательского режима. Пока не используется.
    pub const USER: Self = Self(1 << 3);
    /// Память устройства: трансляция без кеширования и без спекулятивного
    /// доступа. Обязательна для MMIO — иначе запись в регистр может осесть в
    /// кеше и не дойти до железа. Для фреймбуфера годится и обычная память,
    /// но device-семантика гарантированно безопасна.
    pub const DEVICE: Self = Self(1 << 4);

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Права из флагов сегмента ELF, которые передал загрузчик.
    #[must_use]
    pub const fn from_segment_flags(flags: u32) -> Self {
        let mut result = Self::NONE;
        if flags & boot_info::SEG_READ != 0 {
            result = result.union(Self::READ);
        }
        if flags & boot_info::SEG_WRITE != 0 {
            result = result.union(Self::WRITE);
        }
        if flags & boot_info::SEG_EXEC != 0 {
            result = result.union(Self::EXEC);
        }
        result
    }
}

impl BitOr for PageFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for PageFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

impl fmt::Debug for PageFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        let mut bit = |name: &str, flag: PageFlags| -> fmt::Result {
            if self.contains(flag) {
                if !first {
                    f.write_str("|")?;
                }
                first = false;
                f.write_str(name)?;
            }
            Ok(())
        };
        bit("R", Self::READ)?;
        bit("W", Self::WRITE)?;
        bit("X", Self::EXEC)?;
        bit("U", Self::USER)?;
        bit("DEV", Self::DEVICE)?;
        if first {
            f.write_str("-")?;
        }
        Ok(())
    }
}

/// Почему не удалось построить отображение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// Кончились физические кадры под таблицы страниц.
    OutOfFrames,
    /// Адрес не выровнен на границу страницы.
    Misaligned,
    /// Отображение уже существует и указывает на другой кадр.
    AlreadyMapped,
    /// Запрошены одновременно запись и исполнение — прямое нарушение W^X.
    WriteExecute,
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::OutOfFrames => "out of physical frames",
            Self::Misaligned => "address is not page-aligned",
            Self::AlreadyMapped => "virtual address is already mapped elsewhere",
            Self::WriteExecute => "refusing a writable and executable mapping (W^X)",
        };
        f.write_str(text)
    }
}

/// Источник свободных физических кадров.
///
/// Кадры выдаются обнулёнными: таблицы страниц обязаны начинаться с нулей, иначе
/// в них окажется мусор, который процессор истолкует как валидные записи.
pub trait FrameAllocator {
    /// Выдать один обнулённый кадр.
    fn allocate(&mut self) -> Option<PhysAddr>;

    /// Вернуть кадр в пул.
    ///
    /// # Safety
    ///
    /// Кадр должен быть получен из [`FrameAllocator::allocate`] этого же
    /// аллокатора и больше нигде не использоваться — ни в одной таблице
    /// страниц не должно остаться ссылок на него.
    unsafe fn free(&mut self, frame: PhysAddr);

    /// Сколько кадров свободно и сколько всего.
    fn stats(&self) -> FrameStats;
}

/// Состояние пула физических кадров.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub total: usize,
    pub free: usize,
}

impl FrameStats {
    #[must_use]
    pub const fn used(&self) -> usize {
        self.total - self.free
    }

    #[must_use]
    pub const fn free_bytes(&self) -> usize {
        self.free * PAGE_SIZE
    }

    #[must_use]
    pub const fn total_bytes(&self) -> usize {
        self.total * PAGE_SIZE
    }
}

/// Адресное пространство: дерево таблиц страниц, которым управляет ядро.
///
/// Реализуется в арх-специфичном коде: на x86-64 это четыре уровня от PML4, на
/// AArch64 — трансляция VMSAv8-64 с раздельными TTBR0/TTBR1.
pub trait AddressSpace: Sized {
    /// Создать пустое адресное пространство с корневой таблицей.
    fn new(alloc: &mut impl FrameAllocator) -> Result<Self, MapError>;

    /// Отобразить одну страницу.
    ///
    /// Реализация обязана отклонять запрос с одновременными [`PageFlags::WRITE`]
    /// и [`PageFlags::EXEC`], возвращая [`MapError::WriteExecute`]: смысл всей
    /// этой машинерии в том, чтобы такая страница не появилась даже по ошибке.
    ///
    /// # Safety
    ///
    /// Вызывающий отвечает за то, что отображение не нарушит уже работающий код:
    /// переотображение страницы, по которой ядро сейчас исполняется или на
    /// которой лежит его стек, приведёт к немедленному отказу.
    unsafe fn map(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        flags: PageFlags,
        alloc: &mut impl FrameAllocator,
    ) -> Result<(), MapError>;

    /// Отобразить диапазон подряд идущих страниц.
    ///
    /// # Safety
    ///
    /// Те же условия, что у [`AddressSpace::map`].
    unsafe fn map_range(
        &mut self,
        virt: VirtAddr,
        phys: PhysAddr,
        len: usize,
        flags: PageFlags,
        alloc: &mut impl FrameAllocator,
    ) -> Result<(), MapError> {
        let pages = len.div_ceil(PAGE_SIZE);
        for i in 0..pages {
            let offset = i * PAGE_SIZE;
            // SAFETY: условия делегированы вызывающему через контракт метода.
            unsafe {
                self.map(
                    VirtAddr(virt.0 + offset),
                    PhysAddr(phys.0 + offset as u64),
                    flags,
                    alloc,
                )?;
            }
        }
        Ok(())
    }

    /// Физический адрес корневой таблицы — то, что уезжает в CR3 или TTBR.
    fn root(&self) -> PhysAddr;

    /// Переключить процессор на это адресное пространство.
    ///
    /// # Safety
    ///
    /// К моменту вызова адресное пространство обязано отображать всё, что нужно
    /// для продолжения исполнения: текущий код, стек и данные. Иначе следующая
    /// же выбранная инструкция окажется по неотображённому адресу.
    unsafe fn activate(&self);
}
