//! Сторона WLAN блока связи MT6765 (CONNAC `soc1_0`) — до списка сетей
//! (03.11).
//!
//! # Откуда числа
//!
//! Из драйвера MediaTek gen4m той же платформы (Motorola,
//! `vendor-mediatek-kernel_modules-connectivity-wlan-core-gen4m`, ветка
//! `android-10-release-qol`): `hal_pdma.c` (кольца DMA, владение),
//! `cmm_asic_connac.c` (DMASHDL, `GLO_CFG`), `fw_dl.c` (загрузка
//! `WIFI_RAM_CODE`), `nic_init_cmd_event.h` / `nic_cmd_event.h` (заголовки
//! команд), `scan_fsm.c` (запрос поиска), `nic_rx.c` (дескриптор приёма).
//! Окно блока — `0x18000000`: адрес шины блока ниже `0x40000` ложится в
//! окно как есть.
//!
//! # Как пользоваться
//!
//! Шагами, командами `fastboot oem wlan …`, как и `oem consys`: `own` —
//! забрать владение у прошивки, `dma` — кольца, `fw` — прошивка и готовность,
//! `scan` — поиск и разбор ответов. `crate::wifi` проходит их подряд.
//!
//! # Кэш
//!
//! Блок читает и пишет память мимо кэша процессора: дескрипторы и буферы
//! выталкиваются (`dc civac`) до того, как их отдать, и перед тем, как читать
//! то, что записал блок.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::mtk_regs::{read32, write32};
use crate::sync::SpinLock;

/// Владение: `CONN_HIF_ON_LPCTL`. Запись 2 — «отдай нам», бит 0 в ответе —
/// «владеет прошивка».
const LPCTL: u64 = 0x1800_7000;
const IRQ_ENA: u64 = 0x1800_7008;
/// PDMA хоста.
const PDMA: u64 = 0x1800_4000;
const GLO_CFG: u64 = PDMA + 0x208;
const RST_PTR: u64 = PDMA + 0x20c;
const DUMMY_CR: u64 = PDMA + 0xa8;
/// DMASHDL.
const DMASHDL: u64 = 0x1800_6000;
/// Готовность прошивки: `SW_SYNC0`, биты 2:1.
const SW_SYNC0: u64 = 0x180c_1140;

/// Кольца: номер в железе.
const TX_DATA: u32 = 0;
const TX_FWDL: u32 = 3;
const TX_CMD: u32 = 15;
const RX_DATA: u32 = 0;
const RX_EVENT: u32 = 1;

const TX_SLOTS: usize = 16;
const RX_DATA_SLOTS: usize = 64;
const RX_EVENT_SLOTS: usize = 16;
const BUF: usize = 4096;

/// Где лежит наша память DMA: дескрипторы и буферы колец. Ноль — нет.
static BASE: AtomicU64 = AtomicU64::new(0);
/// Номер последовательности команд.
static SEQ: SpinLock<u8> = SpinLock::new(1);

/// Раскладка памяти DMA (смещения от начала).
const TX_RINGS: [u32; 3] = [TX_DATA, TX_FWDL, TX_CMD];
fn tx_desc_off(ring: usize) -> usize {
    ring * 0x1000
}
fn tx_buf_off(ring: usize) -> usize {
    0x3000 + ring * TX_SLOTS * BUF
}
const RX_DATA_DESC: usize = 0x3000 + 3 * TX_SLOTS * BUF;
const RX_EVENT_DESC: usize = RX_DATA_DESC + 0x1000;
const RX_DATA_BUF: usize = RX_EVENT_DESC + 0x1000;
const RX_EVENT_BUF: usize = RX_DATA_BUF + RX_DATA_SLOTS * BUF;
const DMA_SIZE: usize = RX_EVENT_BUF + RX_EVENT_SLOTS * BUF;

fn read(address: u64) -> Result<u32, &'static str> {
    read32(address).ok_or("register window not mapped")
}

