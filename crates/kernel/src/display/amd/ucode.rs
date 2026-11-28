//! Микрокод CP/RLC/SDMA через SMC.

use super::*;

// --- Загрузка микрокода CP/RLC/SDMA через SMC (`amd ucode`) ----------------
//
// Этап P3. Источники — `smu7_smumgr.c` (`smu7_request_smu_load_fw`,
// `smu7_populate_single_firmware_entry`, `smu7_check_fw_load_finish`),
// `smu_ucode_xfer_vi.h`, `smu7_ppsmc.h`, `smu74.h`, `amdgpu_ucode.c`
// (`amdgpu_ucode_init_bo`), `amdgpu_cgs.c` (`amdgpu_cgs_get_firmware_info`)
// из Linux 6.12.
//
// Отличие от Linux: драйвер кладёт образы в GTT (через GART), мы — в
// видеопамять (так делает режим `debug_use_vram_fw_buf`): адреса VRAM попадают
// в системную апертуру MC, и таблица страниц GPU не нужна. Область — 2 МиБ со
// смещения `UCODE_AREA` окна VRAM (BAR0); GPU-адрес = `vram_start` + смещение.
// Раскладка: TOC (4 КиБ), буфер SMU (800 КиБ, содержимое не заполняется), затем
// образы по страницам: SDMA0, SDMA1, CE, PFP, ME, MEC (+ страница с JT), RLC.
//
// Пишет в видеопамять и просит SMC загрузить микрокод в RLC/CP/SDMA; MC,
// SMC и сами блоки — по их состоянию после `amd smc start`.

pub(super) const MM_HDP_MEM_COHERENCY_FLUSH_CNTL: usize = 0x1520;
pub(super) const MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR: usize = 0x80d;
pub(super) const MM_MC_VM_SYSTEM_APERTURE_HIGH_ADDR: usize = 0x80e;
pub(super) const MM_MC_VM_SYSTEM_APERTURE_DEFAULT_ADDR: usize = 0x80f;
pub(super) const PPSMC_MSG_DRV_DRAM_ADDR_HI: u32 = 0x250;
pub(super) const PPSMC_MSG_DRV_DRAM_ADDR_LO: u32 = 0x251;
pub(super) const PPSMC_MSG_SMU_DRAM_ADDR_HI: u32 = 0x252;
pub(super) const PPSMC_MSG_SMU_DRAM_ADDR_LO: u32 = 0x253;
pub(super) const PPSMC_MSG_LOAD_UCODES: u32 = 0x254;
/// Указатель на мягкие регистры в заголовке прошивки SMC: `SMU7_FIRMWARE_HEADER_LOCATION`
/// (0x20000) + смещение `SoftRegisters` в `SMU74_Firmware_Header` (48).
pub(super) const SMC_SOFT_REGISTERS_POINTER: u32 = 0x20000 + 48;
/// `UcodeLoadStatus` в `SMU74_SoftRegisters`.
pub(super) const UCODE_LOAD_STATUS_OFFSET: u32 = 0x6c;
/// Что просим загрузить: RLC_G 0x400 + SDMA0 0x2 + SDMA1 0x4 + CE 0x8 + PFP 0x10 + ME 0x20 + MEC 0x40.
pub(super) const FW_TO_LOAD: u32 = 0x47e;
/// Номера `UCODE_ID_*` для записей TOC.
pub(super) const UCODE_ID_SDMA0: u16 = 1;
pub(super) const UCODE_ID_SDMA1: u16 = 2;
pub(super) const UCODE_ID_CP_CE: u16 = 3;
pub(super) const UCODE_ID_CP_PFP: u16 = 4;
pub(super) const UCODE_ID_CP_ME: u16 = 5;
pub(super) const UCODE_ID_CP_MEC: u16 = 6;
pub(super) const UCODE_ID_CP_MEC_JT1: u16 = 7;
pub(super) const UCODE_ID_CP_MEC_JT2: u16 = 8;
pub(super) const UCODE_ID_RLC_G: u16 = 10;
/// Смещение области в окне VRAM (BAR0): далеко от кадрового буфера прошивки
/// (он в начале видимой части) и внутри 256 МиБ видимого окна.
pub(super) const UCODE_AREA: u64 = 0x0c00_0000;
pub(super) const UCODE_AREA_LEN: usize = 2 * 1024 * 1024;
pub(super) const TOC_SIZE: usize = 4096;
pub(super) const SMU_BUFFER_SIZE: usize = 200 * 4096;
pub(super) const PAGE: usize = 4096;

