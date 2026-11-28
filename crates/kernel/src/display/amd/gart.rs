//! GART для VMID0 и общие регистры VM/PM4.

use super::*;

// --- GART для VMID0 (`amd gart`) ---------------------------------------------
//
// Этап P5, шаг 1. Источники — `gmc_v8_0.c` (`gmc_v8_0_gart_enable`,
// `gmc_v8_0_flush_gpu_tlb`), `amdgpu_gart.c` (`amdgpu_gart_map`),
// `amdgpu_gmc.c` (`amdgpu_gmc_set_pte_pde`), `amdgpu_vm.h` (`AMDGPU_PTE_*`),
// `gmc_8_1_d.h`/`gmc_8_1_sh_mask.h` (Linux 6.12).
//
// GART — таблица страниц контекста 0 (VMID0): она даёт GPU доступ к страницам
// **системной** памяти по GPU-адресам вне видеопамяти. Здесь — 16 МиБ GART
// по адресу `GART_START` (вне VRAM, выровнен на 4 ГиБ, как у Linux), таблица (4096
// записей по 8 байт) лежит в видеопамяти за кольцом. Проверка: CP пакетом
// `WRITE_DATA` пишет слово по GART-адресу, ядро читает системную страницу.
// Регистры L2/TLB меняются «прочитать-изменить-записать», как у Linux:
// значения после POST прошивкой неизвестны.
//
// Запись PTE: `phys & 0x0000_FFFF_FFFF_F000 | VALID | SYSTEM | SNOOPED | EXECUTABLE |
// READABLE | WRITEABLE` (= `0x77`; для кэшируемой страницы CPU).

pub(super) const MM_MC_VM_MX_L1_TLB_CNTL: usize = 0x819;
pub(super) const MM_VM_L2_CNTL2: usize = 0x501;
pub(super) const MM_VM_L2_CNTL3: usize = 0x502;
pub(super) const MM_VM_L2_CNTL4: usize = 0x578;
pub(super) const MM_VM_CONTEXT0_CNTL2: usize = 0x50c;
pub(super) const MM_VM_CONTEXT0_PAGE_TABLE_BASE_ADDR: usize = 0x54f;
pub(super) const MM_VM_CONTEXT0_PAGE_TABLE_START_ADDR: usize = 0x557;
pub(super) const MM_VM_CONTEXT0_PAGE_TABLE_END_ADDR: usize = 0x55f;
pub(super) const MM_VM_CONTEXT0_PROTECTION_FAULT_DEFAULT_ADDR: usize = 0x546;
pub(super) const MM_VM_CONTEXT0_PROTECTION_FAULT_STATUS: usize = 0x536;
pub(super) const MM_VM_CONTEXT0_PROTECTION_FAULT_MCCLIENT: usize = 0x538;
pub(super) const MM_VM_CONTEXT0_PROTECTION_FAULT_ADDR: usize = 0x53e;
pub(super) const MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_LOW_ADDR: usize = 0x575;
pub(super) const MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_HIGH_ADDR: usize = 0x576;
pub(super) const MM_VM_L2_CONTEXT_IDENTITY_PHYSICAL_OFFSET: usize = 0x577;
pub(super) const MM_VM_INVALIDATE_REQUEST: usize = 0x51e;
/// GPU-адрес начала GART (вне VRAM, выровнен на 4 ГиБ).
pub(super) const GART_START: u64 = 0xff_0000_0000;
pub(super) const GART_PAGES: usize = 4096;
/// Таблица GART в окне VRAM кольца: за кольцом и словами проверки.
pub(super) const GART_TABLE_OFFSET: usize = 0x1_0000;
pub(super) const PTE_VALID: u64 = 1;
pub(super) const PTE_SYSTEM: u64 = 1 << 1;
pub(super) const PTE_SNOOPED: u64 = 1 << 2;
pub(super) const PTE_EXECUTABLE: u64 = 1 << 4;
pub(super) const PTE_READABLE: u64 = 1 << 5;
pub(super) const PTE_WRITEABLE: u64 = 1 << 6;

