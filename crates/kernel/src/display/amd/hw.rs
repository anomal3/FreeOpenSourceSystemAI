//! Бэкенд DRM AMD на железе: пользовательское адресное пространство GPU
//! (VMID 1) и отправка буферов команд кольцом GFX, которое поднял `amd ring`.
//!
//! Буферы DRM-слоя (`linux/drm/amdgpu.rs`) - кадры системной памяти, поэтому
//! GPU ходит к ним через PTE с битом SYSTEM (как в `amd cs`, где такая страница
//! проверена на железе). Таблицы страниц - в видеопамяти, в окне BAR0 за
//! кольцом: PD на 64 ГиБ VA (глубина 1, блок 10 бит: одна запись PD - 4 МиБ) и
//! пул таблиц PT по 8 КиБ, которые выдаются по мере надобности. VMID 1 один,
//! поэтому одновременно в железе стоит одно адресное пространство: DRM-слой
//! перестраивает его (`clear` + `map`), когда его меняет другой файл.
//!
//! Отправка синхронная: `INDIRECT_BUFFER` с VMID 1 и за ним `EVENT_WRITE_EOP`
//! с номером в слово видеопамяти; `submit` ждёт этот номер.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{
    system_page, Window, MM_MC_VM_FB_LOCATION, MM_GRBM_STATUS, MM_VM_CONTEXT1_CNTL, MM_VM_CONTEXT1_CNTL2,
    MM_VM_CONTEXT1_PAGE_TABLE_BASE_ADDR, MM_VM_CONTEXT1_PAGE_TABLE_END_ADDR, MM_VM_CONTEXT1_PAGE_TABLE_START_ADDR,
    MM_VM_CONTEXT1_PROTECTION_FAULT_ADDR, MM_VM_CONTEXT1_PROTECTION_FAULT_DEFAULT_ADDR,
    MM_VM_CONTEXT1_PROTECTION_FAULT_MCCLIENT, MM_VM_CONTEXT1_PROTECTION_FAULT_STATUS, MM_VM_INVALIDATE_REQUEST,
    EOP_CONTROL, EOP_DATA_SEL_LOW32, PM4_EVENT_WRITE_EOP_4, PM4_INDIRECT_BUFFER_2, POLARIS_BAR0, POLARIS_MMIO,
    PTE_READABLE, PTE_SNOOPED, PTE_SYSTEM, PTE_VALID, PTE_WRITEABLE, PTE_EXECUTABLE, RING_STATE, UCODE_AREA,
};
use crate::sync::SpinLock;

/// Область таблиц в окне VRAM: за областью кольца (UCODE_AREA + 2 МиБ + 256 КиБ).
const HW_AREA: u64 = UCODE_AREA + 4 * 1024 * 1024;
/// Размер адресного пространства: 2^24 страниц = 64 ГиБ (как `vm_size` Linux).
pub const VM_PAGES: u64 = 1 << 24;
/// Страниц на запись PD (блок 10 бит).
const PAGES_PER_PDE: u64 = 1024;
const PD_BYTES: usize = (VM_PAGES / PAGES_PER_PDE) as usize * 8;
const PT_BYTES: usize = 8192;
const PT_COUNT: usize = 1024;
const FENCE_OFFSET: usize = PD_BYTES + PT_COUNT * PT_BYTES;
/// Всё окно: PD, пул PT, страница под слово fence.
const HW_AREA_LEN: usize = FENCE_OFFSET + 4096;
const PTE_ADDRESS: u64 = 0x0000_ffff_ffff_f000;
/// Страница видеопамяти: чтение, запись, исполнение; без SYSTEM и SNOOPED.
const PTE_VRAM_FLAGS: u64 = PTE_VALID | PTE_READABLE | PTE_WRITEABLE | PTE_EXECUTABLE;
const PTE_SYSTEM_FLAGS: u64 = PTE_VALID | PTE_SYSTEM | PTE_SNOOPED | PTE_READABLE | PTE_WRITEABLE | PTE_EXECUTABLE;

struct Hw {
    /// Виртуальный адрес начала области таблиц.
    area: usize,
    /// GPU-адрес начала видеопамяти.
    vram_start: u64,
    /// Следующая свободная таблица PT.
    pt_next: usize,
    /// Номер PT для записи PD (`PDE -> индекс в пуле`), `u16::MAX` - нет.
    pt_of: Vec<u16>,
    /// Номер последней отправки.
    seq: u32,
}

