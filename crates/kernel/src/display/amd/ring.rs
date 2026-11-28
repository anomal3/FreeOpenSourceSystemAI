//! Кольцо команд GFX.

use super::*;

// --- Кольцо команд GFX (`amd ring`) ------------------------------------------
//
// Этап P4. Источники — `gfx_v8_0.c` (`gfx_v8_0_cp_gfx_enable`, `cp_gfx_resume`,
// `cp_gfx_start`, `ring_test_ring`, `ring_set_wptr_gfx`), `amdgpu_ring.c`
// (`amdgpu_ring_commit`), `vid.h` (PM4), `gfx_8_0_d.h` (Linux 6.12).
//
// Кольцо и writeback лежат в видеопамяти (область за образами микрокода), адреса —
// в системной апертуре MC (её задаёт `amd ucode`). Таблицы страниц GPU не
// используются: у VMID0 `VM_CONTEXT0` выключен. Тест 1 — пакет `SET_UCONFIG_REG`
// пишет `0xDEADBEEF` в `SCRATCH_REG0` (как `gfx_v8_0_ring_test_ring`). Тест 2 —
// пакет `WRITE_DATA` пишет слово в видеопамять, хост читает его через BAR0.
// Golden registers и `constants_init` (тайлинг, SH_MEM) пока не делаются: для
// кольца они не обязательны, для 3D понадобятся.
//
// При неудаче CP снова останавливается (`CP_ME_CNTL` = 0x15000000).

pub(super) const MM_GRBM_GFX_INDEX: usize = 0xc200;
pub(super) const MM_SRBM_GFX_CNTL: usize = 0x391;
pub(super) const MM_GRBM_CNTL: usize = 0x2000;
pub(super) const MM_CP_INT_CNTL_RING0: usize = 0x306a;
pub(super) const MM_CP_RB_WPTR_DELAY: usize = 0x21c1;
pub(super) const MM_CP_RB_VMID: usize = 0x3051;
pub(super) const MM_CP_RB0_CNTL: usize = 0x3041;
pub(super) const MM_CP_RB0_WPTR: usize = 0x3045;
pub(super) const MM_CP_RB0_RPTR: usize = 0x21c0;
pub(super) const MM_CP_RB0_RPTR_ADDR: usize = 0x3043;
pub(super) const MM_CP_RB0_RPTR_ADDR_HI: usize = 0x3044;
pub(super) const MM_CP_RB_WPTR_POLL_ADDR_LO: usize = 0x3046;
pub(super) const MM_CP_RB_WPTR_POLL_ADDR_HI: usize = 0x3047;
pub(super) const MM_CP_RB0_BASE: usize = 0x3040;
pub(super) const MM_CP_RB0_BASE_HI: usize = 0x30b1;
pub(super) const MM_CP_RB_DOORBELL_CONTROL: usize = 0x3059;
pub(super) const MM_CP_MAX_CONTEXT: usize = 0x30ae;
pub(super) const MM_CP_ENDIAN_SWAP: usize = 0x3050;
pub(super) const MM_CP_DEVICE_ID: usize = 0x304b;
pub(super) const MM_CP_STAT: usize = 0x21a0;
pub(super) const MM_SCRATCH_REG0: usize = 0xc040;
pub(super) const MM_HDP_DEBUG0: usize = 0xbcc;
/// CE_HALT | PFP_HALT | ME_HALT в `CP_ME_CNTL`.
pub(super) const CP_HALT_BITS: u32 = 0x1500_0000;
/// `PACKET3(NOP, 0x3FFF)` — так Linux заливает кольцо и добивает до выравнивания.
pub(super) const PM4_NOP: u32 = 0xffff_1000;
/// `PACKET3(SET_UCONFIG_REG, 1)`, `PACKET3(WRITE_DATA, 3)`.
pub(super) const PM4_SET_UCONFIG_REG_1: u32 = 0xc001_7900;
pub(super) const PM4_WRITE_DATA_3: u32 = 0xc003_3700;
/// `WRITE_DATA_DST_SEL(5) | WR_CONFIRM`: память, с подтверждением записи.
pub(super) const WRITE_DATA_MEMORY_CONFIRM: u32 = 0x0010_0500;
/// Область под кольцо — сразу за областью микрокода (2 МиБ + 2 МиБ), 64 КиБ.
pub(super) const RING_AREA: u64 = UCODE_AREA + UCODE_AREA_LEN as u64;
pub(super) const RING_AREA_LEN: usize = 256 * 1024;
pub(super) const RING_BYTES: usize = 8192;
pub(super) const RING_DWORDS: u32 = (RING_BYTES / 4) as u32;
/// Writeback: слоты rptr и wptr-poll, затем слово для проверки записью в память.
pub(super) const RPTR_SLOT: usize = RING_BYTES;
pub(super) const WPTR_SLOT: usize = RING_BYTES + 64;
pub(super) const TEST_SLOT: usize = RING_BYTES + 128;
/// Выравнивание, до которого Linux добивает кольцо пакетами NOP (`align_mask` + 1).
pub(super) const RING_ALIGN: u32 = 256;