fn write(address: u64, value: u32) -> Result<(), &'static str> {
    if write32(address, value) { Ok(()) } else { Err("register window not mapped") }
}

fn tx_reg(ring: u32, field: u64) -> u64 {
    PDMA + 0x300 + 0x10 * u64::from(ring) + field
}

fn rx_reg(ring: u32, field: u64) -> u64 {
    PDMA + 0x400 + 0x10 * u64::from(ring) + field
}

fn dcache(start: usize, len: usize) {
    let mut line = start & !63;
    while line < start + len {
        // SAFETY: адрес — в прямом отображении физической памяти ядра.
        unsafe { core::arch::asm!("dc civac, {}", in(reg) line) };
        line += 64;
    }
    // SAFETY: барьер без побочных эффектов.
    unsafe { core::arch::asm!("dsb sy") };
}

/// Виртуальный адрес памяти DMA и её физический адрес.
fn dma() -> Result<(usize, u64), &'static str> {
    let phys = BASE.load(Ordering::Relaxed);
    if phys == 0 {
        return Err("rings not set up: oem wlan dma");
    }
    let virt = crate::mm::frame::with(|frames| frames.access(crate::mm::PhysAddr::new(phys)).as_usize())
        .ok_or("frame allocator not ready")?;
    Ok((virt, phys))
}

fn put32(virt: usize, value: u32) {
    // SAFETY: адрес внутри нашей памяти DMA.
    unsafe { (virt as *mut u32).write_volatile(value) };
}

fn get32(virt: usize) -> u32 {
    // SAFETY: адрес внутри нашей памяти DMA.
    unsafe { (virt as *const u32).read_volatile() }
}

/// Забрать владение у прошивки (`halSetDriverOwn`).
pub fn own() -> Result<Vec<String>, &'static str> {
    let started = crate::time::uptime_ns();
    let mut last_kick = 0u64;
    loop {
        let now = crate::time::uptime_ns();
        if last_kick == 0 || now.wrapping_sub(last_kick) > 200_000_000 {
            write(LPCTL, 2)?;
            last_kick = now;
        }
        let value = read(LPCTL)?;
        if value & 1 == 0 {
            write(IRQ_ENA, 1)?;
            crate::kprintln!("  wlan        : driver own, LPCTL {value:#x}");
            return Ok(alloc::vec![alloc::format!("driver own, LPCTL {value:08x}")]);
        }
        if now.wrapping_sub(started) > 2_048_000_000 {
            return Err("firmware did not hand over the bus (LPCTL bit 0 stays)");
        }
        crate::sched::sleep_ms(1);
    }
}