static HW: SpinLock<Option<Hw>> = SpinLock::new(None);

/// Пул видеопамяти для буферов GPU (VRAM-домен DRM). Берётся невидимая процессору часть: за 256 МиБ окна BAR0,
/// где лежат микрокод, кольцо и таблицы страниц, и до верха минус запас. Хранит свободные куски
/// `(смещение от начала VRAM, длина)` по возрастанию смещения.
struct VramPool {
    free: Vec<(u64, u64)>,
    total: u64,
    used: u64,
}

static VRAM: SpinLock<Option<VramPool>> = SpinLock::new(None);
/// Начало пула: за видимым окном BAR0 (256 МиБ).
const VRAM_POOL_START: u64 = 256 << 20;
/// Запас сверху: служебные области VBIOS/TMR.
const VRAM_TOP_RESERVE: u64 = 64 << 20;
/// Выравнивание выдачи: 64 КиБ (фрагмент PTE крупнее страницы выгоднее для TLB).
const VRAM_ALIGN: u64 = 64 << 10;

/// Выдать `bytes` видеопамяти; смещение от начала VRAM (в PTE - без `vram_start`).
pub fn vram_alloc(bytes: u64) -> Option<u64> {
    let bytes = bytes.checked_next_multiple_of(4096)?;
    let mut guard = VRAM.lock();
    let pool = guard.as_mut()?;
    let align = if bytes >= VRAM_ALIGN { VRAM_ALIGN } else { 4096 };
    for index in 0..pool.free.len() {
        let (start, len) = pool.free[index];
        let at = start.next_multiple_of(align);
        let lost = at - start;
        if len < lost + bytes {
            continue;
        }
        // Вырезать [at, at + bytes) из куска.
        let tail = len - lost - bytes;
        match (lost > 0, tail > 0) {
            (false, false) => {
                pool.free.remove(index);
            }
            (false, true) => pool.free[index] = (at + bytes, tail),
            (true, false) => pool.free[index] = (start, lost),
            (true, true) => {
                pool.free[index] = (start, lost);
                pool.free.insert(index + 1, (at + bytes, tail));
            }
        }
        pool.used += bytes;
        return Some(at);
    }
    None
}

/// Вернуть видеопамять, выданную `vram_alloc`.
pub fn vram_free(offset: u64, bytes: u64) {
    let bytes = bytes.next_multiple_of(4096);
    let mut guard = VRAM.lock();
    let Some(pool) = guard.as_mut() else { return };
    pool.used = pool.used.saturating_sub(bytes);
    let at = pool.free.partition_point(|&(start, _)| start < offset);
    pool.free.insert(at, (offset, bytes));
    // Слить с соседями.
    if at + 1 < pool.free.len() && pool.free[at].0 + pool.free[at].1 == pool.free[at + 1].0 {
        pool.free[at].1 += pool.free[at + 1].1;
        pool.free.remove(at + 1);
    }
    if at > 0 && pool.free[at - 1].0 + pool.free[at - 1].1 == pool.free[at].0 {
        pool.free[at - 1].1 += pool.free[at].1;
        pool.free.remove(at);
    }
}

/// `(занято, всего)` в пуле видеопамяти; нули, пока бэкенд не поднят.
#[must_use]
pub fn vram_usage() -> (u64, u64) {
    VRAM.lock().as_ref().map_or((0, 0), |pool| (pool.used, pool.total))
}

impl Hw {
    fn put32(&self, offset: usize, value: u32) {
        // SAFETY: слово внутри отображённой области (`HW_AREA_LEN`), выровнено.
        unsafe { core::ptr::write_volatile((self.area + offset) as *mut u32, value) };
    }

    fn put64(&self, offset: usize, value: u64) {
        self.put32(offset, value as u32);
        self.put32(offset + 4, (value >> 32) as u32);
    }

    fn zero(&self, offset: usize, bytes: usize) {
        for at in (offset..offset + bytes).step_by(4) {
            self.put32(at, 0);
        }
    }

    /// GPU-адрес смещения области.
    fn mc(&self, offset: usize) -> u64 {
        self.vram_start + HW_AREA + offset as u64
    }
}

