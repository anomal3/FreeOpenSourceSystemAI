// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! GMMU GP100+ (формат VER2, так же у Turing и Ampere): таблицы страниц,
//! которые пишет процессор.
//!
//! Сверено с Linux 6.12 `nvkm/subdev/mmu/vmmgp100.c`, `vmm.c`, `tu102.c`:
//!
//! ```text
//! уровень  биты VA   записей  размер записи  таблица
//! PD3      47..48    4        8              0x20 (выделяем страницу)
//! PD2      38..46    512      8              4 КиБ
//! PD1      29..37    512      8              4 КиБ
//! PD0      21..28    256      16 (двойная)   4 КиБ
//! PT       12..20    512      8              4 КиБ  (только страницы 4 КиБ)
//! ```
//!
//! * PDE (8 Б): `апертура << 1 | адрес >> 4`; апертура VRAM = 1, системная
//!   когерентная = 2 (+ бит 3 VOL), некогерентная = 3; ноль — нет таблицы.
//! * Двойная PDE у PD0: **`data[0]` (байты 0..7) — таблица больших страниц,
//!   `data[1]` (байты 8..15) — таблица страниц 4 КиБ** (`gp100_vmm_pd0_pde`:
//!   `pt[0]` -> `data[0]`, `pt[1]` = SPT -> `data[1]`). В спецификации 2.5
//!   было наоборот — ошибка. Разреженность на уровне PD0 — бит 3 (VOL_BIG) в
//!   `data[0]`.
//! * PTE (8 Б): `VALID | апертура << 1 | VOL << 3 | PRIV << 5 | RO << 6 |
//!   адрес >> 4 | kind << 56`; апертура VRAM = 0, SYS_COH = 2, NCOH = 3.
//!   Разреженная PTE — только бит 3 (VALID = 0), снятая — 0.
//!
//! Таблицы живут в видеопамяти, доступ к ним — через [`PtMem`] (у WP-A —
//! окно BAR1, у [`SoftGpu`](crate::softgpu::SoftGpu) — поддельная VRAM).
//! Таблицы, которые записал RM (диапазон «server reserved»), [`Gmmu`] не
//! освобождает: он помнит только свои.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::Errno;

pub const PTE_VALID: u64 = 1 << 0;
pub const PTE_VOL: u64 = 1 << 3;
pub const PTE_PRIV: u64 = 1 << 5;
pub const PTE_RO: u64 = 1 << 6;
pub const PTE_ATOMIC_DISABLE: u64 = 1 << 7;
/// Разреженная PTE: VOL без VALID.
pub const PTE_SPARSE: u64 = PTE_VOL;
/// Разреженная двойная PDE уровня PD0: VOL_BIG в `data[0]`.
pub const PD0_SPARSE: u64 = 1 << 3;
/// Поле адреса (биты 8..55) у PDE и PTE: туда ложится `phys >> 4`
/// выровненного на 4 КиБ адреса.
const ADDR_MASK: u64 = 0x00ff_ffff_ffff_ff00;

/// Ширина VA у GP100+ (PD3 берёт биты 47..48).
pub const VA_BITS: u32 = 49;
/// Предел VA, который NVK использует (47 бит, раздел 1.4).
pub const NVK_VA_LIMIT: u64 = 1 << 47;
/// Байт на страницу.
pub const PAGE: u64 = 0x1000;

/// Куда указывает запись.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Vram,
    /// Системная память, когерентная (снуп).
    SysCoh,
    /// Системная память, некогерентная.
    SysNcoh,
}

impl Target {
    /// Код апертуры в PTE.
    pub const fn pte_aperture(self) -> u64 {
        match self {
            Target::Vram => 0,
            Target::SysCoh => 2,
            Target::SysNcoh => 3,
        }
    }

    /// Код апертуры в PDE.
    pub const fn pde_aperture(self) -> u64 {
        match self {
            Target::Vram => 1,
            Target::SysCoh => 2,
            Target::SysNcoh => 3,
        }
    }

