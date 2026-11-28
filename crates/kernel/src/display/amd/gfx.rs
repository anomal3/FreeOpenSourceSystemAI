//! Настройка GFX (golden registers, `SH_MEM_*`) и проверочный шейдер GFX8.

use super::*;

// Источники — `gfx_v8_0.c` (`gfx_v8_0_init_golden_registers`, `gfx_v8_0_constants_init`),
// `amdgpu_device.c` (`amdgpu_device_program_register_sequence`), `vid.h`/`gfx_8_0_d.h` (Linux 6.12);
// шейдер самопроверки (`hw::selftest`) — из теста libdrm 2.4.124 (`tests/amdgpu/basic_tests.c`, GFX8).

/// `golden_settings_polaris10_a11` и `polaris10_golden_common_all`: (регистр, маска, значение).
/// Правило `amdgpu_device_program_register_sequence`: при маске `0xffffffff` — просто значение,
/// иначе `(старое & !маска) | значение`.
pub(super) const GOLDEN_POLARIS10: [(usize, u32, u32); 25] = [
    (0xcd4, 0x000c_0fc0, 0x000c_0200),  // ATC_MISC_CG
    (0x2684, 0x0001_f3cf, 0x0000_7208), // CB_HW_CONTROL
    (0x2686, 0x0f00_0000, 0x0f00_0000), // CB_HW_CONTROL_2
    (0x2683, 0x0000_01ff, 0x0000_0040), // CB_HW_CONTROL_3
    (0x260d, 0xf00f_ffff, 0x0000_0400), // DB_DEBUG2
    (0x22fc, 0xffff_ffff, 0x2000_0001), // PA_SC_ENHANCE
    (0xc281, 0x0000_ff0f, 0x0000_0000), // PA_SC_LINE_STIPPLE_STATE
    (0xa0d4, 0x3f3f_ffff, 0x1600_0012), // PA_SC_RASTER_CONFIG
    (0xa0d5, 0x0000_003f, 0x0000_002a), // PA_SC_RASTER_CONFIG_1
    (0xec49, 0x0000_0003, 0x0001_003c), // RLC_CGCG_CGLS_CTRL
    (0xec9d, 0xffff_ffff, 0x0001_003c), // RLC_CGCG_CGLS_CTRL_3D
    (0x2300, 0x07f8_0000, 0x0718_0000), // SQ_CONFIG
    (0x2542, 0x000f_000f, 0x000b_0000), // TA_CNTL_AUX
    (0x2b80, 0x0010_0000, 0xf31f_ff7f), // TCC_CTRL
    (0x2b05, 0x0000_03ff, 0x0000_00f7), // TCP_ADDR_CONFIG
    (0x2b04, 0xffff_ffff, 0x0000_0000), // TCP_CHAN_STEER_HI
    (0x2232, 0x0000_0004, 0x0000_0004), // VGT_RESET_DEBUG
    (0xc200, 0xffff_ffff, 0xe000_0000), // GRBM_GFX_INDEX (все SE/SH/экземпляры)
    (0xa0d4, 0xffff_ffff, 0x1600_0012), // PA_SC_RASTER_CONFIG
    (0xa0d5, 0xffff_ffff, 0x0000_002a), // PA_SC_RASTER_CONFIG_1
    (0x263e, 0xffff_ffff, 0x2201_1003), // GB_ADDR_CONFIG
    (0x31dc, 0xffff_ffff, 0x0000_0800), // SPI_RESOURCE_RESERVE_CU_0
    (0x31dd, 0xffff_ffff, 0x0000_0800), // SPI_RESOURCE_RESERVE_CU_1
    (0x31e6, 0xffff_ffff, 0x00ff_7fbf), // SPI_RESOURCE_RESERVE_EN_CU_0
    (0x31e7, 0xffff_ffff, 0x00ff_7faf), // SPI_RESOURCE_RESERVE_EN_CU_1
];
pub(super) const MM_SH_MEM_BASES: usize = 0x230a;
pub(super) const MM_SH_MEM_APE1_BASE: usize = 0x230b;
pub(super) const MM_SH_MEM_APE1_LIMIT: usize = 0x230c;
pub(super) const MM_SH_MEM_CONFIG: usize = 0x230d;
/// `SH_MEM_CONFIG` VMID 0: DEFAULT_MTYPE = UC (3), APE1_MTYPE = UC (3), ALIGNMENT_MODE = UNALIGNED (3).
pub(super) const SH_MEM_CONFIG_VMID0: u32 = (3 << 5) | (3 << 8) | (3 << 3);
pub(super) const PM4_SET_SH_REG_START: usize = 0x2c00;
pub(super) const PM4_CONTEXT_CONTROL_1: u32 = 0xc001_2800;
pub(super) const PM4_EVENT_WRITE_0: u32 = 0xc000_4600;
/// `PACKET3(SET_SH_REG, n)` и `PACKET3(DISPATCH_DIRECT, 3)`; бит 1 — «шейдер compute».
pub(super) const fn set_sh_reg(count: u32, compute: bool) -> u32 {
    0xc000_7600 | (count << 16) | if compute { 2 } else { 0 }
}
pub(super) const fn dispatch_direct(compute: bool) -> u32 {
    0xc003_1500 | if compute { 2 } else { 0 }
}
/// Шейдер из libdrm (`shader_bin` для GFX8): задержка циклом, затем `buffer_store_dword` значения 42
/// по V# `s[0:3]` (`s[0:1]` — адрес из `COMPUTE_USER_DATA_0/1`, `s2` = -1, `s3` = 0xf000), `s_endpgm`.
pub(super) const SHADER_GFX8: [u32; 15] = [
    0xbe82_0080, // s_mov_b32 s2, 0
    0xbf08_ff02, 0x0098_967f, // s_cmp_gt_u32 s2, 9999999
    0xbf85_0004, // s_cbranch_scc1 +4
    0x8102_8102, // s_add_i32 s2, s2, 1
    0xbf08_ff02, 0x0098_967f, // s_cmp_gt_u32 s2, 9999999
    0xbf84_fffc, // s_cbranch_scc0 -4
    0xbe83_00ff, 0x0000_f000, // s_mov_b32 s3, 0xf000
    0xbe82_00c1, // s_mov_b32 s2, -1
    0x7e00_02aa, // v_mov_b32 v0, 42
    0xe070_0000, 0x8000_0000, // buffer_store_dword v0, off, s[0:3], 0
    0xbf81_0000, // s_endpgm
];