/// Бэкенд поднят и адресное пространство GPU включено.
#[must_use]
pub fn ready() -> bool {
    HW.lock().is_some()
}

fn window() -> Option<Window> {
    POLARIS_MMIO.lock().map(|(base, _, _)| Window(base))
}

/// Включить контекст 1 (VMID 1) на 64 ГиБ и подготовить таблицы. Требует работающего кольца (`amd ring`).
#[must_use]
pub fn start() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : hw: {text}");
        lines.push(text);
    };
    if ready() {
        note(String::from("already started"));
        return lines;
    }
    let (Some(window), Some(bar0)) = (window(), *POLARIS_BAR0.lock()) else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let Some(vram_start) = RING_STATE.lock().as_ref().map(|state| state.vram_start) else {
        note(String::from("the ring is not running: run 'amd ring' first"));
        return lines;
    };
    let phys = crate::mm::PhysAddr::new(bar0 + HW_AREA);
    let virt = phys.to_direct_map();
    // SAFETY: окно видеопамяти (BAR0) устройства; ядро по нему не исполняется.
    if unsafe {
        crate::arch::map_active(
            virt,
            phys,
            HW_AREA_LEN,
            crate::mm::PageFlags::READ | crate::mm::PageFlags::WRITE | crate::mm::PageFlags::DEVICE,
        )
    }
    .is_err()
    {
        note(format!("could not map the VRAM window at {:#x}", phys.as_u64()));
        return lines;
    }
    let (dummy_phys, _) = match system_page(0) {
        Ok(page) => page,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    let hw = Hw {
        area: virt.as_usize(),
        vram_start,
        pt_next: 0,
        pt_of: alloc::vec![u16::MAX; (VM_PAGES / PAGES_PER_PDE) as usize],
        seq: 0,
    };
    hw.zero(0, PD_BYTES);
    hw.put32(FENCE_OFFSET, 0);
    let pd_mc = hw.mc(0);
    window.hdp_flush();
    // Контекст 1: диапазон, корень PD, страница для ошибок, управление (как в `amd cs`).
    window.wr(MM_VM_CONTEXT1_PAGE_TABLE_START_ADDR * 4, 0);
    window.wr(MM_VM_CONTEXT1_PAGE_TABLE_END_ADDR * 4, (VM_PAGES - 1) as u32);
    window.wr(MM_VM_CONTEXT1_PAGE_TABLE_BASE_ADDR * 4, (pd_mc >> 12) as u32);
    window.wr(MM_VM_CONTEXT1_PROTECTION_FAULT_DEFAULT_ADDR * 4, (dummy_phys >> 12) as u32);
    window.wr(MM_VM_CONTEXT1_CNTL2 * 4, 4);
    let fault_bits: u32 = 0x10 | 0x80 | 0x400 | 0x2000 | 0x1_0000 | 0x8_0000 | 0x40_0000;
    let control_clear: u32 = 0x1 | 0x6 | fault_bits | 0xf00_0000;
    let control = window.rd(MM_VM_CONTEXT1_CNTL * 4);
    window.wr(MM_VM_CONTEXT1_CNTL * 4, (control & !control_clear) | 0x1 | (1 << 1) | fault_bits | (1 << 24));
    window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1 << 1);
    super::super::nvidia::spin_us(100);
    note(format!(
        "context 1 enabled: {} GiB of VA, PD at GPU {pd_mc:#x} ({} KiB), {PT_COUNT} tables of {PT_BYTES} bytes; CONTEXT1_CNTL {control:#x} -> {:#x}",
        VM_PAGES * 4096 >> 30,
        PD_BYTES / 1024,
        window.rd(MM_VM_CONTEXT1_CNTL * 4)
    ));
    note(super::gfx_init(&window));
    // Пул видеопамяти: размер - из MC_VM_FB_LOCATION (база и верх в единицах по 16 МиБ).
    let fb = window.rd(MM_MC_VM_FB_LOCATION * 4);
    let (fb_base, fb_top) = (u64::from(fb & 0xffff) << 24, (u64::from(fb >> 16) << 24) | 0xff_ffff);
    let vram_size = fb_top - fb_base + 1;
    if vram_size > VRAM_POOL_START + VRAM_TOP_RESERVE {
        let total = vram_size - VRAM_TOP_RESERVE - VRAM_POOL_START;
        *VRAM.lock() = Some(VramPool { free: alloc::vec![(VRAM_POOL_START, total)], total, used: 0 });
        note(format!("VRAM pool: {} MiB of {} MiB for GPU buffers (offset {} MiB and up)", total >> 20, vram_size >> 20, VRAM_POOL_START >> 20));
    }
    sh_mem_init(&window);
    note(format!("SH_MEM_* of VMID 1..15 set like gfx_v8_0_constants_init (SH_MEM_CONFIG {SH_MEM_CONFIG_USER:#x}, SH_MEM_BASES {SH_MEM_BASES_USER:#x})"));
    *HW.lock() = Some(hw);
    lines
}