/// Состояние работающего кольца GFX — для следующих этапов (`amd gart` и далее).
pub(super) struct RingState {
    /// Виртуальный адрес окна VRAM с кольцом.
    pub(super) vram: usize,
    /// `vram_start`: GPU-адрес начала видеопамяти.
    pub(super) vram_start: u64,
    /// Указатель записи в dword (0..RING_DWORDS).
    pub(super) wptr: u32,
}

pub(super) static RING_STATE: SpinLock<Option<RingState>> = SpinLock::new(None);

/// Окно VRAM (BAR0, физический адрес) — для `amd ucode`.
pub(super) static POLARIS_BAR0: SpinLock<Option<u64>> = SpinLock::new(None);

pub(super) const fn align_page(value: usize) -> usize {
    (value + PAGE - 1) & !(PAGE - 1)
}

/// Образ микрокода из файла `.bin`: сам код, версия и (для CP) таблица переходов.
pub(super) struct Ucode {
    pub(super) version: u32,
    /// `ucode_size_bytes` заголовка — размер кода (для MEC в нём и JT).
    pub(super) code: Vec<u8>,
    /// `jt_offset` и `jt_size` в dword (у CE/PFP/ME/MEC/RLC).
    pub(super) jt_offset: u32,
    pub(super) jt_size: u32,
}

/// Разобрать `common_firmware_header` + поля `gfx_firmware_header_v1_0`.
pub(super) fn parse_ucode(file: &[u8]) -> Result<Ucode, String> {
    let size = u32_at(file, 0).ok_or("short")? as usize;
    if size != file.len() {
        return Err(format!("header says {size} bytes, file has {}", file.len()));
    }
    let version = u32_at(file, 16).ok_or("short")?;
    let code_size = u32_at(file, 20).ok_or("short")? as usize;
    let code_offset = u32_at(file, 24).ok_or("short")? as usize;
    let code = file
        .get(code_offset..code_offset + code_size)
        .ok_or_else(|| format!("code {code_offset}+{code_size} runs past the file"))?
        .to_vec();
    Ok(Ucode { version, code, jt_offset: u32_at(file, 36).unwrap_or(0), jt_size: u32_at(file, 40).unwrap_or(0) })
}

/// Запись `SMU_Entry` (28 байт): id, version, адрес образа, метаданные (нули),
/// размер, флаги, число регистровых записей.
pub(super) fn toc_entry(id: u16, version: u32, address: u64, size: usize, flags: u16) -> [u8; 28] {
    let mut entry = [0u8; 28];
    entry[0..2].copy_from_slice(&id.to_le_bytes());
    entry[2..4].copy_from_slice(&(version as u16).to_le_bytes());
    entry[4..8].copy_from_slice(&((address >> 32) as u32).to_le_bytes());
    entry[8..12].copy_from_slice(&(address as u32).to_le_bytes());
    entry[20..24].copy_from_slice(&(size as u32).to_le_bytes());
    entry[24..26].copy_from_slice(&flags.to_le_bytes());
    entry
}

/// Записать байты в видеопамять через окно BAR0 32-битными словами.
pub(super) fn write_vram(base: usize, offset: usize, bytes: &[u8]) {
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        // SAFETY: адрес внутри отображённого окна VRAM (`UCODE_AREA_LEN`),
        // проверено вызывающим; запись 32-битная выровненная.
        unsafe { core::ptr::write_volatile((base + offset + index * 4) as *mut u32, u32::from_le_bytes(word)) };
    }
}