/// Кольца DMA и планировщик DMASHDL (`halWpdmaInitRing`, `asicPcieDmaShdlInit`).
pub fn setup_dma() -> Result<Vec<String>, &'static str> {
    if BASE.load(Ordering::Relaxed) == 0 {
        let frames = DMA_SIZE.div_ceil(4096);
        let phys = crate::mm::frame::with(|f| f.allocate_contiguous(frames))
            .flatten()
            .ok_or("no contiguous memory for the Wi-Fi rings")?;
        BASE.store(phys.as_u64(), Ordering::Relaxed);
    }
    let (virt, phys) = dma()?;
    if phys + DMA_SIZE as u64 > 0x1_0000_0000 {
        return Err("Wi-Fi ring memory above 4 GiB");
    }

    // DMASHDL: размеры пакетов, группа 0.
    let max = read(DMASHDL + 0x1c)?;
    write(DMASHDL + 0x1c, (max & !0x0fff_0fff) | 0x1 | (0x18 << 16))?;
    write(DMASHDL + 0x10, 0xfffe_0000)?;
    write(DMASHDL + 0x20, 0x0fff_0003)?;
    for group in 1..15u64 {
        write(DMASHDL + 0x20 + group * 4, 0)?;
    }

    // Остановить DMA и дождаться, пока утихнет.
    let cfg = read(GLO_CFG)?;
    write(GLO_CFG, cfg & !(1 | (1 << 2)))?;
    let started = crate::time::uptime_ns();
    while read(GLO_CFG)? & ((1 << 1) | (1 << 3)) != 0 {
        if crate::time::uptime_ns().wrapping_sub(started) > 100_000_000 {
            return Err("PDMA stays busy");
        }
    }
    write(RST_PTR, 0xffff_ffff)?;

    // Передающие кольца: дескрипторы «пустые, выполнены».
    for (index, ring) in TX_RINGS.iter().enumerate() {
        let desc = virt + tx_desc_off(index);
        for slot in 0..TX_SLOTS {
            put32(desc + slot * 16, 0);
            put32(desc + slot * 16 + 4, 1 << 31);
            put32(desc + slot * 16 + 8, 0);
            put32(desc + slot * 16 + 12, 0);
        }
        dcache(desc, TX_SLOTS * 16);
        write(tx_reg(*ring, 0), (phys + tx_desc_off(index) as u64) as u32)?;
        write(tx_reg(*ring, 4), TX_SLOTS as u32)?;
        write(tx_reg(*ring, 8), 0)?;
        write(PDMA + 0x500 + 4 * index as u64, 0)?;
    }
    // Принимающие: каждому дескриптору — свой буфер; CPU_IDX = последний.
    for (ring, desc_off, buf_off, slots, ext) in [
        (RX_DATA, RX_DATA_DESC, RX_DATA_BUF, RX_DATA_SLOTS, 0x580u64),
        (RX_EVENT, RX_EVENT_DESC, RX_EVENT_BUF, RX_EVENT_SLOTS, 0x584),
    ] {
        let desc = virt + desc_off;
        for slot in 0..slots {
            put32(desc + slot * 16, (phys + (buf_off + slot * BUF) as u64) as u32);
            put32(desc + slot * 16 + 4, (BUF as u32) << 16);
            put32(desc + slot * 16 + 8, 0);
            put32(desc + slot * 16 + 12, 0);
        }
        dcache(desc, slots * 16);
        dcache(virt + buf_off, slots * BUF);
        write(rx_reg(ring, 0), (phys + desc_off as u64) as u32)?;
        write(PDMA + ext, 0)?;
        write(rx_reg(ring, 4), slots as u32)?;
        write(rx_reg(ring, 8), slots as u32 - 1)?;
    }
    // Включить: TX и RX, пакеты по 16 слов, обратная запись DDONE,
    // многоканальность, младший байт первым, без гашения такта.
    let cfg = read(GLO_CFG)?;
    let on = 1 | (1 << 2) | (3 << 4) | (1 << 6) | (2 << 10) | (1 << 12) | (1 << 30);
    write(GLO_CFG, (cfg & !((3 << 4) | (3 << 10))) | on)?;
    write(DUMMY_CR, 0x13)?;
    // `asicPdmaConfig`: о сбоях прошивки — прерыванием (маска битов 2..5),
    // и защита сна при подтверждении APSRC.
    write(PDMA + 0x1f4, 0x3c)?;
    let slp = read(PDMA + 0x160)?;
    write(PDMA + 0x160, slp | (1 << 4))?;
    let cfg = read(GLO_CFG)?;
    crate::kprintln!("  wlan        : rings at {phys:#x}, GLO_CFG {cfg:#x}");
    Ok(alloc::vec![alloc::format!("rings at {phys:#x} ({} KiB), GLO_CFG {cfg:08x}", DMA_SIZE / 1024)])
}