/// `SRBM_GFX_CNTL` (dword 0x391): VMID в битах 7:4.
const MM_SRBM_GFX_CNTL: usize = 0x391;
/// `SH_MEM_CONFIG` VMID > 0 (`gfx_v8_0_constants_init`): ALIGNMENT_MODE = UNALIGNED (3 << 3), DEFAULT_MTYPE и
/// APE1_MTYPE - UC (3), как у VMID 0 (на этой карте проверен именно он; Linux ставит NC для DEFAULT_MTYPE).
const SH_MEM_CONFIG_USER: u32 = (3 << 3) | (3 << 5) | (3 << 8);
/// `shared_aperture_start >> 48` (gmc_v8_0.c: 0x2000_0000_0000_0000).
const SH_MEM_BASES_USER: u32 = 0x2000;

/// `SH_MEM_*` контекстов 1..15: без них шейдеры с VMID > 0 работают с нулевыми базами апертур.
fn sh_mem_init(window: &Window) {
    for vmid in 1..16u32 {
        window.wr(MM_SRBM_GFX_CNTL * 4, vmid << 4);
        window.wr(super::MM_SH_MEM_CONFIG * 4, SH_MEM_CONFIG_USER);
        window.wr(super::MM_SH_MEM_BASES * 4, SH_MEM_BASES_USER);
        window.wr(super::MM_SH_MEM_APE1_BASE * 4, 1);
        window.wr(super::MM_SH_MEM_APE1_LIMIT * 4, 0);
    }
    window.wr(MM_SRBM_GFX_CNTL * 4, 0);
}

/// Сбросить адресное пространство: все PDE пустые, пул PT свободен.
pub fn clear() {
    let mut guard = HW.lock();
    let Some(hw) = guard.as_mut() else { return };
    // Только занятые записи PD: обнулять все 128 КиБ через BAR на каждый GEM_VA слишком дорого.
    for pde in 0..hw.pt_of.len() {
        if hw.pt_of[pde] != u16::MAX {
            hw.put64(pde * 8, 0);
            hw.pt_of[pde] = u16::MAX;
        }
    }
    hw.pt_next = 0;
    flush_locked();
}

/// Сбросить кэши записей CPU в видеопамять и TLB контекста 1.
fn flush_locked() {
    if let Some(window) = window() {
        window.hdp_flush();
        window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1 << 1);
    }
}

/// Отобразить страницы системной памяти `frames` (физические адреса кадров) с адреса GPU `va`.
pub fn map(va: u64, frames: &[u64]) -> Result<(), String> {
    map_with(va, frames.len(), |index| (frames[index] & PTE_ADDRESS) | PTE_SYSTEM_FLAGS)
}

/// Отобразить `pages` страниц видеопамяти с адреса GPU `va`; `vram_offset` - от начала VRAM (так хочет PTE).
pub fn map_vram(va: u64, vram_offset: u64, pages: usize) -> Result<(), String> {
    map_with(va, pages, |index| ((vram_offset + index as u64 * 4096) & PTE_ADDRESS) | PTE_VRAM_FLAGS)
}

