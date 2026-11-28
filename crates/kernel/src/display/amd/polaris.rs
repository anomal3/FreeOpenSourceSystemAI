//! Polaris 10: разведка карты, VBIOS и чтение состояния SMC.

use super::*;

// --- Polaris 10 (RX 470/480/570/580/590), семейство VI --------------------------
//
// Этап A1 для карт без IP discovery. Источники — драйвер Linux `amdgpu` v6.12:
// `amdgpu_drv.c` (список PCI-идентификаторов), `vi.c`, `gmc_v8_0.c`,
// `include/asic_reg/{bif/bif_5_0_d.h, gca/gfx_8_0_d.h, gmc/gmc_8_1_d.h,
// oss/oss_3_0_d.h, smu/smu_7_1_1_d.h}`, `amdgpu.h` (`ASICID_IS_P20/P30`).
// Индексы регистров ниже — dword-номера в BAR5, как в заголовках драйвера;
// байтовое смещение = номер × 4.

/// PCI-идентификаторы Polaris 10 из `pciidlist` (`CHIP_POLARIS10`).
pub(super) const POLARIS10_IDS: [u16; 13] =
    [0x67C0, 0x67C1, 0x67C2, 0x67C4, 0x67C7, 0x67D0, 0x67DF, 0x67C8, 0x67C9, 0x67CA, 0x67CC, 0x67CF, 0x6FDF];

/// Окно регистров (BAR5) у Polaris — 256 КиБ (типовое значение; размер BAR не
/// замеряем, см. начало файла).
pub(super) const MMIO_LEN_VI: usize = 256 * 1024;

pub(super) const MM_CONFIG_MEMSIZE: usize = 0x150a;
pub(super) const MM_CONFIG_APER_SIZE: usize = 0x150c;
pub(super) const MM_BIF_FB_EN: usize = 0x1524;
pub(super) const MM_BIOS_SCRATCH_7: usize = 0x5d0;
pub(super) const MM_SRBM_STATUS: usize = 0x394;
pub(super) const MM_SRBM_STATUS2: usize = 0x393;
pub(super) const MM_MC_VM_FB_LOCATION: usize = 0x809;
pub(super) const MM_VM_L2_CNTL: usize = 0x500;
pub(super) const MM_VM_CONTEXT0_CNTL: usize = 0x504;
pub(super) const MM_GRBM_STATUS: usize = 0x2004;
pub(super) const MM_GRBM_STATUS2: usize = 0x2002;
pub(super) const MM_CC_RB_BACKEND_DISABLE: usize = 0x263d;
pub(super) const MM_GB_ADDR_CONFIG: usize = 0x263e;
pub(super) const MM_CC_GC_SHADER_ARRAY_CONFIG: usize = 0x226f;
pub(super) const MM_RLC_CNTL: usize = 0xec00;
pub(super) const MM_CP_ME_CNTL: usize = 0x21b6;
pub(super) const MM_SDMA0_F32_CNTL: usize = 0x3412;
pub(super) const MM_SDMA1_F32_CNTL: usize = 0x3612;
pub(super) const MM_IH_RB_CNTL: usize = 0xe30;
/// Косвенный доступ к регистрам SMC (`SMC_IND_INDEX_11`/`DATA_11`) и адреса
/// ПЗУ видеокарты внутри SMC (`ixROM_INDEX`/`ixROM_DATA`) — так `vi.c`
/// (`vi_read_bios_from_rom`) читает VBIOS, не трогая PCI config.
pub(super) const MM_SMC_IND_INDEX_11: usize = 0x1ac;
pub(super) const MM_SMC_IND_DATA_11: usize = 0x1ad;
pub(super) const IX_ROM_INDEX: u32 = 0xc060_0010;
pub(super) const IX_ROM_DATA: u32 = 0xc060_0014;

/// `ATOM_S7_ASIC_INIT_COMPLETE_MASK`: прошивка карты (VBIOS) уже выполнила POST.
pub(super) const ATOM_S7_ASIC_INIT_COMPLETE: u32 = 0x0000_0200;

