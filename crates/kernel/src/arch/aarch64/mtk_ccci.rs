//! CCCI поверх CCIF — разговор с модемом MT6765 (05.11, веха M4).
//!
//! # Что здесь
//!
//! На поколении 6293 управление и AT идут не через CLDMA (он только для
//! мобильного интернета), а через **CCIF0** (`0x10209000`): «звонок» по
//! номеру канала (`BUSY`/`TCHNUM`, ответ — `RCHNUM`/`ACK`), 512 байт SRAM для
//! первого обмена (HS1 и ответ runtime data) и **кольца в общей памяти**
//! (`SMEM_USER_CCISM_MCU`: восемь очередей, у каждой своё rx и tx).
//!
//! Порядок: модем после `oem md go` кладёт в SRAM запрос HS1 (заголовок с
//! `reserved = 0x5555FFFF` и 64 байта признаков) и звонит в канал 15. Мы
//! строим кольца, раскладываем ответ (runtime data — список признаков с
//! адресами общей памяти) в общей памяти, кладём в SRAM заголовок и
//! `ap_query_md_feature_v2_1`, звоним в 15. Дальше модем шлёт HS2 по кольцу
//! 0 (канал CONTROL_RX), и порты оживают: FS (14/15, очередь 4), RPC (32/33,
//! очередь 1), SYSTEM (2/3, очередь 0), AT — UART2 (10/12, очередь 5).
//!
//! Форматы и значения — из `eccci` ядра MediaTek для MT6765 (дерево
//! Mayuri-Chan `android_kernel_xiaomi_mt6765`, `10-alps`): `ccci_modem.c`
//! (`ccci_md_prepare_runtime_data`, `config_ap_side_feature`, таблица
//! `md1_6293_noncacheable_fat`), `modem_sys1.c` (`md_cd_send_runtime_data_v2`),
//! `hif/ccci_hif_ccif.c`, `hif/ccci_ringbuf.c`, `port/port_rpc.c`.
//!
//! # Память
//!
//! Общая память — некэшируемая со стороны модема, у нас она в прямом
//! отображении (кэшируемом): после записи — `dc civac`, перед чтением — тоже.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::mtk_regs::{read32, write32};
use crate::sync::SpinLock;

// CCIF0, сторона AP.
const CCIF: u64 = 0x1020_9000;
const CCIF_BUSY: u64 = CCIF + 0x04;
const CCIF_TCHNUM: u64 = CCIF + 0x0C;
const CCIF_RCHNUM: u64 = CCIF + 0x10;
const CCIF_ACK: u64 = CCIF + 0x14;
const SRAM: u64 = CCIF + 0x100;
const SRAM_SIZE: u64 = 512;
/// `ccif_sram_layout`: dl_header (16) + md_query_ap_feature (224) + up_header
/// (16) + ap_rt_data.
const SRAM_DL_HEADER: u64 = SRAM;
const SRAM_MD_FEATURE: u64 = SRAM + 16;
const SRAM_UP_HEADER: u64 = SRAM + 240;
const SRAM_AP_RT: u64 = SRAM + 256;

const CH_SRAM: u32 = 15;

// Общая память без кэша (LK: `ap_md_nc_smem`) и её вид у модема.
const NC_AP: u64 = 0x7800_0000;
const NC_MD: u32 = 0x4000_0000;
const NC_SIZE: u32 = 0x17_0000;
// С кэшем (LK: `ap_md_c_smem`, смещение у модема из тега
// `md1_smem_cahce_offset` = 224 МиБ).
const C_MD: u32 = 0x4E00_0000;
const C_SIZE: u32 = 0x160_0000;

// Области `md1_6293_noncacheable_fat` (смещение, размер).
const MDCCCI_DBG: (u32, u32) = (0, 2 * 1024);
const MDSS_DBG: (u32, u32) = (2 * 1024, 10 * 1024);
const RUNTIME_DATA: (u32, u32) = (58 * 1024, 4 * 1024);
const DBM: (u32, u32) = (64 * 1024 - 176, 176);
const CCB_CTRL: (u32, u32) = (96 * 1024, 4 * 1024);
const NETD: (u32, u32) = (100 * 1024, 4 * 1024);
const USB: (u32, u32) = (104 * 1024, 4 * 1024);
const AUDIO: (u32, u32) = (108 * 1024, 52 * 1024);
const CCISM_MCU: (u32, u32) = (160 * 1024, 721 * 1024);
const CCISM_MCU_EXP: (u32, u32) = (881 * 1024, 121 * 1024);
const LWA: (u32, u32) = (1472 * 1024, 0);
const RUNTIME_AP_SIZE: u32 = 0x800;
const EE_SMEM_TOTAL: u32 = 64 * 1024;

// Кольца: rx (модем → AP) и tx (AP → модем) по очередям, 6293.
const RX_SIZE: [u32; 8] = [80 << 10, 80 << 10, 40 << 10, 80 << 10, 20 << 10, 20 << 10, 64 << 10, 0];
const TX_SIZE: [u32; 8] = [128 << 10, 40 << 10, 8 << 10, 40 << 10, 20 << 10, 20 << 10, 64 << 10, 0];
/// Кольца исключений (`rx/tx_exp_buffer_size`) — в `CCISM_MCU_EXP`.
const RX_EXP_SIZE: [u32; 8] = [12 << 10, 32 << 10, 8 << 10, 0, 0, 0, 8 << 10, 0];
const TX_EXP_SIZE: [u32; 8] = [12 << 10, 32 << 10, 8 << 10, 0, 0, 0, 8 << 10, 0];
const RING_CTL: u32 = 40;
const RBF_HEADER: u32 = 0xEE00_00EE;
const RBF_FOOTER: u32 = 0xFF00_00FF;
const PKG_HEADER: u32 = 0xAABB_AABB;
const PKG_FOOTER: u32 = 0xCCDD_EEFF;

// Каналы (AP-номера).
const CH_CONTROL_RX: u16 = 0;
const CH_CONTROL_TX: u16 = 1;
const CH_SYSTEM_RX: u16 = 2;
const CH_SYSTEM_TX: u16 = 3;
const CH_UART2_RX: u16 = 10;
const CH_UART2_TX: u16 = 12;
const CH_FS_RX: u16 = 14;
const CH_FS_TX: u16 = 15;
const CH_PCM_RX: u16 = 4;
const CH_PCM_TX: u16 = 5;
const Q_FS: usize = 4;
const CH_RPC_RX: u16 = 32;
const CH_RPC_TX: u16 = 33;
const Q_CONTROL: usize = 0;
const Q_RPC: usize = 1;
const Q_AT: usize = 5;

const MD_INIT_CHK_ID: u32 = 0x5555_FFFF;
const MD_FEATURE_PATTERN: u32 = 0x4943_4343;
const AP_FEATURE_PATTERN: u32 = 0x4343_4349;
const MAGIC: u32 = 0xFFFF_FFFF;
const MD_GET_BATTERY_INFO: u32 = 0x105;
const RPC_RESP: u32 = 0xFFFF_0000;