/// Записать пакеты в кольцо GFX и сдвинуть указатель записи: блок в
/// `RING_ALIGN` dword (как `amdgpu_ring_commit`, который добивает NOP до границы).
pub(super) fn ring_submit(window: &Window, packets: &[u32]) -> Result<(), String> {
    let mut guard = RING_STATE.lock();
    let state = guard.as_mut().ok_or("the ring is not running: run 'amd ring' first")?;
    if packets.len() >= RING_ALIGN as usize {
        return Err(format!("{} dwords do not fit one {RING_ALIGN}-dword block", packets.len()));
    }
    let put = |index: u32, value: u32| {
        // SAFETY: слово внутри отображённого окна кольца (`RING_BYTES`).
        unsafe { core::ptr::write_volatile((state.vram + (index % RING_DWORDS) as usize * 4) as *mut u32, value) };
    };
    let start = state.wptr;
    for index in 0..RING_ALIGN {
        put(start + index, PM4_NOP);
    }
    for (index, packet) in packets.iter().enumerate() {
        put(start + index as u32, *packet);
    }
    state.wptr = (start + RING_ALIGN) % RING_DWORDS;
    window.hdp_flush();
    window.wr(MM_CP_RB0_WPTR * 4, state.wptr);
    let _ = window.rd(MM_CP_RB0_WPTR * 4);
    Ok(())
}

/// Страница системной памяти: физический адрес и адрес в прямом отображении.
pub(super) fn system_page(fill: u32) -> Result<(u64, usize), String> {
    use crate::mm::FrameAllocator as _;
    let phys = crate::mm::frame::with(|frames| frames.allocate())
        .flatten()
        .map(crate::mm::PhysAddr::as_u64)
        .ok_or("out of memory pages")?;
    let virt = crate::mm::PhysAddr::new(phys).to_direct_map().as_usize();
    for index in 0..PAGE / 4 {
        // SAFETY: страница только что выделена и отображена прямым отображением.
        unsafe { core::ptr::write_volatile((virt + index * 4) as *mut u32, fill) };
    }
    Ok((phys, virt))
}