/// Окно регистров Polaris после разведки — для `amd vbios`.
pub(super) static POLARIS_MMIO: SpinLock<Option<(usize, u16, u8)>> = SpinLock::new(None);

#[must_use]
pub(super) fn is_polaris10(device: u16) -> bool {
    POLARIS10_IDS.contains(&device)
}

/// `ASICID_IS_P20` (`amdgpu.h`): определяет «kicker»-прошивку SMC.
pub(super) fn is_p20(device: u16, revision: u8) -> bool {
    (device == 0x67DF && matches!(revision, 0xE3 | 0xE4 | 0xE5 | 0xE7 | 0xEF))
        || (device == 0x6FDF && matches!(revision, 0xE7 | 0xEF | 0xFF))
}

/// `ASICID_IS_P30` (`amdgpu.h`): `polaris10_k_mc` и `polaris10_k2_smc`.
pub(super) fn is_p30(device: u16, revision: u8) -> bool {
    device == 0x67DF && matches!(revision, 0xE1 | 0xF7)
}

/// Прошивки, которые запросил бы драйвер Linux для этой карты (v6.12).
///
/// GFX берёт сначала `*_2.bin` и откатывается на `*.bin` (какой из них есть в
/// наборе `linux-firmware`, не проверено); микрокод CP/RLC/SDMA на VI грузит
/// SMC сообщением `LoadUcodes` (`AMDGPU_FW_LOAD_SMU`), поэтому ей нужен и свой
/// файл. UVD/VCE нужны только для видео.
#[must_use]
pub fn polaris_firmware(device: u16, revision: u8) -> Vec<String> {
    let mut names = Vec::new();
    for part in ["ce", "pfp", "me", "mec", "mec2"] {
        names.push(format!("polaris10_{part}_2.bin (or polaris10_{part}.bin)"));
    }
    names.push(String::from("polaris10_rlc.bin"));
    names.push(String::from("polaris10_sdma.bin"));
    names.push(String::from("polaris10_sdma1.bin"));
    names.push(String::from(if is_p30(device, revision) { "polaris10_k_mc.bin" } else { "polaris10_mc.bin" }));
    names.push(String::from(if is_p20(device, revision) {
        "polaris10_k_smc.bin"
    } else if is_p30(device, revision) {
        "polaris10_k2_smc.bin"
    } else {
        "polaris10_smc.bin"
    }));
    names.push(String::from("polaris10_uvd.bin (video only)"));
    names.push(String::from("polaris10_vce.bin (video only)"));
    names
}

