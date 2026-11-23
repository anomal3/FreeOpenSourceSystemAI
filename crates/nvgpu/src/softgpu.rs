// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! `SoftGpu` — программная видеокарта для QEMU (`nvgpu.fake=1`) и тестов на
//! хосте. Реализует [`GpuBackend`] так, как это делает настоящий бэкенд, и
//! исполняет то, что ему положили, *через настоящие структуры*:
//!
//! * видеопамять — поддельная (страницы по требованию, нули по умолчанию),
//!   таблицы страниц пишет [`Gmmu`] в неё же, а исполнение идёт обходом
//!   [`walk`] от PD3 — ошибки кодирования PDE/PTE видны сразу;
//! * системная память — настоящие кадры хозяина через [`SoftHost`] (в ядре —
//!   кадры `mm::frame`, которые слой DRM отдаёт в mmap), так что запись
//!   процесса в отображённый буфер видна «GPU» и наоборот;
//! * канал — как у nouveau: пушбуфер 0x12000 Б (область ядра + кольцо
//!   GPFIFO) и страница забора в системной памяти, отображённые в область
//!   ядра адресного пространства; USERD и блок экземпляра — в видеопамяти;
//! * `chan_submit` пишет записи GPFIFO и сегмент забора, двигает GP_PUT в
//!   USERD и «звонит»: «GPU» тут же читает кольцо *через GMMU* и исполняет
//!   сегменты до GP_PUT.
//!
//! Что исполняется: методы канала (SET_OBJECT, SEMAPHOREA..D, SEM_ADDR/
//! PAYLOAD/EXECUTE — выпуск, проверка захвата, редукции), копировальный
//! движок C6B5/C7B5 (линейное копирование, многострочное с шагом, REMAP с
//! константами — заливка, семафоры после копирования, физические адреса),
//! у 3D/compute/I2M — запись из пушбуфера (LINE_LENGTH_IN..LOAD_INLINE_DATA,
//! так NVK заливает мелкие данные) и семафор отчёта (SET_REPORT_SEMAPHORE,
//! счётчики запросов = 0, время — настоящее). Остальные методы 3D/compute/2D
//! принимаются и пропускаются (счётчик в [`SoftStats`]): шейдеры SoftGpu не
//! исполняет. Обращение по неотображённому адресу, незнакомый
//! заголовок, метод без объекта, блочная раскладка CE, захват семафора,
//! который не выполнен, — канал «убит» (`chan_seq_done` -> `ENODEV`),
//! причина — [`SoftGpu::chan_killed`].
//!
//! Разреженные страницы: чтение — нули, запись теряется (как у железа).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arena::{Arena, RangeAlloc, VramAlloc, align_up};
use crate::backend::{BoHandle, ChanHandle, Errno, GpuBackend, GpuInfo, ObjHandle, Place, Push, VmHandle};
use crate::gmmu::{self, Gmmu, PAGE, PteAttrs, PtMem, Target, Walk, walk};
use crate::gpfifo::{self, Header, Ring, ce, host};
use crate::lock::SpinLock;

/// Что `SoftGpu` берёт у хозяина: системные кадры и время.
///
/// В ядре: `alloc_frame` — обнулённый кадр из пула (его же потом отдают в
/// mmap через `bo_frames`), `read_phys`/`write_phys` — через прямое
/// отображение физической памяти. Обращения никогда не пересекают границу
/// 4 КиБ.
pub trait SoftHost: Send + Sync {
    /// Обнулённый кадр 4 КиБ; его физический адрес.
    fn alloc_frame(&self) -> Option<u64>;
    /// `count` обнулённых кадров подряд (адрес первого) — крупным буферам,
    /// чтобы хозяин мог показать их без копии; `None` — нет куска (тогда по
    /// кадру). Каждый кадр куска возвращается `free_frame` по отдельности.
    fn alloc_run(&self, _count: u64) -> Option<u64> {
        None
    }
    fn free_frame(&self, phys: u64);
    fn read_phys(&self, phys: u64, buf: &mut [u8]);
    fn write_phys(&self, phys: u64, data: &[u8]);
    /// Монотонное время, нс (PTIMER).
    fn now_ns(&self) -> u64;
    /// Строка в журнал хозяина (ядро — `kprintln`): почему убит канал и т. п.
    fn log(&self, _line: &str) {}
}

/// Хозяин из кучи — для тестов на хосте (и тестов WP-C/WP-D).
pub struct HeapHost {
    frames: SpinLock<BTreeMap<u64, Box<[u8; 4096]>>>,
    next: AtomicU64,
    clock: AtomicU64,
}

impl Default for HeapHost {
    fn default() -> Self {
        Self::new()
    }
}

impl HeapHost {
    pub fn new() -> HeapHost {
        HeapHost { frames: SpinLock::new(BTreeMap::new()), next: AtomicU64::new(0x1_0000_0000), clock: AtomicU64::new(1_000_000) }
    }

    /// Сколько кадров сейчас выдано (проверка утечек).
    pub fn live_frames(&self) -> usize {
        self.frames.lock().len()
    }
}

impl SoftHost for HeapHost {
    fn alloc_frame(&self) -> Option<u64> {
        let a = self.next.fetch_add(PAGE, Ordering::Relaxed);
        self.frames.lock().insert(a, Box::new([0; 4096]));
        Some(a)
    }
    fn free_frame(&self, phys: u64) {
        assert!(self.frames.lock().remove(&phys).is_some(), "HeapHost: чужой кадр {phys:#x}");
    }
    fn read_phys(&self, phys: u64, buf: &mut [u8]) {
        let f = self.frames.lock();
        let off = (phys % PAGE) as usize;
        match f.get(&(phys & !(PAGE - 1))) {
            Some(p) => buf.copy_from_slice(&p[off..off + buf.len()]),
            None => panic!("HeapHost: чтение несуществующего кадра {phys:#x}"),
        }
    }
    fn write_phys(&self, phys: u64, data: &[u8]) {
        let mut f = self.frames.lock();
        let off = (phys % PAGE) as usize;
        match f.get_mut(&(phys & !(PAGE - 1))) {
            Some(p) => p[off..off + data.len()].copy_from_slice(data),
            None => panic!("HeapHost: запись в несуществующий кадр {phys:#x}"),
        }
    }
    fn now_ns(&self) -> u64 {
        self.clock.fetch_add(1000, Ordering::Relaxed)
    }
}

/// Счётчики исполнения.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoftStats {
    pub segments: u64,
    pub methods: u64,
    /// Методы 3D/compute/2D/M2MF, которые приняты и пропущены.
    pub ignored_methods: u64,
    pub ce_launches: u64,
    pub ce_bytes: u64,
    pub sem_releases: u64,
    pub kills: u64,
}

// ---------------------------------------------------------------------------
// Видеопамять

#[derive(Default)]
struct Vram {
    pages: BTreeMap<u64, Box<[u8; 4096]>>,
    size: u64,
}

impl Vram {
    fn read(&self, addr: u64, buf: &mut [u8]) {
        let mut done = 0;
        while done < buf.len() {
            let a = addr + done as u64;
            let off = (a % PAGE) as usize;
            let n = (4096 - off).min(buf.len() - done);
            match self.pages.get(&(a - off as u64)) {
                Some(p) => buf[done..done + n].copy_from_slice(&p[off..off + n]),
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
    }

    fn write(&mut self, addr: u64, data: &[u8]) {
        let mut done = 0;
        while done < data.len() {
            let a = addr + done as u64;
            let off = (a % PAGE) as usize;
            let n = (4096 - off).min(data.len() - done);
            let p = self.pages.entry(a - off as u64).or_insert_with(|| Box::new([0; 4096]));
            p[off..off + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
    }

    fn zero(&mut self, addr: u64, len: u64) {
        let mut a = addr & !(PAGE - 1);
        while a < addr + len {
            self.pages.remove(&a);
            a += PAGE;
        }
    }

    fn r64(&self, addr: u64) -> u64 {
        let mut b = [0; 8];
        self.read(addr, &mut b);
        u64::from_le_bytes(b)
    }
    fn w64(&mut self, addr: u64, v: u64) {
        self.write(addr, &v.to_le_bytes());
    }
    fn r32(&self, addr: u64) -> u32 {
        let mut b = [0; 4];
        self.read(addr, &mut b);
        u32::from_le_bytes(b)
    }
    fn w32(&mut self, addr: u64, v: u32) {
        self.write(addr, &v.to_le_bytes());
    }
}

/// Таблицы страниц в поддельной видеопамяти (служебная арена).
struct VramPt<'a> {
    vram: &'a mut Vram,
    valloc: &'a mut VramAlloc,
}

impl PtMem for VramPt<'_> {
    fn read64(&self, addr: u64) -> u64 {
        self.vram.r64(addr)
    }
    fn write64(&mut self, addr: u64, v: u64) {
        self.vram.w64(addr, v)
    }
    fn alloc_table(&mut self) -> Option<u64> {
        let a = self.valloc.alloc(Arena::Cpu, PAGE, PAGE)?;
        self.vram.zero(a, PAGE);
        Some(a)
    }
    fn free_table(&mut self, addr: u64) {
        self.valloc.free(Arena::Cpu, addr, PAGE);
        self.vram.zero(addr, PAGE);
    }
}

/// Только чтение — для обхода «как GPU».
struct VramRo<'a>(&'a Vram);

impl PtMem for VramRo<'_> {
    fn read64(&self, addr: u64) -> u64 {
        self.0.r64(addr)
    }
    fn write64(&mut self, _: u64, _: u64) {
        unreachable!("VramRo: запись")
    }
    fn alloc_table(&mut self) -> Option<u64> {
        None
    }
    fn free_table(&mut self, _: u64) {}
}

// ---------------------------------------------------------------------------
// Объекты

enum Backing {
    Vram { addr: u64 },
    Sys { frames: Vec<u64> },
}

struct Bo {
    size: u64,
    backing: Backing,
}

struct Vm {
    gmmu: Gmmu,
    kernel: (u64, u64),
    /// Распределитель области ядра (служебные отображения каналов).
    kva: RangeAlloc,
}