/// `amd gart`: включить GART для VMID0 и проверить доступ GPU к системной памяти.
#[must_use]
pub fn polaris_gart() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : gart: {text}");
        lines.push(text);
    };
    let (Some((base, _, _)), Some(bar0)) = (*POLARIS_MMIO.lock(), *POLARIS_BAR0.lock()) else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    let Some(vram) = RING_STATE.lock().as_ref().map(|state| (state.vram, state.vram_start)) else {
        note(String::from("the ring is not running: run 'amd ring' first"));
        return lines;
    };
    let (vram_virt, vram_start) = vram;
    let _ = bar0;
    // Страницы: пустая для ошибок доступа и страница проверки.
    let (dummy_phys, _) = match system_page(0) {
        Ok(page) => page,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    let (test_phys, test_virt) = match system_page(0x1111_1111) {
        Ok(page) => page,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    // Таблица GART в видеопамяти: все записи недействительны, кроме страницы 1.
    let table_mc = vram_start + RING_AREA + GART_TABLE_OFFSET as u64;
    for index in 0..GART_PAGES * 2 {
        // SAFETY: таблица внутри окна VRAM (`RING_AREA_LEN`); 32-битные записи.
        unsafe { core::ptr::write_volatile((vram_virt + GART_TABLE_OFFSET + index * 4) as *mut u32, 0) };
    }
    let pte = (test_phys & 0x0000_ffff_ffff_f000) | PTE_VALID | PTE_SYSTEM | PTE_SNOOPED | PTE_EXECUTABLE | PTE_READABLE | PTE_WRITEABLE;
    let gart_va = GART_START + PAGE as u64;
    for (half, value) in [(0usize, pte as u32), (1, (pte >> 32) as u32)] {
        // SAFETY: запись 1 таблицы (8 байт со смещения 8), внутри окна VRAM.
        unsafe { core::ptr::write_volatile((vram_virt + GART_TABLE_OFFSET + 8 + half * 4) as *mut u32, value) };
    }
    note(format!(
        "table at GPU {table_mc:#x} ({} KiB), GART {GART_START:#x}.. ({} MiB), test page phys {test_phys:#x} (PTE {pte:#x}), dummy {dummy_phys:#x}",
        GART_PAGES * 8 / 1024,
        GART_PAGES * PAGE / (1024 * 1024)
    ));
    let before = [
        window.rd(MM_MC_VM_MX_L1_TLB_CNTL * 4),
        window.rd(MM_VM_L2_CNTL * 4),
        window.rd(MM_VM_L2_CNTL3 * 4),
        window.rd(MM_VM_CONTEXT0_CNTL * 4),
    ];
    // `gmc_v8_0_gart_enable`: TLB, L2, контекст 0.
    let modify = |register: usize, clear: u32, set: u32| {
        let value = window.rd(register * 4);
        window.wr(register * 4, (value & !clear) | set);
    };
    // ENABLE_L1_TLB, ENABLE_L1_FRAGMENT_PROCESSING, SYSTEM_ACCESS_MODE = 3, ADVANCED_DRIVER_MODEL;
    // SYSTEM_APERTURE_UNMAPPED_ACCESS = 0.
    modify(MM_MC_VM_MX_L1_TLB_CNTL, 0x1 | 0x2 | 0x18 | 0x20 | 0x40, 0x1 | 0x2 | 0x18 | 0x40);
    // ENABLE_L2_CACHE, FRAGMENT_PROCESSING, PTE/PDE0 LRU_UPDATE_BY_WRITE, QUEUE_SIZE = 7,
    // CONTEXT1_IDENTITY_ACCESS_MODE = 1, ENABLE_DEFAULT_PAGE_OUT_TO_SYSTEM_MEMORY.
    modify(MM_VM_L2_CNTL, 0x1 | 0x2 | 0x200 | 0x400 | 0x3_8000 | 0x18_0000 | 0x800, 0x1 | 0x2 | 0x200 | 0x400 | 0x3_8000 | 0x8_0000 | 0x800);
    modify(MM_VM_L2_CNTL2, 0, 0x1 | 0x2); // INVALIDATE_ALL_L1_TLBS | INVALIDATE_L2_CACHE
    // BANK_SELECT = 9, L2_CACHE_BIGK_FRAGMENT_SIZE = 9, L2_CACHE_BIGK_ASSOCIATIVITY = 1 (fragment_size = 9).
    modify(MM_VM_L2_CNTL3, 0x3f | 0xf_8000 | 0x10_0000, 9 | (9 << 15) | 0x10_0000);
    modify(MM_VM_L2_CNTL4, 0x3_ffc0, 0); // запросы VMC_TAP — по умолчанию
    // Контекст 0: диапазон GART, таблица, страница для ошибок.
    window.wr(MM_VM_CONTEXT0_PAGE_TABLE_START_ADDR * 4, (GART_START >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_PAGE_TABLE_END_ADDR * 4, ((GART_START + (GART_PAGES * PAGE) as u64 - 1) >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_PAGE_TABLE_BASE_ADDR * 4, (table_mc >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_PROTECTION_FAULT_DEFAULT_ADDR * 4, (dummy_phys >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_CNTL2 * 4, 0);
    // ENABLE_CONTEXT, PAGE_TABLE_DEPTH = 0, RANGE_PROTECTION_FAULT_ENABLE_DEFAULT.
    modify(MM_VM_CONTEXT0_CNTL, 0x1 | 0x6 | 0x10, 0x1 | 0x10);
    window.wr(MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_LOW_ADDR * 4, 0);
    window.wr(MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_HIGH_ADDR * 4, 0);
    window.wr(MM_VM_L2_CONTEXT_IDENTITY_PHYSICAL_OFFSET * 4, 0);
    // Записи таблицы должны дойти до GPU раньше сброса TLB.
    window.hdp_flush();
    window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1);
    super::super::nvidia::spin_us(100);
    note(format!(
        "context 0 enabled: L1_TLB {:#x}->{:#x}, L2_CNTL {:#x}->{:#x}, L2_CNTL3 {:#x}->{:#x}, CONTEXT0_CNTL {:#x}->{:#x}",
        before[0],
        window.rd(MM_MC_VM_MX_L1_TLB_CNTL * 4),
        before[1],
        window.rd(MM_VM_L2_CNTL * 4),
        before[2],
        window.rd(MM_VM_L2_CNTL3 * 4),
        before[3],
        window.rd(MM_VM_CONTEXT0_CNTL * 4)
    ));
    // Проверка: CP пишет слово по GART-адресу страницы 1, ядро читает системную страницу.
    let packets = [PM4_WRITE_DATA_3, WRITE_DATA_MEMORY_CONFIRM, gart_va as u32, (gart_va >> 32) as u32, 0xface_feed];
    if let Err(why) = ring_submit(&window, &packets) {
        note(why);
        return lines;
    }
    let mut word = 0;
    for _ in 0..100_000 {
        // SAFETY: страница выделена выше и отображена прямым отображением.
        word = unsafe { core::ptr::read_volatile(test_virt as *const u32) };
        if word == 0xface_feed {
            break;
        }
        super::super::nvidia::spin_us(1);
    }
    if word == 0xface_feed {
        note(format!("TEST PASSED: the CP wrote 0xfacefeed through the GART at GPU {gart_va:#x} into the system page {test_phys:#x}"));
    } else {
        note(format!(
            "TEST FAILED: system page word {word:#x} (expected 0xfacefeed); fault status {:#x}, address {:#x}, client {:#x}",
            window.rd(MM_VM_CONTEXT0_PROTECTION_FAULT_STATUS * 4),
            window.rd(MM_VM_CONTEXT0_PROTECTION_FAULT_ADDR * 4),
            window.rd(MM_VM_CONTEXT0_PROTECTION_FAULT_MCCLIENT * 4)
        ));
        // Вернуть всё как было: контекст 0 выключить.
        modify(MM_VM_CONTEXT0_CNTL, 0x1, 0);
        window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1);
        note(String::from("context 0 disabled again"));
    }
    lines
}

// --- Регистры VMID 1 и пакеты fence/IB (для бэкенда `hw`) ---------------------
//
// Источники — `gfx_v8_0.c` (`ring_emit_fence_gfx`, `ring_emit_ib_gfx`), `gmc_v8_0.c`,
// `amdgpu_vm.h` (Linux 6.12).

pub(super) const MM_VM_CONTEXT1_CNTL: usize = 0x505;
pub(super) const MM_VM_CONTEXT1_CNTL2: usize = 0x50d;
pub(super) const MM_VM_CONTEXT1_PAGE_TABLE_BASE_ADDR: usize = 0x550;
pub(super) const MM_VM_CONTEXT1_PAGE_TABLE_START_ADDR: usize = 0x558;
pub(super) const MM_VM_CONTEXT1_PAGE_TABLE_END_ADDR: usize = 0x560;
pub(super) const MM_VM_CONTEXT1_PROTECTION_FAULT_DEFAULT_ADDR: usize = 0x547;
pub(super) const MM_VM_CONTEXT1_PROTECTION_FAULT_STATUS: usize = 0x537;
pub(super) const MM_VM_CONTEXT1_PROTECTION_FAULT_MCCLIENT: usize = 0x539;
pub(super) const MM_VM_CONTEXT1_PROTECTION_FAULT_ADDR: usize = 0x53f;
/// `PACKET3(EVENT_WRITE_EOP, 4)`, `PACKET3(INDIRECT_BUFFER, 2)`.
pub(super) const PM4_EVENT_WRITE_EOP_4: u32 = 0xc004_4700;
pub(super) const PM4_INDIRECT_BUFFER_2: u32 = 0xc002_3f00;
/// EOP_TCL1_ACTION_EN | EOP_TC_ACTION_EN | EOP_TC_WB_ACTION_EN | EVENT_TYPE(CACHE_FLUSH_AND_INV_TS_EVENT = 0x14) | EVENT_INDEX(5).
pub(super) const EOP_CONTROL: u32 = (1 << 16) | (1 << 17) | (1 << 15) | 0x14 | (5 << 8);
/// DATA_SEL(1): записать младшие 32 бита значения; INT_SEL(0): без прерывания.
pub(super) const EOP_DATA_SEL_LOW32: u32 = 1 << 29;