/// Положить пакет в передающее кольцо и дождаться, пока блок его заберёт.
fn transmit(ring: u32, packet: &[u8]) -> Result<(), &'static str> {
    let (virt, phys) = dma()?;
    let index = TX_RINGS.iter().position(|r| *r == ring).ok_or("unknown ring")?;
    if packet.len() > BUF {
        return Err("packet larger than a ring buffer");
    }
    let cpu = read(tx_reg(ring, 8))? as usize % TX_SLOTS;
    let buf = virt + tx_buf_off(index) + cpu * BUF;
    // SAFETY: буфер слота — в нашей памяти DMA, длина проверена.
    unsafe { core::ptr::copy_nonoverlapping(packet.as_ptr(), buf as *mut u8, packet.len()) };
    dcache(buf, packet.len());
    let desc = virt + tx_desc_off(index) + cpu * 16;
    put32(desc, (phys + (tx_buf_off(index) + cpu * BUF) as u64) as u32);
    put32(desc + 4, ((packet.len() as u32 & 0x3fff) << 16) | (1 << 30));
    put32(desc + 8, 0);
    put32(desc + 12, 0);
    dcache(desc, 16);
    let next = ((cpu + 1) % TX_SLOTS) as u32;
    write(tx_reg(ring, 8), next)?;
    let started = crate::time::uptime_ns();
    while read(tx_reg(ring, 12))? != next {
        if crate::time::uptime_ns().wrapping_sub(started) > 500_000_000 {
            return Err("Wi-Fi block did not take the packet from the ring");
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// Забрать следующий принятый пакет из кольца, если он есть к сроку.
fn receive(ring: u32, timeout_ms: u64) -> Result<Option<Vec<u8>>, &'static str> {
    let (virt, phys) = dma()?;
    let (desc_off, buf_off, slots) = match ring {
        RX_DATA => (RX_DATA_DESC, RX_DATA_BUF, RX_DATA_SLOTS),
        _ => (RX_EVENT_DESC, RX_EVENT_BUF, RX_EVENT_SLOTS),
    };
    let cpu = read(rx_reg(ring, 8))? as usize % slots;
    let slot = (cpu + 1) % slots;
    let desc = virt + desc_off + slot * 16;
    let started = crate::time::uptime_ns();
    loop {
        dcache(desc, 16);
        let ctrl = get32(desc + 4);
        if ctrl & (1 << 31) != 0 {
            let len = ((ctrl >> 16) & 0x3fff) as usize;
            let buf = virt + buf_off + slot * BUF;
            dcache(buf, len.min(BUF));
            let mut data = alloc::vec![0u8; len.min(BUF)];
            // SAFETY: буфер слота — в нашей памяти DMA.
            unsafe { core::ptr::copy_nonoverlapping(buf as *const u8, data.as_mut_ptr(), data.len()) };
            // Вернуть дескриптор блоку.
            put32(desc, (phys + (buf_off + slot * BUF) as u64) as u32);
            put32(desc + 4, (BUF as u32) << 16);
            dcache(desc, 16);
            write(rx_reg(ring, 8), slot as u32)?;
            return Ok(Some(data));
        }
        if crate::time::uptime_ns().wrapping_sub(started) > timeout_ms * 1_000_000 {
            return Ok(None);
        }
        crate::sched::sleep_ms(1);
    }
}

fn next_seq() -> u8 {
    let mut seq = SEQ.lock();
    *seq = seq.wrapping_add(1).max(1);
    *seq
}

/// Заголовок init-команды (`INIT_HIF_TX_HEADER`, 64 байта) и тело.
fn init_packet(pq: u16, pkt_ft: u8, cid: u8, seq: u8, payload: &[u8]) -> Vec<u8> {
    let total = 64 + payload.len();
    let mut packet = alloc::vec![0u8; total.next_multiple_of(4)];
    packet[0..2].copy_from_slice(&(total as u16).to_le_bytes());
    packet[2..4].copy_from_slice(&pq.to_le_bytes());
    packet[5] = 0xa0;
    packet[7] = pkt_ft;
    packet[36] = cid;
    packet[37] = 0xa0;
    packet[39] = seq;
    packet[64..total].copy_from_slice(payload);
    packet
}

/// Обычная команда (`WIFI_CMD`, 64 байта заголовка) — на кольцо команд.
fn wifi_command(cid: u8, set: bool, payload: &[u8]) -> Result<u8, &'static str> {
    let seq = next_seq();
    let total = (64 + payload.len()).next_multiple_of(4);
    let mut packet = alloc::vec![0u8; total];
    packet[0..2].copy_from_slice(&(total as u16).to_le_bytes());
    packet[2..4].copy_from_slice(&0x8000u16.to_le_bytes());
    packet[5] = 0xa0;
    packet[7] = 0x02;
    packet[32..34].copy_from_slice(&((total - 32) as u16).to_le_bytes());
    packet[36] = cid;
    packet[37] = 0xa0;
    packet[38] = u8::from(set);
    packet[39] = seq;
    packet[42] = 3;
    packet[64..64 + payload.len()].copy_from_slice(payload);
    transmit(TX_CMD, &packet)?;
    Ok(seq)
}