    /// Код апертуры в указателе на PD3 в блоке экземпляра
    /// (`gf100_vmm_join_`: VRAM 0, HOST 2 + VOL бит 2, NCOH 3).
    pub const fn pdb_aperture(self) -> u64 {
        match self {
            Target::Vram => 0,
            Target::SysCoh => 2 | (1 << 2),
            Target::SysNcoh => 3,
        }
    }
}

/// Атрибуты PTE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PteAttrs {
    pub target: Target,
    pub vol: bool,
    pub privileged: bool,
    pub ro: bool,
    pub kind: u8,
}

impl PteAttrs {
    /// Как ставит nouveau uvmm: VOL для системной памяти, kind как есть.
    pub const fn user(target: Target, kind: u8) -> PteAttrs {
        PteAttrs { target, vol: !matches!(target, Target::Vram), privileged: false, ro: false, kind }
    }
}

/// Закодировать PTE страницы 4 КиБ по адресу `phys`.
pub const fn pte(phys: u64, a: PteAttrs) -> u64 {
    PTE_VALID
        | (a.target.pte_aperture() << 1)
        | ((a.vol as u64) << 3)
        | ((a.privileged as u64) << 5)
        | ((a.ro as u64) << 6)
        | ((phys >> 4) & ADDR_MASK)
        | ((a.kind as u64) << 56)
}

/// Закодировать PDE (8 Б, уровни PD3/PD2/PD1 и половинки PD0).
pub const fn pde(table_phys: u64, target: Target) -> u64 {
    let vol = if matches!(target, Target::SysCoh) { 1 << 3 } else { 0 };
    (target.pde_aperture() << 1) | vol | ((table_phys >> 4) & ADDR_MASK)
}

/// Разобрать PDE: таблица и её апертура или `None`, если записи нет.
pub const fn decode_pde(v: u64) -> Option<(u64, Target)> {
    let t = match (v >> 1) & 3 {
        1 => Target::Vram,
        2 => Target::SysCoh,
        3 => Target::SysNcoh,
        _ => return None,
    };
    Some(((v & ADDR_MASK) << 4, t))
}

/// Что лежит в PTE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pte {
    Invalid,
    Sparse,
    Valid { phys: u64, attrs: PteAttrs },
}

pub const fn decode_pte(v: u64) -> Pte {
    if v & PTE_VALID == 0 {
        return if v & PTE_SPARSE != 0 { Pte::Sparse } else { Pte::Invalid };
    }
    let target = match (v >> 1) & 3 {
        0 => Target::Vram,
        2 => Target::SysCoh,
        3 => Target::SysNcoh,
        // 1 = PEER, у нас не бывает.
        _ => return Pte::Invalid,
    };
    Pte::Valid {
        phys: (v & ADDR_MASK) << 4,
        attrs: PteAttrs {
            target,
            vol: v & PTE_VOL != 0,
            privileged: v & PTE_PRIV != 0,
            ro: v & PTE_RO != 0,
            kind: (v >> 56) as u8,
        },
    }
}

/// Таблица kind Turing+ (`tu102_mmu_kind`): 16 значений, 0x07 —
/// недопустимый, 8..15 — сжатые варианты, отображаются на несжатые.
const KIND_MAP: [u8; 16] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x06, 0x06, 0x02, 0x01, 0x03, 0x04, 0x05, 0x07];
const KIND_INVALID: u8 = 0x07;

/// Проверить kind из VM_BIND и вернуть то, что писать в PTE.
/// `kind >= 16`, 0x07 и 0x0f (отображается на 0x07) — `EINVAL`; сжатые 8..14
/// заменяются несжатыми (сжатия у нас нет — раздел 2.5 спецификации;
/// nouveau на страницах 4 КиБ их бы отверг, мы мягче).
pub const fn pte_kind(kind: u8) -> Result<u8, Errno> {
    if kind as usize >= KIND_MAP.len() || KIND_MAP[kind as usize] == KIND_INVALID {
        return Err(Errno::EINVAL);
    }
    Ok(KIND_MAP[kind as usize])
}

/// Уровень дерева.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    /// Младший бит VA, который индексирует этот уровень.
    pub shift: u32,
    pub bits: u32,
    /// Байт на запись.
    pub entry: u64,
}