/// Размер диапазона, который у настоящего бэкенда отдаётся RM (раздел 2.2).
pub const RM_RESERVED_BYTES: u64 = 0x2000_0000;

struct CeState {
    regs: Box<[u32; 0x1000]>,
}

/// Общая у 3D, compute и I2M часть: запись в память из пушбуфера
/// (LINE_LENGTH_IN 0x180 .. LOAD_INLINE_DATA 0x1b4) и семафор отчёта
/// (SET_REPORT_SEMAPHORE_A..D 0x1b00..0x1b0c). Так NVK заливает мелкие
/// данные (`vkCmdUpdateBuffer`, дескрипторы) и пишет запросы/время.
#[derive(Default)]
struct EngState {
    regs: BTreeMap<u32, u32>,
    inline: Option<Inline>,
}

struct Inline {
    buf: Vec<u8>,
    need: usize,
    line: u64,
    lines: u64,
    dst: u64,
    pitch: u64,
}

/// Методы общей части 3D/compute/I2M.
pub mod eng {
    pub const LINE_LENGTH_IN: u32 = 0x180;
    pub const LINE_COUNT: u32 = 0x184;
    pub const OFFSET_OUT_UPPER: u32 = 0x188;
    pub const OFFSET_OUT: u32 = 0x18c;
    pub const PITCH_OUT: u32 = 0x190;
    pub const LAUNCH_DMA: u32 = 0x1b0;
    pub const LAUNCH_DMA_DST_PITCH: u32 = 1 << 0;
    pub const LOAD_INLINE_DATA: u32 = 0x1b4;
    pub const SET_REPORT_SEMAPHORE_A: u32 = 0x1b00;
    pub const SET_REPORT_SEMAPHORE_B: u32 = 0x1b04;
    pub const SET_REPORT_SEMAPHORE_C: u32 = 0x1b08;
    pub const SET_REPORT_SEMAPHORE_D: u32 = 0x1b0c;
    pub const REPORT_OP_MASK: u32 = 0x3;
    pub const REPORT_OP_RELEASE: u32 = 0;
    pub const REPORT_OP_ACQUIRE: u32 = 1;
    pub const REPORT_OP_REPORT_ONLY: u32 = 2;
    pub const REPORT_REDUCTION_ENABLE: u32 = 1 << 3;
    pub const REPORT_REDUCTION_SHIFT: u32 = 9;
    /// Поле REPORT 27:23: 0 = NONE (пишется полезная нагрузка).
    pub const REPORT_SHIFT: u32 = 23;
    pub const REPORT_ONE_WORD: u32 = 1 << 28;
}

struct Chan {
    vm: u32,
    chid: u32,
    inst: u64,
    userd: u64,
    pushbuf: u32,
    push_va: u64,
    fence_bo: u32,
    fence_va: u64,
    seq: u64,
    ring: Ring,
    /// Позиция в области ядра пушбуфера для сегментов заборов.
    kpos: u64,
    killed: Option<String>,
    objs: BTreeMap<u32, u32>,
    subch: [u32; 8],
    // Состояние методов канала.
    sem_a: u32,
    sem_b: u32,
    sem_c: u32,
    sem_addr: u64,
    sem_payload: u64,
    ce: CeState,
    eng: BTreeMap<u32, EngState>,
}

struct State {
    vram: Vram,
    valloc: VramAlloc,
    bos: BTreeMap<u32, Bo>,
    vms: BTreeMap<u32, Vm>,
    chans: BTreeMap<u32, Chan>,
    next_id: u32,
    stats: SoftStats,
}

impl State {
    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }
}

/// Куда попал адрес GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Loc {
    Vram(u64),
    Sys(u64),
    Sparse,
}

/// Программная видеокарта.
pub struct SoftGpu {
    info: GpuInfo,
    host: Arc<dyn SoftHost>,
    st: SpinLock<State>,
}

/// Раскладка поддельной видеопамяти: служебная арена 64 МиБ с 1 МиБ, общая —
/// дальше до конца.
const CPU_ARENA: (u64, u64) = (0x0010_0000, 0x0400_0000);

impl SoftGpu {
    /// RTX 3060 с 256 МиБ поддельной видеопамяти и 1 ГиБ «GART».
    pub fn new(host: Arc<dyn SoftHost>) -> SoftGpu {
        SoftGpu::with_info(host, GpuInfo::ga106(256 << 20, 1 << 30))
    }

    /// С заданным описанием (`info.vram_bytes` >= 128 МиБ).
    pub fn with_info(host: Arc<dyn SoftHost>, info: GpuInfo) -> SoftGpu {
        assert!(info.vram_bytes >= 128 << 20, "SoftGpu: мало видеопамяти");
        let bulk_start = CPU_ARENA.0 + CPU_ARENA.1;
        let mut valloc = VramAlloc::with_ranges(&[CPU_ARENA], &[(bulk_start, info.vram_bytes - bulk_start)]);
        // Как глобальные буферы контекста GR у настоящей карты: VRAM_USED > 0
        // с самого начала.
        valloc.alloc(Arena::Bulk, 0x10_0000, PAGE);
        let st = State {
            vram: Vram { pages: BTreeMap::new(), size: info.vram_bytes },
            valloc,
            bos: BTreeMap::new(),
            vms: BTreeMap::new(),
            chans: BTreeMap::new(),
            next_id: 0,
            stats: SoftStats::default(),
        };
        SoftGpu { info, host, st: SpinLock::new(st) }
    }

    pub fn host(&self) -> &Arc<dyn SoftHost> {
        &self.host
    }

    pub fn stats(&self) -> SoftStats {
        self.st.lock().stats
    }

    /// Почему канал убит (`None` — жив или нет такого).
    pub fn chan_killed(&self, ch: ChanHandle) -> Option<String> {
        self.st.lock().chans.get(&ch.0).and_then(|c| c.killed.clone())
    }

    /// Обход таблиц `vm` для `va` (для тестов и `gpu vm`-подобных отладок).
    pub fn vm_translate(&self, vm: VmHandle, va: u64) -> Option<Walk> {
        let st = self.st.lock();
        let v = st.vms.get(&vm.0)?;
        Some(walk(&VramRo(&st.vram), v.gmmu.pd3(), va))
    }

    /// PD3 адресного пространства (физический адрес в поддельной VRAM).
    pub fn vm_pd3(&self, vm: VmHandle) -> Option<u64> {
        self.st.lock().vms.get(&vm.0).map(|v| v.gmmu.pd3())
    }

    /// Прочитать память «глазами GPU» (как CE с виртуальным адресом).
    pub fn read_va(&self, vm: VmHandle, va: u64, buf: &mut [u8]) -> Result<(), Errno> {
        let st = self.st.lock();
        let pd3 = st.vms.get(&vm.0).ok_or(Errno::ENOENT)?.gmmu.pd3();
        gpu_read(&st.vram, &*self.host, pd3, va, buf).map_err(|_| Errno::EFAULT)
    }

    /// Записать память «глазами GPU».
    pub fn write_va(&self, vm: VmHandle, va: u64, data: &[u8]) -> Result<(), Errno> {
        let mut st = self.st.lock();
        let pd3 = st.vms.get(&vm.0).ok_or(Errno::ENOENT)?.gmmu.pd3();
        gpu_write(&mut st.vram, &*self.host, pd3, va, data).map_err(|_| Errno::EFAULT)
    }

    /// Прочитать буфер напрямую (VRAM или кадры).
    pub fn bo_read(&self, bo: BoHandle, off: u64, buf: &mut [u8]) -> Result<(), Errno> {
        let st = self.st.lock();
        let b = st.bos.get(&bo.0).ok_or(Errno::ENOENT)?;
        if off + buf.len() as u64 > b.size {
            return Err(Errno::EINVAL);
        }
        match &b.backing {
            Backing::Vram { addr } => st.vram.read(addr + off, buf),
            Backing::Sys { frames } => {
                let mut done = 0;
                while done < buf.len() {
                    let o = off + done as u64;
                    let n = ((PAGE - o % PAGE) as usize).min(buf.len() - done);
                    self.host.read_phys(frames[(o / PAGE) as usize] + o % PAGE, &mut buf[done..done + n]);
                    done += n;
                }
            }
        }
        Ok(())
    }