/// Дождаться события с номером `eid` на кольце событий (прочие — в журнал).
#[allow(dead_code)]
fn wait_event(eid: u8, timeout_ms: u64) -> Result<Vec<u8>, &'static str> {
    let started = crate::time::uptime_ms();
    loop {
        let left = timeout_ms.saturating_sub(crate::time::uptime_ms().saturating_sub(started));
        if left == 0 {
            return Err("no event from the Wi-Fi firmware");
        }
        match receive(RX_EVENT, left)? {
            Some(event) if event.len() > 20 && event[20] == eid => return Ok(event),
            Some(event) => {
                crate::kprintln!("  wlan        : event {:02x?}", &event[..event.len().min(32)]);
            }
            None => return Err("no event from the Wi-Fi firmware"),
        }
    }
}

const PQ_CMD: u16 = 0x8000;
const PQ_SCATTER: u16 = 0xf800;

/// Прошивка WLAN (`WIFI_RAM_CODE`): регионы EMI — копией, остальные —
/// командами загрузки; затем старт и ожидание готовности.
pub fn firmware() -> Result<Vec<String>, &'static str> {
    let path = alloc::format!("{}/WIFI_RAM_CODE_soc1_0_1_1.bin", super::mtk_consys::FIRMWARE_DIR);
    let image = match crate::fs::read(&path, 1 << 20) {
        Some(Ok((data, _))) => data,
        _ => return Err("WIFI_RAM_CODE missing in /lib/firmware/connsys"),
    };
    if image.len() < 36 {
        return Err("WIFI_RAM_CODE too short");
    }
    let common = &image[image.len() - 36..];
    let regions = usize::from(common[2]);
    if regions == 0 || regions > 10 || image.len() < 36 + 40 * regions {
        return Err("WIFI_RAM_CODE tailer does not parse");
    }
    let tailers = &image[image.len() - 36 - 40 * regions..image.len() - 36];
    let mut report = Vec::new();

    // Загрузка через кольцо 3 идёт мимо планировщика DMASHDL.
    let cfg = read(GLO_CFG)?;
    write(GLO_CFG, cfg | (1 << 9))?;

    let mut offset = 0usize;
    for index in 0..regions {
        let t = &tailers[index * 40..index * 40 + 40];
        let address = u32::from_le_bytes([t[16], t[17], t[18], t[19]]);
        let len = u32::from_le_bytes([t[20], t[21], t[22], t[23]]) as usize;
        let feature = t[24];
        let data = image.get(offset..offset + len).ok_or("region beyond the file")?;
        offset += len;
        if feature & 0x80 != 0 {
            super::mtk_consys::emi_write((address & 0x00ff_ffff) as usize, data)?;
            report.push(alloc::format!("emi {address:#010x} {len} B"));
            continue;
        }
        let mut mode = 0x8000_0000u32;
        if feature & 1 != 0 {
            mode |= 1 | (u32::from(feature) & 0x6) | (1 << 3);
        }
        let mut payload = [0u8; 12];
        payload[0..4].copy_from_slice(&address.to_le_bytes());
        payload[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        payload[8..12].copy_from_slice(&mode.to_le_bytes());
        let seq = next_seq();
        transmit(TX_FWDL, &init_packet(PQ_CMD, 2, 1, seq, &payload))?;
        let answer = receive(RX_EVENT, 1000)?.ok_or("no answer to DOWNLOAD_CONFIG")?;
        let status = answer.get(24).copied().unwrap_or(0xff);
        report.push(alloc::format!("cfg {address:#010x} {len} B mode {mode:08x} -> {status}"));
        if status != 0 {
            crate::kprintln!("  wlan        : download config answer {:02x?}", &answer[..answer.len().min(32)]);
            return Err("firmware refused DOWNLOAD_CONFIG");
        }
        for chunk in data.chunks(2048) {
            transmit(TX_FWDL, &init_packet(PQ_SCATTER, 3, 0, 0, chunk))?;
        }
    }
    // Старт: точки входа в файле нет — без подмены адреса.
    let seq = next_seq();
    transmit(TX_FWDL, &init_packet(PQ_CMD, 2, 2, seq, &[0u8; 8]))?;
    let answer = receive(RX_EVENT, 2000)?.ok_or("no answer to WIFI_START")?;
    report.push(alloc::format!("start -> {}", answer.get(24).copied().unwrap_or(0xff)));
    let cfg = read(GLO_CFG)?;
    write(GLO_CFG, cfg & !(1 << 9))?;

    // Готовность: биты 2:1 `SW_SYNC0`.
    let started = crate::time::uptime_ms();
    let mut sync = read(SW_SYNC0)?;
    while sync & 0x6 != 0x6 {
        if crate::time::uptime_ms().saturating_sub(started) > 5000 {
            report.push(alloc::format!("SW_SYNC0 {sync:08x} — not ready"));
            return Err("Wi-Fi firmware did not report ready");
        }
        crate::sched::sleep_ms(5);
        sync = read(SW_SYNC0)?;
    }
    report.push(alloc::format!("ready, SW_SYNC0 {sync:08x}"));
    crate::kprintln!("  wlan        : firmware ready, SW_SYNC0 {sync:#x}");
    Ok(report)
}

