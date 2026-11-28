//! Запуск SMC (микроконтроллера питания) образом с флешки.

use super::*;

// --- Запуск SMC (`amd smc start`) -------------------------------------------
//
// Этап P2, шаг 2 — первый, который **пишет** в карту. Повторяет защищённый
// путь `polaris10_start_smu_in_protection_mode` (`polaris10_smumgr.c`,
// Linux 6.12) и `smu7_upload_smu_firmware_image`/`smu7_upload_smc_firmware_data`
// (`smu7_smumgr.c`). Выполняется только по команде, не на загрузке.
//
// Отказывается, если: SMC уже работает (как `polaris10_start_smu`), режим не
// защищённый или ключ `smu_sel` не 1 (для нас лежит только `polaris10_k_smc.bin`
// — образ для `smu_sel` = 1), образа нет на флешке (`FREEOS/AMD/SMC.BIN`) или
// он не прошёл проверку заголовка. При сбое пытается вернуть SMC в прежнее
// состояние — снять сброс (`rst_reg` = 0): тогда она снова исполняет ПЗУ, как
// до команды.

pub(super) const MM_SMC_IND_ACCESS_CNTL: usize = 0x92;
/// `AUTO_INCREMENT_IND_11` — бит 11 `SMC_IND_ACCESS_CNTL`.
pub(super) const AUTO_INCREMENT_IND_11: u32 = 1 << 11;
/// SRAM SMC, куда кладётся образ (`smu7_upload_smc_firmware_data`).
pub(super) const SMC_IMAGE_ADDRESS: u32 = 0x20000;
/// Предел образа (`SMU7_SMC_SIZE`).
pub(super) const SMC_IMAGE_LIMIT: usize = 0x20000;
/// `PPSMC_MSG_Test` — сообщение, запускающее проверку образа (`smu7_ppsmc.h`).
pub(super) const PPSMC_MSG_TEST: u32 = 0x100;
pub(super) const FIRMWARE_DIR: [&str; 4] = ["/usb", "/usb1", "/usb2", "/usb3"];

impl Window {
    pub(super) fn smc_write(&self, ix: u32, value: u32) {
        self.wr(MM_SMC_IND_INDEX_11 * 4, ix);
        self.wr(MM_SMC_IND_DATA_11 * 4, value);
    }

    /// Прочитать-изменить-записать регистр SMC (`PHM_WRITE_VFPF_INDIRECT_FIELD`).
    pub(super) fn smc_modify(&self, ix: u32, clear: u32, set: u32) {
        let value = self.smc_read(ix);
        self.smc_write(ix, (value & !clear) | set);
    }

    /// Ждать, пока `ready(значение регистра SMC)` не станет истинным, до `ms`.
    pub(super) fn smc_wait(&self, ix: u32, ms: u32, ready: impl Fn(u32) -> bool) -> bool {
        for _ in 0..ms * 10 {
            if ready(self.smc_read(ix)) {
                return true;
            }
            super::super::nvidia::spin_us(100);
        }
        ready(self.smc_read(ix))
    }

    /// Сообщение SMC с параметром (`smu7_send_msg_to_smc_with_parameter`).
    /// Возвращает ответ из `SMC_RESP_0` (1 — выполнено).
    pub(super) fn smc_message(&self, message: u32, argument: u32) -> Result<u32, String> {
        let wait_response = |what: &str| -> Result<u32, String> {
            for _ in 0..10_000 {
                let response = self.rd(MM_SMC_RESP_0 * 4) & 0xffff;
                if response != 0 {
                    return Ok(response);
                }
                super::super::nvidia::spin_us(100);
            }
            Err(format!("SMC did not answer ({what})"))
        };
        wait_response("before the message")?;
        self.wr(MM_SMC_MSG_ARG_0 * 4, argument);
        self.wr(MM_SMC_RESP_0 * 4, 0);
        self.wr(MM_SMC_MESSAGE_0 * 4, message);
        wait_response("after the message")
    }
}