/// Признаки, которые AP поддерживает (как `config_ap_side_feature` для 6293
/// без SCP, PHY_CAP, UDC и 32K_LESS): 2 — обязательно, 1 — не поддерживаем.
/// Без CCB (15), DHL_RAW (16) и MD_CONSYS (30), хоть у Linux они и
/// включены: их раскладка в кэшируемой памяти зависит от `ccb_configs`, которых
/// у нас нет, и с выдуманными размерами модем падал в assert
/// `ccci_shm_bm.c:734` (05.11). Модем объявляет их «по желанию».
const AP_MUST: &[u8] = &[0, 1, 2, 5, 7, 8, 9, 10, 11, 12, 17, 18, 19, 20, 25, 26, 27, 28];
const AP_OPTIONAL: &[u8] = &[13, 32];

/// Готово ли: 0 — нет, 1 — ответ на HS1 отдан, 2 — HS2 пришёл (READY).
static STAGE: AtomicU32 = AtomicU32::new(0);
static TASK: AtomicBool = AtomicBool::new(false);

struct Link {
    tx_seq: [u16; 128],
    /// Принятые строки AT (последние).
    at_text: String,
    /// Недособранная строка AT.
    at_line: String,
    /// Журнал пакетов для `oem md log`.
    log: Vec<String>,
}

static LINK: SpinLock<Link> =
    SpinLock::new(Link { tx_seq: [0; 128], at_text: String::new(), at_line: String::new(), log: Vec::new() });
/// Модем сказал `+EIND: 128` — AT-порт готов.
static AT_READY: AtomicBool = AtomicBool::new(false);
/// Включать голос само по ходу вызова (`oem audio auto on|off`).
pub static VOICE_AUTO: AtomicBool = AtomicBool::new(true);

fn note(text: String) {
    crate::kprintln!("  ccci        : {text}");
    let mut link = LINK.lock();
    if link.log.len() >= 2048 {
        link.log.remove(0);
    }
    let stamp = crate::time::uptime_ms();
    link.log.push(format!("{stamp:>8} {text}"));
}

/// Весь журнал обмена текстом (`oem md logall` → `get_staged`).
pub fn log_text() -> String {
    let mut text = String::new();
    for line in &LINK.lock().log {
        text.push_str(line);
        text.push('\n');
    }
    text
}

// ---------------------------------------------------------------------------
// Общая память
// ---------------------------------------------------------------------------

fn virt(phys: u64) -> *mut u8 {
    (crate::mm::PHYS_MAP_BASE as u64 + phys) as *mut u8
}

/// Вытолкнуть и сбросить строки кэша участка (модем видит память мимо кэша).
fn flush(phys: u64, len: u64) {
    let start = phys & !63;
    let end = phys + len;
    let mut at = start;
    while at < end {
        let address = virt(at) as u64;
        // SAFETY: адрес — в прямом отображении ОЗУ; `dc civac` только чистит кэш.
        unsafe { core::arch::asm!("dc civac, {0}", in(reg) address, options(nostack)) };
        at += 64;
    }
    // SAFETY: барьер.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

fn nc(offset: u32) -> u64 {
    NC_AP + u64::from(offset)
}

fn nc_md(region: (u32, u32)) -> u32 {
    NC_MD + region.0
}

fn put32(phys: u64, value: u32) {
    // SAFETY: адрес — в резерве общей памяти, отображённом прямо.
    unsafe { (virt(phys) as *mut u32).write_volatile(value) };
}

fn get32(phys: u64) -> u32 {
    // SAFETY: как у `put32`.
    unsafe { (virt(phys) as *const u32).read_volatile() }
}

fn put_bytes(phys: u64, bytes: &[u8]) {
    // SAFETY: как у `put32`; длина проверяется вызывающим.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), virt(phys), bytes.len()) };
}

fn get_bytes(phys: u64, out: &mut [u8]) {
    // SAFETY: как у `put32`.
    unsafe { core::ptr::copy_nonoverlapping(virt(phys), out.as_mut_ptr(), out.len()) };
}

fn zero(phys: u64, len: u64) {
    // SAFETY: как у `put32`.
    unsafe { core::ptr::write_bytes(virt(phys), 0, len as usize) };
}

// ---------------------------------------------------------------------------
// Кольца
// ---------------------------------------------------------------------------

/// Начало блока очереди `q` (заголовок `0xEE0000EE`), блоки — подряд.
fn ring_base(q: usize) -> u64 {
    let mut at = nc(CCISM_MCU.0);
    for i in 0..q {
        at += u64::from(RING_CTL + RX_SIZE[i] + TX_SIZE[i]);
    }
    at
}

// Поля управления: rx {read, write, length}, tx {read, write, length}.
fn rx_ctl(q: usize) -> u64 {
    ring_base(q) + 8
}
fn tx_ctl(q: usize) -> u64 {
    ring_base(q) + 20
}
fn rx_buf(q: usize) -> u64 {
    ring_base(q) + 32
}
fn tx_buf(q: usize) -> u64 {
    rx_buf(q) + u64::from(RX_SIZE[q])
}

/// Построить блоки колец подряд с `start` (`ccci_create_ringbuf`); вернуть
/// их общий размер.
fn build_rings(start: u64, rx: &[u32; 8], tx: &[u32; 8]) -> u32 {
    let mut at = start;
    for q in 0..8 {
        let len = u64::from(RING_CTL + rx[q] + tx[q]);
        zero(at, len);
        put32(at, RBF_HEADER);
        put32(at + 4, RBF_HEADER);
        put32(at + len - 8, RBF_FOOTER);
        put32(at + len - 4, RBF_FOOTER);
        put32(at + 8 + 8, rx[q]); // rx_control.length
        put32(at + 20 + 8, tx[q]); // tx_control.length
        at += len;
    }
    flush(start, at - start);
    (at - start) as u32
}

/// Обычные кольца и кольца исключений. Размер обычных — то, что уходит
/// модему в `CCIF_SHARE_MEMORY` (Linux: `ccism->size = total_smem_size`).
fn rings_init() -> u32 {
    let total = build_rings(nc(CCISM_MCU.0), &RX_SIZE, &TX_SIZE);
    build_rings(nc(CCISM_MCU_EXP.0), &RX_EXP_SIZE, &TX_EXP_SIZE);
    total
}

/// Суммарный размер обычных колец.
fn rings_total() -> u32 {
    (0..8).map(|q| RING_CTL + RX_SIZE[q] + TX_SIZE[q]).sum()
}

/// Скопировать в кольцо с переносом через конец.
fn ring_write(buf: u64, length: u32, pos: u32, bytes: &[u8]) {
    let first = (length - pos).min(bytes.len() as u32) as usize;
    put_bytes(buf + u64::from(pos), &bytes[..first]);
    if first < bytes.len() {
        put_bytes(buf, &bytes[first..]);
    }
}

fn ring_read(buf: u64, length: u32, pos: u32, out: &mut [u8]) {
    let first = (length - pos).min(out.len() as u32) as usize;
    get_bytes(buf + u64::from(pos), &mut out[..first]);
    if first < out.len() {
        let rest = out.len() - first;
        get_bytes(buf, &mut out[first..first + rest]);
    }
}

/// Положить пакет (заголовок CCCI уже в `packet`) в tx-кольцо очереди `q` и
/// позвонить модему.
fn ring_send(q: usize, packet: &[u8]) -> Result<(), String> {
    let ctl = tx_ctl(q);
    flush(ctl, 12);
    let (read, write, length) = (get32(ctl), get32(ctl + 4), get32(ctl + 8));
    if length == 0 {
        return Err(format!("queue {q} has no tx ring"));
    }
    let free = if read == write {
        length - 1
    } else if read < write {
        length - write - 1 + read
    } else {
        read - write - 1
    };
    let need = 8 + ((packet.len() as u32 + 7) & !7) + 8;
    if need >= free {
        return Err(format!("queue {q} full ({free} free)"));
    }
    let buf = tx_buf(q);
    let mut header = [0u8; 8];
    header[..4].copy_from_slice(&PKG_HEADER.to_le_bytes());
    header[4..].copy_from_slice(&(packet.len() as u32).to_le_bytes());
    let mut at = write;
    ring_write(buf, length, at, &header);
    at = (at + 8) % length;
    ring_write(buf, length, at, packet);
    at = (at + ((packet.len() as u32 + 7) & !7)) % length;
    let mut footer = [0u8; 8];
    footer[..4].copy_from_slice(&PKG_FOOTER.to_le_bytes());
    footer[4..].copy_from_slice(&PKG_FOOTER.to_le_bytes());
    ring_write(buf, length, at, &footer);
    at = (at + 8) % length;
    flush(buf, u64::from(length));
    put32(ctl + 4, at);
    flush(ctl, 12);
    ring_bell(q as u32)
}

/// Взять из rx-кольца очереди `q` очередной пакет (с заголовком CCCI).
fn ring_receive(q: usize) -> Option<Vec<u8>> {
    let ctl = rx_ctl(q);
    flush(ctl, 12);
    let (read, write, length) = (get32(ctl), get32(ctl + 4), get32(ctl + 8));
    if length == 0 || read >= length || write >= length {
        return None;
    }
    let size = if write >= read { write - read } else { write + length - read };
    if size < 8 + 8 + 16 {
        return None;
    }
    let buf = rx_buf(q);
    flush(buf, u64::from(length));
    let mut header = [0u8; 8];
    ring_read(buf, length, read, &mut header);
    let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let whole = (8 + len + 8 + 7) & !7;
    if magic != PKG_HEADER || whole > size || len > 4096 {
        note(format!("q{q}: bad packet header {magic:#x} len {len} (read {read} write {write})"));
        // Сбросить очередь: дальше читать нечего.
        put32(ctl, write);
        flush(ctl, 12);
        return None;
    }
    let mut packet = alloc::vec![0u8; len as usize];
    ring_read(buf, length, (read + 8) % length, &mut packet);
    put32(ctl, (read + whole) % length);
    flush(ctl, 12);
    Some(packet)
}

// ---------------------------------------------------------------------------
// CCIF
// ---------------------------------------------------------------------------

fn ring_bell(channel: u32) -> Result<(), String> {
    let start = crate::time::uptime_ns();
    while read32(CCIF_BUSY).unwrap_or(0) & (1 << channel) != 0 {
        if crate::time::uptime_ns() - start > 20_000_000 {
            return Err(format!("CCIF channel {channel} stays busy"));
        }
    }
    write32(CCIF_BUSY, 1 << channel);
    write32(CCIF_TCHNUM, channel);
    Ok(())
}

fn sram32(offset: u64) -> u32 {
    read32(offset).unwrap_or(0)
}

/// Заголовок CCCI.
fn header(data0: u32, data1: u32, channel: u16, reserved: u32) -> [u8; 16] {
    let seq = {
        let mut link = LINK.lock();
        let seq = link.tx_seq[usize::from(channel) & 127];
        link.tx_seq[usize::from(channel) & 127] = seq.wrapping_add(1) & 0x7fff;
        seq
    };
    let word2 = u32::from(channel) | (u32::from(seq) << 16) | (1 << 31);
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&data0.to_le_bytes());
    out[4..8].copy_from_slice(&data1.to_le_bytes());
    out[8..12].copy_from_slice(&word2.to_le_bytes());
    out[12..16].copy_from_slice(&reserved.to_le_bytes());
    out
}