/// Найденная сеть.
#[derive(Clone, Debug)]
pub struct Network {
    pub ssid: String,
    pub bssid: [u8; 6],
    pub channel: u8,
}

static NETWORKS: SpinLock<Vec<Network>> = SpinLock::new(Vec::new());

/// Последний список сетей.
#[must_use]
pub fn networks() -> Vec<Network> {
    NETWORKS.lock().clone()
}

/// Найти в принятом пакете кадр beacon/probe-response и вынуть SSID.
fn parse_beacon(packet: &[u8]) -> Option<Network> {
    if packet.len() < 16 + 36 {
        return None;
    }
    let kind = u16::from_le_bytes([packet[2], packet[3]]);
    let groups = (kind >> 9) & 0xf;
    let mut off = 16usize;
    if groups & (1 << 3) != 0 {
        off += 16; // G4
    }
    if groups & 1 != 0 {
        off += 16; // G1
    }
    if groups & (1 << 1) != 0 {
        off += 8; // G2
    }
    if groups & (1 << 2) != 0 {
        off += 24; // G3
    }
    if packet[6] & 0x40 != 0 {
        off += 2;
    }
    // Где начинается кадр 802.11 — по группам дескриптора; если там не
    // beacon/probe-response, поискать его на ближних границах по 4 байта
    // (раскладка групп у этой прошивки проверена только косвенно).
    let looks = |at: usize| packet.get(at..).is_some_and(|f| f.len() >= 36 && matches!(f[0], 0x80 | 0x50) && f[1] == 0);
    let off = if looks(off) {
        off
    } else {
        (16..packet.len().min(128)).step_by(2).find(|at| looks(*at))?
    };
    let frame = packet.get(off..)?;
    let mut bssid = [0u8; 6];
    bssid.copy_from_slice(&frame[16..22]);
    let mut ies = &frame[36..];
    let mut ssid = String::new();
    let mut channel = packet[5];
    while ies.len() >= 2 {
        let (tag, len) = (ies[0], usize::from(ies[1]));
        let body = ies.get(2..2 + len)?;
        match tag {
            0 => ssid = String::from_utf8_lossy(body).into_owned(),
            3 if len == 1 => channel = body[0],
            _ => {}
        }
        ies = &ies[2 + len..];
    }
    Some(Network { ssid, bssid, channel })
}