/// Файл прошивки со флешки (`/usb*/FREEOS/AMD/<имя>`).
pub(super) fn firmware_file(name: &str) -> Result<Vec<u8>, String> {
    for root in FIRMWARE_DIR {
        let path = format!("{root}/FREEOS/AMD/{name}");
        let Some(Ok(node)) =
            crate::fs::resolve_as(crate::vfs::perm::Credentials::ROOT, &path, crate::vfs::perm::Access::NONE)
        else {
            continue;
        };
        let size = node.metadata().size as usize;
        let mut data = alloc::vec![0u8; size];
        let mut done = 0;
        while done < size {
            let n = node.read_at(done as u64, &mut data[done..]).map_err(|error| format!("read {path}: {error:?}"))?;
            if n == 0 {
                return Err(format!("{path} ended after {done} of {size} bytes"));
            }
            done += n;
        }
        return Ok(data);
    }
    Err(format!("FREEOS/AMD/{name} not found on the stick (/usb*)"))
}

/// Проверить заголовок образа SMC и вернуть срез с кодом.
/// Заголовок — `smc_firmware_header_v1_0`: общая часть (32 байта) +
/// `ucode_start_addr`.
pub(super) fn smc_image(file: &[u8]) -> Result<&[u8], String> {
    let size = u32_at(file, 0).ok_or("firmware too short")? as usize;
    if size != file.len() {
        return Err(format!("header says {size} bytes, file has {}", file.len()));
    }
    let (ip_major, ip_minor) = (u16_at(file, 12).ok_or("short")?, u16_at(file, 14).ok_or("short")?);
    if (ip_major, ip_minor) != (7, 2) {
        return Err(format!("not an SMC 7.2 image (ip version {ip_major}.{ip_minor})"));
    }
    let code_size = u32_at(file, 20).ok_or("short")? as usize;
    let code_offset = u32_at(file, 24).ok_or("short")? as usize;
    if code_size == 0 || code_size % 4 != 0 || code_size > SMC_IMAGE_LIMIT {
        return Err(format!("image size {code_size} is not a multiple of 4 within 1..={SMC_IMAGE_LIMIT}"));
    }
    file.get(code_offset..code_offset + code_size).ok_or_else(|| format!("image {code_offset}+{code_size} runs past the file"))
}

