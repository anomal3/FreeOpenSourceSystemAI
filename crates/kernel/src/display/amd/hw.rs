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
    system_page, Window, MM_GRBM_STATUS, MM_VM_CONTEXT1_CNTL, MM_VM_CONTEXT1_CNTL2,
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
    hw.zero(0, PD_BYTES);
    hw.pt_next = 0;
    hw.pt_of.iter_mut().for_each(|entry| *entry = u16::MAX);
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
    if va % 4096 != 0 || va / 4096 + frames.len() as u64 > VM_PAGES {
        return Err(format!("VA {va:#x} + {} pages is outside of the {} GiB space", frames.len(), VM_PAGES * 4096 >> 30));
    }
    let mut guard = HW.lock();
    let hw = guard.as_mut().ok_or("the hardware backend is not started")?;
    for (index, &frame) in frames.iter().enumerate() {
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
        hw.put64(table + (pfn % PAGES_PER_PDE) as usize * 8, (frame & PTE_ADDRESS) | PTE_SYSTEM_FLAGS);
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
    lines
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