/// Поиск сетей: активировать BSS, запросить поиск, собрать кадры до конца.
pub fn scan() -> Result<Vec<String>, &'static str> {
    let mut report = Vec::new();
    // Пустая команда — драйвер шлёт её первой после готовности.
    wifi_command(0x00, true, &[])?;
    // Сеть AIS (клиент) на BSS 0 с локальным адресом.
    let mut bss = [0u8; 12];
    bss[1] = 1;
    bss[4..10].copy_from_slice(&[0x02, 0xf0, 0x55, 0x41, 0x69, 0x01]);
    wifi_command(0x11, true, &bss)?;
    // CMD_SCAN_REQ_V2: активный, любой SSID, все каналы.
    let mut request = alloc::vec![0u8; 1188];
    let seq = next_seq();
    request[0] = seq;
    request[2] = 1;
    request[3] = 1;
    request[7] = 1;
    request[1112..1118].copy_from_slice(&[0xff; 6]);
    wifi_command(0x03, true, &request)?;

    let mut found: Vec<Network> = Vec::new();
    let started = crate::time::uptime_ms();
    let mut done = false;
    let mut dumped = false;
    while crate::time::uptime_ms().saturating_sub(started) < 8000 && !done {
        while let Some(packet) = receive(RX_DATA, 5)? {
            if let Some(net) = parse_beacon(&packet) {
                if !found.iter().any(|known| known.bssid == net.bssid) {
                    crate::kprintln!("  wlan        : '{}' ch {}", net.ssid, net.channel);
                    found.push(net);
                }
            }
        }
        // На аппарате (03.11) кадры эфира приходят в кольцо событий, а не
        // данных: тип пакета `0xE001` (кадр управления от прошивки), пятый
        // байт — канал. Событие прошивки — тип `0xE000`, номер на 20-м байте.
        while let Some(event) = receive(RX_EVENT, 20)? {
            let kind = event.get(2..4).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            if kind & 0xe00f == 0xe001 {
                if let Some(net) = parse_beacon(&event) {
                    if !found.iter().any(|known| known.bssid == net.bssid) {
                        crate::kprintln!("  wlan        : '{}' ch {}", net.ssid, net.channel);
                        found.push(net);
                    }
                } else if !dumped {
                    dumped = true;
                    for row in event[..event.len().min(96)].chunks(24) {
                        crate::kprintln!("  wlan        : raw {row:02x?}");
                    }
                }
            } else if kind == 0xe000 && event.get(20) == Some(&0x0d) {
                done = true;
            } else {
                crate::kprintln!("  wlan        : event {:02x?}", &event[..event.len().min(32)]);
            }
        }
    }
    report.push(alloc::format!("{} network(s){}", found.len(), if done { "" } else { ", no scan-done event" }));
    for net in found.iter().take(6) {
        report.push(alloc::format!("{} ch {}", if net.ssid.is_empty() { "(hidden)" } else { &net.ssid }, net.channel));
    }
    *NETWORKS.lock() = found;
    Ok(report)
}

/// Весь путь после запущенного блока связи: владение, кольца, прошивка.
pub fn bring_up() -> Result<Vec<String>, &'static str> {
    let mut report = own()?;
    report.extend(setup_dma()?);
    report.extend(firmware()?);
    Ok(report)
}