    /// Записать в буфер напрямую.
    pub fn bo_write(&self, bo: BoHandle, off: u64, data: &[u8]) -> Result<(), Errno> {
        let mut st = self.st.lock();
        let st = &mut *st;
        let b = st.bos.get(&bo.0).ok_or(Errno::ENOENT)?;
        if off + data.len() as u64 > b.size {
            return Err(Errno::EINVAL);
        }
        match &b.backing {
            Backing::Vram { addr } => {
                let a = *addr;
                st.vram.write(a + off, data)
            }
            Backing::Sys { frames } => {
                let mut done = 0;
                while done < data.len() {
                    let o = off + done as u64;
                    let n = ((PAGE - o % PAGE) as usize).min(data.len() - done);
                    self.host.write_phys(frames[(o / PAGE) as usize] + o % PAGE, &data[done..done + n]);
                    done += n;
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Доступ «глазами GPU»

struct Fault(u64, &'static str);

fn locate(vram: &Vram, pd3: u64, va: u64, write: bool) -> Result<Loc, Fault> {
    match walk(&VramRo(vram), pd3, va) {
        Walk::Unmapped => Err(Fault(va, "нет отображения")),
        Walk::Sparse => Ok(Loc::Sparse),
        Walk::Mapped { attrs, .. } if write && attrs.ro => Err(Fault(va, "запись в RO")),
        Walk::Mapped { phys, attrs } => Ok(match attrs.target {
            Target::Vram => {
                if phys >= vram.size {
                    return Err(Fault(va, "PTE за концом VRAM"));
                }
                Loc::Vram(phys)
            }
            Target::SysCoh | Target::SysNcoh => Loc::Sys(phys),
        }),
    }
}

fn gpu_read(vram: &Vram, host: &dyn SoftHost, pd3: u64, va: u64, buf: &mut [u8]) -> Result<(), Fault> {
    let mut done = 0;
    while done < buf.len() {
        let a = va + done as u64;
        let n = ((PAGE - a % PAGE) as usize).min(buf.len() - done);
        match locate(vram, pd3, a, false)? {
            Loc::Vram(p) => vram.read(p, &mut buf[done..done + n]),
            Loc::Sys(p) => host.read_phys(p, &mut buf[done..done + n]),
            Loc::Sparse => buf[done..done + n].fill(0),
        }
        done += n;
    }
    Ok(())
}

fn gpu_write(vram: &mut Vram, host: &dyn SoftHost, pd3: u64, va: u64, data: &[u8]) -> Result<(), Fault> {
    let mut done = 0;
    while done < data.len() {
        let a = va + done as u64;
        let n = ((PAGE - a % PAGE) as usize).min(data.len() - done);
        match locate(vram, pd3, a, true)? {
            Loc::Vram(p) => vram.write(p, &data[done..done + n]),
            Loc::Sys(p) => host.write_phys(p, &data[done..done + n]),
            Loc::Sparse => {}
        }
        done += n;
    }
    Ok(())
}

/// Адресация CE: виртуальная через GMMU или физическая по SET_*_PHYS_MODE.
#[derive(Clone, Copy)]
enum CeAddr {
    Virt(u64),
    Phys { addr: u64, sys: bool },
}

fn ce_read(vram: &Vram, host: &dyn SoftHost, pd3: u64, a: CeAddr, buf: &mut [u8]) -> Result<(), Fault> {
    match a {
        CeAddr::Virt(va) => gpu_read(vram, host, pd3, va, buf),
        CeAddr::Phys { addr, sys: false } => {
            if addr + buf.len() as u64 > vram.size {
                return Err(Fault(addr, "CE: физический адрес за концом VRAM"));
            }
            vram.read(addr, buf);
            Ok(())
        }
        CeAddr::Phys { addr, sys: true } => {
            let mut done = 0;
            while done < buf.len() {
                let p = addr + done as u64;
                let n = ((PAGE - p % PAGE) as usize).min(buf.len() - done);
                host.read_phys(p, &mut buf[done..done + n]);
                done += n;
            }
            Ok(())
        }
    }
}

fn ce_write(vram: &mut Vram, host: &dyn SoftHost, pd3: u64, a: CeAddr, data: &[u8]) -> Result<(), Fault> {
    match a {
        CeAddr::Virt(va) => gpu_write(vram, host, pd3, va, data),
        CeAddr::Phys { addr, sys: false } => {
            if addr + data.len() as u64 > vram.size {
                return Err(Fault(addr, "CE: физический адрес за концом VRAM"));
            }
            vram.write(addr, data);
            Ok(())
        }
        CeAddr::Phys { addr, sys: true } => {
            let mut done = 0;
            while done < data.len() {
                let p = addr + done as u64;
                let n = ((PAGE - p % PAGE) as usize).min(data.len() - done);
                host.write_phys(p, &data[done..done + n]);
                done += n;
            }
            Ok(())
        }
    }
}

impl CeAddr {
    fn add(self, off: u64) -> CeAddr {
        match self {
            CeAddr::Virt(v) => CeAddr::Virt(v + off),
            CeAddr::Phys { addr, sys } => CeAddr::Phys { addr: addr + off, sys },
        }
    }
}

/// Редукция семафора (общая для хоста и CE: IMIN..DEC = 0..7).
fn reduce(op: u32, old: u64, val: u64, signed: bool, wide: bool) -> u64 {
    let mask = if wide { u64::MAX } else { u32::MAX as u64 };
    let (o, v) = (old & mask, val & mask);
    let sx = |x: u64| if wide { x as i64 } else { x as u32 as i32 as i64 };
    let r = match op {
        0 => if signed { if sx(o) < sx(v) { o } else { v } } else { o.min(v) },
        1 => if signed { if sx(o) > sx(v) { o } else { v } } else { o.max(v) },
        2 => o ^ v,
        3 => o & v,
        4 => o | v,
        5 => o.wrapping_add(v),
        // INC/DEC: с переходом через значение (как у NVIDIA: INC до val, затем 0).
        6 => if o >= v { 0 } else { o + 1 },
        7 => if o == 0 || o > v { v } else { o - 1 },
        _ => o,
    };
    r & mask
}

// ---------------------------------------------------------------------------
// Исполнение

struct Exec<'a> {
    vram: &'a mut Vram,
    host: &'a dyn SoftHost,
    stats: &'a mut SoftStats,
    pd3: u64,
}

impl Exec<'_> {
    fn read_words(&mut self, va: u64, len: u32) -> Result<Vec<u32>, Fault> {
        let mut bytes = vec![0u8; len as usize];
        gpu_read(self.vram, self.host, self.pd3, va, &mut bytes)?;
        Ok(bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    fn r32(&mut self, va: u64) -> Result<u32, Fault> {
        let mut b = [0; 4];
        gpu_read(self.vram, self.host, self.pd3, va, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn r64(&mut self, va: u64) -> Result<u64, Fault> {
        let mut b = [0; 8];
        gpu_read(self.vram, self.host, self.pd3, va, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn w32(&mut self, va: u64, v: u32) -> Result<(), Fault> {
        gpu_write(self.vram, self.host, self.pd3, va, &v.to_le_bytes())
    }
    fn w64(&mut self, va: u64, v: u64) -> Result<(), Fault> {
        gpu_write(self.vram, self.host, self.pd3, va, &v.to_le_bytes())
    }

    /// Исполнить один сегмент пушбуфера.
    fn segment(&mut self, ch: &mut Chan, va: u64, len: u32) -> Result<(), String> {
        self.stats.segments += 1;
        let w = self.read_words(va, len).map_err(|f| format!("чтение пушбуфера {:#x}: {}", f.0, f.1))?;
        let mut i = 0;
        while i < w.len() {
            let h = w[i];
            i += 1;
            let (subc, mthd, count, step) = match gpfifo::decode_header(h) {
                Header::Inc { subc, mthd, count } => (subc, mthd, count, 1u8),
                Header::NonInc { subc, mthd, count } => (subc, mthd, count, 0),
                Header::OneInc { subc, mthd, count } => (subc, mthd, count, 2),
                Header::Immd { subc, mthd, data } => {
                    self.method(ch, subc, mthd, data)?;
                    continue;
                }
                Header::EndSegment => break,
                Header::Unsupported(h) => return Err(format!("заголовок {h:#010x} в {:#x}", va + (i as u64 - 1) * 4)),
            };
            let count = count as usize;
            if i + count > w.len() {
                return Err(format!("пачка методов {mthd:#x} x{count} обрезана концом сегмента"));
            }
            for k in 0..count {
                let m = match step {
                    1 => mthd + 4 * k as u32,
                    0 => mthd,
                    _ => mthd + if k == 0 { 0 } else { 4 },
                };
                self.method(ch, subc, m, w[i + k])?;
            }
            i += count;
        }
        Ok(())
    }

    fn method(&mut self, ch: &mut Chan, subc: u32, mthd: u32, data: u32) -> Result<(), String> {
        self.stats.methods += 1;
        if mthd < host::LIMIT {
            return self.host_method(ch, subc, mthd, data);
        }
        let mut class = ch.subch[subc as usize];
        // Подканал 4 у Volta+ — копировальный движок и без SET_OBJECT:
        // NVK его не привязывает (`nvk_upload_queue.c`, заливки и копии
        // `nvk_cmd_copy.c` идут сразу методами NV90B5), и железо это
        // принимает (20.11, `gpu gametest soft`: канал убивался на 0x408).
        if class == 0 && subc == 4 {
            class = 0xc7b5;
        }
        if class == 0 {
            return Err(format!("метод {mthd:#x} на подканале {subc} без объекта"));
        }
        if class & 0xff == 0xb5 {
            ch.ce.regs[(mthd / 4) as usize] = data;
            if mthd == ce::LAUNCH_DMA {
                self.ce_launch(ch, data)?;
            }
        } else if matches!(class & 0xff, 0x97 | 0xc0 | 0x40 | 0x39) {
            self.eng_method(ch, class, mthd, data)?;
        } else {
            self.stats.ignored_methods += 1;
        }
        Ok(())
    }

    /// 3D/compute/I2M: запись из пушбуфера и семафор отчёта; остальное —
    /// пропускается.
    fn eng_method(&mut self, ch: &mut Chan, class: u32, mthd: u32, data: u32) -> Result<(), String> {
        let st = ch.eng.entry(class).or_default();
        let reg = |st: &EngState, m: u32| st.regs.get(&m).copied().unwrap_or(0);
        match mthd {
            eng::LAUNCH_DMA => {
                if data & eng::LAUNCH_DMA_DST_PITCH == 0 {
                    return Err(format!("класс {class:#x}: LAUNCH_DMA в блочную раскладку не поддержан в SoftGpu"));
                }
                let line = reg(st, eng::LINE_LENGTH_IN) as u64;
                let lines = reg(st, eng::LINE_COUNT) as u64;
                let need = (line * lines) as usize;
                st.inline = (need > 0).then(|| Inline {
                    buf: Vec::with_capacity(need + 3),
                    need,
                    line,
                    lines,
                    dst: ((reg(st, eng::OFFSET_OUT_UPPER) as u64 & 0x1ffff) << 32) | reg(st, eng::OFFSET_OUT) as u64,
                    pitch: reg(st, eng::PITCH_OUT) as u64,
                });
            }
            eng::LOAD_INLINE_DATA => {
                let Some(inl) = &mut st.inline else { return Ok(()) };
                inl.buf.extend_from_slice(&data.to_le_bytes());
                if inl.buf.len() >= inl.need {
                    let inl = st.inline.take().unwrap();
                    for l in 0..inl.lines {
                        let s = (l * inl.line) as usize;
                        let d = &inl.buf[s..s + inl.line as usize];
                        gpu_write(self.vram, self.host, self.pd3, inl.dst + l * inl.pitch, d).map_err(|f| format!("LOAD_INLINE_DATA {:#x}: {}", f.0, f.1))?;
                    }
                    self.stats.ce_bytes += inl.need as u64;
                }
            }
            eng::SET_REPORT_SEMAPHORE_D => {
                let addr = ((reg(st, eng::SET_REPORT_SEMAPHORE_A) as u64 & 0xff) << 32) | reg(st, eng::SET_REPORT_SEMAPHORE_B) as u64;
                let payload = reg(st, eng::SET_REPORT_SEMAPHORE_C);
                let fault = |f: Fault| format!("семафор отчёта {:#x}: {}", f.0, f.1);
                match data & eng::REPORT_OP_MASK {
                    op @ (eng::REPORT_OP_RELEASE | eng::REPORT_OP_REPORT_ONLY) => {
                        // Счётчиков у нас нет: REPORT_NONE пишет нагрузку, прочие — 0.
                        let mut v = if op == eng::REPORT_OP_RELEASE && (data >> eng::REPORT_SHIFT) & 0x1f == 0 { payload as u64 } else { 0 };
                        if data & eng::REPORT_REDUCTION_ENABLE != 0 {
                            let old = self.r32(addr).map_err(fault)? as u64;
                            v = reduce((data >> eng::REPORT_REDUCTION_SHIFT) & 7, old, v, data & (1 << 17) == 0, false);
                        }
                        self.stats.sem_releases += 1;
                        if data & eng::REPORT_ONE_WORD != 0 {
                            self.w32(addr, v as u32).map_err(fault)?;
                        } else {
                            let t = self.host.now_ns();
                            self.w64(addr, v).map_err(fault)?;
                            self.w64(addr + 8, t).map_err(fault)?;
                        }
                    }
                    eng::REPORT_OP_ACQUIRE => {
                        let v = self.r32(addr).map_err(fault)?;
                        // COMPARISON (бит 16): 0 = EQ, 1 = GE.
                        let ok = if data & (1 << 16) != 0 { v >= payload } else { v == payload };
                        if !ok {
                            return Err(format!("захват семафора отчёта {addr:#x}: {v:#x} против {payload:#x} — ждать некого"));
                        }
                    }
                    _ => {}
                }
            }
            m => {
                st.regs.insert(m, data);
                if !matches!(m, eng::LINE_LENGTH_IN..=eng::PITCH_OUT | eng::SET_REPORT_SEMAPHORE_A..=eng::SET_REPORT_SEMAPHORE_C) {
                    self.stats.ignored_methods += 1;
                }
            }
        }
        Ok(())
    }

    fn host_method(&mut self, ch: &mut Chan, subc: u32, mthd: u32, data: u32) -> Result<(), String> {
        let fault = |f: Fault| format!("семафор {:#x}: {}", f.0, f.1);
        match mthd {
            host::SET_OBJECT => {
                let class = data & 0xffff;
                if !ch.objs.values().any(|&c| c == class) {
                    return Err(format!("SET_OBJECT {class:#x}: объекта такого класса в канале нет"));
                }
                ch.subch[subc as usize] = class;
            }
            host::SEMAPHOREA => ch.sem_a = data,
            host::SEMAPHOREB => ch.sem_b = data,
            host::SEMAPHOREC => ch.sem_c = data,
            host::SEMAPHORED => {
                let addr = ((ch.sem_a as u64 & 0xff) << 32) | (ch.sem_b as u64 & !3);
                let payload = ch.sem_c;
                match data & host::SEMD_OP_MASK {
                    host::SEMD_OP_RELEASE => {
                        self.stats.sem_releases += 1;
                        self.w32(addr, payload).map_err(fault)?;
                        if data & host::SEMD_RELEASE_SIZE_4BYTE == 0 {
                            let t = self.host.now_ns();
                            self.w32(addr + 4, 0).map_err(fault)?;
                            self.w64(addr + 8, t).map_err(fault)?;
                        }
                    }
                    op @ (host::SEMD_OP_ACQUIRE | host::SEMD_OP_ACQ_GEQ | host::SEMD_OP_ACQ_AND) => {
                        let v = self.r32(addr).map_err(fault)?;
                        let ok = match op {
                            host::SEMD_OP_ACQUIRE => v == payload,
                            host::SEMD_OP_ACQ_GEQ => (v.wrapping_sub(payload) as i32) >= 0,
                            _ => v & payload != 0,
                        };
                        if !ok {
                            return Err(format!("захват семафора {addr:#x}: {v:#x} против {payload:#x} — ждать некого"));
                        }
                    }
                    host::SEMD_OP_REDUCTION => {
                        let old = self.r32(addr).map_err(fault)? as u64;
                        let r = reduce((data >> 27) & 0xf, old, payload as u64, data & (1 << 31) == 0, false);
                        self.w32(addr, r as u32).map_err(fault)?;
                    }
                    op => return Err(format!("SEMAPHORED: операция {op:#x}")),
                }
            }
            host::SEM_ADDR_LO => ch.sem_addr = (ch.sem_addr & !0xffff_ffff) | (data as u64 & !3),
            host::SEM_ADDR_HI => ch.sem_addr = (ch.sem_addr & 0xffff_ffff) | ((data as u64 & 0xff) << 32),
            host::SEM_PAYLOAD_LO => ch.sem_payload = (ch.sem_payload & !0xffff_ffff) | data as u64,
            host::SEM_PAYLOAD_HI => ch.sem_payload = (ch.sem_payload & 0xffff_ffff) | ((data as u64) << 32),
            host::SEM_EXECUTE => {
                let addr = ch.sem_addr;
                let wide = data & host::SEMX_PAYLOAD_64BIT != 0;
                let payload = if wide { ch.sem_payload } else { ch.sem_payload & 0xffff_ffff };
                let read = |e: &mut Self| -> Result<u64, String> {
                    if wide { e.r64(addr).map_err(fault) } else { e.r32(addr).map(|v| v as u64).map_err(fault) }
                };
                match data & host::SEMX_OP_MASK {
                    host::SEMX_OP_RELEASE | host::SEMX_OP_REDUCTION => {
                        let v = if data & host::SEMX_OP_MASK == host::SEMX_OP_REDUCTION {
                            let old = read(self)?;
                            reduce((data >> host::SEMX_REDUCTION_SHIFT) & 0xf, old, payload, data & (1 << 31) == 0, wide)
                        } else {
                            self.stats.sem_releases += 1;
                            payload
                        };
                        if wide { self.w64(addr, v).map_err(fault)? } else { self.w32(addr, v as u32).map_err(fault)? }
                        if data & host::SEMX_RELEASE_TIMESTAMP != 0 {
                            let t = self.host.now_ns();
                            self.w64(addr + 8, t).map_err(fault)?;
                        }
                    }
                    op => {
                        let v = read(self)?;
                        let ok = match op {
                            host::SEMX_OP_ACQUIRE => v == payload,
                            host::SEMX_OP_ACQ_STRICT_GEQ => v >= payload,
                            host::SEMX_OP_ACQ_CIRC_GEQ => {
                                if wide { (v.wrapping_sub(payload) as i64) >= 0 } else { ((v as u32).wrapping_sub(payload as u32) as i32) >= 0 }
                            }
                            host::SEMX_OP_ACQ_AND => v & payload != 0,
                            host::SEMX_OP_ACQ_NOR => v & payload == 0,
                            _ => return Err(format!("SEM_EXECUTE: операция {op}")),
                        };
                        if !ok {
                            return Err(format!("захват семафора {addr:#x}: {v:#x} против {payload:#x} — ждать некого"));
                        }
                    }
                }
            }
            // NOP, NON_STALL_INTERRUPT, MEM_OP_*, SET_REFERENCE, WFI, YIELD и пр.
            _ => {}
        }
        Ok(())
    }

    fn ce_launch(&mut self, ch: &mut Chan, flags: u32) -> Result<(), String> {
        self.stats.ce_launches += 1;
        let r = |m: u32| ch.ce.regs[(m / 4) as usize];
        let addr = |hi: u32, lo: u32| ((r(hi) as u64 & 0x1ffff) << 32) | r(lo) as u64;
        let phys_mode = |m: u32| r(m) & 3 != ce::PHYS_LOCAL_FB;
        let src = if flags & ce::DMA_SRC_PHYSICAL != 0 {
            CeAddr::Phys { addr: addr(ce::OFFSET_IN_UPPER, ce::OFFSET_IN_LOWER), sys: phys_mode(ce::SET_SRC_PHYS_MODE) }
        } else {
            CeAddr::Virt(addr(ce::OFFSET_IN_UPPER, ce::OFFSET_IN_LOWER))
        };
        let dst = if flags & ce::DMA_DST_PHYSICAL != 0 {
            CeAddr::Phys { addr: addr(ce::OFFSET_OUT_UPPER, ce::OFFSET_OUT_LOWER), sys: phys_mode(ce::SET_DST_PHYS_MODE) }
        } else {
            CeAddr::Virt(addr(ce::OFFSET_OUT_UPPER, ce::OFFSET_OUT_LOWER))
        };
        let fault = |f: Fault| format!("CE {:#x}: {}", f.0, f.1);

        if flags & ce::DMA_TRANSFER_MASK != ce::DMA_TRANSFER_NONE {
            if flags & ce::DMA_SRC_PITCH == 0 || flags & ce::DMA_DST_PITCH == 0 {
                return Err(String::from("CE: блочная раскладка (BLOCKLINEAR) не поддержана в SoftGpu"));
            }
            let len = r(ce::LINE_LENGTH_IN) as u64;
            let (lines, pin, pout) = if flags & ce::DMA_MULTI_LINE != 0 {
                (r(ce::LINE_COUNT) as u64, r(ce::PITCH_IN) as u64, r(ce::PITCH_OUT) as u64)
            } else {
                (1, 0, 0)
            };
            for l in 0..lines {
                let s = src.add(l * pin);
                let d = dst.add(l * pout);
                if flags & ce::DMA_REMAP != 0 {
                    self.ce_remap_line(ch, s, d, len).map_err(fault)?;
                } else {
                    // Кусками по 64 КиБ: строки бывают большими.
                    let mut off = 0;
                    let mut buf = vec![0u8; len.min(0x10000) as usize];
                    while off < len {
                        let n = (len - off).min(0x10000) as usize;
                        ce_read(self.vram, self.host, self.pd3, s.add(off), &mut buf[..n]).map_err(fault)?;
                        ce_write(self.vram, self.host, self.pd3, d.add(off), &buf[..n]).map_err(fault)?;
                        off += n as u64;
                    }
                    self.stats.ce_bytes += len;
                }
            }
        }

        let sem = (flags >> ce::DMA_SEMAPHORE_SHIFT) & 3;
        if sem != ce::DMA_SEMAPHORE_NONE {
            let sa = ((r(ce::SET_SEMAPHORE_A) as u64 & 0x1ffff) << 32) | r(ce::SET_SEMAPHORE_B) as u64;
            let wide = flags & ce::DMA_SEMAPHORE_PAYLOAD_64BIT != 0;
            let mut payload = r(ce::SET_SEMAPHORE_PAYLOAD) as u64;
            if wide {
                payload |= (r(ce::SET_SEMAPHORE_PAYLOAD_UPPER) as u64) << 32;
            }
            let sfault = |f: Fault| format!("семафор CE {:#x}: {}", f.0, f.1);
            if flags & ce::DMA_SEMAPHORE_REDUCTION_ENABLE != 0 {
                let old = if wide { self.r64(sa).map_err(sfault)? } else { self.r32(sa).map_err(sfault)? as u64 };
                payload = reduce((flags >> ce::DMA_SEMAPHORE_REDUCTION_SHIFT) & 0xf, old, payload, flags & (1 << 18) == 0, wide);
            }
            self.stats.sem_releases += 1;
            if wide { self.w64(sa, payload).map_err(sfault)? } else { self.w32(sa, payload as u32).map_err(sfault)? }
            if sem == ce::DMA_SEMAPHORE_FOUR_WORD {
                if !wide {
                    self.w32(sa + 4, 0).map_err(sfault)?;
                }
                let t = self.host.now_ns();
                self.w64(sa + 8, t).map_err(sfault)?;
            }
        }
        Ok(())
    }

    /// Одна строка с REMAP: `len` — в элементах.
    fn ce_remap_line(&mut self, ch: &Chan, src: CeAddr, dst: CeAddr, len: u64) -> Result<(), Fault> {
        let comp = ch.ce.regs[(ce::SET_REMAP_COMPONENTS / 4) as usize];
        let csize = (((comp >> ce::REMAP_COMPONENT_SIZE_SHIFT) & 3) + 1) as usize;
        let nsrc = (((comp >> ce::REMAP_NUM_SRC_SHIFT) & 3) + 1) as usize;
        let ndst = (((comp >> ce::REMAP_NUM_DST_SHIFT) & 3) + 1) as usize;
        let consts = [ch.ce.regs[(ce::SET_REMAP_CONST_A / 4) as usize], ch.ce.regs[(ce::SET_REMAP_CONST_B / 4) as usize]];
        let sel: Vec<u32> = (0..ndst).map(|j| (comp >> (4 * j)) & 7).collect();
        let needs_src = sel.iter().any(|&s| s < 4);
        let needs_dst = sel.iter().any(|&s| s >= ce::REMAP_NO_WRITE);
        let (spx, dpx) = (csize * nsrc, csize * ndst);
        const CHUNK: u64 = 4096;
        let mut x = 0;
        while x < len {
            let n = (len - x).min(CHUNK) as usize;
            let mut sbuf = vec![0u8; if needs_src { n * spx } else { 0 }];
            if needs_src {
                ce_read(self.vram, self.host, self.pd3, src.add(x * spx as u64), &mut sbuf)?;
            }
            let mut dbuf = vec![0u8; n * dpx];
            if needs_dst {
                ce_read(self.vram, self.host, self.pd3, dst.add(x * dpx as u64), &mut dbuf)?;
            }
            for p in 0..n {
                for (j, &s) in sel.iter().enumerate() {
                    let out = &mut dbuf[p * dpx + j * csize..p * dpx + (j + 1) * csize];
                    match s {
                        0..=3 => {
                            let si = p * spx + s as usize * csize;
                            if (s as usize) < nsrc {
                                out.copy_from_slice(&sbuf[si..si + csize]);
                            }
                        }
                        4 | 5 => out.copy_from_slice(&consts[s as usize - 4].to_le_bytes()[..csize.min(4)]),
                        _ => {}
                    }
                }
            }
            ce_write(self.vram, self.host, self.pd3, dst.add(x * dpx as u64), &dbuf)?;
            self.stats.ce_bytes += (n * dpx) as u64;
            x += n as u64;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Бэкенд

fn check_pages(va: u64, len: u64) -> Result<(), Errno> {
    if va % PAGE != 0 || len % PAGE != 0 || len == 0 || va.checked_add(len).is_none_or(|e| e > gmmu::NVK_VA_LIMIT) {
        return Err(Errno::EINVAL);
    }
    Ok(())
}

fn overlaps(a: u64, alen: u64, b: (u64, u64)) -> bool {
    a < b.0 + b.1 && b.0 < a + alen
}

impl SoftGpu {
    fn free_bo_locked(&self, st: &mut State, bo: u32) {
        if let Some(b) = st.bos.remove(&bo) {
            match b.backing {
                Backing::Vram { addr } => {
                    st.valloc.free(Arena::Bulk, addr, b.size);
                    st.vram.zero(addr, b.size);
                }
                Backing::Sys { frames } => {
                    for f in frames {
                        self.host.free_frame(f);
                    }
                }
            }
        }
    }

    fn alloc_sys_bo(&self, st: &mut State, size: u64) -> Result<u32, Errno> {
        let n = align_up(size, PAGE) / PAGE;
        let mut frames = Vec::with_capacity(n as usize);
        // Крупный буфер (кадр WSI) — одним куском, если хозяин может.
        if n >= 256 {
            if let Some(base) = self.host.alloc_run(n) {
                frames.extend((0..n).map(|i| base + i * PAGE));
            }
        }
        for _ in frames.len() as u64..n {
            match self.host.alloc_frame() {
                Some(f) => frames.push(f),
                None => {
                    for f in frames {
                        self.host.free_frame(f);
                    }
                    return Err(Errno::ENOMEM);
                }
            }
        }
        let id = st.id();
        st.bos.insert(id, Bo { size: n * PAGE, backing: Backing::Sys { frames } });
        Ok(id)
    }

    /// Отобразить служебный системный буфер в область ядра `vm`.
    fn map_kernel_bo(&self, st: &mut State, vm: u32, bo: u32) -> Result<u64, Errno> {
        let State { vram, valloc, vms, bos, .. } = st;
        let v = vms.get_mut(&vm).ok_or(Errno::ENOENT)?;
        let b = bos.get(&bo).ok_or(Errno::ENOENT)?;
        let va = v.kva.alloc(b.size, PAGE).ok_or(Errno::ENOSPC)?;
        let Backing::Sys { frames } = &b.backing else { return Err(Errno::EINVAL) };
        let mut pt = VramPt { vram, valloc };
        v.gmmu.map(&mut pt, va, b.size, PteAttrs::user(Target::SysCoh, 0), |i| frames[i as usize])?;
        Ok(va)
    }

    fn unmap_kernel(&self, st: &mut State, vm: u32, va: u64, size: u64) {
        let State { vram, valloc, vms, .. } = st;
        if let Some(v) = vms.get_mut(&vm) {
            let mut pt = VramPt { vram, valloc };
            let _ = v.gmmu.unmap(&mut pt, va, size);
            v.kva.free(va, size);
        }
    }

    fn destroy_chan_locked(&self, st: &mut State, ch: u32) {
        let Some(c) = st.chans.remove(&ch) else { return };
        self.unmap_kernel(st, c.vm, c.push_va, gpfifo::PUSHBUF_BYTES);
        self.unmap_kernel(st, c.vm, c.fence_va, PAGE);
        self.free_bo_locked(st, c.pushbuf);
        self.free_bo_locked(st, c.fence_bo);
        for a in [c.inst, c.userd] {
            st.valloc.free(Arena::Cpu, a, PAGE);
            st.vram.zero(a, PAGE);
        }
    }

    /// Звонок: исполнить кольцо от GP_GET до GP_PUT.
    fn run(&self, st: &mut State, ch: u32) {
        let State { vram, chans, vms, stats, .. } = st;
        let Some(c) = chans.get_mut(&ch) else { return };
        let Some(v) = vms.get(&c.vm) else { return };
        let mut ex = Exec { vram, host: &*self.host, stats, pd3: v.gmmu.pd3() };
        loop {
            if c.killed.is_some() {
                return;
            }
            let get = ex.vram.r32(c.userd + gpfifo::USERD_GP_GET);
            let put = ex.vram.r32(c.userd + gpfifo::USERD_GP_PUT);
            if get == put {
                return;
            }
            let ring_va = c.push_va + gpfifo::PUSHBUF_RING_OFFSET + Ring::offset(get);
            let res = match ex.r64(ring_va) {
                Err(f) => Err(format!("чтение GPFIFO {:#x}: {}", f.0, f.1)),
                Ok(e) => {
                    let e = gpfifo::gp_entry_decode(e);
                    if e.subroutine {
                        Err(String::from("GPFIFO: подпрограммы не поддержаны"))
                    } else if e.len_bytes == 0 {
                        Ok(())
                    } else {
                        ex.segment(c, e.va, e.len_bytes)
                    }
                }
            };
            if let Err(why) = res {
                ex.stats.kills += 1;
                let reason = format!("chid {} GP_GET {}: {}", c.chid, get, why);
                self.host.log(&format!("softgpu: channel killed - {reason}"));
                c.killed = Some(reason);
                return;
            }
            ex.vram.w32(c.userd + gpfifo::USERD_GP_GET, (get + 1) & gpfifo::IB_MAX);
        }
    }
}

impl GpuBackend for SoftGpu {
    fn info(&self) -> &GpuInfo {
        &self.info
    }

    fn bo_alloc(&self, size: u64, place: Place) -> Result<BoHandle, Errno> {
        if size == 0 {
            return Err(Errno::EINVAL);
        }
        let mut st = self.st.lock();
        let st = &mut *st;
        match place {
            Place::Sysmem => {
                if size > self.info.gart_bytes {
                    return Err(Errno::ENOMEM);
                }
                self.alloc_sys_bo(st, size).map(BoHandle)
            }
            Place::Vram => {
                let size = align_up(size, PAGE);
                let addr = st.valloc.alloc(Arena::Bulk, size, PAGE).ok_or(Errno::ENOMEM)?;
                st.vram.zero(addr, size);
                let id = st.id();
                st.bos.insert(id, Bo { size, backing: Backing::Vram { addr } });
                Ok(BoHandle(id))
            }
        }
    }

    fn bo_free(&self, bo: BoHandle) {
        let mut st = self.st.lock();
        self.free_bo_locked(&mut st, bo.0);
    }

    fn bo_frames(&self, bo: BoHandle) -> Option<Vec<u64>> {
        match &self.st.lock().bos.get(&bo.0)?.backing {
            Backing::Sys { frames } => Some(frames.clone()),
            Backing::Vram { .. } => None,
        }
    }

    fn vm_create(&self, kernel_va: (u64, u64)) -> Result<VmHandle, Errno> {
        let (ka, ks) = kernel_va;
        check_pages(ka, ks)?;
        let mut st = self.st.lock();
        let st = &mut *st;
        let mut pt = VramPt { vram: &mut st.vram, valloc: &mut st.valloc };
        let mut g = Gmmu::new(&mut pt)?;
        let mut kva = RangeAlloc::new(PAGE);
        kva.add_range(ka, ks);
        // Как настоящий бэкенд: первые 512 МиБ области ядра — RM (раздел
        // 2.2 п.5), с готовыми PD3 -> PD2 (-> PD1).
        if ks > RM_RESERVED_BYTES && ka % RM_RESERVED_BYTES == 0 {
            kva.alloc_at(ka, RM_RESERVED_BYTES);
            if let Err(e) = g.ensure_path(&mut pt, ka, gmmu::PD1) {
                g.destroy(&mut pt);
                return Err(e);
            }
        }
        let id = st.id();
        st.vms.insert(id, Vm { gmmu: g, kernel: kernel_va, kva });
        Ok(VmHandle(id))
    }

    fn vm_destroy(&self, vm: VmHandle) {
        let mut st = self.st.lock();
        let st = &mut *st;
        let chans: Vec<u32> = st.chans.iter().filter(|(_, c)| c.vm == vm.0).map(|(&k, _)| k).collect();
        for c in chans {
            self.destroy_chan_locked(st, c);
        }
        if let Some(v) = st.vms.remove(&vm.0) {
            let mut pt = VramPt { vram: &mut st.vram, valloc: &mut st.valloc };
            v.gmmu.destroy(&mut pt);
        }
    }

    fn vm_map(&self, vm: VmHandle, va: u64, bo: BoHandle, bo_off: u64, len: u64, kind: u8) -> Result<(), Errno> {
        check_pages(va, len)?;
        if bo_off % PAGE != 0 {
            return Err(Errno::EINVAL);
        }
        let kind = gmmu::pte_kind(kind)?;
        let mut st = self.st.lock();
        let State { vram, valloc, vms, bos, .. } = &mut *st;
        let v = vms.get_mut(&vm.0).ok_or(Errno::ENOENT)?;
        if overlaps(va, len, v.kernel) {
            return Err(Errno::EINVAL);
        }
        let b = bos.get(&bo.0).ok_or(Errno::ENOENT)?;
        if bo_off.checked_add(len).is_none_or(|e| e > b.size) {
            return Err(Errno::EINVAL);
        }
        let mut pt = VramPt { vram, valloc };
        match &b.backing {
            Backing::Vram { addr } => v.gmmu.map_contig(&mut pt, va, len, addr + bo_off, PteAttrs::user(Target::Vram, kind)),
            Backing::Sys { frames } => {
                let first = (bo_off / PAGE) as usize;
                v.gmmu.map(&mut pt, va, len, PteAttrs::user(Target::SysCoh, kind), |i| frames[first + i as usize])
            }
        }
    }

    fn vm_sparse(&self, vm: VmHandle, va: u64, len: u64, on: bool) -> Result<(), Errno> {
        check_pages(va, len)?;
        let mut st = self.st.lock();
        let State { vram, valloc, vms, .. } = &mut *st;
        let v = vms.get_mut(&vm.0).ok_or(Errno::ENOENT)?;
        if overlaps(va, len, v.kernel) {
            return Err(Errno::EINVAL);
        }
        let mut pt = VramPt { vram, valloc };
        v.gmmu.fill(&mut pt, va, len, on)
    }

    fn vm_unmap(&self, vm: VmHandle, va: u64, len: u64, restore_sparse: bool) -> Result<(), Errno> {
        self.vm_sparse(vm, va, len, restore_sparse)
    }

    fn vm_flush(&self, _vm: VmHandle) {
        // TLB у SoftGpu нет: каждый доступ — свежий обход таблиц.
    }

    fn chan_create(&self, vm: VmHandle) -> Result<ChanHandle, Errno> {
        let mut st = self.st.lock();
        let st = &mut *st;
        let pd3 = st.vms.get(&vm.0).ok_or(Errno::ENOENT)?.gmmu.pd3();
        let inst = st.valloc.alloc(Arena::Cpu, PAGE, PAGE).ok_or(Errno::ENOMEM)?;
        let Some(userd) = st.valloc.alloc(Arena::Cpu, PAGE, PAGE) else {
            st.valloc.free(Arena::Cpu, inst, PAGE);
            return Err(Errno::ENOMEM);
        };
        st.vram.zero(inst, PAGE);
        st.vram.zero(userd, PAGE);
        {
            let vram = &mut st.vram;
            gmmu::inst_block_join(|o, v| vram.w32(inst + o as u64, v), pd3, Target::Vram, gmmu::NVK_VA_LIMIT);
        }
        let mut cleanup: Vec<u32> = Vec::new();
        let r = (|| -> Result<(u32, u64, u32, u64), Errno> {
            let pushbuf = self.alloc_sys_bo(st, gpfifo::PUSHBUF_BYTES)?;
            cleanup.push(pushbuf);
            let fence_bo = self.alloc_sys_bo(st, PAGE)?;
            cleanup.push(fence_bo);
            let push_va = self.map_kernel_bo(st, vm.0, pushbuf)?;
            let fence_va = match self.map_kernel_bo(st, vm.0, fence_bo) {
                Ok(v) => v,
                Err(e) => {
                    self.unmap_kernel(st, vm.0, push_va, gpfifo::PUSHBUF_BYTES);
                    return Err(e);
                }
            };
            Ok((pushbuf, push_va, fence_bo, fence_va))
        })();
        let (pushbuf, push_va, fence_bo, fence_va) = match r {
            Ok(x) => x,
            Err(e) => {
                for b in cleanup {
                    self.free_bo_locked(st, b);
                }
                for a in [inst, userd] {
                    st.valloc.free(Arena::Cpu, a, PAGE);
                }
                return Err(e);
            }
        };
        let id = st.id();
        // chid как у настоящей карты: 8 на страницу USERD.
        let chid = ((userd - CPU_ARENA.0) / PAGE) as u32 * 8;
        st.chans.insert(
            id,
            Chan {
                vm: vm.0,
                chid,
                inst,
                userd,
                pushbuf,
                push_va,
                fence_bo,
                fence_va,
                seq: 0,
                ring: Ring::default(),
                kpos: 0,
                killed: None,
                objs: BTreeMap::new(),
                subch: [0; 8],
                sem_a: 0,
                sem_b: 0,
                sem_c: 0,
                sem_addr: 0,
                sem_payload: 0,
                ce: CeState { regs: Box::new([0; 0x1000]) },
                eng: BTreeMap::new(),
            },
        );
        Ok(ChanHandle(id))
    }

    fn chan_obj(&self, ch: ChanHandle, class: u32) -> Result<ObjHandle, Errno> {
        if !self.info.classes.contains(&class) {
            return Err(Errno::EINVAL);
        }
        let mut st = self.st.lock();
        let id = st.id();
        let c = st.chans.get_mut(&ch.0).ok_or(Errno::ENOENT)?;
        c.objs.insert(id, class);
        Ok(ObjHandle(id))
    }

    fn chan_obj_free(&self, ch: ChanHandle, o: ObjHandle) {
        if let Some(c) = self.st.lock().chans.get_mut(&ch.0) {
            if let Some(class) = c.objs.remove(&o.0) {
                if !c.objs.values().any(|&k| k == class) {
                    for s in c.subch.iter_mut().filter(|s| **s == class) {
                        *s = 0;
                    }
                }
            }
        }
    }

    fn chan_destroy(&self, ch: ChanHandle) {
        let mut st = self.st.lock();
        self.destroy_chan_locked(&mut st, ch.0);
    }

    fn chan_submit(&self, ch: ChanHandle, pushes: &[Push]) -> Result<u64, Errno> {
        for p in pushes {
            if p.va % 4 != 0 || p.len % 4 != 0 || p.len >= crate::uapi::NOUVEAU_EXEC_PUSH_MAX_LEN || p.flags & !Push::NO_PREFETCH != 0 {
                return Err(Errno::EINVAL);
            }
        }
        let mut st = self.st.lock();
        let st = &mut *st;
        let c = st.chans.get_mut(&ch.0).ok_or(Errno::ENOENT)?;
        if c.killed.is_some() {
            return Err(Errno::ENODEV);
        }
        let get = st.vram.r32(c.userd + gpfifo::USERD_GP_GET);
        if (c.ring.free(get) as usize) < pushes.len() + 1 {
            return Err(Errno::EBUSY);
        }
        let frames = match &st.bos.get(&c.pushbuf).ok_or(Errno::EIO)?.backing {
            Backing::Sys { frames } => frames.clone(),
            Backing::Vram { .. } => return Err(Errno::EIO),
        };
        let write = |off: u64, data: &[u8]| {
            // Записи по 8 и сегмент забора не пересекают страницу (см. ниже).
            self.host.write_phys(frames[(off / PAGE) as usize] + off % PAGE, data);
        };
        let put_entry = |c: &mut Chan, e: u64| {
            let i = c.ring.advance();
            write(gpfifo::PUSHBUF_RING_OFFSET + Ring::offset(i), &e.to_le_bytes());
        };
        for p in pushes {
            put_entry(c, gpfifo::gp_entry(p.va, p.len, p.flags & Push::NO_PREFETCH != 0));
        }
        // Сегмент забора в области ядра; 24 Б, не пересекает страницу.
        c.seq += 1;
        let words = gpfifo::fence_release(c.fence_va, c.seq as u32);
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        if c.kpos % PAGE + bytes.len() as u64 > PAGE {
            c.kpos = align_up(c.kpos, PAGE);
        }
        if c.kpos + bytes.len() as u64 > gpfifo::PUSHBUF_KERNEL_AREA {
            c.kpos = 0;
        }
        write(c.kpos, &bytes);
        let seg_va = c.push_va + c.kpos;
        c.kpos += bytes.len() as u64;
        put_entry(c, gpfifo::gp_entry(seg_va, bytes.len() as u32, false));
        let seq = c.seq;
        let put = c.ring.put;
        st.vram.w32(c.userd + gpfifo::USERD_GP_PUT, put);
        self.run(st, ch.0);
        Ok(seq)
    }

    fn chan_seq_done(&self, ch: ChanHandle) -> Result<u64, Errno> {
        let st = self.st.lock();
        let c = st.chans.get(&ch.0).ok_or(Errno::ENOENT)?;
        if c.killed.is_some() {
            return Err(Errno::ENODEV);
        }
        let pd3 = st.vms.get(&c.vm).ok_or(Errno::ENOENT)?.gmmu.pd3();
        let mut b = [0; 4];
        gpu_read(&st.vram, &*self.host, pd3, c.fence_va, &mut b).map_err(|_| Errno::EIO)?;
        Ok(gpfifo::widen_seq(c.seq, u32::from_le_bytes(b)))
    }

    fn time_ns(&self) -> u64 {
        self.host.now_ns()
    }

    fn vram_used(&self) -> u64 {
        self.st.lock().valloc.used()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::class;
    use crate::gpfifo::PushBuilder;

    const KVA: (u64, u64) = (1 << 39, (1 << 40) - (1 << 39));

    fn setup() -> (Arc<HeapHost>, SoftGpu, VmHandle, ChanHandle) {
        let host = Arc::new(HeapHost::new());
        let gpu = SoftGpu::new(host.clone());
        let vm = gpu.vm_create(KVA).unwrap();
        let ch = gpu.chan_create(vm).unwrap();
        (host, gpu, vm, ch)
    }

    /// Положить пушбуфер в новый системный буфер по `va` и отправить.
    fn submit(gpu: &SoftGpu, vm: VmHandle, ch: ChanHandle, va: u64, p: &PushBuilder) -> Result<u64, Errno> {
        let bo = gpu.bo_alloc(align_up(p.len_bytes() as u64, PAGE), Place::Sysmem).unwrap();
        gpu.bo_write(bo, 0, &p.bytes()).unwrap();
        gpu.vm_map(vm, va, bo, 0, align_up(p.len_bytes() as u64, PAGE), 0).unwrap();
        gpu.chan_submit(ch, &[Push { va, len: p.len_bytes(), flags: 0 }])
    }

    #[test]
    fn empty_submit_releases_fence() {
        let (_h, gpu, _vm, ch) = setup();
        assert_eq!(gpu.chan_seq_done(ch), Ok(0));
        assert_eq!(gpu.chan_submit(ch, &[]), Ok(1));
        assert_eq!(gpu.chan_seq_done(ch), Ok(1));
        assert_eq!(gpu.chan_submit(ch, &[]), Ok(2));
        assert_eq!(gpu.chan_seq_done(ch), Ok(2));
        assert!(gpu.vram_used() > 0);
        assert_eq!(gpu.stats().sem_releases, 2);
    }

    #[test]
    fn ce_copy_sysmem_vram_sysmem() {
        let (host, gpu, vm, ch) = setup();
        let n = 3 * PAGE + 0x100;
        let size = align_up(n, PAGE);
        let src = gpu.bo_alloc(size, Place::Sysmem).unwrap();
        let mid = gpu.bo_alloc(size, Place::Vram).unwrap();
        let dst = gpu.bo_alloc(size, Place::Sysmem).unwrap();
        let pat: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
        // Запись «процессом» прямо в кадры буфера (как через mmap).
        let frames = gpu.bo_frames(src).unwrap();
        assert_eq!(frames.len(), 4);
        for (i, f) in frames.iter().enumerate() {
            let s = i * 4096;
            let e = (s + 4096).min(n as usize);
            host.write_phys(*f, &pat[s..e]);
        }
        assert!(gpu.bo_frames(mid).is_none());
        gpu.vm_map(vm, 0x10_0000, src, 0, size, 0).unwrap();
        gpu.vm_map(vm, 0x20_0000, mid, 0, size, 6).unwrap();
        gpu.vm_map(vm, 0x30_0000, dst, 0, size, 0).unwrap();
        gpu.vm_flush(vm);

        let obj = gpu.chan_obj(ch, class::AMPERE_DMA_COPY_B).unwrap();
        let sem = gpu.bo_alloc(PAGE, Place::Sysmem).unwrap();
        gpu.vm_map(vm, 0x40_0000, sem, 0, PAGE, 0).unwrap();
        let mut p = PushBuilder::new();
        p.set_object(4, class::AMPERE_DMA_COPY_B)
            .ce_copy(4, 0x10_0000, 0x20_0000, n as u32, None)
            .ce_copy(4, 0x20_0000, 0x30_0000, n as u32, Some((0x40_0000, 0xcafe)));
        let seq = submit(&gpu, vm, ch, 0x1000_0000, &p).unwrap();
        assert_eq!(gpu.chan_killed(ch), None);
        assert_eq!(gpu.chan_seq_done(ch), Ok(seq));
        let mut out = vec![0u8; n as usize];
        gpu.bo_read(dst, 0, &mut out).unwrap();
        assert_eq!(out, pat);
        let mut s = [0u8; 4];
        gpu.bo_read(sem, 0, &mut s).unwrap();
        assert_eq!(u32::from_le_bytes(s), 0xcafe);
        assert_eq!(gpu.stats().ce_launches, 2);
        assert_eq!(gpu.stats().ce_bytes, 2 * n);
        gpu.chan_obj_free(ch, obj);
    }

    #[test]
    fn ce_fill_and_multiline() {
        let (_h, gpu, vm, ch) = setup();
        let buf = gpu.bo_alloc(2 * PAGE, Place::Sysmem).unwrap();
        gpu.vm_map(vm, 0x5_0000, buf, 0, 2 * PAGE, 0).unwrap();
        gpu.chan_obj(ch, class::AMPERE_DMA_COPY_B).unwrap();
        let mut p = PushBuilder::new();
        p.set_object(4, class::AMPERE_DMA_COPY_B).ce_fill(4, 0x5_0000, 0x100, 0xdead_beef);
        // Многострочное: 4 строки по 8 байт из [0x5_0000 + 16*i] в [0x5_1000 + 32*i].
        p.inc(4, ce::OFFSET_IN_UPPER, &[0, 0x5_0000, 0, 0x5_1000]);
        p.inc(4, ce::PITCH_IN, &[16, 32, 8, 4]);
        p.inc(4, ce::LAUNCH_DMA, &[ce::DMA_TRANSFER_NON_PIPELINED | ce::DMA_SRC_PITCH | ce::DMA_DST_PITCH | ce::DMA_MULTI_LINE]);
        submit(&gpu, vm, ch, 0x100_0000, &p).unwrap();
        assert_eq!(gpu.chan_killed(ch), None);
        let mut b = vec![0u8; 0x104];
        gpu.bo_read(buf, 0, &mut b).unwrap();
        assert!(b[..0x100].chunks(4).all(|c| c == 0xdead_beefu32.to_le_bytes()));
        assert_eq!(&b[0x100..], &[0, 0, 0, 0]);
        let mut m = vec![0u8; 128];
        gpu.bo_read(buf, 0x1000, &mut m).unwrap();
        for i in 0..4 {
            assert!(m[i * 32..i * 32 + 8].chunks(4).all(|c| c == 0xdead_beefu32.to_le_bytes()), "строка {i}");
            assert!(m[i * 32 + 8..i * 32 + 32].iter().all(|&x| x == 0));
        }
    }

    #[test]
    fn host_semaphores() {
        let (_h, gpu, vm, ch) = setup();
        let sem = gpu.bo_alloc(PAGE, Place::Sysmem).unwrap();
        gpu.vm_map(vm, 0x8000, sem, 0, PAGE, 0).unwrap();
        let mut p = PushBuilder::new();
        p.host_release(0x8000, 41);
        // SEM_EXECUTE: 64-битная редукция ADD на 0x8010 и выпуск с временем.
        p.inc(0, host::SEM_ADDR_LO, &[0x8010, 0, 5, 0, host::SEMX_OP_REDUCTION | host::SEMX_PAYLOAD_64BIT | (5 << host::SEMX_REDUCTION_SHIFT) | (1 << 31)]);
        p.inc(0, host::SEM_ADDR_LO, &[0x8010, 0, 5, 0, host::SEMX_OP_REDUCTION | host::SEMX_PAYLOAD_64BIT | (5 << host::SEMX_REDUCTION_SHIFT) | (1 << 31)]);
        // Захват, который выполнен.
        p.inc(0, host::SEMAPHOREA, &[0, 0x8000, 41, host::SEMD_OP_ACQUIRE]);
        submit(&gpu, vm, ch, 0x10_0000, &p).unwrap();
        assert_eq!(gpu.chan_killed(ch), None);
        let mut b = [0u8; 0x18];
        gpu.bo_read(sem, 0, &mut b).unwrap();
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 41);
        assert_eq!(u64::from_le_bytes(b[0x10..0x18].try_into().unwrap()), 10);
        // Невыполнимый захват — канал убит, ENODEV.
        let mut p = PushBuilder::new();
        p.inc(0, host::SEMAPHOREA, &[0, 0x8000, 42, host::SEMD_OP_ACQUIRE]);
        submit(&gpu, vm, ch, 0x20_0000, &p).unwrap();
        assert!(gpu.chan_killed(ch).unwrap().contains("захват"));
        assert_eq!(gpu.chan_seq_done(ch), Err(Errno::ENODEV));
        assert_eq!(gpu.chan_submit(ch, &[]), Err(Errno::ENODEV));
    }

    #[test]
    fn inline_data_and_report_semaphore() {
        let (_h, gpu, vm, ch) = setup();
        let buf = gpu.bo_alloc(PAGE, Place::Sysmem).unwrap();
        gpu.vm_map(vm, 0x9000, buf, 0, PAGE, 0).unwrap();
        gpu.chan_obj(ch, class::AMPERE_COMPUTE_B).unwrap();
        gpu.chan_obj(ch, class::AMPERE_B).unwrap();
        let mut p = PushBuilder::new();
        p.set_object(1, class::AMPERE_COMPUTE_B).set_object(0, class::AMPERE_B);
        // Две строки по 6 байт с шагом 0x10 — как NVK: 1INC LAUNCH_DMA + данные.
        p.inc(1, eng::LINE_LENGTH_IN, &[6, 2, 0, 0x9100, 0x10]);
        p.words.push(gpfifo::mthd_1inc(1, eng::LAUNCH_DMA, 4));
        p.words.extend_from_slice(&[1, 0x0403_0201, 0x0807_0605, 0x0c0b_0a09]);
        // Неизвестные методы 3D пропускаются.
        p.inc(0, 0x1234 * 4 & 0x3ffc, &[1, 2, 3]);
        // Семафор отчёта: один 32-битный и 16-байтный с временем.
        p.inc(0, eng::SET_REPORT_SEMAPHORE_A, &[0, 0x9000, 77, eng::REPORT_ONE_WORD]);
        p.inc(1, eng::SET_REPORT_SEMAPHORE_A, &[0, 0x9010, 78, 0]);
        submit(&gpu, vm, ch, 0x10_0000, &p).unwrap();
        assert_eq!(gpu.chan_killed(ch), None);
        let mut b = vec![0u8; 0x120];
        gpu.bo_read(buf, 0, &mut b).unwrap();
        assert_eq!(&b[0x100..0x106], &[1, 2, 3, 4, 5, 6]);
        assert_eq!(&b[0x106..0x110], &[0; 10]);
        assert_eq!(&b[0x110..0x116], &[7, 8, 9, 10, 11, 12]);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 77);
        assert_eq!(u64::from_le_bytes(b[0x10..0x18].try_into().unwrap()), 78);
        assert_ne!(u64::from_le_bytes(b[0x18..0x20].try_into().unwrap()), 0, "время");
        assert_eq!(gpu.stats().ignored_methods, 3);
    }

    #[test]
    fn faults_kill_channel() {
        // Пуш по неотображённому адресу.
        let (_h, gpu, _vm, ch) = setup();
        gpu.chan_submit(ch, &[Push { va: 0x7000_0000, len: 16, flags: 0 }]).unwrap();
        assert!(gpu.chan_killed(ch).unwrap().contains("нет отображения"));
        assert_eq!(gpu.chan_seq_done(ch), Err(Errno::ENODEV));
        // Метод движка без SET_OBJECT.
        let (_h, gpu, vm, ch) = setup();
        let mut p = PushBuilder::new();
        p.inc(1, 0x300, &[0]);
        submit(&gpu, vm, ch, 0x10_0000, &p).unwrap();
        assert!(gpu.chan_killed(ch).unwrap().contains("без объекта"));
        // SET_OBJECT класса, которого нет в канале.
        let (_h, gpu, vm, ch) = setup();
        let mut p = PushBuilder::new();
        p.set_object(0, class::AMPERE_B);
        submit(&gpu, vm, ch, 0x10_0000, &p).unwrap();
        assert!(gpu.chan_killed(ch).is_some());
        // Копирование в неотображённое.
        let (_h, gpu, vm, ch) = setup();
        gpu.chan_obj(ch, class::AMPERE_DMA_COPY_B).unwrap();
        let mut p = PushBuilder::new();
        p.set_object(4, class::AMPERE_DMA_COPY_B).ce_copy(4, 0x10_0000, 0x9999_0000, 4, None);
        submit(&gpu, vm, ch, 0x10_0000, &p).unwrap();
        assert!(gpu.chan_killed(ch).unwrap().contains("CE"));
    }

    #[test]
    fn sparse_reads_zero_writes_dropped() {
        let (_h, gpu, vm, ch) = setup();
        gpu.vm_sparse(vm, 0x4000_0000, 4 << 20, true).unwrap();
        assert_eq!(gpu.vm_translate(vm, 0x4010_0000), Some(Walk::Sparse));
        let src = gpu.bo_alloc(PAGE, Place::Sysmem).unwrap();
        gpu.bo_write(src, 0, &[0xaa; 64]).unwrap();
        gpu.vm_map(vm, 0x10_0000, src, 0, PAGE, 0).unwrap();
        gpu.chan_obj(ch, class::AMPERE_DMA_COPY_A).unwrap();
        let mut p = PushBuilder::new();
        // Запись в разреженное теряется, чтение из разреженного — нули.
        p.set_object(2, class::AMPERE_DMA_COPY_A).ce_copy(2, 0x10_0000, 0x4000_0000, 64, None).ce_copy(2, 0x4000_0000, 0x10_0000, 32, None);
        submit(&gpu, vm, ch, 0x20_0000, &p).unwrap();
        assert_eq!(gpu.chan_killed(ch), None);
        let mut b = [0u8; 64];
        gpu.bo_read(src, 0, &mut b).unwrap();
        assert_eq!(&b[..32], &[0; 32]);
        assert_eq!(&b[32..], &[0xaa; 32]);
        // Снять с восстановлением разреженности / снять разреженность.
        gpu.vm_unmap(vm, 0x4000_0000, PAGE, false).unwrap();
        assert_eq!(gpu.vm_translate(vm, 0x4000_0000), Some(Walk::Unmapped));
        assert_eq!(gpu.vm_translate(vm, 0x4000_1000), Some(Walk::Sparse));
    }

    #[test]
    fn contract_errors() {
        let (_h, gpu, vm, ch) = setup();
        let bo = gpu.bo_alloc(PAGE, Place::Vram).unwrap();
        // Невыровненное, за концом буфера, в области ядра, плохой kind.
        assert_eq!(gpu.vm_map(vm, 0x1800, bo, 0, PAGE, 0), Err(Errno::EINVAL));
        assert_eq!(gpu.vm_map(vm, 0x1000, bo, 0, 2 * PAGE, 0), Err(Errno::EINVAL));
        assert_eq!(gpu.vm_map(vm, 1 << 39, bo, 0, PAGE, 0), Err(Errno::EINVAL));
        assert_eq!(gpu.vm_map(vm, 0x1000, bo, 0, PAGE, 7), Err(Errno::EINVAL));
        assert_eq!(gpu.vm_map(vm, 0x1000, BoHandle(999), 0, PAGE, 0), Err(Errno::ENOENT));
        assert_eq!(gpu.chan_obj(ch, 0x1234), Err(Errno::EINVAL));
        assert_eq!(gpu.chan_submit(ch, &[Push { va: 2, len: 4, flags: 0 }]), Err(Errno::EINVAL));
        assert_eq!(gpu.bo_alloc(0, Place::Sysmem), Err(Errno::EINVAL));
        assert_eq!(gpu.vm_create((0x1001, PAGE)), Err(Errno::EINVAL));
    }

    #[test]
    fn kernel_region_layout() {
        let (_h, gpu, vm, ch) = setup();
        // Пушбуфер канала — сразу за 512 МиБ RM в области ядра, через SYS_COH.
        let w = gpu.vm_translate(vm, (1 << 39) + RM_RESERVED_BYTES).unwrap();
        assert!(matches!(w, Walk::Mapped { attrs: PteAttrs { target: Target::SysCoh, vol: true, .. }, .. }), "{w:?}");
        assert_eq!(gpu.vm_translate(vm, 1 << 39), Some(Walk::Unmapped));
        // Кольцо через сотни отправок оборачивается без ошибок.
        for i in 1..=2100u64 {
            assert_eq!(gpu.chan_submit(ch, &[]), Ok(i));
        }
        assert_eq!(gpu.chan_seq_done(ch), Ok(2100));
    }

    #[test]
    fn teardown_frees_everything() {
        let host = Arc::new(HeapHost::new());
        let gpu = SoftGpu::new(host.clone());
        let base = gpu.vram_used();
        let vm = gpu.vm_create(KVA).unwrap();
        let ch = gpu.chan_create(vm).unwrap();
        let bo = gpu.bo_alloc(5 * PAGE, Place::Sysmem).unwrap();
        let vbo = gpu.bo_alloc(1 << 20, Place::Vram).unwrap();
        gpu.vm_map(vm, 0x10_0000, bo, 0, 5 * PAGE, 0).unwrap();
        gpu.vm_map(vm, 0x20_0000, vbo, 0, 1 << 20, 0).unwrap();
        gpu.chan_submit(ch, &[]).unwrap();
        gpu.vm_unmap(vm, 0x10_0000, 5 * PAGE, false).unwrap();
        gpu.vm_unmap(vm, 0x20_0000, 1 << 20, false).unwrap();
        gpu.bo_free(bo);
        gpu.bo_free(vbo);
        gpu.chan_destroy(ch);
        gpu.vm_destroy(vm);
        assert_eq!(host.live_frames(), 0);
        assert_eq!(gpu.vram_used(), base);
    }

    #[test]
    fn is_send_sync_dyn() {
        fn takes(_: Arc<dyn GpuBackend>) {}
        let host = Arc::new(HeapHost::new());
        takes(Arc::new(SoftGpu::new(host)));
    }
}