impl Level {
    pub const fn entries(&self) -> u64 {
        1 << self.bits
    }
    /// Сколько VA покрывает одна запись.
    pub const fn span(&self) -> u64 {
        1 << self.shift
    }
    pub const fn index(&self, va: u64) -> u64 {
        (va >> self.shift) & ((1 << self.bits) - 1)
    }
    /// Размер самой таблицы в байтах.
    pub const fn table_bytes(&self) -> u64 {
        self.entries() * self.entry
    }
}

pub const PD3: usize = 0;
pub const PD2: usize = 1;
pub const PD1: usize = 2;
pub const PD0: usize = 3;
pub const PT: usize = 4;

/// `gp100_vmm_desc_12`, сверху вниз.
pub const LEVELS: [Level; 5] = [
    Level { shift: 47, bits: 2, entry: 8 },
    Level { shift: 38, bits: 9, entry: 8 },
    Level { shift: 29, bits: 9, entry: 8 },
    Level { shift: 21, bits: 8, entry: 16 },
    Level { shift: 12, bits: 9, entry: 8 },
];

/// Доступ к памяти таблиц (видеопамять у настоящей карты).
pub trait PtMem {
    fn read64(&self, addr: u64) -> u64;
    fn write64(&mut self, addr: u64, v: u64);
    /// Новая обнулённая таблица 4 КиБ, выровненная на 4 КиБ.
    fn alloc_table(&mut self) -> Option<u64>;
    fn free_table(&mut self, addr: u64);
}

/// Результат обхода.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Walk {
    Unmapped,
    Sparse,
    Mapped { phys: u64, attrs: PteAttrs },
}

/// Дерево таблиц одного адресного пространства.
#[derive(Clone, Debug)]
pub struct Gmmu {
    pd3: u64,
    /// Где лежат таблицы (апертура в PDE).
    table_target: Target,
    /// Таблицы, которые выделили мы (RM-овские здесь не появляются).
    owned: BTreeSet<u64>,
}

impl Gmmu {
    /// Новое пустое дерево: одна таблица PD3.
    pub fn new(mem: &mut dyn PtMem) -> Result<Gmmu, Errno> {
        Gmmu::with_target(mem, Target::Vram)
    }

    pub fn with_target(mem: &mut dyn PtMem, table_target: Target) -> Result<Gmmu, Errno> {
        let pd3 = mem.alloc_table().ok_or(Errno::ENOMEM)?;
        let mut owned = BTreeSet::new();
        owned.insert(pd3);
        Ok(Gmmu { pd3, table_target, owned })
    }

    /// Физический адрес PD3 (для блока экземпляра и сброса TLB).
    pub fn pd3(&self) -> u64 {
        self.pd3
    }

    pub fn table_target(&self) -> Target {
        self.table_target
    }

    /// Сколько таблиц выделено нами.
    pub fn table_count(&self) -> usize {
        self.owned.len()
    }

    fn check_range(va: u64, len: u64) -> Result<(), Errno> {
        if va % PAGE != 0 || len % PAGE != 0 || len == 0 {
            return Err(Errno::EINVAL);
        }
        match va.checked_add(len) {
            Some(end) if end <= 1 << VA_BITS => Ok(()),
            _ => Err(Errno::EINVAL),
        }
    }

    /// Адрес записи уровня `lvl` для `va` в таблице `table`.
    fn entry_addr(table: u64, lvl: usize, va: u64) -> u64 {
        table + LEVELS[lvl].index(va) * LEVELS[lvl].entry
    }

    /// Адрес «указателя вниз» в записи уровня `lvl` (у PD0 — вторая половина).
    fn ptr_addr(table: u64, lvl: usize, va: u64) -> u64 {
        let e = Self::entry_addr(table, lvl, va);
        if lvl == PD0 { e + 8 } else { e }
    }

    fn new_table(&mut self, mem: &mut dyn PtMem) -> Result<u64, Errno> {
        let t = mem.alloc_table().ok_or(Errno::ENOMEM)?;
        self.owned.insert(t);
        Ok(t)
    }