/// `amd ucode`: загрузить микрокод RLC/CP/SDMA через SMC (`LoadUcodes`).
///
/// Требует работающей SMC (`amd smc start`) и прошивок `FREEOS/AMD/{CE,PFP,ME,
/// MEC,RLC,SDMA0,SDMA1}.BIN` на флешке. Каждый шаг — строка и в результат, и в журнал.
#[must_use]
pub fn polaris_ucode() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : ucode: {text}");
        lines.push(text);
    };
    let (Some((base, _, _)), Some(bar0)) = (*POLARIS_MMIO.lock(), *POLARIS_BAR0.lock()) else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // 0. Условия: SMC работает, обращения хоста к VRAM разрешены.
    let clock = window.smc_read(IX_SMC_SYSCON_CLOCK_CNTL_0);
    let pc = window.smc_read(IX_SMC_PC_C);
    if clock & 1 != 0 || pc < SMC_PC_RUNNING {
        note(format!("SMC is not running (PC {pc:#x}) - run 'amd smc start' first"));
        return lines;
    }
    if window.rd(MM_BIF_FB_EN * 4) & 3 != 3 {
        note(String::from("BIF_FB_EN: host access to VRAM is off"));
        return lines;
    }
    let fb = window.rd(MM_MC_VM_FB_LOCATION * 4);
    let vram_start = u64::from(fb & 0xffff) << 24;
    let vram_end = (u64::from(fb >> 16) << 24) | 0xff_ffff;
    // 1. Образы с флешки.
    let load = |name: &str| -> Result<Ucode, String> { parse_ucode(&firmware_file(name)?).map_err(|why| format!("{name}: {why}")) };
    let images = (|| -> Result<[Ucode; 7], String> {
        Ok([load("SDMA0.BIN")?, load("SDMA1.BIN")?, load("CE.BIN")?, load("PFP.BIN")?, load("ME.BIN")?, load("MEC.BIN")?, load("RLC.BIN")?])
    })();
    let [sdma0, sdma1, ce, pfp, me, mec, rlc] = match images {
        Ok(images) => images,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    // MEC2 нужен только для версии записи JT2 (сам образ SMC не получает).
    let mec2_version = firmware_file("MEC2.BIN").ok().and_then(|file| parse_ucode(&file).ok()).map_or(mec.version, |u| u.version);
    // 2. Раскладка в области: TOC, буфер SMU, образы по страницам.
    let toc_offset = 0usize;
    let smu_offset = TOC_SIZE;
    let mut cursor = TOC_SIZE + SMU_BUFFER_SIZE;
    let mut place = |len: usize| {
        let at = cursor;
        cursor += align_page(len);
        at
    };
    let at_sdma0 = place(sdma0.code.len());
    let at_sdma1 = place(sdma1.code.len());
    let at_ce = place(ce.code.len());
    let at_pfp = place(pfp.code.len());
    let at_me = place(me.code.len());
    let at_mec = place(mec.code.len());
    let jt_bytes = (mec.jt_size as usize) * 4;
    let at_jt = place(jt_bytes.max(1));
    let at_rlc = place(rlc.code.len());
    let total = cursor;
    if total > UCODE_AREA_LEN {
        note(format!("images need {total} bytes, the area has {UCODE_AREA_LEN}"));
        return lines;
    }
    let jt_start = (mec.jt_offset as usize) * 4;
    let Some(jt) = mec.code.get(jt_start..jt_start + jt_bytes) else {
        note(format!("MEC jump table {jt_start}+{jt_bytes} runs past the code ({} bytes)", mec.code.len()));
        return lines;
    };
    let mc = |offset: usize| vram_start + UCODE_AREA + offset as u64;
    note(format!(
        "VRAM GPU {vram_start:#x}..={vram_end:#x}, area {:#x}.. ({} KiB): TOC {:#x}, SMU buffer {:#x}, images up to {:#x}",
        mc(0),
        total / 1024,
        mc(toc_offset),
        mc(smu_offset),
        mc(total)
    ));
    // 3. Системная апертура MC должна покрывать адреса VRAM (Linux задаёт её в `mc_program`).
    let low = window.rd(MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR * 4);
    let high = window.rd(MM_MC_VM_SYSTEM_APERTURE_HIGH_ADDR * 4);
    let default = window.rd(MM_MC_VM_SYSTEM_APERTURE_DEFAULT_ADDR * 4);
    let (first, last) = (vram_start >> 12, vram_end >> 12);
    let covered = u64::from(low) <= first && u64::from(high) >= last && low <= high;
    note(format!(
        "system aperture LOW {low:#x} HIGH {high:#x} DEFAULT {default:#x} (VRAM pages {first:#x}..={last:#x}): {}",
        if covered { "covers VRAM" } else { "does NOT cover VRAM - programming it like mc_program" }
    ));
    if !covered {
        window.wr(MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR * 4, first as u32);
        window.wr(MM_MC_VM_SYSTEM_APERTURE_HIGH_ADDR * 4, last as u32);
        window.wr(MM_MC_VM_SYSTEM_APERTURE_DEFAULT_ADDR * 4, (mc(UCODE_AREA_LEN - PAGE) >> 12) as u32);
    }
    // 4. Окно VRAM под область и запись образов.
    let phys = crate::mm::PhysAddr::new(bar0 + UCODE_AREA);
    let virt = phys.to_direct_map();
    // SAFETY: окно видеопамяти (BAR0) устройства; ядро по нему не исполняется.
    if unsafe { crate::arch::map_active(virt, phys, UCODE_AREA_LEN, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
        note(format!("could not map the VRAM window at {:#x}", phys.as_u64()));
        return lines;
    }
    let vram = virt.as_usize();
    write_vram(vram, at_sdma0, &sdma0.code);
    write_vram(vram, at_sdma1, &sdma1.code);
    write_vram(vram, at_ce, &ce.code);
    write_vram(vram, at_pfp, &pfp.code);
    write_vram(vram, at_me, &me.code);
    write_vram(vram, at_mec, &mec.code);
    write_vram(vram, at_jt, jt);
    write_vram(vram, at_rlc, &rlc.code);
    // Проверка записи: первые слова каждого образа читаются обратно.
    let mut bad = Vec::new();
    for (name, at, code) in [("SDMA0", at_sdma0, &sdma0.code), ("CE", at_ce, &ce.code), ("PFP", at_pfp, &pfp.code), ("RLC", at_rlc, &rlc.code)] {
        let back = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        // SAFETY: чтение из отображённой области.
        let got = unsafe { core::ptr::read_volatile((vram + at) as *const u32) };
        if got != back {
            bad.push(format!("{name}: wrote {back:#x}, read back {got:#x}"));
        }
    }
    if !bad.is_empty() {
        note(format!("VRAM write check failed: {}", bad.join("; ")));
        return lines;
    }
    note(format!("images written: SDMA0 {} B, SDMA1 {} B, CE {} B, PFP {} B, ME {} B, MEC {} B (+JT {} B), RLC {} B", sdma0.code.len(), sdma1.code.len(), ce.code.len(), pfp.code.len(), me.code.len(), mec.code.len(), jt_bytes, rlc.code.len()));
    // 5. TOC: порядок записей как в `smu7_request_smu_load_fw`.
    let entries = [
        toc_entry(UCODE_ID_RLC_G, rlc.version, mc(at_rlc), rlc.code.len(), 1),
        toc_entry(UCODE_ID_CP_CE, ce.version, mc(at_ce), ce.code.len(), 0),
        toc_entry(UCODE_ID_CP_PFP, pfp.version, mc(at_pfp), pfp.code.len(), 0),
        toc_entry(UCODE_ID_CP_ME, me.version, mc(at_me), me.code.len(), 0),
        toc_entry(UCODE_ID_CP_MEC, mec.version, mc(at_mec), (mec.jt_offset as usize) * 4, 1),
        toc_entry(UCODE_ID_CP_MEC_JT1, mec.version, mc(at_mec) + align_page(mec.code.len()) as u64, jt_bytes, 0),
        toc_entry(UCODE_ID_CP_MEC_JT2, mec2_version, mc(at_mec) + align_page(mec.code.len()) as u64, jt_bytes, 0),
        toc_entry(UCODE_ID_SDMA0, sdma0.version, mc(at_sdma0), sdma0.code.len(), 0),
        toc_entry(UCODE_ID_SDMA1, sdma1.version, mc(at_sdma1), sdma1.code.len(), 0),
    ];
    let mut toc = Vec::with_capacity(8 + entries.len() * 28);
    toc.extend_from_slice(&1u32.to_le_bytes()); // structure_version
    toc.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in &entries {
        toc.extend_from_slice(entry);
    }
    // 6. Сообщения SMC в порядке `smu7_request_smu_load_fw`.
    let soft = window.smc_read(SMC_SOFT_REGISTERS_POINTER);
    if !(0x20000..0x40000).contains(&soft) {
        note(format!("soft registers pointer {soft:#x} (SRAM {SMC_SOFT_REGISTERS_POINTER:#x}) is not in the SMC RAM"));
        return lines;
    }
    let status_ix = soft + UCODE_LOAD_STATUS_OFFSET;
    window.smc_write(status_ix, 0);
    note(format!("soft registers at SRAM {soft:#x}, UcodeLoadStatus at {status_ix:#x} cleared"));
    let send = |note: &mut dyn FnMut(String), message: u32, argument: u32, what: &str| -> bool {
        match window.smc_message(message, argument) {
            Ok(1) => true,
            Ok(response) => {
                note(format!("{what}: SMC answered {response:#x} (1 = ok, 0xfe = unknown message, 0xff = failed)"));
                false
            }
            Err(why) => {
                note(format!("{what}: {why}"));
                false
            }
        }
    };
    if !send(&mut note, PPSMC_MSG_SMU_DRAM_ADDR_HI, (mc(smu_offset) >> 32) as u32, "SMU_DRAM_ADDR_HI")
        || !send(&mut note, PPSMC_MSG_SMU_DRAM_ADDR_LO, mc(smu_offset) as u32, "SMU_DRAM_ADDR_LO")
    {
        return lines;
    }
    write_vram(vram, toc_offset, &toc);
    // Записи CPU в видеопамять через BAR должны дойти до MC раньше, чем SMC начнёт читать.
    window.wr(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4, 1);
    let _ = window.rd(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4);
    note(format!("TOC written ({} entries, {} B) at {:#x}, HDP flushed", entries.len(), toc.len(), mc(toc_offset)));
    if !send(&mut note, PPSMC_MSG_DRV_DRAM_ADDR_HI, (mc(toc_offset) >> 32) as u32, "DRV_DRAM_ADDR_HI")
        || !send(&mut note, PPSMC_MSG_DRV_DRAM_ADDR_LO, mc(toc_offset) as u32, "DRV_DRAM_ADDR_LO")
        || !send(&mut note, PPSMC_MSG_LOAD_UCODES, FW_TO_LOAD, "LoadUcodes")
    {
        return lines;
    }
    note(format!("LoadUcodes({FW_TO_LOAD:#x}) sent, waiting for UcodeLoadStatus"));
    // 7. Ждать, пока SMC отметит все блоки (до 3 с; Linux ждёт 1 с).
    let done = window.smc_wait(status_ix, 3000, |value| value & FW_TO_LOAD == FW_TO_LOAD);
    let status = window.smc_read(status_ix);
    note(format!(
        "UcodeLoadStatus {status:#x} of {FW_TO_LOAD:#x}: {}",
        if done { "ALL loaded" } else { "TIMEOUT - not everything was loaded" }
    ));
    // 8. Что стало с блоками.
    let rlc_state = window.rd(MM_RLC_CNTL * 4);
    let cp = window.rd(MM_CP_ME_CNTL * 4);
    note(format!(
        "after: RLC_CNTL {rlc_state:#010x}, CP_ME_CNTL {cp:#010x}, SDMA0_F32_CNTL {:#010x}, SDMA1_F32_CNTL {:#010x}, GRBM_STATUS {:#010x}",
        window.rd(MM_SDMA0_F32_CNTL * 4),
        window.rd(MM_SDMA1_F32_CNTL * 4),
        window.rd(MM_GRBM_STATUS * 4)
    ));
    lines
}