impl Window {
    /// Сброс буфера HDP: записи CPU в видеопамять через BAR0 должны дойти до GPU.
    pub(super) fn hdp_flush(&self) {
        self.wr(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4, 1);
        let _ = self.rd(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4);
    }

    /// Сброс кэша чтения HDP: хост должен увидеть то, что записал GPU (`vi_invalidate_hdp`).
    pub(super) fn hdp_invalidate(&self) {
        self.wr(MM_HDP_DEBUG0 * 4, 1);
        let _ = self.rd(MM_HDP_DEBUG0 * 4);
    }
}

/// `amd ring`: запустить CP и проверить кольцо GFX двумя пакетами.
#[must_use]
pub fn polaris_ring() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : ring: {text}");
        lines.push(text);
    };
    let (Some((base, _, _)), Some(bar0)) = (*POLARIS_MMIO.lock(), *POLARIS_BAR0.lock()) else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // 0. Условия: микрокод загружен (RLC работает), апертура MC задана.
    let rlc = window.rd(MM_RLC_CNTL * 4);
    let low = window.rd(MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR * 4);
    if rlc & 1 == 0 || low == 0 {
        note(format!("RLC_CNTL {rlc:#x}, system aperture LOW {low:#x}: run 'amd smc start' and 'amd ucode' first"));
        return lines;
    }
    let fb = window.rd(MM_MC_VM_FB_LOCATION * 4);
    let vram_start = u64::from(fb & 0xffff) << 24;
    let gpu = |offset: usize| vram_start + RING_AREA + offset as u64;
    // Окно VRAM под кольцо.
    let phys = crate::mm::PhysAddr::new(bar0 + RING_AREA);
    let virt = phys.to_direct_map();
    // SAFETY: окно видеопамяти (BAR0) устройства; ядро по нему не исполняется.
    if unsafe { crate::arch::map_active(virt, phys, RING_AREA_LEN, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
        note(format!("could not map the VRAM window at {:#x}", phys.as_u64()));
        return lines;
    }
    let vram = virt.as_usize();
    let vram_read = |offset: usize| -> u32 {
        // SAFETY: чтение из отображённой области (`RING_AREA_LEN`), выровнено.
        unsafe { core::ptr::read_volatile((vram + offset) as *const u32) }
    };
    let vram_write = |offset: usize, value: u32| {
        // SAFETY: запись в отображённую область, выровнено.
        unsafe { core::ptr::write_volatile((vram + offset) as *mut u32, value) };
    };
    let stop_cp = |window: &Window| {
        let value = window.rd(MM_CP_ME_CNTL * 4);
        window.wr(MM_CP_ME_CNTL * 4, value | CP_HALT_BITS);
    };

    // 1. Общее: широковещание по SE/SH, обычный SRBM, таймаут чтения GRBM.
    window.wr(MM_GRBM_GFX_INDEX * 4, 0xe000_0000);
    window.wr(MM_SRBM_GFX_CNTL * 4, 0);
    let grbm_cntl = window.rd(MM_GRBM_CNTL * 4);
    window.wr(MM_GRBM_CNTL * 4, (grbm_cntl & !0xff) | 0xff);
    // Прерывания CP от занятости/простоя не нужны (Linux сбрасывает их в `rlc_stop`).
    let int_cntl = window.rd(MM_CP_INT_CNTL_RING0 * 4);
    window.wr(MM_CP_INT_CNTL_RING0 * 4, int_cntl & !(0x80000 | 0x100000 | 0x40000 | 0x200000));
    // 2. Кольцо: всё NOP, writeback-слоты нули, проверочное слово — маркер.
    for dword in 0..RING_DWORDS as usize {
        vram_write(dword * 4, PM4_NOP);
    }
    vram_write(RPTR_SLOT, 0);
    vram_write(WPTR_SLOT, 0);
    vram_write(TEST_SLOT, 0xcafe_dead);
    // 3. `gfx_v8_0_cp_gfx_resume`.
    window.wr(MM_CP_RB_WPTR_DELAY * 4, 0);
    window.wr(MM_CP_RB_VMID * 4, 0);
    // RB_BUFSZ = order_base_2(ring_size / 8), RB_BLKSZ = BUFSZ - 2, MTYPE = 3, MIN_IB_AVAILSZ = 1.
    let bufsz = (RING_BYTES / 8).trailing_zeros();
    let cntl = bufsz | ((bufsz - 2) << 8) | (3 << 15) | (1 << 22);
    window.wr(MM_CP_RB0_CNTL * 4, cntl);
    window.wr(MM_CP_RB0_CNTL * 4, cntl | 0x8000_0000); // RB_RPTR_WR_ENA
    window.wr(MM_CP_RB0_WPTR * 4, 0);
    window.wr(MM_CP_RB0_RPTR_ADDR * 4, gpu(RPTR_SLOT) as u32);
    window.wr(MM_CP_RB0_RPTR_ADDR_HI * 4, ((gpu(RPTR_SLOT) >> 32) & 0xff) as u32);
    window.wr(MM_CP_RB_WPTR_POLL_ADDR_LO * 4, gpu(WPTR_SLOT) as u32);
    window.wr(MM_CP_RB_WPTR_POLL_ADDR_HI * 4, (gpu(WPTR_SLOT) >> 32) as u32);
    super::super::nvidia::spin_us(1000);
    window.wr(MM_CP_RB0_CNTL * 4, cntl); // WR_ENA снят
    let base_address = gpu(0) >> 8;
    window.wr(MM_CP_RB0_BASE * 4, base_address as u32);
    window.wr(MM_CP_RB0_BASE_HI * 4, (base_address >> 32) as u32);
    // Без doorbell: `CP_RB_DOORBELL_CONTROL.DOORBELL_EN` (бит 30) = 0.
    let doorbell = window.rd(MM_CP_RB_DOORBELL_CONTROL * 4);
    window.wr(MM_CP_RB_DOORBELL_CONTROL * 4, doorbell & !(1 << 30));
    // `gfx_v8_0_cp_gfx_start`: контекст и ID, затем снять останов CE/PFP/ME.
    window.wr(MM_CP_MAX_CONTEXT * 4, 7);
    window.wr(MM_CP_ENDIAN_SWAP * 4, 0);
    window.wr(MM_CP_DEVICE_ID * 4, 1);
    let me = window.rd(MM_CP_ME_CNTL * 4);
    window.wr(MM_CP_ME_CNTL * 4, me & !CP_HALT_BITS);
    super::super::nvidia::spin_us(50);
    note(format!(
        "CP started: ring {} B at GPU {:#x}, RB0_CNTL {cntl:#x}, CP_ME_CNTL {:#x} -> {:#x}",
        RING_BYTES,
        gpu(0),
        me,
        window.rd(MM_CP_ME_CNTL * 4)
    ));
    // Отправка: пакеты уже в кольце, добиваем NOP до границы (кольцо залито NOP).
    let submit = |window: &Window, wptr: u32| {
        window.hdp_flush();
        window.wr(MM_CP_RB0_WPTR * 4, wptr);
        let _ = window.rd(MM_CP_RB0_WPTR * 4);
    };
    // Тест 1: SET_UCONFIG_REG пишет 0xDEADBEEF в SCRATCH_REG0 (смещение 0x40 от 0xc000).
    window.wr(MM_SCRATCH_REG0 * 4, 0xcafe_dead);
    vram_write(0, PM4_SET_UCONFIG_REG_1);
    vram_write(4, (MM_SCRATCH_REG0 - 0xc000) as u32);
    vram_write(8, 0xdead_beef);
    submit(&window, RING_ALIGN);
    let mut value = 0;
    for _ in 0..100_000 {
        value = window.rd(MM_SCRATCH_REG0 * 4);
        if value == 0xdead_beef {
            break;
        }
        super::super::nvidia::spin_us(1);
    }
    let state = |window: &Window| {
        format!(
            "RB0_RPTR {:#x}, rptr slot {:#x}, GRBM_STATUS {:#010x}, CP_STAT {:#010x}",
            window.rd(MM_CP_RB0_RPTR * 4),
            vram_read(RPTR_SLOT),
            window.rd(MM_GRBM_STATUS * 4),
            window.rd(MM_CP_STAT * 4)
        )
    };
    if value != 0xdead_beef {
        note(format!("TEST 1 FAILED: SCRATCH_REG0 {value:#x} (expected 0xdeadbeef); {}", state(&window)));
        stop_cp(&window);
        note(String::from("CP halted again"));
        return lines;
    }
    note(format!("TEST 1 PASSED: SCRATCH_REG0 = 0xdeadbeef written by the CP from the ring; {}", state(&window)));
    // Тест 2: WRITE_DATA в видеопамять (DST_SEL 5, WR_CONFIRM), хост читает через BAR0.
    let at = RING_ALIGN as usize;
    vram_write(at * 4, PM4_WRITE_DATA_3);
    vram_write(at * 4 + 4, WRITE_DATA_MEMORY_CONFIRM);
    vram_write(at * 4 + 8, gpu(TEST_SLOT) as u32);
    vram_write(at * 4 + 12, (gpu(TEST_SLOT) >> 32) as u32);
    vram_write(at * 4 + 16, 0xbeef_cafe);
    submit(&window, 2 * RING_ALIGN);
    let mut word = 0;
    for _ in 0..100_000 {
        window.hdp_invalidate();
        word = vram_read(TEST_SLOT);
        if word == 0xbeef_cafe {
            break;
        }
        super::super::nvidia::spin_us(1);
    }
    if word == 0xbeef_cafe {
        note(format!("TEST 2 PASSED: the CP wrote 0xbeefcafe into VRAM at GPU {:#x}; {}", gpu(TEST_SLOT), state(&window)));
    } else {
        note(format!("TEST 2 FAILED: VRAM word {word:#x} (expected 0xbeefcafe); {}", state(&window)));
    }
    // Кольцо живо (тест 1 пройден): следующие этапы шлют пакеты тем же кольцом.
    *RING_STATE.lock() = Some(RingState { vram, vram_start, wptr: 2 * RING_ALIGN });
    lines
}