    /// Пройти от PD3 к таблице уровня `want` (PD2..PT), создавая
    /// недостающие при `create`. `Ok(None)` — нет и не создавали.
    fn table_for(&mut self, mem: &mut dyn PtMem, va: u64, want: usize, create: bool) -> Result<Option<u64>, Errno> {
        let mut table = self.pd3;
        for lvl in PD3..want {
            let p = Self::ptr_addr(table, lvl, va);
            match decode_pde(mem.read64(p)) {
                Some((next, _)) => table = next,
                None if !create => return Ok(None),
                None => {
                    let next = self.new_table(mem)?;
                    if lvl == PD0 {
                        // Под разреженной PD0 новая PT начинается разреженной.
                        let e = Self::entry_addr(table, lvl, va);
                        let big = mem.read64(e);
                        if decode_pde(big).is_none() && big & PD0_SPARSE != 0 {
                            for i in 0..LEVELS[PT].entries() {
                                mem.write64(next + i * 8, PTE_SPARSE);
                            }
                            mem.write64(e, 0);
                        }
                    }
                    mem.write64(p, pde(next, self.table_target));
                    table = next;
                }
            }
        }
        Ok(Some(table))
    }

    /// Убедиться, что таблицы до уровня `depth` для `va` существуют, и вернуть
    /// их адреса сверху вниз (`[PD3, PD2, ...]`, `depth + 1` штук). Для
    /// `NV90F1_CTRL_CMD_VASPACE_COPY_SERVER_RESERVED_PDES` (раздел 2.2 п.5):
    /// `depth = PD1` даёт PD3, PD2 и PD1.
    pub fn ensure_path(&mut self, mem: &mut dyn PtMem, va: u64, depth: usize) -> Result<Vec<u64>, Errno> {
        let mut out = Vec::with_capacity(depth + 1);
        for lvl in PD3..=depth {
            out.push(self.table_for(mem, va, lvl, true)?.ok_or(Errno::ENOMEM)?);
        }
        Ok(out)
    }

    /// Отобразить `[va, va+len)` постранично: `phys_of(i)` — адрес i-й
    /// страницы.
    pub fn map(&mut self, mem: &mut dyn PtMem, va: u64, len: u64, attrs: PteAttrs, mut phys_of: impl FnMut(u64) -> u64) -> Result<(), Errno> {
        Self::check_range(va, len)?;
        let mut off = 0;
        while off < len {
            let cur = va + off;
            let pt = self.table_for(mem, cur, PT, true)?.ok_or(Errno::ENOMEM)?;
            // До конца этой PT или диапазона.
            let in_pt = (LEVELS[PD0].span() - (cur & (LEVELS[PD0].span() - 1))).min(len - off);
            let mut o = 0;
            while o < in_pt {
                let v = cur + o;
                mem.write64(Self::entry_addr(pt, PT, v), pte(phys_of((off + o) / PAGE), attrs));
                o += PAGE;
            }
            off += in_pt;
        }
        Ok(())
    }

    /// Непрерывный кусок памяти.
    pub fn map_contig(&mut self, mem: &mut dyn PtMem, va: u64, len: u64, phys: u64, attrs: PteAttrs) -> Result<(), Errno> {
        self.map(mem, va, len, attrs, |i| phys + i * PAGE)
    }

    /// Снять отображение (`sparse = false`, PTE = 0) или сделать диапазон
    /// разреженным (`sparse = true`). Целые 2 МиБ без таблицы PT
    /// помечаются в PD0, не создавая PT.
    pub fn fill(&mut self, mem: &mut dyn PtMem, va: u64, len: u64, sparse: bool) -> Result<(), Errno> {
        Self::check_range(va, len)?;
        let value = if sparse { PTE_SPARSE } else { 0 };
        let end = va + len;
        let mut cur = va;
        while cur < end {
            let span = LEVELS[PD0].span();
            let chunk_end = ((cur & !(span - 1)) + span).min(end);
            let whole = cur % span == 0 && chunk_end - cur == span;
            // Дойти до PD0; если выше нет таблиц и снимаем — пропустить
            // целиком то, что покрывает отсутствующая запись.
            let Some(pd0) = self.table_for(mem, cur, PD0, sparse)? else {
                cur = self.skip_missing(&*mem, cur).min(end);
                continue;
            };
            let e = Self::entry_addr(pd0, PD0, cur);
            let small = decode_pde(mem.read64(e + 8));
            let big = mem.read64(e);
            let big_is_table = decode_pde(big).is_some();
            match small {
                None if whole && !big_is_table => {
                    mem.write64(e, if sparse { PD0_SPARSE } else { 0 });
                }
                None if !sparse && !big_is_table && big & PD0_SPARSE == 0 => {
                    // Нечего снимать.
                }
                _ => {
                    let pt = self.table_for(mem, cur, PT, true)?.ok_or(Errno::ENOMEM)?;
                    let mut v = cur;
                    while v < chunk_end {
                        mem.write64(Self::entry_addr(pt, PT, v), value);
                        v += PAGE;
                    }
                }
            }
            cur = chunk_end;
        }
        Ok(())
    }