/// Golden registers Polaris10 и `SH_MEM_*` VMID 0.
pub(super) fn gfx_init(window: &Window) -> String {
    for (register, mask, value) in GOLDEN_POLARIS10 {
        let new = if mask == 0xffff_ffff { value } else { (window.rd(register * 4) & !mask) | value };
        window.wr(register * 4, new);
    }
    // VMID 0 в SRBM, как `vi_srbm_select(adev, 0, 0, 0, 0)`.
    window.wr(MM_SRBM_GFX_CNTL * 4, 0);
    window.wr(MM_SH_MEM_CONFIG * 4, SH_MEM_CONFIG_VMID0);
    window.wr(MM_SH_MEM_BASES * 4, 0);
    window.wr(MM_SH_MEM_APE1_BASE * 4, 1);
    window.wr(MM_SH_MEM_APE1_LIMIT * 4, 0);
    format!(
        "golden registers ({} entries) and SH_MEM (VMID 0) applied; SQ_CONFIG {:#x}, SPI_RESOURCE_RESERVE_EN_CU_0 {:#x}, SH_MEM_CONFIG {:#x}",
        GOLDEN_POLARIS10.len(),
        window.rd(0x2300 * 4),
        window.rd(0x31e6 * 4),
        window.rd(MM_SH_MEM_CONFIG * 4)
    )
}