fn send_data(q: usize, channel: u16, payload: &[u8]) -> Result<(), String> {
    let mut packet = Vec::with_capacity(16 + payload.len());
    packet.extend_from_slice(&header(0, 16 + payload.len() as u32, channel, 0));
    packet.extend_from_slice(payload);
    ring_send(q, &packet)
}

fn send_msg(q: usize, channel: u16, msg: u32, param: u32) -> Result<(), String> {
    ring_send(q, &header(MAGIC, msg, channel, param))
}

// ---------------------------------------------------------------------------
// HS1 и runtime data
// ---------------------------------------------------------------------------

/// Снимок SRAM и звонков — без записей.
pub fn status() -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!(
        "CCIF busy {:08x} rch {:08x} stage {}",
        sram32(CCIF_BUSY),
        sram32(CCIF_RCHNUM),
        STAGE.load(Ordering::Acquire)
    ));
    out.push(format!(
        "dl {:08x} {:08x} {:08x} {:08x}",
        sram32(SRAM_DL_HEADER),
        sram32(SRAM_DL_HEADER + 4),
        sram32(SRAM_DL_HEADER + 8),
        sram32(SRAM_DL_HEADER + 12)
    ));
    out.push(format!("md feature head {:08x} tail {:08x}", sram32(SRAM_MD_FEATURE), sram32(SRAM_MD_FEATURE + 68)));
    if let Some(stats) = read32(0x1020_E300) {
        out.push(format!("MD boot stats {stats:08x} {:08x}", read32(0x1020_E304).unwrap_or(0)));
    }
    let link = LINK.lock();
    for line in link.log.iter().rev().take(3) {
        out.push(line.clone());
    }
    out
}

/// Признаки модема из HS1 (по байту: младшая тетрада — поддержка, старшая —
/// версия).
fn md_features() -> Result<[u8; 64], String> {
    let (h1, h2, h3) = (sram32(SRAM_DL_HEADER + 4), sram32(SRAM_DL_HEADER + 8), sram32(SRAM_DL_HEADER + 12));
    if h1 != 0 || h2 & 0xffff != u32::from(CH_CONTROL_RX) || h3 != MD_INIT_CHK_ID {
        return Err(format!("no HS1 in CCIF SRAM: {h1:#x} {h2:#x} {h3:#x}"));
    }
    if sram32(SRAM_MD_FEATURE) != MD_FEATURE_PATTERN || sram32(SRAM_MD_FEATURE + 68) != MD_FEATURE_PATTERN {
        return Err(String::from("HS1 feature patterns are wrong"));
    }
    let mut features = [0u8; 64];
    for (i, chunk) in features.chunks_mut(4).enumerate() {
        chunk.copy_from_slice(&sram32(SRAM_MD_FEATURE + 4 + 4 * i as u64).to_le_bytes());
    }
    Ok(features)
}

fn ap_mask(id: u8) -> u8 {
    if AP_MUST.contains(&id) {
        2
    } else if AP_OPTIONAL.contains(&id) {
        3
    } else {
        // Как у Linux: всё, что не названо, — «нет такого» (0).
        if matches!(id, 3 | 4 | 14 | 21 | 24 | 29 | 31 | 34) { 1 } else { 0 }
    }
}