    /// Снять отображение.
    pub fn unmap(&mut self, mem: &mut dyn PtMem, va: u64, len: u64) -> Result<(), Errno> {
        self.fill(mem, va, len, false)
    }

    /// Сделать разреженным.
    pub fn sparse(&mut self, mem: &mut dyn PtMem, va: u64, len: u64) -> Result<(), Errno> {
        self.fill(mem, va, len, true)
    }

    /// Первый адрес после `va`, который может иметь таблицы (по первому
    /// отсутствующему уровню).
    fn skip_missing(&self, mem: &dyn PtMem, va: u64) -> u64 {
        let mut table = self.pd3;
        for lvl in PD3..PD0 {
            match decode_pde(mem.read64(Self::ptr_addr(table, lvl, va))) {
                Some((next, _)) => table = next,
                None => {
                    let span = LEVELS[lvl].span();
                    return (va & !(span - 1)) + span;
                }
            }
        }
        let span = LEVELS[PD0].span();
        (va & !(span - 1)) + span
    }

    /// Обход, как его делает GPU.
    pub fn translate(&self, mem: &dyn PtMem, va: u64) -> Walk {
        walk(mem, self.pd3, va)
    }

    /// Освободить все свои таблицы (RM-овские не трогаются).
    pub fn destroy(self, mem: &mut dyn PtMem) {
        for t in self.owned {
            mem.free_table(t);
        }
    }
}

/// Обход дерева от `pd3` — как это делает GPU, без [`Gmmu`] (SoftGpu идёт по
/// тому, что лежит в памяти, а не по нашим записям).
pub fn walk(mem: &dyn PtMem, pd3: u64, va: u64) -> Walk {
    if va >= 1 << VA_BITS {
        return Walk::Unmapped;
    }
    let mut table = pd3;
    for lvl in PD3..PT {
        let e = Gmmu::entry_addr(table, lvl, va);
        if lvl == PD0 {
            match decode_pde(mem.read64(e + 8)) {
                Some((next, _)) => table = next,
                None => {
                    let big = mem.read64(e);
                    return if decode_pde(big).is_none() && big & PD0_SPARSE != 0 { Walk::Sparse } else { Walk::Unmapped };
                }
            }
        } else {
            match decode_pde(mem.read64(e)) {
                Some((next, _)) => table = next,
                None => return Walk::Unmapped,
            }
        }
    }
    match decode_pte(mem.read64(Gmmu::entry_addr(table, PT, va))) {
        Pte::Invalid => Walk::Unmapped,
        Pte::Sparse => Walk::Sparse,
        Pte::Valid { phys, attrs } => Walk::Mapped { phys: phys | (va & (PAGE - 1)), attrs },
    }
}

/// Блок экземпляра канала: указатель на PD3 и таблица подконтекстов
/// (`gp100_vmm_join` + `gv100_vmm_join`). `write32(смещение, значение)` —
/// запись в блок (0x1000 Б, заранее обнулённый). `limit` — предел VA
/// (у нас [`NVK_VA_LIMIT`]).
pub fn inst_block_join(mut write32: impl FnMut(u32, u32), pd3: u64, pd3_target: Target, limit: u64) {
    let base = (1u64 << 10) /* VER2 */ | (1 << 11) /* 64 КиБ большие страницы */ | pd3_target.pdb_aperture() | pd3;
    let lim = limit - 1;
    write32(0x200, base as u32);
    write32(0x204, (base >> 32) as u32);
    write32(0x208, lim as u32);
    write32(0x20c, (lim >> 32) as u32);
    write32(0x21c, 0);
    let mask: u64 = 1;
    for i in 0..64u32 {
        let (lo, hi) = if mask & (1 << i) != 0 { (base as u32, (base >> 32) as u32) } else { (1, 1) };
        write32(0x2a4 + i * 0x10, hi);
        write32(0x2a0 + i * 0x10, lo);
        write32(0x2a8 + i * 0x10, 0);
    }
    write32(0x298, mask as u32);
    write32(0x29c, (mask >> 32) as u32);
}