/// Общая часть `map`/`map_vram`: `pte(i)` - запись для страницы `i`.
fn map_with(va: u64, pages: usize, pte: impl Fn(usize) -> u64) -> Result<(), String> {
    if va % 4096 != 0 || va / 4096 + pages as u64 > VM_PAGES {
        return Err(format!("VA {va:#x} + {pages} pages is outside of the {} GiB space", VM_PAGES * 4096 >> 30));
    }
    let mut guard = HW.lock();
    let hw = guard.as_mut().ok_or("the hardware backend is not started")?;
    for index in 0..pages {
        let pfn = va / 4096 + index as u64;
        let pde = (pfn / PAGES_PER_PDE) as usize;
        if hw.pt_of[pde] == u16::MAX {
            if hw.pt_next >= PT_COUNT {
                return Err(format!("out of page tables ({PT_COUNT} x 4 MiB of VA)"));
            }
            let slot = hw.pt_next;
            hw.pt_next += 1;
            let offset = PD_BYTES + slot * PT_BYTES;
            hw.zero(offset, PT_BYTES);
            hw.pt_of[pde] = slot as u16;
            hw.put64(pde * 8, hw.mc(offset) | PTE_VALID);
        }
        let table = PD_BYTES + usize::from(hw.pt_of[pde]) * PT_BYTES;
        hw.put64(table + (pfn % PAGES_PER_PDE) as usize * 8, pte(index));
    }
    flush_locked();
    Ok(())
}