/// Читает регистры Polaris, ничего не записывая.
pub(super) fn polaris(device: &crate::pci::Device, lines: &mut Vec<String>) {
    let Some(phys) = device.memory_bar(5) else {
        say(lines, String::from("no register window (BAR5) - nothing to read"));
        return;
    };
    let mmio = phys.to_direct_map();
    // SAFETY: окно регистров устройства (BAR5); ядро по нему не исполняется.
    // Отображение на запись нужно только для `amd vbios` (индексные регистры).
    if unsafe { crate::arch::map_active(mmio, phys, MMIO_LEN_VI, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
        say(lines, format!("could not map BAR5 at {:#x}", phys.as_u64()));
        return;
    }
    let window = Window(mmio.as_usize());
    let reg = |index: usize| window.rd(index * 4);
    let memsize_raw = reg(MM_CONFIG_MEMSIZE);
    if memsize_raw == u32::MAX {
        say(lines, String::from("BAR5 answers all ones - memory decoding is off or the card sleeps"));
        return;
    }
    // `gmc_v8_0_mc_init`: значение — МиБ; при мусоре в старших 16 разрядах
    // остаются младшие.
    let memsize = if memsize_raw & 0xffff_0000 != 0 && memsize_raw & 0xffff != 0 { memsize_raw & 0xffff } else { memsize_raw };
    say(
        lines,
        format!(
            "Polaris 10 (VI, GFX8) {:04x} rev {:02x}: CONFIG_MEMSIZE {memsize} MiB, CONFIG_APER_SIZE {:#x}, BIF_FB_EN {:#x}",
            device.device,
            device.revision,
            reg(MM_CONFIG_APER_SIZE),
            reg(MM_BIF_FB_EN)
        ),
    );
    let scratch = reg(MM_BIOS_SCRATCH_7);
    say(
        lines,
        format!(
            "BIOS_SCRATCH_7 {scratch:#010x}: card {} by firmware (Linux {} POST)",
            if scratch & ATOM_S7_ASIC_INIT_COMPLETE != 0 { "initialised" } else { "NOT initialised" },
            if scratch & ATOM_S7_ASIC_INIT_COMPLETE != 0 { "skips" } else { "would run" }
        ),
    );
    let fb = reg(MM_MC_VM_FB_LOCATION);
    say(
        lines,
        format!(
            "MC_VM_FB_LOCATION {fb:#010x}: VRAM in GPU space {:#x}..={:#x}",
            u64::from(fb & 0xffff) << 24,
            (u64::from(fb >> 16) << 24) | 0xff_ffff
        ),
    );
    say(
        lines,
        format!(
            "VM_L2_CNTL {:#010x}, VM_CONTEXT0_CNTL {:#010x}, IH_RB_CNTL {:#010x}",
            reg(MM_VM_L2_CNTL),
            reg(MM_VM_CONTEXT0_CNTL),
            reg(MM_IH_RB_CNTL)
        ),
    );
    let grbm = reg(MM_GRBM_STATUS);
    say(
        lines,
        format!(
            "GRBM_STATUS {grbm:#010x} ({}), GRBM_STATUS2 {:#010x}, SRBM_STATUS {:#010x}, SRBM_STATUS2 {:#010x}",
            // GUI_ACTIVE — бит 31 (`gfx_8_0_sh_mask.h`).
            if grbm >> 31 == 0 { "gfx idle" } else { "gfx busy" },
            reg(MM_GRBM_STATUS2),
            reg(MM_SRBM_STATUS),
            reg(MM_SRBM_STATUS2)
        ),
    );
    let rlc = reg(MM_RLC_CNTL);
    let me = reg(MM_CP_ME_CNTL);
    say(
        lines,
        format!(
            "RLC_CNTL {rlc:#010x} (RLC {}), CP_ME_CNTL {me:#010x} (CE {}, PFP {}, ME {})",
            if rlc & 1 != 0 { "running" } else { "stopped" },
            if me & (1 << 24) != 0 { "halted" } else { "running" },
            if me & (1 << 26) != 0 { "halted" } else { "running" },
            if me & (1 << 28) != 0 { "halted" } else { "running" }
        ),
    );
    say(
        lines,
        format!(
            "SDMA0_F32_CNTL {:#010x}, SDMA1_F32_CNTL {:#010x} (bit 0 = halted)",
            reg(MM_SDMA0_F32_CNTL),
            reg(MM_SDMA1_F32_CNTL)
        ),
    );
    say(
        lines,
        format!(
            "GB_ADDR_CONFIG {:#010x}, CC_RB_BACKEND_DISABLE {:#010x}, CC_GC_SHADER_ARRAY_CONFIG {:#010x}",
            reg(MM_GB_ADDR_CONFIG),
            reg(MM_CC_RB_BACKEND_DISABLE),
            reg(MM_CC_GC_SHADER_ARRAY_CONFIG)
        ),
    );
    say(lines, format!("firmware Linux would load: {}", polaris_firmware(device.device, device.revision).join(", ")));
    *POLARIS_MMIO.lock() = Some((mmio.as_usize(), device.device, device.revision));
    *POLARIS_BAR0.lock() = device.memory_bar(0).map(|bar| bar.as_u64());
    // Состояние SMC — тоже на загрузке (19.11: клавиатура на живой флешке не
    // всегда опознаётся, а команду `amd smc` без неё не набрать). Только
    // чтение, плюс индекс косвенного доступа, как у `amd smc`.
    for line in polaris_smc() {
        say(lines, format!("smc: {line}"));
    }
}

/// `amd vbios`: прочитать VBIOS Polaris из ПЗУ карты через SMC, как
/// `vi_read_bios_from_rom`. Пишет только индексные регистры SMC (BAR5), в PCI
/// config не пишет; выполняется по команде, а не на загрузке.
#[must_use]
pub fn polaris_vbios() -> Vec<String> {
    let mut lines = Vec::new();
    let Some((base, _, _)) = *POLARIS_MMIO.lock() else {
        lines.push(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // Каждое чтение начинается с нулевого индекса ПЗУ; ROM_DATA сам
    // увеличивает его после каждого dword.
    let read = |bytes: usize| -> Vec<u8> {
        window.wr(MM_SMC_IND_INDEX_11 * 4, IX_ROM_INDEX);
        window.wr(MM_SMC_IND_DATA_11 * 4, 0);
        window.wr(MM_SMC_IND_INDEX_11 * 4, IX_ROM_DATA);
        let mut out = Vec::with_capacity(bytes);
        for _ in 0..bytes.div_ceil(4) {
            out.extend_from_slice(&window.rd(MM_SMC_IND_DATA_11 * 4).to_le_bytes());
        }
        out.truncate(bytes);
        out
    };
    let head = read(64);
    if head.first() != Some(&0x55) || head.get(1) != Some(&0xAA) {
        lines.push(format!("ROM has no 55AA signature (first bytes {:02x?})", &head[..8.min(head.len())]));
        return lines;
    }
    // Длина образа — в 512-байтных блоках (байт 2), как у `amdgpu_read_bios_from_rom`.
    let length = (usize::from(head[2]) << 9).clamp(512, 512 * 1024);
    let bios = read(length);
    lines.extend(atom_summary(&bios));
    lines
}

// --- SMC (микроконтроллер управления питанием), только чтение ---------------
//
// Этап P2, шаг 1. Источники — `smu7_smumgr.c` (`smu7_is_smc_ram_running`,
// `smu7_send_msg_to_smc`), `polaris10_smumgr.c` (`polaris10_start_smu`),
// `smu_7_1_3_d.h`/`smu_7_1_3_sh_mask.h`, `smu7_ppsmc.h` (Linux 6.12).
//
// Драйвер Linux запускает SMC (заливает образ в её SRAM) только если она ещё
// **не** работает: ck_disable == 0 и счётчик команд >= 0x20100. После POST
// прошивкой видеокарты (VBIOS `ASIC_Init`) она обычно уже запущена, и тогда
// остаётся лишь слать ей сообщения (`LoadUcodes` и т.д.). Этот шаг как раз
// выясняет, так ли это на живой карте, и ничего не пишет, кроме индекса
// косвенного доступа `SMC_IND_INDEX_11`.

pub(super) const MM_SMC_MESSAGE_0: usize = 0x94;
pub(super) const MM_SMC_RESP_0: usize = 0x95;
pub(super) const MM_SMC_MSG_ARG_0: usize = 0xa4;
pub(super) const IX_SMC_SYSCON_RESET_CNTL: u32 = 0x8000_0000;
pub(super) const IX_SMC_SYSCON_CLOCK_CNTL_0: u32 = 0x8000_0004;
pub(super) const IX_SMC_SYSCON_MISC_CNTL: u32 = 0x8000_0010;
pub(super) const IX_SMC_SYSCON_MSG_ARG_0: u32 = 0x8000_0068;
pub(super) const IX_SMC_PC_C: u32 = 0x8000_0370;
pub(super) const IX_RCU_UC_EVENTS: u32 = 0xc000_0004;
pub(super) const IX_SMU_STATUS: u32 = 0xe000_3088;
pub(super) const IX_SMU_FIRMWARE: u32 = 0xe000_30a4;
/// Слово в SRAM SMC, которое Linux ждёт после старта (`FIRMWARE_FLAGS`, бит 0
/// `INTERRUPTS_ENABLED`).
pub(super) const IX_FIRMWARE_FLAGS: u32 = 0x3f000;
/// Порог счётчика команд, с которого драйвер считает SMC запущенной.
pub(super) const SMC_PC_RUNNING: u32 = 0x20100;

impl Window {
    /// Прочитать регистр SMC по косвенному адресу `ix` — как
    /// `cgs_read_ind_register(CGS_IND_REG__SMC, ix)`.
    pub(super) fn smc_read(&self, ix: u32) -> u32 {
        self.wr(MM_SMC_IND_INDEX_11 * 4, ix);
        self.rd(MM_SMC_IND_DATA_11 * 4)
    }
}

/// `amd smc`: состояние SMC (микроконтроллера питания) Polaris. Только чтение.
#[must_use]
pub fn polaris_smc() -> Vec<String> {
    let mut lines = Vec::new();
    let Some((base, _, _)) = *POLARIS_MMIO.lock() else {
        lines.push(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    let reset = window.smc_read(IX_SMC_SYSCON_RESET_CNTL);
    let clock = window.smc_read(IX_SMC_SYSCON_CLOCK_CNTL_0);
    let misc = window.smc_read(IX_SMC_SYSCON_MISC_CNTL);
    let pc = window.smc_read(IX_SMC_PC_C);
    let events = window.smc_read(IX_RCU_UC_EVENTS);
    let status = window.smc_read(IX_SMU_STATUS);
    let firmware = window.smc_read(IX_SMU_FIRMWARE);
    let flags = window.smc_read(IX_FIRMWARE_FLAGS);
    let msg_arg = window.smc_read(IX_SMC_SYSCON_MSG_ARG_0);
    lines.push(format!(
        "SMC_SYSCON_RESET_CNTL {reset:#010x} (rst_reg {}), CLOCK_CNTL_0 {clock:#010x} (ck_disable {}), MISC_CNTL {misc:#010x}",
        reset & 1,
        clock & 1
    ));
    lines.push(format!("SMC_PC_C {pc:#010x} (running threshold {SMC_PC_RUNNING:#x})"));
    // `smu7_is_smc_ram_running`.
    let running = clock & 1 == 0 && pc >= SMC_PC_RUNNING;
    lines.push(format!(
        "SMC {}: Linux would {} the SMC firmware image",
        if running { "is RUNNING" } else { "is NOT running" },
        if running { "NOT upload" } else { "upload" }
    ));
    lines.push(format!(
        "RCU_UC_EVENTS {events:#010x} (boot_seq_done {}, interrupts_enabled {}), SMU_STATUS {status:#010x} (done {}, pass {})",
        (events >> 7) & 1,
        (events >> 16) & 1,
        status & 1,
        (status >> 1) & 1
    ));
    lines.push(format!(
        "SMU_FIRMWARE {firmware:#010x} (smu_mode {} = {}, smu_sel {}), FIRMWARE_FLAGS(SRAM 0x3f000) {flags:#010x} (interrupts_enabled {})",
        (firmware >> 16) & 1,
        if (firmware >> 16) & 1 == 0 { "non-protected" } else { "protected" },
        (firmware >> 17) & 1,
        flags & 1
    ));
    // Почтовый ящик: ответ 1 = OK, 0xFE = неизвестная команда, 0xFF = сбой.
    lines.push(format!(
        "mailbox: SMC_MESSAGE_0 {:#x}, SMC_RESP_0 {:#x}, SMC_MSG_ARG_0 {:#x}, SMC_SYSCON_MSG_ARG_0 {msg_arg:#x}",
        window.rd(MM_SMC_MESSAGE_0 * 4),
        window.rd(MM_SMC_RESP_0 * 4),
        window.rd(MM_SMC_MSG_ARG_0 * 4)
    ));
    lines
}