/// Регистры сброса TLB (Turing+, `tu102_vmm_flush`) в BAR0.
pub mod tlb {
    pub const PDB: u32 = 0x00b8_30a0;
    pub const PDB_HI: u32 = 0x00b8_30a4;
    pub const TRIGGER: u32 = 0x00b8_30b0;
    /// Бит «идёт» в TRIGGER: ждать, пока сбросится (2 с).
    pub const PENDING: u32 = 0x8000_0000;
    pub const PAGE_ALL: u32 = 0x1;
    /// HUB_ONLY | все PDB — для BAR.
    pub const HUB_ALL_PDB: u32 = 0x6;

    /// Что писать: `[(PDB, pd3 >> 8), (PDB_HI, 0), (TRIGGER, PENDING | PAGE_ALL)]`.
    pub const fn sequence(pd3_vram: u64) -> [(u32, u32); 3] {
        [(PDB, (pd3_vram >> 8) as u32), (PDB_HI, 0), (TRIGGER, PENDING | PAGE_ALL)]
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    /// Память таблиц для тестов: страницы по требованию.
    #[derive(Default)]
    pub struct TestMem {
        pub words: BTreeMap<u64, u64>,
        pub next: u64,
        pub live: BTreeSet<u64>,
    }

    impl TestMem {
        pub fn new() -> TestMem {
            TestMem { next: 0x10_0000, ..Default::default() }
        }
    }

    impl PtMem for TestMem {
        fn read64(&self, addr: u64) -> u64 {
            assert_eq!(addr % 8, 0);
            *self.words.get(&addr).unwrap_or(&0)
        }
        fn write64(&mut self, addr: u64, v: u64) {
            assert!(self.live.contains(&(addr & !0xfff)), "запись мимо таблицы {addr:#x}");
            self.words.insert(addr, v);
        }
        fn alloc_table(&mut self) -> Option<u64> {
            let a = self.next;
            self.next += 0x1000;
            self.live.insert(a);
            Some(a)
        }
        fn free_table(&mut self, addr: u64) {
            assert!(self.live.remove(&addr), "двойное освобождение таблицы");
            self.words.retain(|&k, _| k & !0xfff != addr);
        }
    }

    #[test]
    fn golden_vectors() {
        // PTE VRAM 0x1234_5000, kind 6: VALID, апертура 0, адрес >> 4.
        let v = pte(0x1234_5000, PteAttrs::user(Target::Vram, 6));
        assert_eq!(v, 0x0600_0000_0123_4501);
        // Системная: апертура 2 (бит 2), VOL (бит 3).
        let s = pte(0x8_0000_1000, PteAttrs::user(Target::SysCoh, 0));
        assert_eq!(s, 0x0000_0000_8000_010d);
        // PDE в VRAM: апертура 1 -> бит 1.
        assert_eq!(pde(0x0610_0000, Target::Vram), 0x0061_0002);
        // PDE в системной: 2<<1 | VOL.
        assert_eq!(pde(0x1000, Target::SysCoh), 0x10c);
        assert_eq!(decode_pde(0x0061_0002), Some((0x0610_0000, Target::Vram)));
        assert_eq!(decode_pde(0), None);
        assert_eq!(decode_pte(PTE_SPARSE), Pte::Sparse);
        assert_eq!(decode_pte(0), Pte::Invalid);
        match decode_pte(v) {
            Pte::Valid { phys, attrs } => {
                assert_eq!(phys, 0x1234_5000);
                assert_eq!(attrs.kind, 6);
                assert!(!attrs.vol);
            }
            p => panic!("{p:?}"),
        }
    }

    #[test]
    fn kinds() {
        assert_eq!(pte_kind(0), Ok(0));
        assert_eq!(pte_kind(6), Ok(6));
        assert_eq!(pte_kind(7), Err(Errno::EINVAL));
        assert_eq!(pte_kind(8), Ok(6));
        assert_eq!(pte_kind(0x0b), Ok(1));
        assert_eq!(pte_kind(0x0f), Err(Errno::EINVAL));
        assert_eq!(pte_kind(0x10), Err(Errno::EINVAL));
        assert_eq!(pte_kind(0xfe), Err(Errno::EINVAL));
    }

    #[test]
    fn levels_cover_49_bits() {
        let total: u32 = LEVELS.iter().map(|l| l.bits).sum::<u32>() + 12;
        assert_eq!(total, VA_BITS);
        assert_eq!(LEVELS[PD0].table_bytes(), 0x1000);
        assert_eq!(LEVELS[PD3].table_bytes(), 0x20);
        // NVK: VA 1<<39 — PD3[0], PD2[2].
        assert_eq!(LEVELS[PD3].index(1 << 39), 0);
        assert_eq!(LEVELS[PD2].index(1 << 39), 2);
        assert_eq!(LEVELS[PD1].index(1 << 39), 0);
    }

    #[test]
    fn map_walk_unmap() {
        let mut m = TestMem::new();
        let mut g = Gmmu::new(&mut m).unwrap();
        let va = 0x7f_ffff_e000; // пересекает 1 << 39: новые запись PD2, PD1, PD0 и PT
        g.map_contig(&mut m, va, 0x4000, 0x2000_0000, PteAttrs::user(Target::Vram, 0)).unwrap();
        for i in 0..4 {
            match g.translate(&m, va + i * PAGE + 0x10) {
                Walk::Mapped { phys, .. } => assert_eq!(phys, 0x2000_0000 + i * PAGE + 0x10),
                w => panic!("{i}: {w:?}"),
            }
        }
        assert_eq!(g.translate(&m, va + 0x4000), Walk::Unmapped);
        assert_eq!(g.translate(&m, va - PAGE), Walk::Unmapped);
        // Малая PT — во второй половине записи PD0.
        let path = g.ensure_path(&mut m, va, PD0).unwrap();
        let e = path[PD0] + LEVELS[PD0].index(va) * 16;
        assert_eq!(m.read64(e), 0, "data[0] (большие страницы) пуст");
        assert!(decode_pde(m.read64(e + 8)).is_some(), "data[1] — PT страниц 4 КиБ");

        g.unmap(&mut m, va + PAGE, 0x2000).unwrap();
        assert!(matches!(g.translate(&m, va), Walk::Mapped { .. }));
        assert_eq!(g.translate(&m, va + PAGE), Walk::Unmapped);
        assert_eq!(g.translate(&m, va + 2 * PAGE), Walk::Unmapped);
        assert!(matches!(g.translate(&m, va + 3 * PAGE), Walk::Mapped { .. }));

        let n = g.table_count();
        g.destroy(&mut m);
        assert!(m.live.is_empty(), "все {n} таблиц освобождены");
    }

    #[test]
    fn sysmem_pages() {
        let mut m = TestMem::new();
        let mut g = Gmmu::new(&mut m).unwrap();
        let frames = [0x5000u64, 0x9000, 0x3000];
        g.map(&mut m, 0x1_0000, 0x3000, PteAttrs::user(Target::SysCoh, 0), |i| frames[i as usize]).unwrap();
        for (i, f) in frames.iter().enumerate() {
            match g.translate(&m, 0x1_0000 + i as u64 * PAGE) {
                Walk::Mapped { phys, attrs } => {
                    assert_eq!(phys, *f);
                    assert_eq!(attrs.target, Target::SysCoh);
                    assert!(attrs.vol);
                }
                w => panic!("{w:?}"),
            }
        }
    }

    #[test]
    fn sparse_pd0_and_pt() {
        let mut m = TestMem::new();
        let mut g = Gmmu::new(&mut m).unwrap();
        // 6 МиБ разреженных с выровненного адреса: три целых PD0, PT не нужны.
        let base = 0x4000_0000u64;
        g.sparse(&mut m, base, 6 << 20).unwrap();
        let tables = g.table_count();
        assert_eq!(tables, 4, "PD3, PD2, PD1, PD0 — без PT");
        assert_eq!(g.translate(&m, base + 0x12_3000), Walk::Sparse);
        assert_eq!(g.translate(&m, base + (6 << 20)), Walk::Unmapped);
        // Отображаем страницу внутри: PT создаётся разреженной, соседи
        // остаются разреженными.
        g.map_contig(&mut m, base + 0x20_0000, PAGE, 0xabc000, PteAttrs::user(Target::Vram, 0)).unwrap();
        assert!(matches!(g.translate(&m, base + 0x20_0000), Walk::Mapped { phys: 0xabc000, .. }));
        assert_eq!(g.translate(&m, base + 0x20_1000), Walk::Sparse);
        assert_eq!(g.translate(&m, base + 0x3f_f000), Walk::Sparse);
        // Снять отображение с восстановлением разреженности.
        g.sparse(&mut m, base + 0x20_0000, PAGE).unwrap();
        assert_eq!(g.translate(&m, base + 0x20_0000), Walk::Sparse);
        // Снять разреженность целиком.
        g.unmap(&mut m, base, 6 << 20).unwrap();
        for off in [0u64, 0x20_0000, 0x20_1000, 0x5f_f000] {
            assert_eq!(g.translate(&m, base + off), Walk::Unmapped, "{off:#x}");
        }
        // Невыровненная разреженность — на уровне PT.
        g.sparse(&mut m, 0x1000, 0x3000).unwrap();
        assert_eq!(g.translate(&m, 0x0000), Walk::Unmapped);
        assert_eq!(g.translate(&m, 0x1000), Walk::Sparse);
        assert_eq!(g.translate(&m, 0x4000), Walk::Unmapped);
    }

    #[test]
    fn unmap_nothing_is_cheap_and_ok() {
        let mut m = TestMem::new();
        let mut g = Gmmu::new(&mut m).unwrap();
        // Снять огромный пустой диапазон: таблицы не создаются.
        g.unmap(&mut m, 0, 1 << 46).unwrap();
        assert_eq!(g.table_count(), 1);
        assert_eq!(g.map_contig(&mut m, 0x123, PAGE, 0, PteAttrs::user(Target::Vram, 0)), Err(Errno::EINVAL));
        assert_eq!(g.unmap(&mut m, (1 << 49) - PAGE, 2 * PAGE), Err(Errno::EINVAL));
    }

    #[test]
    fn rm_tables_not_freed() {
        let mut m = TestMem::new();
        let mut g = Gmmu::new(&mut m).unwrap();
        let path = g.ensure_path(&mut m, 1 << 39, PD2).unwrap();
        assert_eq!(path.len(), 2);
        assert_eq!(path[0], g.pd3());
        // RM кладёт свою PD1 в нашу PD2.
        let rm_pd1 = m.alloc_table().unwrap();
        m.write64(path[1] + LEVELS[PD2].index(1 << 39) * 8, pde(rm_pd1, Target::Vram));
        g.destroy(&mut m);
        assert_eq!(m.live.len(), 1);
        assert!(m.live.contains(&rm_pd1));
    }

    #[test]
    fn inst_block() {
        let mut w = BTreeMap::new();
        inst_block_join(|o, v| { w.insert(o, v); }, 0x0610_0000, Target::Vram, NVK_VA_LIMIT);
        assert_eq!(w[&0x200], 0x0610_0000 | 0xc00);
        assert_eq!(w[&0x204], 0);
        assert_eq!(w[&0x208], 0xffff_ffff);
        assert_eq!(w[&0x20c], 0x7fff);
        assert_eq!(w[&0x2a0], 0x0610_0c00);
        assert_eq!(w[&0x2b0], 1);
        assert_eq!(w[&0x2b4], 1);
        assert_eq!(w[&0x298], 1);
        assert_eq!(tlb::sequence(0x0610_0000)[0], (0xb830a0, 0x61000));
    }
}