/// `amd hw`: самопроверка бэкенда без пользовательской программы. Две страницы системной памяти
/// (буфер команд и данные) ставятся в VMID 1 выше 4 ГиБ - там, где их берёт libdrm_amdgpu, -
/// буфер выполняется кольцом, данные читает CPU.
#[must_use]
pub fn selftest() -> Vec<String> {
    let mut lines = start();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : hw: {text}");
        lines.push(text);
    };
    if !ready() {
        return lines;
    }
    const IB_VA: u64 = 0x1_4000_0000; // 5 ГиБ
    const DATA_VA: u64 = 0x2_8000_0000; // 10 ГиБ
    let (Ok((ib_phys, ib_virt)), Ok((data_phys, data_virt))) = (system_page(0), system_page(0)) else {
        note(String::from("out of memory pages"));
        return lines;
    };
    clear();
    if let Err(why) = map(IB_VA, &[ib_phys]).and_then(|()| map(DATA_VA, &[data_phys])) {
        note(why);
        return lines;
    }
    // Два WRITE_DATA (память, с подтверждением) по адресам данных.
    let write = |address: u64, value: u32| [super::PM4_WRITE_DATA_3, super::WRITE_DATA_MEMORY_CONFIRM, address as u32, (address >> 32) as u32, value];
    let ib: Vec<u32> = [(DATA_VA, 0x600d_beef_u32), (DATA_VA + 0x10, 0xfeed_0001)].iter().flat_map(|&(address, value)| write(address, value)).collect();
    for (index, word) in ib.iter().enumerate() {
        // SAFETY: страница выделена выше, прямое отображение, слово внутри неё.
        unsafe { core::ptr::write_volatile((ib_virt + index * 4) as *mut u32, *word) };
    }
    note(format!("IB of {} dwords at VA {IB_VA:#x} (system page {ib_phys:#x}), data at VA {DATA_VA:#x} (page {data_phys:#x})", ib.len()));
    match submit(IB_VA, ib.len()) {
        Ok(()) => {
            // SAFETY: страница данных, прямое отображение.
            let (a, b) = unsafe { (core::ptr::read_volatile(data_virt as *const u32), core::ptr::read_volatile((data_virt + 0x10) as *const u32)) };
            let ok = a == 0x600d_beef && b == 0xfeed_0001;
            note(format!("HW RESULT: {} (words {a:#x}, {b:#x}; the IB ran with VMID 1 above 4 GiB and the fence came back)", if ok { "PASSED" } else { "FAILED" }));
        }
        Err(why) => note(format!("HW RESULT: FAILED: {why}")),
    }
    // Второй этап: compute-шейдер из пользовательского VA (то, что делает RADV для vkCmdFillBuffer).
    const SHADER_VA: u64 = 0x3_0000_0000; // 12 ГиБ
    let (Ok((shader_phys, shader_virt)), Ok((ib2_phys, ib2_virt))) = (system_page(0), system_page(0)) else {
        note(String::from("out of memory pages"));
        return lines;
    };
    const IB2_VA: u64 = 0x3_0010_0000;
    if let Err(why) = map(SHADER_VA, &[shader_phys]).and_then(|()| map(IB2_VA, &[ib2_phys])) {
        note(why);
        return lines;
    }
    for (index, word) in super::SHADER_GFX8.iter().enumerate() {
        // SAFETY: страница выделена выше, шейдер (15 слов) внутри неё.
        unsafe { core::ptr::write_volatile((shader_virt + index * 4) as *mut u32, *word) };
    }
    let flag = DATA_VA + 0x40;
    let data = DATA_VA + 0x20;
    // SAFETY: страница данных, прямое отображение.
    unsafe {
        core::ptr::write_volatile((data_virt + 0x20) as *mut u32, 0);
        core::ptr::write_volatile((data_virt + 0x40) as *mut u32, 0);
    }
    let program = SHADER_VA >> 8;
    let sh = |count: u32| super::set_sh_reg(count, false);
    let reg = |register: usize| (register - super::PM4_SET_SH_REG_START) as u32;
    let ib2: Vec<u32> = alloc::vec![
        super::PM4_CONTEXT_CONTROL_1, 0x8000_0000, 0x8000_0000,
        sh(2), reg(0x2e0c), program as u32, (program >> 32) as u32, // COMPUTE_PGM_LO/HI
        sh(2), reg(0x2e12), 0x002c_0040, 0x0000_0010, // COMPUTE_PGM_RSRC1/2
        sh(3), reg(0x2e16), 0xffff_ffff, 0xffff_ffff, 0x0000_0100, // STATIC_THREAD_MGMT_SE0/1, TMPRING_SIZE
        sh(2), reg(0x2e19), 0xffff_ffff, 0xffff_ffff, // STATIC_THREAD_MGMT_SE2/3
        sh(2), reg(0x2e40), data as u32, (data >> 32) as u32, // COMPUTE_USER_DATA_0/1
        sh(1), reg(0x2e15), 0, // COMPUTE_RESOURCE_LIMITS
        sh(3), reg(0x2e07), 1, 1, 1, // COMPUTE_NUM_THREAD_X/Y/Z
        super::dispatch_direct(false), 1, 1, 1, 0x0000_0045, // DISPATCH_DIRECT 1x1x1
        super::PM4_EVENT_WRITE_0, 0x0000_0407, // CS_PARTIAL_FLUSH
        super::PM4_WRITE_DATA_3, super::WRITE_DATA_MEMORY_CONFIRM, flag as u32, (flag >> 32) as u32, 99,
    ];
    for (index, word) in ib2.iter().enumerate() {
        // SAFETY: страница выделена выше, буфер (около 60 слов) внутри неё.
        unsafe { core::ptr::write_volatile((ib2_virt + index * 4) as *mut u32, *word) };
    }
    note(format!("shader at VA {SHADER_VA:#x}, IB of {} dwords at VA {IB2_VA:#x}, buffer_store_dword to VA {data:#x}, flag at {flag:#x}", ib2.len()));
    match submit_timeout(IB2_VA, ib2.len(), 3000) {
        Ok(()) => {
            // SAFETY: страница данных, прямое отображение.
            let (word, flag_word) = unsafe { (core::ptr::read_volatile((data_virt + 0x20) as *const u32), core::ptr::read_volatile((data_virt + 0x40) as *const u32)) };
            let ok = word == 42 && flag_word == 99;
            note(format!(
                "HW SHADER RESULT: {} (the shader in VMID 1 wrote {word:#x} (want 0x2a), the flag {flag_word:#x} (want 0x63))",
                if ok { "PASSED" } else { "FAILED" }
            ));
        }
        Err(why) => note(format!("HW SHADER RESULT: FAILED: {why}")),
    }
    let extra = vram_test();
    lines.extend(extra);
    lines
}

/// Пакет `DMA_DATA` (память в память, `CP_SYNC`: CP ждёт конца копирования): `bytes` < 2 МиБ.
fn dma_copy(src: u64, dst: u64, bytes: u32) -> [u32; 7] {
    [0xc005_5000, 1 << 31, src as u32, (src >> 32) as u32, dst as u32, (dst >> 32) as u32, bytes]
}