fn shm(out: &mut Vec<u8>, addr: u32, size: u32) {
    out.extend_from_slice(&addr.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
}

fn words(out: &mut Vec<u8>, w: [u32; 4]) {
    for word in w {
        out.extend_from_slice(&word.to_le_bytes());
    }
}

/// Данные признака `id` (как `ccci_md_prepare_runtime_data`).
fn feature_data(id: u8) -> Vec<u8> {
    let mut d = Vec::new();
    let region = |out: &mut Vec<u8>, r: (u32, u32)| shm(out, nc_md(r), r.1);
    match id {
        0 => words(&mut d, [u32::from(CH_CONTROL_RX), 0, 0, 0]), // BOOT_INFO: NORMAL_BOOT_ID
        1 => shm(&mut d, nc_md(MDCCCI_DBG), EE_SMEM_TOTAL),
        2 => shm(&mut d, nc_md(CCISM_MCU), rings_total()),
        28 => region(&mut d, CCISM_MCU_EXP),
        15 => words(&mut d, [nc_md(CCB_CTRL), CCB_CTRL.1, C_MD, 2 << 20]), // CCB
        16 => shm(&mut d, C_MD + (2 << 20), 20 << 20), // DHL_RAW
        25 => region(&mut d, LWA),
        17 => region(&mut d, NETD),
        18 => region(&mut d, USB),
        26 => region(&mut d, AUDIO),
        30 => shm(&mut d, C_MD, 0), // MD_CONSYS: размер 0
        27 => {}                     // MULTI_MD_MPU: без данных
        7 => words(&mut d, [crate::time::uptime_ns() as u32 ^ 0x5a5a_1234, 0, 0, 0]),
        10 => words(&mut d, [0b11, 0, 0, 0]), // MISC_INFO_CCCI: проверка номеров, опрос
        11 => {
            let now = crate::time::now_unix().unwrap_or(0);
            let tz = (-crate::time::offset_minutes()) as u32;
            words(&mut d, [now as u32, (now >> 32) as u32, tz, 0]);
        }
        20 => d.extend_from_slice(&(3584u32 - 16).to_le_bytes()), // AP_CCMNI_MTU
        13 => shm(&mut d, 0x8E00_0000, 0x7E0_0000),             // MD_IMAGE_START
        32 => d.extend_from_slice(&0u32.to_le_bytes()),         // MTEE: нет
        _ => words(&mut d, [0; 4]), // прочие MISC_*: нули
    }
    d
}

/// `oem md hs`: построить кольца и общую память, ответить на HS1.
pub fn answer_hs1() -> Result<Vec<String>, String> {
    let md = md_features()?;
    let mut out = Vec::new();
    // Linux чистит общую память до запуска модема (`ccci_md_clear_smem`); у
    // нас модем уже идёт и мог что-то положить в свои области (отладка,
    // MD-часть runtime data) — чистятся только кольца и наша часть runtime.
    flush(nc(0), u64::from(NC_SIZE));
    let total = rings_init();
    out.push(format!("rings {total:#x} bytes (+ exception rings)"));

    // Runtime data AP: запись на каждый из 64 признаков.
    let mut rt = Vec::new();
    let mut must = 0;
    for id in 0..64u8 {
        let mdf = md[usize::from(id)];
        let (md_mask, md_ver) = (mdf & 0xf, mdf >> 4);
        let ap = ap_mask(id);
        if md_mask == 2 && ap < 2 {
            return Err(format!("MD requires feature {id}, AP lacks it"));
        }
        let result = match md_mask {
            0 | 2 => mdf,
            3 => {
                if md_ver == 0 && ap >= 2 { 2 } else { 1 }
            }
            // BACKWARD_COMPAT: версия модема не ниже нашей (0) — всегда да.
            4 => 2,
            _ => 1,
        };
        let data = if result & 0xf == 2 { feature_data(id) } else { Vec::new() };
        if result & 0xf == 2 {
            must += 1;
        }
        rt.push(id);
        rt.push(result);
        rt.extend_from_slice(&[0, 0]);
        rt.extend_from_slice(&(data.len() as u32).to_le_bytes());
        rt.extend_from_slice(&data);
    }
    if rt.len() as u32 > RUNTIME_AP_SIZE {
        return Err(format!("runtime data {} bytes > {RUNTIME_AP_SIZE}", rt.len()));
    }
    zero(nc(RUNTIME_DATA.0), u64::from(RUNTIME_AP_SIZE));
    put_bytes(nc(RUNTIME_DATA.0), &rt);
    // DBM: охранные слова по краям.
    let dbm = nc(DBM.0);
    put32(dbm, 0x4444_4444);
    put32(dbm + 4, 0x4444_4444);
    zero(dbm + 8, 160);
    put32(dbm + 168, 0x4444_4444);
    put32(dbm + 172, 0x4444_4444);
    flush(nc(0), u64::from(NC_SIZE));
    out.push(format!("runtime data {} bytes, {must} features on", rt.len()));

    // Хвост SRAM CCIF: метка и отладочная область MDSS_DBG.
    write32(SRAM + SRAM_SIZE - 12, 0x7274_626E);
    write32(SRAM + SRAM_SIZE - 8, nc_md(MDSS_DBG));
    write32(SRAM + SRAM_SIZE - 4, MDSS_DBG.1);

    // Ответ в SRAM: up_header + ap_query_md_feature_v2_1 (156 байт).
    let packet_size = 156 + 16;
    write32(SRAM_UP_HEADER, 0);
    write32(SRAM_UP_HEADER + 4, packet_size);
    write32(SRAM_UP_HEADER + 8, u32::from(CH_CONTROL_TX));
    write32(SRAM_UP_HEADER + 12, MD_INIT_CHK_ID);
    let mut ap = Vec::new();
    ap.extend_from_slice(&AP_FEATURE_PATTERN.to_le_bytes());
    let mut set = [0u8; 64];
    set[1] = 1 << 4; // feature_set[1]: mask 0, version 1
    ap.extend_from_slice(&set);
    for word in [
        2u32, // share_memory_support = MULTI_MD_MPU_SUPPORT
        nc_md(RUNTIME_DATA),
        RUNTIME_AP_SIZE,
        nc_md(RUNTIME_DATA) + RUNTIME_AP_SIZE,
        0x800,
        NC_MD,
        NC_SIZE,
        C_MD,
        C_SIZE,
    ] {
        ap.extend_from_slice(&word.to_le_bytes());
    }
    ap.extend_from_slice(&[0u8; 48]);
    ap.extend_from_slice(&AP_FEATURE_PATTERN.to_le_bytes());
    for (i, chunk) in ap.chunks(4).enumerate() {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        write32(SRAM_AP_RT + 4 * i as u64, u32::from_le_bytes(word));
    }
    // Снять звонок HS1 и позвонить в ответ.
    let rch = sram32(CCIF_RCHNUM);
    write32(CCIF_ACK, rch & 0xffff);
    LINK.lock().tx_seq = [0; 128];
    STAGE.store(1, Ordering::Release);
    ring_bell(CH_SRAM)?;
    out.push(format!("answered HS1 (rch was {rch:#x})"));
    note(String::from("HS1 answered with runtime data"));
    if !TASK.swap(true, Ordering::AcqRel) {
        if let Err(err) = crate::sched::spawn_daemon("ccci", service_task) {
            out.push(format!("no ccci task: {err:?}"));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Приём
// ---------------------------------------------------------------------------

fn le32(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn handle(q: usize, packet: &[u8]) {
    let (d0, d1, w2, reserved) = (le32(packet, 0), le32(packet, 4), le32(packet, 8), le32(packet, 12));
    let channel = (w2 & 0xffff) as u16;
    match channel {
        CH_CONTROL_RX => {
            if d0 == MAGIC || d1 == 0 {
                if d1 == 0 && reserved != MD_INIT_CHK_ID {
                    STAGE.store(2, Ordering::Release);
                    note(format!("HS2: modem READY (reserved {reserved:#x})"));
                } else {
                    note(format!("control msg {d1:#x} param {reserved:#x}"));
                }
            } else {
                note(format!("control data {} bytes", packet.len()));
            }
        }
        CH_SYSTEM_RX => {
            note(format!("system msg {d1:#x} param {reserved:#x} data0 {d0:#x}"));
            if d1 == MD_GET_BATTERY_INFO {
                let mv = crate::battery::get().map_or(3900, |s| s.millivolts);
                let _ = send_msg(Q_CONTROL, CH_SYSTEM_TX, MD_GET_BATTERY_INFO, mv);
            }
        }
        CH_RPC_RX => {
            let op = le32(packet, 16);
            let params = rpc_params(packet);
            let answer = rpc_answer(op, &params);
            note(format!("rpc op {op:#x} {} -> {answer:?}", rpc_describe(&params)));
            let mut reply = Vec::new();
            reply.extend_from_slice(&packet[..16]);
            reply.extend_from_slice(&(op | RPC_RESP).to_le_bytes());
            reply.extend_from_slice(&(answer.len() as u32).to_le_bytes());
            for value in &answer {
                reply.extend_from_slice(&(value.len() as u32).to_le_bytes());
                reply.extend_from_slice(value);
                while reply.len() % 4 != 0 {
                    reply.push(0);
                }
            }
            let len = reply.len() as u32;
            let h = header(0, len, CH_RPC_TX, reserved);
            reply[..16].copy_from_slice(&h);
            if let Err(err) = ring_send(Q_RPC, &reply) {
                note(format!("rpc reply: {err}"));
            }
        }
        CH_UART2_RX => {
            let text = String::from_utf8_lossy(&packet[16.min(packet.len())..]).into_owned();
            // Модем шлёт и по символу (эхо), и строками: журнал и индикатор —
            // по целым строкам.
            let lines: Vec<String> = {
                let mut link = LINK.lock();
                link.at_line.push_str(&text);
                let mut done = Vec::new();
                while let Some(at) = link.at_line.find(['\r', '\n']) {
                    let line: String = link.at_line[..at].trim().into();
                    link.at_line.drain(..=at);
                    if !line.is_empty() {
                        done.push(line);
                    }
                }
                done
            };
            for line in &lines {
                note(format!("AT <- {line}"));
                if line.starts_with("+EIND: 128") {
                    AT_READY.store(true, Ordering::Release);
                }
                cellular_from_at(line);
                call_from_at(line);
                time_from_at(line);
            }
            let mut link = LINK.lock();
            link.at_text.push_str(&text);
            if link.at_text.len() > 4096 {
                let cut = link.at_text.len() - 4096;
                link.at_text.drain(..cut);
            }
        }
        CH_PCM_RX => note(format!("speech <- {d1:#010x} param {reserved:#x} ({} bytes)", packet.len())),
        CH_FS_RX => {
            let (replies, text) = super::mtk_ccci_fs::handle(packet);
            if !text.is_empty() {
                note(text);
            }
            for (data0, slot, payload) in replies {
                let mut out = Vec::with_capacity(16 + payload.len());
                out.extend_from_slice(&header(data0, 16 + payload.len() as u32, CH_FS_TX, slot));
                out.extend_from_slice(&payload);
                if let Err(err) = ring_send(Q_FS, &out) {
                    note(format!("fs reply: {err}"));
                }
            }
        }
        _ => note(format!("q{q} ch {channel} msg {d1:#x} {} bytes", packet.len())),
    }
}

/// Параметры запроса RPC (`rpc_buffer`: op, para_num, затем {len, данные
/// с выравниванием до 4}).
fn rpc_params(packet: &[u8]) -> Vec<Vec<u8>> {
    let count = le32(packet, 20).min(6) as usize;
    let mut at = 24;
    let mut out = Vec::new();
    for _ in 0..count {
        let len = le32(packet, at) as usize;
        at += 4;
        let Some(data) = packet.get(at..at + len) else { break };
        out.push(data.to_vec());
        at += (len + 3) & !3;
    }
    out
}

fn rpc_describe(params: &[Vec<u8>]) -> String {
    let mut text = String::new();
    for p in params {
        if p.len() > 4 && p.iter().take(p.len() - 1).all(|b| b.is_ascii_graphic() || *b == 0) {
            text.push_str(String::from_utf8_lossy(p).trim_end_matches('\0'));
        } else {
            let _ = core::fmt::Write::write_fmt(&mut text, format_args!("{:#x}", le32(p, 0)));
        }
        text.push(' ');
    }
    text
}

/// Ответ RPC — параметры (`port_rpc.c`, для `0x400F` — `ccci_rpcd.c`).
fn rpc_answer(op: u32, params: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let ints = |v: Vec<i32>| v.into_iter().map(|x| x.to_le_bytes().to_vec()).collect();
    match op {
        // IPC_RPC_DTSI_QUERY_OP: {u8 req, u8 index, char name[64]} → [0],
        // {u32 value; char str[64]}, по умолчанию всё 0x0F. Значения — из
        // `/md_attr_node` дерева LK.
        0x400E => {
            let name = params.first().map_or(String::new(), |p| {
                String::from_utf8_lossy(p.get(2..).unwrap_or(&[])).trim_end_matches('\0').into()
            });
            let mut out = alloc::vec![0x0Fu8; 68];
            let value = match name.as_str() {
                "mediatek,md_product_name_model_id" => Some(0x0F0F_0F0Fu32),
                "mediatek,md_drdi_rf_set_idx" => Some(9),
                _ => None,
            };
            if let Some(v) = value {
                out[..4].copy_from_slice(&v.to_le_bytes());
            }
            alloc::vec![0i32.to_le_bytes().to_vec(), out]
        }
        // IPC_RPC_QUERY_AP_SYS_PROPERTY (у Android — `ccci_rpcd`): [длина
        // значения], [значение\0]; неизвестное — пустая строка.
        0x400F => {
            let name = params.first().map_or(String::new(), |p| String::from_utf8_lossy(p).trim_end_matches('\0').into());
            let value = match name.as_str() {
                "ro.product.device" | "ro.product.name" | "ro.build.product" => "dandelion",
                "ro.product.model" => "Redmi 9C",
                "ro.product.brand" | "ro.product.manufacturer" => "Xiaomi",
                "ro.boot.hwc" => "Global",
                "ro.boot.hwversion" => "1.39.1",
                _ => "",
            };
            let mut text = value.as_bytes().to_vec();
            text.push(0);
            alloc::vec![(value.len() as i32).to_le_bytes().to_vec(), text]
        }
        _ => ints(rpc_answer_ints(op, params)),
    }
}

/// Не сообщать модему датчик вставки SIM (см. `0x4005`). Выключено: «SIM
/// не видна» 06.11 было оттого, что карта стояла во втором слоте, а `ttyC0`
/// без CMUX — это протокол 1 (слот 1).
const NO_HOTPLUG: bool = false;

/// Ответы RPC из одних чисел по 4 байта.
fn rpc_answer_ints(op: u32, params: &[Vec<u8>]) -> Vec<i32> {
    const FS_NO_OP: i32 = -1;
    const QUERY_STRING: i32 = -11; // ERR_SIM_HOT_PLUG_QUERY_STRING
    match op {
        // IPC_RPC_GET_EINT_ATTR_OP: имя узла, ?, тип. Значения — из
        // `md1_sim{1,2}_hot_plug_eint` дерева LK: interrupts <n 8>, debounce
        // <n 10000> (мкс → мс), dedicated <n 0>, src_pin <n n+1>,
        // sockettype <n 0>. Полярность и чувствительность из 8
        // (IRQ_TYPE_LEVEL_LOW): (8 & 5) != 0 и (8 & 3) != 0 — обе 0.
        // Опыт 06.11: с ответами по дереву модем считал SIM1 вынутой (VSIM1 не
        // включал). Без узла датчика — как на платах без горячей вставки —
        // модем должен опрашивать карту сам.
        0x4005 if NO_HOTPLUG => alloc::vec![QUERY_STRING, QUERY_STRING],
        0x4005 if params.len() >= 3 => {
            let name = String::from_utf8_lossy(&params[0]).to_ascii_uppercase();
            let sim = if name.starts_with("MD1_SIM1_HOT_PLUG_EINT") {
                0
            } else if name.starts_with("MD1_SIM2_HOT_PLUG_EINT") {
                1
            } else {
                return alloc::vec![QUERY_STRING, QUERY_STRING];
            };
            let value = match le32(&params[2], 0) {
                0 => sim,      // номер EINT
                1 => 10,       // дребезг, мс
                2 | 3 => 0,    // полярность, чувствительность
                4 | 5 => 0,    // тип гнезда, выделенный
                6 => sim + 1,  // исходный вывод
                _ => return alloc::vec![-12, -12], // QUERY_TYPE
            };
            alloc::vec![0, value]
        }
        _ => alloc::vec![FS_NO_OP],
    }
}

/// Обновить индикатор сотовой связи (`crate::cellular`) по строке AT:
/// `+CREG/+CEREG/+CGREG: [n,]stat[,lac,ci[,act]]`, `+CSQ: rssi,ber`,
/// `+COPS: mode[,format,"oper",act]`, `+CPIN`/`+ESIMS` — SIM.
fn cellular_from_at(line: &str) {
    use crate::cellular::{self, Radio, Service};
    let mut state = cellular::get();
    let nums = |rest: &str| -> Vec<i64> {
        rest.split(',').map(|f| f.trim().trim_matches('"')).map(|f| f.parse::<i64>().unwrap_or(-1)).collect()
    };
    if let Some(rest) = line
        .strip_prefix("+CREG:")
        .or_else(|| line.strip_prefix("+CEREG:"))
        .or_else(|| line.strip_prefix("+CGREG:"))
    {
        let f = nums(rest);
        // Ответ на запрос — «n,stat[,"lac","ci",act]»: второе поле — число;
        // незапрошенное — «stat[,"lac","ci",act]»: второе поле в кавычках или
        // его нет.
        let quoted = rest.split(',').nth(1).is_some_and(|f| f.trim().starts_with('"'));
        let (stat, act) = if f.len() >= 2 && !quoted {
            (f[1], f.get(4).copied())
        } else {
            (f[0], f.get(3).copied())
        };
        state.service = match stat {
            1 | 5 => Service::Registered,
            2 => Service::Searching,
            3 => Service::Emergency,
            _ => if state.service == Service::NoSim { Service::NoSim } else { Service::Searching },
        };
        state.roaming = stat == 5;
        if let Some(act) = act.filter(|a| *a >= 0) {
            state.radio = Radio::from_act(act as u32);
        }
    } else if let Some(rest) = line.strip_prefix("+CSQ:") {
        let f = nums(rest);
        if let Some(rssi) = f.first() {
            state.bars = cellular::bars_from_csq(*rssi as u32);
        }
    } else if let Some(rest) = line.strip_prefix("+COPS:") {
        let f = nums(rest);
        if let Some(act) = f.get(3).filter(|a| **a >= 0) {
            state.radio = Radio::from_act(*act as u32);
        }
    } else if line.starts_with("+CPIN: READY") || line == "+ESIMS: 1" {
        if state.service == Service::NoSim || state.service == Service::NoModem {
            state.service = Service::Searching;
        }
    } else if line == "+ESIMS: 0" || line == "+CME ERROR: 10" {
        state.service = Service::NoSim;
    } else {
        return;
    }
    cellular::set(state);
}

/// После READY: включить радио и регистрацию с сообщениями; дальше раз в
/// 10 с спрашивать уровень и сеть — индикатор в строке состояния.
fn radio_tick(started: &mut bool, last: &mut u64) {
    if STAGE.load(Ordering::Acquire) < 2 || !AT_READY.load(Ordering::Acquire) {
        return;
    }
    let now = crate::time::uptime_ms();
    if !*started {
        *started = true;
        crate::cellular::set(crate::cellular::State {
            service: crate::cellular::Service::Searching,
            radio: crate::cellular::Radio::Unknown,
            bars: 0,
            roaming: false,
        });
        for cmd in ["ATE0\r", "AT+CMEE=2\r", "AT+CREG=2\r", "AT+CEREG=2\r", "AT+CLIP=1\r", "AT+CRC=1\r", "AT+CTZR=2\r", "AT+CFUN=1\r"] {
            let _ = send_data(Q_AT, CH_UART2_TX, cmd.as_bytes());
        }
        *last = now;
        return;
    }
    if now - *last >= 10_000 && CALL.lock().0 == user_abi::CALL_IDLE {
        *last = now;
        for cmd in ["AT+CSQ\r", "AT+COPS?\r", "AT+CPIN?\r"] {
            let _ = send_data(Q_AT, CH_UART2_TX, cmd.as_bytes());
        }
    }
}

/// Опрос звонков модема раз в 10 мс.
fn service_task() {
    let mut exception = 0;
    let (mut radio_started, mut radio_last, mut call_last) = (false, 0u64, 0u64);
    let mut voice_on = false;
    loop {
        radio_tick(&mut radio_started, &mut radio_last);
        call_tick(&mut call_last);
        if VOICE_AUTO.load(Ordering::Acquire) {
            let state = CALL.lock().0;
            super::mtk_audio::follow_call(state, &mut voice_on);
        }
        let rch = sram32(CCIF_RCHNUM);
        if rch != 0 {
            write32(CCIF_ACK, rch & 0xffff);
            // Биты 16+ — путь исключения модема (EX_INIT, …): их Linux
            // обрабатывает отдельно; здесь — одна строка на каждую смену.
            if rch & !0xffff != exception {
                exception = rch & !0xffff;
                note(format!("CCIF exception bits {rch:#x}: modem assert? see oem dump 78000000"));
            }
            if rch & (1 << CH_SRAM) != 0 {
                note(String::from("SRAM doorbell again (new HS1?)"));
            }
        }
        for q in 0..7 {
            while let Some(packet) = ring_receive(q) {
                handle(q, &packet);
            }
        }
        crate::sched::sleep_ms(10);
    }
}

/// Запуск модема при загрузке (06.11): то же, что `oem md power`, `pll`,
/// `go`, `hs` по кабелю. Ждёт HS1 до 10 с.
pub fn autostart_task() {
    use super::mtk_md;
    // Дать загрузиться столу и USB: модем не к спеху.
    crate::sched::sleep_ms(3000);
    let steps: [(&str, fn() -> Result<Vec<String>, String>); 3] =
        [("power", mtk_md::power_on), ("pll", mtk_md::pll), ("go", mtk_md::go)];
    for (name, step) in steps {
        if let Err(err) = step() {
            crate::kprintln!("  modem       : {name} failed: {err}");
            return;
        }
    }
    let start = crate::time::uptime_ms();
    while sram32(CCIF_RCHNUM) & (1 << CH_SRAM) == 0 {
        if crate::time::uptime_ms() - start > 10_000 {
            crate::kprintln!("  modem       : no HS1 in 10 s");
            return;
        }
        crate::sched::sleep_ms(50);
    }
    match answer_hs1() {
        Ok(lines) => crate::kprintln!("  modem       : HS1 answered ({})", lines.join("; ")),
        Err(err) => crate::kprintln!("  modem       : HS1 failed: {err}"),
    }
}

// ---------------------------------------------------------------------------
// Вызов (06.11): состояние для звонилки (`SYS_MODEM`)
// ---------------------------------------------------------------------------

/// Состояние вызова (`user_abi::CALL_*`) и номер собеседника.
static CALL: SpinLock<(u8, String)> = SpinLock::new((0, String::new()));
/// Ждём ответа на свой `AT+CLCC`; `.1` — пришла ли в нём строка `+CLCC`.
static CLCC_WAIT: SpinLock<(bool, bool)> = SpinLock::new((false, false));
/// Последний `RING`/`+CRING`/`+CLIP` (мс) и начало набора (мс).
static LAST_RING: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DIAL_AT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Входящий считается законченным, если звонок не повторялся столько (модем
/// шлёт `+CRING` раз в 4 с).
const RING_GONE_MS: u64 = 9000;
/// Пустой `AT+CLCC` в первые столько мс набора — ещё не значит отбой.
const DIAL_GRACE_MS: u64 = 5000;
/// Когда звонилка последний раз спрашивала состояние (мс): она спрашивает
/// раз в 0,5 с, значит, если давно, — окна нет.
static PHONE_SEEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Входящий, а звонилка не открыта — открыть её.
fn open_phone_for_incoming() {
    let now = crate::time::uptime_ms();
    if now.saturating_sub(PHONE_SEEN.load(Ordering::Acquire)) < 2000 {
        return;
    }
    PHONE_SEEN.store(now, Ordering::Release); // не запускать дважды подряд
    match crate::user::spawn("/bin/phone", crate::user::session::credentials()) {
        Ok(id) => crate::kprintln!("  call        : incoming, opened /bin/phone as {id}"),
        Err(err) => crate::kprintln!("  call        : incoming, cannot open /bin/phone: {err}"),
    }
}

fn set_call(state: u8, number: Option<&str>) {
    let mut call = CALL.lock();
    let became_incoming = call.0 != state && state == user_abi::CALL_INCOMING;
    if call.0 != state {
        crate::kprintln!("  call        : state {} -> {state}", call.0);
    }
    call.0 = state;
    if let Some(n) = number {
        call.1 = String::from(n);
    } else if state == user_abi::CALL_IDLE {
        call.1.clear();
    }
    drop(call);
    if became_incoming {
        open_phone_for_incoming();
    }
}

/// Строка AT про вызов: `RING`, `+CLIP`, `+CLCC`, `NO CARRIER`…
fn call_from_at(line: &str) {
    use user_abi::*;
    if line == "RING" || line.starts_with("+CRING") {
        LAST_RING.store(crate::time::uptime_ms(), Ordering::Release);
        if CALL.lock().0 == CALL_IDLE {
            set_call(CALL_INCOMING, None);
        }
    } else if let Some(rest) = line.strip_prefix("+CLIP:") {
        let number = rest.split(',').next().unwrap_or("").trim().trim_matches('"');
        LAST_RING.store(crate::time::uptime_ms(), Ordering::Release);
        let state = CALL.lock().0;
        set_call(if state == CALL_IDLE { CALL_INCOMING } else { state }, Some(number));
    } else if let Some(rest) = line.strip_prefix("+CLCC:") {
        // id,dir,stat,mode,mpty,"number",type — stat: 0 разговор, 1 удержан,
        // 2 набор, 3 звонит у собеседника, 4 входящий, 5 ожидает.
        let f: Vec<&str> = rest.split(',').map(str::trim).collect();
        let stat = f.get(2).and_then(|s| s.parse::<u8>().ok()).unwrap_or(9);
        let number = f.get(5).map(|s| s.trim_matches('"'));
        let state = match stat {
            0 | 1 => CALL_ACTIVE,
            2 => CALL_DIALING,
            3 => CALL_ALERTING,
            4 | 5 => CALL_INCOMING,
            _ => return,
        };
        CLCC_WAIT.lock().1 = true;
        set_call(state, number);
    } else if matches!(line, "NO CARRIER" | "BUSY" | "NO ANSWER" | "NO DIALTONE") {
        set_call(CALL_IDLE, None);
    } else if line == "OK" {
        let mut wait = CLCC_WAIT.lock();
        if wait.0 {
            wait.0 = false;
            if !wait.1 {
                drop(wait);
                // Ответ на `AT+CLCC` без единой строки — вызовов нет. Но
                // модем MTK не показывает в `CLCC` звонящий входящий (06.11:
                // из-за этого окно «Входящий» гасло через секунду), а в первые
                // секунды набора — ещё не начатый исходящий.
                let state = CALL.lock().0;
                let dialing_young = state == CALL_DIALING
                    && crate::time::uptime_ms().saturating_sub(DIAL_AT.load(Ordering::Acquire)) < DIAL_GRACE_MS;
                if state != CALL_IDLE && state != CALL_INCOMING && !dialing_young {
                    set_call(CALL_IDLE, None);
                }
            }
        }
    }
}

/// Раз в секунду во время вызова — `AT+CLCC` (состояние на той стороне).
fn call_tick(last: &mut u64) {
    let state = CALL.lock().0;
    if state == user_abi::CALL_IDLE || STAGE.load(Ordering::Acquire) < 2 {
        return;
    }
    let now = crate::time::uptime_ms();
    // Входящий, который перестал звонить (положили на той стороне, а `NO
    // CARRIER` не пришёл), — закончить.
    if state == user_abi::CALL_INCOMING && now.saturating_sub(LAST_RING.load(Ordering::Acquire)) > RING_GONE_MS {
        set_call(user_abi::CALL_IDLE, None);
        return;
    }
    if now - *last < 1000 {
        return;
    }
    *last = now;
    *CLCC_WAIT.lock() = (true, false);
    let _ = send_data(Q_AT, CH_UART2_TX, b"AT+CLCC\r");
}

/// Для звонилки: состояние и номер.
pub fn call_state() -> (u8, String) {
    PHONE_SEEN.store(crate::time::uptime_ms(), Ordering::Release);
    if STAGE.load(Ordering::Acquire) < 2 || !AT_READY.load(Ordering::Acquire) {
        return (user_abi::CALL_NO_MODEM, String::new());
    }
    CALL.lock().clone()
}

/// Набрать номер (`ATD<номер>;` — голосовой вызов).
pub fn dial(number: &str) -> Result<(), String> {
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '*' | '#')) {
        return Err(String::from("bad number"));
    }
    if CALL.lock().0 != user_abi::CALL_IDLE {
        return Err(String::from("a call is already on"));
    }
    DIAL_AT.store(crate::time::uptime_ms(), Ordering::Release);
    at_send(&format!("ATD{number};"))?;
    note(format!("dial {number}"));
    set_call(user_abi::CALL_DIALING, Some(number));
    Ok(())
}

/// Принять входящий (`ATA`) или положить трубку (`ATH`).
pub fn answer_or_hangup(answer: bool) -> Result<(), String> {
    at_send(if answer { "ATA" } else { "ATH" })?;
    if !answer {
        set_call(user_abi::CALL_IDLE, None);
    }
    Ok(())
}

/// Сообщение речи модему по каналу 5 (`ccci_aud`, очередь 0): заголовок
/// без данных, `data0 = 0xFFFFFFFF`, `data1 = id << 16 | param`.
pub fn send_pcm_msg(data1: u32) -> Result<(), String> {
    if STAGE.load(Ordering::Acquire) < 2 {
        return Err(String::from("modem not READY"));
    }
    note(format!("speech -> {data1:#010x}"));
    ring_send(Q_CONTROL, &header(MAGIC, data1, CH_PCM_TX, 0))
}


// ---------------------------------------------------------------------------
// Время из сети (NITZ, 06.11)
// ---------------------------------------------------------------------------

/// Пояс в четвертях часа со знаком (`"+12"`, `-08`) → минуты.
fn quarters(text: &str) -> Option<i32> {
    let t = text.trim().trim_matches('"');
    let (sign, digits) = match t.as_bytes().first()? {
        b'-' => (-1, &t[1..]),
        b'+' => (1, &t[1..]),
        _ => (1, t),
    };
    let q: i32 = digits.trim().parse().ok()?;
    (q.abs() <= 56).then_some(sign * q * 15)
}

/// `+CTZV: tz`, `+CTZE: tz,dst[,time]` — пояс; `+CCLK: "yy/MM/dd,hh:mm:ss±zz"`
/// — местное время сети. По поясу — часовой пояс системы и запрос часов, по
/// часам — время, если своего нет или оно ушло больше чем на 2 минуты.
fn time_from_at(line: &str) {
    let (minutes, stamp) = if let Some(rest) = line.strip_prefix("+CTZE:") {
        // tz,dst,"yyyy/MM/dd,hh:mm:ss" — время местное (сверено 06.11: сеть
        // дала 02:09 при 22:09 UTC и поясе +16 четвертей).
        let mut parts = rest.splitn(3, ',');
        let tz = parts.next().and_then(quarters);
        let _dst = parts.next();
        (tz, parts.next().map(|t| String::from(t.trim().trim_matches('"'))))
    } else if let Some(rest) = line.strip_prefix("+CTZV:") {
        (rest.split(',').next().and_then(quarters), None)
    } else if let Some(rest) = line.strip_prefix("+CCLK:") {
        // "yy/MM/dd,hh:mm:ss±zz" — тоже местное; нули — модем не знает.
        let t = rest.trim().trim_matches('"');
        let split = t.rfind(['+', '-']).filter(|at| *at > 8).unwrap_or(t.len());
        (quarters(&t[split..]).or(Some(crate::time::offset_minutes())), Some(String::from(&t[..split])))
    } else {
        return;
    };
    let Some(minutes) = minutes else { return };
    if !line.starts_with("+CCLK") {
        crate::time::adopt_network_timezone(minutes);
    }
    let Some(stamp) = stamp else { return };
    let (date, time) = stamp.split_once(',').unwrap_or(("", ""));
    let d: Vec<i64> = date.split('/').filter_map(|x| x.trim().parse().ok()).collect();
    let c: Vec<i64> = time.split(':').filter_map(|x| x.trim().parse().ok()).collect();
    let ([yy, mo, dd], [hh, mi, ss]) = (d.as_slice(), c.as_slice()) else { return };
    let year = if *yy < 100 { 2000 + yy } else { *yy };
    if year < 2025 || !(1..=12).contains(mo) || !(1..=31).contains(dd) {
        return; // модем ещё не знает времени
    }
    let days = calendar::days_from_civil(year, *mo as u8, *dd as u8);
    let utc = days * 86_400 + hh * 3600 + mi * 60 + ss - i64::from(minutes) * 60;
    if utc <= 0 {
        return;
    }
    let utc = utc as u64;
    let off = crate::time::now_unix().map(|now| now.abs_diff(utc));
    if off.is_none_or(|o| o > 120) {
        note(format!("network time {stamp} (UTC{:+} min), clock was off by {off:?} s", minutes));
        crate::time::set_wall_clock(utc, 0, "the cellular network (NITZ)");
    }
}

/// Отправить строку AT без ожидания ответа (для звонилки).
pub fn at_send(command: &str) -> Result<(), String> {
    if STAGE.load(Ordering::Acquire) < 2 {
        return Err(String::from("modem not READY"));
    }
    let mut text = String::from(command);
    text.push('\r');
    send_data(Q_AT, CH_UART2_TX, text.as_bytes())
}

// ---------------------------------------------------------------------------
// AT
// ---------------------------------------------------------------------------

/// `oem md at <команда>`: отправить и через секунду отдать ответ.
pub fn at(command: &str) -> Result<Vec<String>, String> {
    if STAGE.load(Ordering::Acquire) < 2 {
        return Err(String::from("modem not READY (no HS2 yet)"));
    }
    LINK.lock().at_text.clear();
    let mut text = String::from(command);
    text.push('\r');
    send_data(Q_AT, CH_UART2_TX, text.as_bytes())?;
    let start = crate::time::uptime_ms();
    loop {
        crate::sched::sleep_ms(50);
        let done = {
            let link = LINK.lock();
            link.at_text.contains("OK") || link.at_text.contains("ERROR")
        };
        if done || crate::time::uptime_ms() - start > 3000 {
            break;
        }
    }
    let link = LINK.lock();
    let lines: Vec<String> = link.at_text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    if lines.is_empty() { Ok(alloc::vec![String::from("(no answer in 3 s)")]) } else { Ok(lines) }
}

/// `oem md log`: последние строки журнала обмена.
pub fn log() -> Vec<String> {
    LINK.lock().log.iter().rev().take(8).rev().cloned().collect()
}