/// `amd smc start`: залить образ в SRAM SMC и запустить её.
///
/// Каждый шаг пишется строкой — и в результат, и в журнал ядра, чтобы при
/// зависании по `LOG.TXT` было видно, где остановились.
#[must_use]
pub fn polaris_smc_start() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : smc start: {text}");
        lines.push(text);
    };
    let Some((base, device, revision)) = *POLARIS_MMIO.lock() else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // Условия отказа — до первой записи.
    let clock = window.smc_read(IX_SMC_SYSCON_CLOCK_CNTL_0);
    let pc = window.smc_read(IX_SMC_PC_C);
    if clock & 1 == 0 && pc >= SMC_PC_RUNNING {
        note(format!("SMC already runs (PC {pc:#x}) - nothing to do"));
        return lines;
    }
    let firmware = window.smc_read(IX_SMU_FIRMWARE);
    let (protected, select) = ((firmware >> 16) & 1, (firmware >> 17) & 1);
    if protected != 1 || select != 1 {
        note(format!(
            "SMU_FIRMWARE {firmware:#010x}: smu_mode {protected}, smu_sel {select} - only protected mode with smu_sel 1 is implemented"
        ));
        return lines;
    }
    if !is_p20(device, revision) {
        note(format!("{device:04x} rev {revision:02x} is not a P20 card: polaris10_k_smc.bin is not its image"));
        return lines;
    }
    let file = match firmware_file("SMC.BIN") {
        Ok(file) => file,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    let image = match smc_image(&file) {
        Ok(image) => image,
        Err(why) => {
            note(format!("SMC.BIN rejected: {why}"));
            return lines;
        }
    };
    note(format!("image {} bytes from FREEOS/AMD/SMC.BIN, PC before {pc:#x}", image.len()));

    // Любой сбой дальше: вернуть SMC в состояние ПЗУ (снять сброс) и остановиться.
    let fail = |window: &Window, note: &mut dyn FnMut(String), why: String| {
        window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 1, 0);
        note(format!("FAILED: {why}; reset released, SMC left in ROM"));
    };

    // 1. Сброс SMC.
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 0, 1);
    note(String::from("1. rst_reg = 1 (SMC held in reset)"));
    // 2. Образ — в SRAM с адреса 0x20000, автоинкремент индекса.
    window.wr(MM_SMC_IND_INDEX_11 * 4, SMC_IMAGE_ADDRESS);
    let access = window.rd(MM_SMC_IND_ACCESS_CNTL * 4);
    window.wr(MM_SMC_IND_ACCESS_CNTL * 4, access | AUTO_INCREMENT_IND_11);
    for word in image.chunks_exact(4) {
        window.wr(MM_SMC_IND_DATA_11 * 4, u32::from_le_bytes([word[0], word[1], word[2], word[3]]));
    }
    let access = window.rd(MM_SMC_IND_ACCESS_CNTL * 4);
    window.wr(MM_SMC_IND_ACCESS_CNTL * 4, access & !AUTO_INCREMENT_IND_11);
    note(format!("2. {} bytes written to SMC SRAM at {SMC_IMAGE_ADDRESS:#x}", image.len()));
    // 3-5. Сбросить статус, включить такт, снять сброс.
    window.smc_write(IX_SMU_STATUS, 0);
    window.smc_modify(IX_SMC_SYSCON_CLOCK_CNTL_0, 1, 0);
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 1, 0);
    note(String::from("3-5. SMU_STATUS = 0, ck_disable = 0, rst_reg = 0 (SMC released)"));
    // 6. Ждать флаг прерываний RCU.
    if !window.smc_wait(IX_RCU_UC_EVENTS, 500, |value| (value >> 16) & 1 == 1) {
        fail(&window, &mut note, String::from("RCU_UC_EVENTS.interrupts_enabled never set"));
        return lines;
    }
    note(String::from("6. RCU_UC_EVENTS.interrupts_enabled = 1"));
    // 7. Сообщение «Test» с параметром 0x20000 запускает проверку образа.
    match window.smc_message(PPSMC_MSG_TEST, SMC_IMAGE_ADDRESS) {
        Ok(response) => note(format!("7. message Test({SMC_IMAGE_ADDRESS:#x}) answered {response:#x}")),
        Err(why) => {
            fail(&window, &mut note, why);
            return lines;
        }
    }
    // 8. Ждать результат проверки: SMU_STATUS.done, затем .pass.
    if !window.smc_wait(IX_SMU_STATUS, 500, |value| value & 1 != 0) {
        fail(&window, &mut note, String::from("SMU_STATUS.done never set"));
        return lines;
    }
    let status = window.smc_read(IX_SMU_STATUS);
    if (status >> 1) & 1 != 1 {
        fail(&window, &mut note, format!("SMU_STATUS {status:#010x}: image check did not pass"));
        return lines;
    }
    note(format!("8. SMU_STATUS {status:#010x}: image check passed"));
    // 9-10. Сбросить флаги прошивки и перезапустить SMC.
    window.smc_write(IX_FIRMWARE_FLAGS, 0);
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 0, 1);
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 1, 0);
    note(String::from("9-10. FIRMWARE_FLAGS = 0, SMC reset pulse"));
    // 11. Ждать, пока прошивка скажет, что готова.
    if !window.smc_wait(IX_FIRMWARE_FLAGS, 1000, |value| value & 1 == 1) {
        fail(&window, &mut note, String::from("FIRMWARE_FLAGS.interrupts_enabled never set"));
        return lines;
    }
    let pc = window.smc_read(IX_SMC_PC_C);
    note(format!(
        "11. firmware is up: PC {pc:#x} ({})",
        if pc >= SMC_PC_RUNNING { "above the running threshold" } else { "BELOW the running threshold" }
    ));
    lines
}