/// Самопроверка видеопамяти: данные идут система -> VRAM -> VRAM -> система без потерь, затем замер
/// скорости копирования GPU в четырёх сочетаниях (64 МиБ на сочетание). `HW VRAM RESULT` и `HW VRAM SPEED`.
fn vram_test() -> Vec<String> {
    let mut out = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : hw: {text}");
        out.push(text);
    };
    const MIB: usize = 1 << 20;
    const PAGES: usize = 4 * MIB / 4096;
    const VA_A: u64 = 0x4_0000_0000;
    const VA_B: u64 = 0x4_1000_0000;
    const VA_V1: u64 = 0x4_2000_0000;
    const VA_V2: u64 = 0x4_3000_0000;
    const VA_IB: u64 = 0x4_4000_0000;
    let (Some(v1), Some(v2)) = (vram_alloc(4 * MIB as u64), vram_alloc(4 * MIB as u64)) else {
        note(String::from("HW VRAM RESULT: no VRAM pool (the card reports too little memory?)"));
        return out;
    };
    let mut a: Vec<(u64, usize)> = Vec::new();
    let mut b: Vec<(u64, usize)> = Vec::new();
    for list in [&mut a, &mut b] {
        for _ in 0..PAGES {
            match system_page(0) {
                Ok(page) => list.push(page),
                Err(why) => {
                    note(format!("HW VRAM RESULT: {why}"));
                    return out;
                }
            }
        }
    }
    let Ok((ib_phys, ib_virt)) = system_page(0) else { return out };
    // Узор в A: у каждой страницы своё слово в начале и в конце.
    for (index, &(_, virt)) in a.iter().enumerate() {
        // SAFETY: страницы выделены выше, прямое отображение, слова внутри страницы.
        unsafe {
            core::ptr::write_volatile(virt as *mut u32, 0x5a5a_0000 ^ index as u32);
            core::ptr::write_volatile((virt + 4092) as *mut u32, 0xa5a5_0000 ^ index as u32);
        }
    }
    clear();
    let a_frames: Vec<u64> = a.iter().map(|page| page.0).collect();
    let b_frames: Vec<u64> = b.iter().map(|page| page.0).collect();
    let mapped = map(VA_A, &a_frames)
        .and(map(VA_B, &b_frames))
        .and(map_vram(VA_V1, v1, PAGES))
        .and(map_vram(VA_V2, v2, PAGES))
        .and(map(VA_IB, &[ib_phys]));
    if let Err(why) = mapped {
        note(format!("HW VRAM RESULT: {why}"));
        return out;
    }
    let run = |packets: &[[u32; 7]]| -> Result<u64, String> {
        for (index, word) in packets.iter().flatten().enumerate() {
            // SAFETY: страница IB выделена выше; пакетов не больше 64 (448 слов).
            unsafe { core::ptr::write_volatile((ib_virt + index * 4) as *mut u32, *word) };
        }
        let started = crate::time::uptime_ns();
        submit_timeout(VA_IB, packets.len() * 7, 8000)?;
        Ok(crate::time::uptime_ns() - started)
    };
    // 1. Корректность: A -> V1 -> V2 -> B, по 4 пакета по 1 МиБ на шаг (три отправки).
    let chunks = |src: u64, dst: u64| -> Vec<[u32; 7]> { (0..4u64).map(|i| dma_copy(src + i * MIB as u64, dst + i * MIB as u64, MIB as u32)).collect() };
    let steps = [(VA_A, VA_V1), (VA_V1, VA_V2), (VA_V2, VA_B)];
    let mut failed = None;
    for (src, dst) in steps {
        if let Err(why) = run(&chunks(src, dst)) {
            failed = Some(why);
            break;
        }
    }
    if let Some(why) = failed {
        note(format!("HW VRAM RESULT: FAILED: {why}"));
    } else {
        let bad = b
            .iter()
            .enumerate()
            .filter(|&(index, &(_, virt))| {
                // SAFETY: страницы B выделены выше.
                let (first, last) = unsafe { (core::ptr::read_volatile(virt as *const u32), core::ptr::read_volatile((virt + 4092) as *const u32)) };
                first != 0x5a5a_0000 ^ index as u32 || last != 0xa5a5_0000 ^ index as u32
            })
            .count();
        note(format!(
            "HW VRAM RESULT: {} (4 MiB went system -> VRAM -> VRAM -> system through the CP DMA, {bad} of {PAGES} pages differ; VRAM at offsets {v1:#x} and {v2:#x})",
            if bad == 0 { "PASSED" } else { "FAILED" }
        ));
        // 2. Скорость: по 64 пакета по 1 МиБ (64 МиБ) на сочетание.
        let bench = |src: u64, dst: u64| -> Result<u64, String> {
            let packets: Vec<[u32; 7]> = (0..64u64).map(|i| dma_copy(src + (i % 4) * MIB as u64, dst + (i % 4) * MIB as u64, MIB as u32)).collect();
            run(&packets).map(|ns| 64 * 1_000_000_000 / ns.max(1))
        };
        let mut line = String::new();
        for (name, src, dst) in [("system->system", VA_A, VA_B), ("system->VRAM", VA_A, VA_V1), ("VRAM->VRAM", VA_V1, VA_V2), ("VRAM->system", VA_V2, VA_B)] {
            match bench(src, dst) {
                Ok(mib_s) => line.push_str(&format!("{name} {mib_s} MiB/s; ")),
                Err(why) => line.push_str(&format!("{name} failed ({why}); ")),
            }
        }
        note(format!("HW VRAM SPEED: CP DMA copy, 64 MiB each: {line}"));
    }
    // Убрать за собой: таблицы, страницы, видеопамять.
    clear();
    vram_free(v1, 4 * MIB as u64);
    vram_free(v2, 4 * MIB as u64);
    use crate::mm::FrameAllocator as _;
    for (phys, _) in a.iter().chain(b.iter()).chain(core::iter::once(&(ib_phys, ib_virt))) {
        crate::mm::frame::with(|pool| {
            // SAFETY: страницы выданы `system_page` этой функции и больше не отображены в GPU (`clear`).
            unsafe { pool.free(crate::mm::PhysAddr(*phys)) };
        });
    }
    out
}

/// Выполнить буфер команд из `dwords` слов по адресу GPU `va` (VMID 1) и дождаться его конца.
pub fn submit(va: u64, dwords: usize) -> Result<(), String> {
    submit_timeout(va, dwords, 200)
}

/// То же, но ждать забор до `timeout_ms` миллисекунд (шейдеру с долгим циклом нужны секунды).
pub fn submit_timeout(va: u64, dwords: usize, timeout_ms: u32) -> Result<(), String> {
    let Some(window) = window() else { return Err(String::from("no Polaris card was probed")) };
    let (seq, fence_mc, area) = {
        let mut guard = HW.lock();
        let hw = guard.as_mut().ok_or("the hardware backend is not started")?;
        hw.seq = hw.seq.wrapping_add(1);
        (hw.seq, hw.mc(FENCE_OFFSET), hw.area)
    };
    // IB с VMID 1, за ним fence: EVENT_WRITE_EOP пишет номер в видеопамять после сброса кэшей.
    let packets = [
        PM4_INDIRECT_BUFFER_2,
        va as u32 & !3,
        (va >> 32) as u32 & 0xffff,
        dwords as u32 | (1 << 24),
        PM4_EVENT_WRITE_EOP_4,
        EOP_CONTROL,
        fence_mc as u32 & !3,
        ((fence_mc >> 32) as u32 & 0xffff) | EOP_DATA_SEL_LOW32,
        seq,
        0,
    ];
    super::ring_submit(&window, &packets)?;
    let mut value = 0;
    for _ in 0..u64::from(timeout_ms) * 20 {
        window.hdp_invalidate();
        // SAFETY: слово fence внутри отображённой области, выровнено.
        value = unsafe { core::ptr::read_volatile((area + FENCE_OFFSET) as *const u32) };
        if value == seq {
            break;
        }
        super::super::nvidia::spin_us(50);
    }
    if value == seq {
        return Ok(());
    }
    Err(format!(
        "IB at {va:#x} ({dwords} dwords) did not finish: fence {value:#x} (expected {seq:#x}), GRBM_STATUS {:#x}, fault status {:#x} address {:#x} client {:#x}",
        window.rd(MM_GRBM_STATUS * 4),
        window.rd(MM_VM_CONTEXT1_PROTECTION_FAULT_STATUS * 4),
        window.rd(MM_VM_CONTEXT1_PROTECTION_FAULT_ADDR * 4),
        window.rd(MM_VM_CONTEXT1_PROTECTION_FAULT_MCCLIENT * 4)
    ))
}
