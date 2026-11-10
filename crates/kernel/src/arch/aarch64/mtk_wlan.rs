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

/// 64, а не 16 (10.11): на странице по TLS 16 мест переполнялись (130 кадров
/// из 2552 отброшено), и TCP ждал повтора по таймеру — страница шла 50 с.
const TX_SLOTS: usize = 64;
const RX_DATA_SLOTS: usize = 64;
/// Кольцо событий большое: кадры эфира при поиске идут сюда же, потоком,
/// и на 16 местах (как у драйвера) прошивка переполняла его и вставала, если
/// задача стола не успевала забирать (03.11: две сети вместо тринадцати).
const RX_EVENT_SLOTS: usize = 128;
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
    *RX_NEXT.lock() = [0, 0];
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

/// Кольцо передачи — одно на команды прошивки и кадры данных (04.11): их
/// кладут разные задачи, и без замка две записи в один слот портили кольцо.
/// Замок — только на запись слота и индекса, ждать под ним нельзя.
static TX_LOCK: SpinLock<()> = SpinLock::new(());

/// Положить пакет в передающее кольцо и дождаться, пока блок его заберёт.
fn transmit(ring: u32, packet: &[u8]) -> Result<(), &'static str> {
    let slot = enqueue(ring, packet, false)?;
    let started = crate::time::uptime_ns();
    // Забран — когда блок ушёл со слота (за ним могли положить ещё).
    while read(tx_reg(ring, 12))? as usize % TX_SLOTS == slot {
        if crate::time::uptime_ns().wrapping_sub(started) > 500_000_000 {
            return Err("Wi-Fi block did not take the packet from the ring");
        }
        core::hint::spin_loop();
    }
    Ok(())
}

/// Положить пакет и не ждать — для кадров данных: сетевая задача держит
/// замок сети, и ожидание под ним 04.11 повесило телефон целиком (браузер
/// слал кадры, блок их не забирал, по полсекунды на каждый с закрытыми
/// прерываниями). Места нет — `Err`, кадр повторит TCP.
fn transmit_nowait(ring: u32, packet: &[u8]) -> Result<(), &'static str> {
    enqueue(ring, packet, true).map(|_| ())
}

/// Записать пакет в следующий слот кольца; номер слота. `need_room` —
/// отказать, если кольцо полно.
fn enqueue(ring: u32, packet: &[u8], need_room: bool) -> Result<usize, &'static str> {
    let (virt, phys) = dma()?;
    let index = TX_RINGS.iter().position(|r| *r == ring).ok_or("unknown ring")?;
    if packet.len() > BUF {
        return Err("packet larger than a ring buffer");
    }
    let _guard = TX_LOCK.lock();
    let cpu = read(tx_reg(ring, 8))? as usize % TX_SLOTS;
    if need_room && (cpu + 1) % TX_SLOTS == read(tx_reg(ring, 12))? as usize % TX_SLOTS {
        return Err("transmit ring full");
    }
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
    Ok(cpu)
}

/// Следующий слот к чтению в кольцах приёма (данные, события).
static RX_NEXT: SpinLock<[usize; 2]> = SpinLock::new([0, 0]);

/// Дескрипторов в строке кэша (16 байт против 64).
const DESC_PER_LINE: usize = 4;

/// Забрать следующий принятый пакет из кольца, если он есть к сроку.
///
/// Дескрипторы возвращаются блоку только целой строкой кэша, когда все
/// четыре прочитаны (10.11): возврат по одному писал в память всю строку
/// с устаревшими соседями и затирал дескриптор, который блок как раз
/// заполнил, — приём вставал навсегда на первой же странице по TLS.
fn receive(ring: u32, timeout_ms: u64) -> Result<Option<Vec<u8>>, &'static str> {
    let (virt, phys) = dma()?;
    let (desc_off, buf_off, slots, which) = match ring {
        RX_DATA => (RX_DATA_DESC, RX_DATA_BUF, RX_DATA_SLOTS, 0),
        _ => (RX_EVENT_DESC, RX_EVENT_BUF, RX_EVENT_SLOTS, 1),
    };
    let started = crate::time::uptime_ns();
    loop {
        {
            let mut next = RX_NEXT.lock();
            let slot = next[which] % slots;
            let desc = virt + desc_off + slot * 16;
            // Строка чистая (мы её не пишем, пока блок владеет соседями),
            // так что это только сброс копии.
            dcache(desc, 16);
            let ctrl = get32(desc + 4);
            if ctrl & (1 << 31) != 0 {
                let len = ((ctrl >> 16) & 0x3fff) as usize;
                let buf = virt + buf_off + slot * BUF;
                dcache(buf, len.min(BUF));
                let mut data = alloc::vec![0u8; len.min(BUF)];
                // SAFETY: буфер слота — в нашей памяти DMA.
                unsafe { core::ptr::copy_nonoverlapping(buf as *const u8, data.as_mut_ptr(), data.len()) };
                next[which] = (slot + 1) % slots;
                if slot % DESC_PER_LINE == DESC_PER_LINE - 1 {
                    // Вся строка прочитана — вернуть четыре дескриптора разом.
                    let first = slot + 1 - DESC_PER_LINE;
                    for s in first..=slot {
                        let d = virt + desc_off + s * 16;
                        put32(d, (phys + (buf_off + s * BUF) as u64) as u32);
                        put32(d + 4, (BUF as u32) << 16);
                        put32(d + 8, 0);
                        put32(d + 12, 0);
                    }
                    dcache(virt + desc_off + first * 16, DESC_PER_LINE * 16);
                    write(rx_reg(ring, 8), slot as u32)?;
                }
                return Ok(Some(data));
            }
        }
        if crate::time::uptime_ns().wrapping_sub(started) > timeout_ms * 1_000_000 {
            return Ok(None);
        }
        // Короткие ожидания (поиск сетей) — без сна: квант планировщика
        // длиннее, чем блок успевает заполнить кольцо.
        if timeout_ms > 20 {
            crate::sched::sleep_ms(1);
        } else {
            core::hint::spin_loop();
        }
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
    /// Тело RSN IE точки доступа (без тега и длины); пусто — сеть открытая
    /// или WPA1. По нему выбираются шифры в запросе ассоциации: MARK69
    /// отказал с кодом 41 («не тот групповой шифр»), когда мы всегда
    /// заявляли CCMP, а у неё группа — TKIP (смешанный WPA/WPA2).
    pub rsn: Vec<u8>,
    /// Тело старого WPA IE (`dd .. 00 50 f2 01`); у второй точки MARK69 (04.11)
    /// RSN нет — на ней, видимо, только WPA1.
    pub wpa: Vec<u8>,
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
    let mut rsn = Vec::new();
    let mut wpa = Vec::new();
    while ies.len() >= 2 {
        let (tag, len) = (ies[0], usize::from(ies[1]));
        let Some(body) = ies.get(2..2 + len) else { break };
        match tag {
            0 => ssid = String::from_utf8_lossy(body).into_owned(),
            3 if len == 1 => channel = body[0],
            48 => rsn = body.to_vec(),
            221 if body.starts_with(&[0x00, 0x50, 0xf2, 0x01]) => wpa = body[4..].to_vec(),
            _ => {}
        }
        ies = &ies[2 + len..];
    }
    Some(Network { ssid, bssid, channel, rsn, wpa })
}

/// Сеть в журнал: имя, канал, точка и её RSN — короткими строками (`oem
/// log` обрезает длинные). По RSN разбирают отказы ассоциации (коды 41, 43).
fn log_network(net: &Network) {
    let b = net.bssid;
    crate::kprintln!("  wlan        : '{}' ch {} {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}", net.ssid, net.channel, b[0], b[1], b[2], b[3], b[4], b[5]);
    for row in net.rsn.chunks(16) {
        let mut hex = String::new();
        for byte in row {
            hex.push_str(&alloc::format!("{byte:02x}"));
        }
        crate::kprintln!("  wlan        :   rsn {hex}");
    }
    for row in net.wpa.chunks(16) {
        let mut hex = String::new();
        for byte in row {
            hex.push_str(&alloc::format!("{byte:02x}"));
        }
        crate::kprintln!("  wlan        :   wpa {hex}");
    }
}

/// BSS уже активирован (пустая команда и `BSS_ACTIVATE` — один раз).
static ACTIVATED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Поиск сетей: до двух проходов, если первый не дошёл до конца.
pub fn scan() -> Result<Vec<String>, &'static str> {
    let _busy = Busy::take();
    // Пустая команда и сеть AIS (клиент) на BSS 0 — один раз.
    activate()?;
    let mut found: Vec<Network> = Vec::new();
    let mut done = false;
    for _ in 0..2 {
        done = scan_pass(&mut found)?;
        if done {
            break;
        }
    }
    let mut report = Vec::new();
    report.push(alloc::format!("{} network(s){}", found.len(), if done { "" } else { ", no scan-done event" }));
    for net in found.iter().take(6) {
        report.push(alloc::format!("{} ch {}", if net.ssid.is_empty() { "(hidden)" } else { &net.ssid }, net.channel));
    }
    found.sort_by_key(|net| net.channel);
    *NETWORKS.lock() = found;
    Ok(report)
}

/// Один проход поиска: запрос и сбор кадров до события конца или срока.
fn scan_pass(found: &mut Vec<Network>) -> Result<bool, &'static str> {
    // CMD_SCAN_REQ_V2: активный, любой SSID, все каналы.
    let mut request = alloc::vec![0u8; 1188];
    let seq = next_seq();
    request[0] = seq;
    request[2] = 1;
    request[3] = 1;
    request[7] = 1;
    request[1112..1118].copy_from_slice(&[0xff; 6]);
    wifi_command(0x03, true, &request)?;

    let started = crate::time::uptime_ms();
    let mut done = false;
    let mut dumped = false;
    while crate::time::uptime_ms().saturating_sub(started) < 8000 && !done {
        while let Some(packet) = receive(RX_DATA, 5)? {
            if let Some(net) = parse_beacon(&packet) {
                if !found.iter().any(|known| known.bssid == net.bssid) {
                    log_network(&net);
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
                        log_network(&net);
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
    Ok(done)
}

// ---------------------------------------------------------------------------
// Подключение к сети WPA2-PSK (03.11)
// ---------------------------------------------------------------------------
//
// Порядок и поля — из gen4m (`saa_fsm.c`, `ais_fsm.c`, `cnm_mem.c`,
// `nic.c: nicUpdateBss`, `wlan_oid.c: wlanoidSetAddKey`): право на канал
// (CID 0x1C, событие 0x10), запись STA_REC о точке доступа (CID 0x13),
// кадры аутентификации и ассоциации — по кольцу команд с 32-байтным TXD,
// ответы — в кольце событий как кадры `0xE001`; затем BSS_INFO (0x12),
// STA_REC в состоянии 3, четырёхсторонний обмен (крейт `wpa`) кадрами EAPOL
// тем же кольцом, ключи — CID 0x07.

/// Наш адрес (локально назначенный) — тот же, что в `BSS_ACTIVATE`.
const OWN_MAC: [u8; 6] = [0x02, 0xf0, 0x55, 0x41, 0x69, 0x01];
/// Место в таблице станций (WTBL): широковещательная запись BSS — 4, точка
/// доступа — 5, как раздаёт драйвер gen4m (`privacy.c`: записи 0–3 —
/// для BIP/IGTK, `ucStartIDX = 4`). До 05.11 было 0 и 1: групповой ключ
/// ложился в запись BIP.
const AP_STA_INDEX: u8 = 0;
const AP_WLAN: u8 = 5;
const BMC_WLAN: u8 = 4;

/// Подключение: имя сети и что сделано — для «Параметров».
static CONNECTED: SpinLock<Option<(String, [u8; 6])>> = SpinLock::new(None);

/// К какой сети подключены.
#[must_use]
pub fn connected() -> Option<String> {
    CONNECTED.lock().as_ref().map(|(ssid, _)| ssid.clone())
}

fn activate() -> Result<(), &'static str> {
    if !ACTIVATED.swap(true, Ordering::Relaxed) {
        wifi_command(0x00, true, &[])?;
        let mut bss = [0u8; 12];
        bss[1] = 1;
        bss[4..10].copy_from_slice(&OWN_MAC);
        bss[10] = BMC_WLAN;
        wifi_command(0x11, true, &bss)?;
    }
    Ok(())
}

/// Событие прошивки `eid` (тип пакета `0xE000`), прочее — мимо, к сроку.
fn wait_fw_event(eid: u8, timeout_ms: u64) -> Option<Vec<u8>> {
    let started = crate::time::uptime_ms();
    while crate::time::uptime_ms().saturating_sub(started) < timeout_ms {
        let Ok(Some(event)) = receive(RX_EVENT, 50) else { continue };
        let kind = event.get(2..4).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        if kind == 0xe000 && event.get(20) == Some(&eid) {
            return Some(event);
        }
    }
    None
}

/// Начало кадра 802.11 в принятом пакете: по группам дескриптора, а если
/// там не тот кадр — ближняя граница, где первый байт равен `fc`.
fn frame_in(packet: &[u8], fc: u8) -> Option<&[u8]> {
    let kind = u16::from_le_bytes([*packet.get(2)?, *packet.get(3)?]);
    let groups = (kind >> 9) & 0xf;
    let mut off = 16usize;
    for (bit, len) in [(3u16, 16usize), (0, 16), (1, 8), (2, 24)] {
        if groups & (1 << bit) != 0 {
            off += len;
        }
    }
    if packet.get(6).is_some_and(|b| b & 0x40 != 0) {
        off += 2;
    }
    let fits = |at: usize| packet.get(at..at + 24).is_some_and(|f| f[0] == fc && f[1] == 0);
    let at = if fits(off) { off } else { (16..packet.len().min(128)).step_by(2).find(|at| fits(*at))? };
    packet.get(at..)
}

/// Ждать кадр управления `fc` от точки доступа `bssid`.
fn wait_mgmt(fc: u8, bssid: &[u8; 6], timeout_ms: u64) -> Option<Vec<u8>> {
    let started = crate::time::uptime_ms();
    let mut other = 0u32;
    while crate::time::uptime_ms().saturating_sub(started) < timeout_ms {
        let Ok(Some(packet)) = receive(RX_EVENT, 20) else { continue };
        if let Some(frame) = frame_in(&packet, fc) {
            if frame.get(10..16) == Some(bssid) {
                return Some(frame.to_vec());
            }
        }
        // Что пришло вместо ответа — в журнал (первые три): так видно,
        // слышит ли радио эфир вообще.
        other += 1;
        if other <= 3 {
            let kind = packet.get(2..4).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            crate::kprintln!("  wlan        : not ours: type {kind:#06x}, {} B", packet.len());
        }
    }
    if other > 3 {
        crate::kprintln!("  wlan        : {other} packets, none from the access point");
    }
    None
}

/// Кадр EAPOL в любом принятом пакете: после `88 8E` (Ethernet II или LLC)
/// идёт заголовок 802.1X — версия 1 или 2, тип 3 (Key).
fn eapol_in(packet: &[u8]) -> Option<&[u8]> {
    let at = (0..packet.len().saturating_sub(4)).find(|i| {
        packet[*i] == 0x88 && packet[*i + 1] == 0x8e && matches!(packet[*i + 2], 1 | 2) && packet[*i + 3] == 3
    })?;
    let body = &packet[at + 2..];
    let len = 4 + usize::from(u16::from_be_bytes([*body.get(2)?, *body.get(3)?]));
    body.get(..len)
}

/// Ждать EAPOL-Key от точки доступа — в обоих кольцах приёма.
fn wait_eapol(timeout_ms: u64) -> Option<Vec<u8>> {
    let started = crate::time::uptime_ms();
    while crate::time::uptime_ms().saturating_sub(started) < timeout_ms {
        for ring in [RX_DATA, RX_EVENT] {
            if let Ok(Some(packet)) = receive(ring, 10) {
                if let Some(eapol) = eapol_in(&packet) {
                    return Some(eapol.to_vec());
                }
            }
        }
    }
    None
}

/// Ждать подписанное сообщение ключа: личного (`pairwise`) или группового.
/// Повтор первого сообщения (без MIC) или третьего — пропускаются: иначе
/// опоздавший повтор читался как «неверный пароль».
fn wait_key(timeout_ms: u64, pairwise: bool) -> Option<Vec<u8>> {
    let started = crate::time::uptime_ms();
    loop {
        let left = timeout_ms.checked_sub(crate::time::uptime_ms().saturating_sub(started))?;
        let frame = wait_eapol(left.max(1))?;
        let Some(key) = wpa::parse(&frame) else { continue };
        let signed = key.info & wpa::info::MIC != 0;
        if signed && (key.info & wpa::info::PAIRWISE != 0) == pairwise {
            return Some(frame);
        }
        crate::kprintln!("  wlan        : skipped EAPOL-Key info {:#06x}", key.info);
    }
}

/// TXD для кадра управления: формат 802.11, очередь MCU, нижняя скорость.
fn mgmt_txd(frame_len: usize, subtype: u8) -> [u8; 32] {
    let mut txd = [0u8; 32];
    txd[0..2].copy_from_slice(&((32 + frame_len) as u16).to_le_bytes());
    txd[2] = 14;
    txd[3] = 0x84;
    txd[4] = AP_WLAN;
    txd[5] = 0xcc;
    txd[7] = 2;
    txd[8] = subtype;
    txd[11] = 0x80;
    txd[12..14].copy_from_slice(&0xf000u16.to_le_bytes());
    txd
}

/// TXD для кадра EAPOL: формат Ethernet II.
fn eapol_txd(frame_len: usize) -> [u8; 32] {
    let mut txd = [0u8; 32];
    txd[0..2].copy_from_slice(&((32 + frame_len) as u16).to_le_bytes());
    txd[2] = 8;
    txd[3] = 0x84;
    txd[4] = AP_WLAN;
    txd[5] = 0x90;
    txd[7] = 2;
    txd[11] = 0x80;
    txd[12..14].copy_from_slice(&0xf000u16.to_le_bytes());
    txd
}

fn send_frame(txd: [u8; 32], frame: &[u8]) -> Result<(), &'static str> {
    let mut packet = Vec::with_capacity(32 + frame.len());
    packet.extend_from_slice(&txd);
    packet.extend_from_slice(frame);
    while packet.len() % 4 != 0 {
        packet.push(0);
    }
    transmit(TX_CMD, &packet)
}

fn mgmt_header(fc: u8, bssid: &[u8; 6]) -> Vec<u8> {
    let mut frame = alloc::vec![fc, 0, 0, 0];
    frame.extend_from_slice(bssid);
    frame.extend_from_slice(&OWN_MAC);
    frame.extend_from_slice(bssid);
    frame.extend_from_slice(&[0, 0]);
    frame
}

/// Наборы скоростей b/g: 1, 2, 5.5, 11 (обязательные), 6, 9, 12, 18 и 24–54.
const RATES: [u8; 8] = [0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24];
const EXT_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6c];
/// HT Capabilities: 20 МГц, короткий интервал, SM PS выключен, MCS 0–7.
const HT_CAP_IE: [u8; 28] = [
    45, 26, 0x2c, 0x00, 0x03, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];
/// WMM Information Element (станция, без U-APSD).
const WMM_IE: [u8; 9] = [0xdd, 0x07, 0x00, 0x50, 0xf2, 0x02, 0x00, 0x01, 0x00];
/// Код отказа точки доступа при последней попытке — для «Параметров».
static LAST_STATUS: SpinLock<Option<u16>> = SpinLock::new(None);

/// Код отказа последней ассоциации, если он был.
#[must_use]
pub fn last_status() -> Option<u16> {
    *LAST_STATUS.lock()
}
const RATE_MASK_ALL: u16 = 0x3fcf;
const RATE_MASK_BASIC: u16 = 0x000f;

/// STA_REC точки доступа (CID 0x13, 136 байт): состояние 0/1/2 = 1/2/3.
fn sta_rec(bssid: &[u8; 6], state: u8, aid: u16, need_response: bool) -> Result<(), &'static str> {
    let mut rec = [0u8; 136];
    rec[0] = AP_STA_INDEX;
    rec[1] = 0x41;
    rec[2..8].copy_from_slice(bssid);
    rec[8..10].copy_from_slice(&aid.to_le_bytes());
    rec[10..12].copy_from_slice(&10u16.to_le_bytes());
    // b/g + HT (бит 4); QoS — HT без него не бывает.
    rec[13] = 0x13;
    rec[14..16].copy_from_slice(&RATE_MASK_ALL.to_le_bytes());
    rec[16..18].copy_from_slice(&RATE_MASK_BASIC.to_le_bytes());
    rec[18] = 1;
    rec[20] = state;
    rec[51] = u8::from(need_response);
    rec[54] = AP_WLAN;
    rec[55] = 0xff;
    rec[68] = 3;
    wifi_command(0x13, true, &rec)?;
    Ok(())
}

/// BSS_INFO после ассоциации (CID 0x12, 116 байт).
fn bss_info(ssid: &str, bssid: &[u8; 6], channel: u8, secure: bool) -> Result<(), &'static str> {
    let mut info = [0u8; 116];
    info[1] = 0; // connected
    let name = ssid.as_bytes();
    info[3] = name.len().min(32) as u8;
    info[4..4 + name.len().min(32)].copy_from_slice(&name[..name.len().min(32)]);
    info[36..42].copy_from_slice(bssid);
    info[44..46].copy_from_slice(&RATE_MASK_ALL.to_le_bytes());
    info[46..48].copy_from_slice(&RATE_MASK_BASIC.to_le_bytes());
    info[48] = AP_STA_INDEX;
    info[52] = 1;
    info[53] = if secure { 7 } else { 0 };
    info[54] = if secure { 6 } else { 0 };
    info[55] = 0x13;
    info[42] = 1; // QBSS
    info[58] = BMC_WLAN;
    info[69] = 1;
    info[70] = channel;
    info[88] = 1;
    info[89] = 1;
    info[93] = 1;
    wifi_command(0x12, true, &info)?;
    Ok(())
}

/// Ключ в прошивку (CID 0x07, 64 байта): пара или группа, CCMP.
fn add_key(peer: &[u8; 6], pairwise: bool, key_id: u8, algorithm: u8, key: &[u8]) -> Result<(), &'static str> {
    let mut cmd = [0u8; 64];
    cmd[0] = 1;
    cmd[1] = u8::from(pairwise);
    cmd[2] = u8::from(pairwise);
    cmd[4..10].copy_from_slice(peer);
    cmd[11] = algorithm; // 4 — CCMP, 2 — TKIP
    cmd[12] = key_id;
    cmd[13] = key.len().min(32) as u8;
    cmd[14] = if pairwise { AP_WLAN } else { BMC_WLAN };
    cmd[16..16 + key.len().min(32)].copy_from_slice(&key[..key.len().min(32)]);
    wifi_command(0x07, true, &cmd)?;
    if !pairwise {
        *KEYS.lock() = Some(algorithm);
    } else {
        KEYS.lock().get_or_insert(4);
    }
    Ok(())
}

/// Снять ключи прошлого сеанса (тот же CID 0x07, «убрать»): с ними в
/// прошивке второе сообщение обмена уходило зашифрованным старым ключом, и
/// точка его не понимала (04.11 — повторное подключение без перезагрузки).
///
/// Только если ключи ставили мы, и с тем алгоритмом, что ставили: команда
/// «убрать» с нулевым алгоритмом 04.11 остановила прошивку (канал больше не
/// выдавался, кольцо команд встало).
fn remove_keys(peer: &[u8; 6]) {
    let Some(group_algorithm) = KEYS.lock().take() else { return };
    for (pairwise, key_id, algorithm) in [(true, 0u8, 4u8), (false, 1, group_algorithm), (false, 2, group_algorithm)] {
        let mut cmd = [0u8; 64];
        cmd[1] = u8::from(pairwise);
        cmd[2] = u8::from(pairwise);
        cmd[4..10].copy_from_slice(peer);
        cmd[11] = algorithm;
        cmd[12] = key_id;
        cmd[14] = if pairwise { AP_WLAN } else { BMC_WLAN };
        let _ = wifi_command(0x07, true, &cmd);
    }
}

/// Ключи в прошивке: алгоритм группового (`None` — ключей нет).
static KEYS: SpinLock<Option<u8>> = SpinLock::new(None);

/// Строка отчёта подключения — и в журнал: при ошибке отчёт не доходит до
/// «Параметров», а шаг, на котором встали, нужен.
fn note(report: &mut Vec<String>, line: String) {
    crate::kprintln!("  wlan        : {line}");
    report.push(line);
}

/// Шифр группы, как его понимает прошивка: номер алгоритма и длина ключа.
#[derive(Clone, Copy)]
struct GroupCipher {
    algorithm: u8,
    key_len: usize,
}

/// Наш RSN IE под точку доступа: её групповой шифр, личный CCMP, PSK.
fn station_rsn(ap: &[u8]) -> Result<(Vec<u8>, GroupCipher), &'static str> {
    if ap.len() < 8 {
        return Err("network has no WPA2 (RSN) element");
    }
    let group = &ap[2..6];
    let group_cipher = match group {
        [0x00, 0x0f, 0xac, 0x04] => GroupCipher { algorithm: 4, key_len: 16 },
        [0x00, 0x0f, 0xac, 0x02] => GroupCipher { algorithm: 2, key_len: 32 },
        _ => return Err("group cipher of this network is not supported"),
    };
    let pairwise_count = usize::from(u16::from_le_bytes([ap[6], ap[7]]));
    let pairwise = ap.get(8..8 + 4 * pairwise_count).ok_or("broken RSN element")?;
    if !pairwise.chunks(4).any(|suite| suite == [0x00, 0x0f, 0xac, 0x04]) {
        return Err("network offers no AES (CCMP) for the station");
    }
    let at = 8 + 4 * pairwise_count;
    let akm_count = usize::from(u16::from_le_bytes([*ap.get(at).ok_or("broken RSN element")?, *ap.get(at + 1).ok_or("broken RSN element")?]));
    let akms = ap.get(at + 2..at + 2 + 4 * akm_count).ok_or("broken RSN element")?;
    if !akms.chunks(4).any(|suite| suite == [0x00, 0x0f, 0xac, 0x02]) {
        return Err("network wants WPA3 or 802.1X, not a password (PSK)");
    }
    let mut ie = alloc::vec![0x30, 0x14, 0x01, 0x00];
    ie.extend_from_slice(group);
    ie.extend_from_slice(&[0x01, 0x00, 0x00, 0x0f, 0xac, 0x04]);
    ie.extend_from_slice(&[0x01, 0x00, 0x00, 0x0f, 0xac, 0x02]);
    ie.extend_from_slice(&[0x00, 0x00]);
    Ok((ie, group_cipher))
}

/// Наш WPA IE (старый WPA) под точку: её групповой шифр, личный AES, PSK.
/// `ap` — тело её WPA IE после OUI и типа.
fn station_wpa(ap: &[u8]) -> Result<(Vec<u8>, GroupCipher), &'static str> {
    const MS: [u8; 3] = [0x00, 0x50, 0xf2];
    if ap.len() < 8 {
        return Err("network has no WPA2 (RSN) element");
    }
    let group = &ap[2..6];
    let group_cipher = match group {
        [0x00, 0x50, 0xf2, 0x04] => GroupCipher { algorithm: 4, key_len: 16 },
        [0x00, 0x50, 0xf2, 0x02] => GroupCipher { algorithm: 2, key_len: 32 },
        _ => return Err("group cipher of this network is not supported"),
    };
    let pairwise_count = usize::from(u16::from_le_bytes([ap[6], ap[7]]));
    let pairwise = ap.get(8..8 + 4 * pairwise_count).ok_or("broken RSN element")?;
    if !pairwise.chunks(4).any(|suite| suite[..3] == MS && suite[3] == 4) {
        return Err("network offers no AES (CCMP) for the station");
    }
    let at = 8 + 4 * pairwise_count;
    let akm_count = usize::from(u16::from_le_bytes([*ap.get(at).ok_or("broken RSN element")?, *ap.get(at + 1).ok_or("broken RSN element")?]));
    let akms = ap.get(at + 2..at + 2 + 4 * akm_count).ok_or("broken RSN element")?;
    if !akms.chunks(4).any(|suite| suite[..3] == MS && suite[3] == 2) {
        return Err("network wants WPA3 or 802.1X, not a password (PSK)");
    }
    let mut ie = alloc::vec![0xdd, 0x16, 0x00, 0x50, 0xf2, 0x01, 0x01, 0x00];
    ie.extend_from_slice(group);
    ie.extend_from_slice(&[0x01, 0x00, 0x00, 0x50, 0xf2, 0x04]);
    ie.extend_from_slice(&[0x01, 0x00, 0x00, 0x50, 0xf2, 0x02]);
    Ok((ie, group_cipher))
}

/// Подключиться к сети: `password` пустой — открытая сеть.
///
/// У сети бывает несколько точек (MARK69 — две на одном канале): отказала
/// одна — пробуем следующую.
pub fn connect(ssid: &str, password: &str) -> Result<Vec<String>, &'static str> {
    let _busy = Busy::take();
    disconnect();
    let mut candidates: Vec<Network> = networks().into_iter().filter(|net| net.ssid == ssid).collect();
    // Сначала точки с RSN (WPA2), потом со старым WPA.
    candidates.sort_by_key(|net| net.rsn.is_empty());
    let mut last = "network not in the last scan";
    for net in candidates.iter() {
        log_network(net);
        let result = connect_bss(net, password);
        // После отказа канал вернуть прошивке: невозвращённый держит её
        // занятой, и следующая попытка получала «did not grant the channel»
        // (04.11). После удачи — нет (05.11): параметров канала точки
        // (`BSS_RLM_PARAM`) мы прошивке не даём, и, вернув право, она уводила
        // радио с канала — кадры данных переставали приходить.
        if result.is_err() {
            let _ = wifi_command(0x1c, true, &privilege(net.channel, true));
        }
        match result {
            Ok(report) => {
                attach();
                return Ok(report);
            }
            Err(err) => {
                crate::kprintln!("  wlan        : {err}");
                last = err;
            }
        }
    }
    Err(last)
}

/// После ассоциации: радио всегда на связи и принимает всё, что нам
/// (05.11). Без этого прошивка после подключения засыпала до маяков —
/// точка придерживала кадры для спящей станции, а опроса мы не шлём, — и
/// данные то шли, то нет (DHCP без ответа через раз).
///
/// * `CMD_ID_POWER_SAVE_MODE` (0x05, `CMD_PS_PROFILE`): BSS 0, профиль 0 —
///   `CONTINUOUS_ACTIVE` (CAM);
/// * `CMD_ID_SET_RX_FILTER` (0x0A, `CMD_RX_PACKET_FILTER`): свои кадры,
///   групповые, все групповые и широковещательные (биты 0–3).
fn awake_and_listen() {
    let profile = [0u8, 0, 0, 0];
    if let Err(err) = wifi_command(0x05, true, &profile) {
        crate::kprintln!("  wlan        : power save off refused: {err}");
    }
    let mut filter = [0u8; 68];
    filter[..4].copy_from_slice(&0x0fu32.to_le_bytes());
    if let Err(err) = wifi_command(0x0a, true, &filter) {
        crate::kprintln!("  wlan        : rx filter refused: {err}");
    }
    crate::kprintln!("  wlan        : power save off (CAM), rx filter directed+multicast+broadcast");
}

/// `CH_PRIVILEGE`: взять канал на 4 с (`release` — вернуть).
fn privilege(channel: u8, release: bool) -> [u8; 24] {
    let mut privilege = [0u8; 24];
    privilege[1] = 0x5a;
    privilege[2] = u8::from(release);
    privilege[3] = channel;
    privilege[5] = 1;
    privilege[10] = 3;
    privilege[12..16].copy_from_slice(&4000u32.to_le_bytes());
    privilege
}

fn connect_bss(net: &Network, password: &str) -> Result<Vec<String>, &'static str> {
    let ssid = net.ssid.as_str();
    let (bssid, channel) = (net.bssid, net.channel);
    let secure = !password.is_empty();
    // WPA2, если точка его объявляет; иначе — старый WPA (вторая MARK69).
    let wpa1 = secure && net.rsn.is_empty() && !net.wpa.is_empty();
    let (rsn_ie, group) = if !secure {
        (Vec::new(), GroupCipher { algorithm: 0, key_len: 0 })
    } else if wpa1 {
        station_wpa(&net.wpa)?
    } else {
        station_rsn(&net.rsn)?
    };
    let mut hex = String::new();
    for byte in rsn_ie.iter() {
        hex.push_str(&alloc::format!("{byte:02x}"));
    }
    crate::kprintln!("  wlan        : our rsn {hex}");
    let mut report = Vec::new();
    activate()?;

    remove_keys(&bssid);

    // Право на канал точки доступа.
    wifi_command(0x1c, true, &privilege(channel, false))?;
    let grant = wait_fw_event(0x10, 2000).ok_or("firmware did not grant the channel")?;
    note(&mut report, alloc::format!("channel {channel} granted ({:02x?})", grant.get(28..32)));

    sta_rec(&bssid, 0, 0, false)?;

    // Аутентификация, открытая система.
    // До четырёх попыток, как у wpa_supplicant: слабую точку первая часто
    // не достаёт (04.11, MARK69 — «нет ответа» на одну попытку).
    let mut auth = mgmt_header(0xb0, &bssid);
    auth.extend_from_slice(&[0, 0, 1, 0, 0, 0]);
    let mut answer = None;
    for attempt in 0..4 {
        send_frame(mgmt_txd(auth.len(), 0x0b), &auth)?;
        answer = wait_mgmt(0xb0, &bssid, 500);
        if answer.is_some() {
            break;
        }
        crate::kprintln!("  wlan        : no auth answer, attempt {}", attempt + 1);
    }
    let answer = answer.ok_or("no authentication answer")?;
    let status = answer.get(28..30).map_or(0xffff, |b| u16::from_le_bytes([b[0], b[1]]));
    note(&mut report, alloc::format!("auth status {status}"));
    if status != 0 {
        return Err("access point refused authentication");
    }

    // Ассоциация: SSID, скорости, RSN (для сети с паролем).
    let mut assoc = mgmt_header(0x00, &bssid);
    let cap: u16 = 0x0001 | 0x0400 | if secure { 0x0010 } else { 0 };
    assoc.extend_from_slice(&cap.to_le_bytes());
    assoc.extend_from_slice(&10u16.to_le_bytes());
    assoc.push(0);
    assoc.push(ssid.len() as u8);
    assoc.extend_from_slice(ssid.as_bytes());
    assoc.push(1);
    assoc.push(RATES.len() as u8);
    assoc.extend_from_slice(&RATES);
    assoc.push(50);
    assoc.push(EXT_RATES.len() as u8);
    assoc.extend_from_slice(&EXT_RATES);
    assoc.extend_from_slice(&rsn_ie);
    // HT (802.11n) и WMM: точки «только n» без них отказывают в ассоциации
    // (03.11, MARK69 — отказ сразу после успешной аутентификации).
    assoc.extend_from_slice(&HT_CAP_IE);
    assoc.extend_from_slice(&WMM_IE);
    let mut answer = None;
    for _ in 0..3 {
        send_frame(mgmt_txd(assoc.len(), 0x00), &assoc)?;
        answer = wait_mgmt(0x10, &bssid, 600);
        if answer.is_some() {
            break;
        }
    }
    let answer = answer.ok_or("no association answer")?;
    let status = answer.get(26..28).map_or(0xffff, |b| u16::from_le_bytes([b[0], b[1]]));
    let aid = answer.get(28..30).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) & 0x3fff);
    crate::kprintln!("  wlan        : association status {status}, aid {aid}");
    note(&mut report, alloc::format!("assoc status {status}, aid {aid}"));
    if status != 0 {
        *LAST_STATUS.lock() = Some(status);
        return Err("access point refused association");
    }

    bss_info(ssid, &bssid, channel, secure)?;
    sta_rec(&bssid, 2, aid, true)?;
    let _ = wait_fw_event(0x0c, 500);
    awake_and_listen();

    if secure {
        report.extend(handshake(ssid, password, &bssid, &rsn_ie, group, wpa1)?);
    }
    *CONNECTED.lock() = Some((String::from(ssid), bssid));
    let security = match (secure, wpa1) {
        (false, _) => "открытая",
        (true, true) => "WPA (AES)",
        (true, false) if group.algorithm == 2 => "WPA2 (AES), группа TKIP",
        (true, false) => "WPA2-Personal (AES)",
    };
    *LINK.lock() = Some((channel, security, bssid));
    crate::kprintln!("  wlan        : connected to '{ssid}'");
    note(&mut report, alloc::format!("connected to {ssid}"));
    Ok(report)
}

/// Четырёхсторонний обмен WPA2 (или старого WPA — `wpa1`) и установка ключей.
fn handshake(ssid: &str, password: &str, bssid: &[u8; 6], rsn_ie: &[u8], group: GroupCipher, wpa1: bool) -> Result<Vec<String>, &'static str> {
    let mut report = Vec::new();
    let pmk = wpa::pmk(password.as_bytes(), ssid.as_bytes());
    let first = wait_eapol(3000).ok_or("no EAPOL message 1 from the access point")?;
    let msg1 = wpa::parse(&first).ok_or("EAPOL message 1 does not parse")?;
    let mut snonce = [0u8; 32];
    crate::random::fill(&mut snonce);
    // Ответы повторяют версию 802.1X, тип дескриптора и (у старого WPA)
    // длину ключа из сообщения точки — как wpa_supplicant.
    let (version, descriptor) = (msg1.version, msg1.descriptor);
    let key_length = if wpa1 { msg1.key_length } else { 0 };
    let mut eapol = [0u8; 256];
    let info2 = wpa::info::VERSION_HMAC_SHA1_AES | wpa::info::PAIRWISE | wpa::info::MIC;
    // Второе сообщение — на каждое первое: точка повторяет первое, если
    // наш ответ не дошёл (04.11: TP-Link повторила его четыре раза, а мы
    // ждали третье). Ключевые данные — ровно тот RSN (WPA IE), что в ассоциации.
    let mut answer = |msg: &wpa::KeyFrame<'_>| -> Result<wpa::Ptk, &'static str> {
        let ptk = wpa::ptk(&pmk, bssid, &OWN_MAC, &msg.nonce, &snonce);
        let len = wpa::build_as(&mut eapol, version, descriptor, key_length, info2, &msg.replay, &snonce, rsn_ie, &ptk.kck);
        send_eapol(bssid, &eapol[..len])?;
        Ok(ptk)
    };
    let mut ptk = answer(&msg1)?;
    note(&mut report, String::from("eapol 2 sent"));

    let started = crate::time::uptime_ms();
    let third = loop {
        let left = 6000u64.checked_sub(crate::time::uptime_ms().saturating_sub(started)).ok_or("no EAPOL message 3 — wrong password?")?;
        let Some(frame) = wait_eapol(left.max(1)) else { return Err("no EAPOL message 3 — wrong password?") };
        let Some(key) = wpa::parse(&frame) else { continue };
        let pairwise = key.info & wpa::info::PAIRWISE != 0;
        if pairwise && key.info & wpa::info::MIC == 0 {
            crate::kprintln!("  wlan        : message 1 again, answering again");
            ptk = answer(&key)?;
            continue;
        }
        if pairwise {
            break frame;
        }
    };
    let mut eapol = [0u8; 256];
    if !wpa::verify(&ptk.kck, &third) {
        return Err("EAPOL message 3 MIC mismatch — wrong password");
    }
    let msg3 = wpa::parse(&third).ok_or("EAPOL message 3 does not parse")?;
    if wpa1 {
        return handshake_wpa1(bssid, &ptk, &msg3.replay, group, version, key_length, report);
    }
    let mut keys = [0u8; 256];
    let plain = wpa::unwrap_key(&ptk.kek, msg3.data, &mut keys).ok_or("group key does not unwrap")?;
    let (gtk_id, gtk) = wpa::find_gtk(&keys[..plain]).ok_or("no group key in message 3")?;
    let gtk = gtk.to_vec();

    let info4 = wpa::info::VERSION_HMAC_SHA1_AES | wpa::info::PAIRWISE | wpa::info::MIC | wpa::info::SECURE;
    let len = wpa::build_as(&mut eapol, version, descriptor, 0, info4, &msg3.replay, &[0; 32], &[], &ptk.kck);
    send_eapol(bssid, &eapol[..len])?;
    note(&mut report, String::from("eapol 4 sent"));

    add_key(bssid, true, 0, 4, &ptk.tk)?;
    add_key(bssid, false, gtk_id, group.algorithm, &gtk[..gtk.len().min(group.key_len)])?;
    note(&mut report, alloc::format!("keys installed (group key {gtk_id})"));
    crate::kprintln!("  wlan        : WPA2 handshake done, group key {gtk_id}");
    Ok(report)
}

/// Старый WPA: третье сообщение без группового ключа, четвёртое без Secure,
/// затем групповой обмен — ключ под AES Key Wrap, номер в битах 4–5.
#[allow(clippy::too_many_arguments)]
fn handshake_wpa1(bssid: &[u8; 6], ptk: &wpa::Ptk, replay3: &[u8; 8], group: GroupCipher, version: u8, key_length: u16, mut report: Vec<String>) -> Result<Vec<String>, &'static str> {
    use wpa::info;
    let mut eapol = [0u8; 256];
    let info4 = info::VERSION_HMAC_SHA1_AES | info::PAIRWISE | info::MIC;
    let len = wpa::build_as(&mut eapol, version, wpa::DESC_WPA, key_length, info4, replay3, &[0; 32], &[], &ptk.kck);
    send_eapol(bssid, &eapol[..len])?;
    note(&mut report, String::from("eapol 4 sent (WPA)"));
    // Личный ключ — сразу: групповое сообщение уже идёт под ним.
    add_key(bssid, true, 0, 4, &ptk.tk)?;

    let first = wait_key(3000, false).ok_or("no WPA group key message")?;
    if !wpa::verify(&ptk.kck, &first) {
        return Err("WPA group key message MIC mismatch");
    }
    let msg = wpa::parse(&first).ok_or("WPA group key message does not parse")?;
    let gtk_id = ((msg.info >> info::KEY_INDEX_SHIFT) & 3) as u8;
    let mut keys = [0u8; 64];
    let plain = wpa::unwrap_key(&ptk.kek, msg.data, &mut keys).ok_or("group key does not unwrap")?;
    let gtk = &keys[..plain.min(group.key_len)];

    let info2 = info::VERSION_HMAC_SHA1_AES | info::MIC | info::SECURE | (u16::from(gtk_id) << info::KEY_INDEX_SHIFT);
    let len = wpa::build_as(&mut eapol, version, wpa::DESC_WPA, msg.key_length, info2, &msg.replay, &[0; 32], &[], &ptk.kck);
    send_eapol(bssid, &eapol[..len])?;
    add_key(bssid, false, gtk_id, group.algorithm, gtk)?;
    note(&mut report, alloc::format!("keys installed (WPA, group key {gtk_id})"));
    crate::kprintln!("  wlan        : WPA handshake done, group key {gtk_id}");
    Ok(report)
}

fn send_eapol(bssid: &[u8; 6], eapol: &[u8]) -> Result<(), &'static str> {
    let mut frame = Vec::with_capacity(14 + eapol.len());
    frame.extend_from_slice(bssid);
    frame.extend_from_slice(&OWN_MAC);
    frame.extend_from_slice(&[0x88, 0x8e]);
    frame.extend_from_slice(eapol);
    send_frame(eapol_txd(frame.len()), &frame)
}

/// Прошивка WLAN запущена этим ядром и готова.
static READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Готова ли прошивка WLAN.
#[must_use]
pub fn is_ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Блок связи выключен — прошивки WLAN больше нет (кольца остаются нашими).
pub fn forget() {
    *KEYS.lock() = None;
    READY.store(false, Ordering::Relaxed);
    ACTIVATED.store(false, Ordering::Relaxed);
    *CONNECTED.lock() = None;
    NETWORKS.lock().clear();
}

/// Весь путь после запущенного блока связи: владение, кольца, прошивка.
pub fn bring_up() -> Result<Vec<String>, &'static str> {
    if is_ready() {
        return Ok(alloc::vec![String::from("Wi-Fi firmware already running")]);
    }
    let mut report = own()?;
    report.extend(setup_dma()?);
    report.extend(firmware()?);
    READY.store(true, Ordering::Relaxed);
    Ok(report)
}

// ---------------------------------------------------------------------------
// Карта для сетевого стека (04.11)
// ---------------------------------------------------------------------------
//
// После подключения Wi-Fi становится обычной картой `net::Card`: кадры
// Ethernet уходят тем же путём, что EAPOL (TXD «Ethernet II», кольцо команд
// — прошивка сама строит 802.11 и шифрует ключом из `ADD_KEY`), а приходят
// в кольцо данных — то ли уже кадром Ethernet (перевод заголовка в блоке),
// то ли кадром 802.11 с LLC; [`ethernet_in`] понимает оба. Первые пакеты —
// в журнал: раскладку RXD прошивки на аппарате видно только так.

/// Радио занято поиском или подключением: карта кольца не трогает, иначе
/// забирала бы у них ответы точки.
static RADIO_BUSY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// Сколько пакетов данных уже показано в журнале.
static DUMPED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Занять радио на время поиска или подключения.
struct Busy;

impl Busy {
    fn take() -> Self {
        RADIO_BUSY.store(true, Ordering::Relaxed);
        Busy
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        RADIO_BUSY.store(false, Ordering::Relaxed);
    }
}

fn is_ours(da: &[u8]) -> bool {
    da == OWN_MAC || da[0] & 1 != 0
}

/// Кадр Ethernet из принятого пакета: уже Ethernet (после RXD) или 802.11
/// с LLC/SNAP — тогда заголовок собирается заново.
fn ethernet_in(packet: &[u8], bssid: &[u8; 6]) -> Option<Vec<u8>> {
    let kind = u16::from_le_bytes([*packet.get(2)?, *packet.get(3)?]);
    let groups = (kind >> 9) & 0xf;
    let mut off = 16usize;
    for (bit, len) in [(3u16, 16usize), (0, 16), (1, 8), (2, 24)] {
        if groups & (1 << bit) != 0 {
            off += len;
        }
    }
    let byte_count = usize::from(u16::from_le_bytes([packet[0], packet[1]])).min(packet.len());
    let packet = &packet[..byte_count.max(off)];
    for at in [off, off + 2] {
        let Some(f) = packet.get(at..) else { continue };
        if f.len() < 24 {
            continue;
        }
        // 802.11, тип «данные», от нашей точки (FromDS, второй адрес —
        // она): шифрованная рассылка соседней сети на том же канале — мимо.
        if f[0] & 0x0c == 0x08 && f[1] & 0x03 == 0x02 && is_ours(&f[4..10]) {
            if f[10..16] != *bssid {
                return None;
            }
            // LLC — сразу за заголовком или за 8 байтами CCMP, если блок,
            // расшифровав, их оставил.
            let base = 24 + if f[0] & 0x80 != 0 { 2 } else { 0 };
            const SNAP: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00];
            let hdr = [base, base + 8].into_iter().find(|at| f.get(*at..*at + 6) == Some(&SNAP[..]))?;
            let mut out = Vec::with_capacity(f.len() - hdr + 6);
            out.extend_from_slice(&f[4..10]); // DA
            out.extend_from_slice(&f[16..22]); // SA
            out.extend_from_slice(&f[hdr + 6..]);
            return Some(out);
        }
        // Уже Ethernet: свой адрес и известный тип.
        let ethertype = u16::from_be_bytes([f[12], f[13]]);
        if is_ours(&f[0..6]) && matches!(ethertype, 0x0800 | 0x0806 | 0x86dd | 0x888e) {
            return Some(f.to_vec());
        }
    }
    None
}

fn dump(packet: &[u8], why: &str) {
    if DUMPED.fetch_add(1, Ordering::Relaxed) >= 10 {
        return;
    }
    crate::kprintln!("  wlan        : rx {why}, {} B", packet.len());
    for row in packet[..packet.len().min(80)].chunks(16) {
        let mut hex = String::new();
        for byte in row {
            hex.push_str(&alloc::format!("{byte:02x}"));
        }
        crate::kprintln!("  wlan        :   {hex}");
    }
}

/// Wi-Fi как сетевая карта.
pub struct Card {
    stats: crate::net::card::Stats,
    /// Принятые пакеты по кольцам и от нашей точки — для сводки в журнал.
    seen_data: u64,
    seen_event: u64,
    seen_ours: u64,
    /// Когда печаталась сводка (мс).
    summary_at: u64,
}

impl Card {
    /// Раз в 10 с — сводка: на аппарате по ней видно, где рвётся путь.
    fn summary(&mut self) {
        let now = crate::time::uptime_ms();
        if now.saturating_sub(self.summary_at) < 10_000 {
            return;
        }
        self.summary_at = now;
        crate::kprintln!(
            "  wlan        : tx {} ({} dropped), rx data {} event {} ours {}, to stack {}",
            self.stats.tx_frames,
            self.stats.tx_dropped,
            self.seen_data,
            self.seen_event,
            self.seen_ours,
            self.stats.rx_frames
        );
        // Указатели колец приёма (CPU — наш, DMA — блока): застрявшее
        // кольцо видно по ним, а не по счётчикам.
        let idx = |ring: u32, field: u64| read(rx_reg(ring, field)).unwrap_or(u32::MAX);
        crate::kprintln!(
            "  wlan        : rings data cpu {} dma {}, event cpu {} dma {}",
            idx(RX_DATA, 8),
            idx(RX_DATA, 12),
            idx(RX_EVENT, 8),
            idx(RX_EVENT, 12)
        );
    }
}

impl crate::net::card::Card for Card {
    fn name(&self) -> &'static str {
        "wifi"
    }

    fn mac(&self) -> [u8; 6] {
        OWN_MAC
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), crate::net::card::CardError> {
        use crate::net::card::CardError;
        if CONNECTED.lock().is_none() || !is_ready() {
            self.stats.tx_dropped += 1;
            return Err(CardError::Stopped);
        }
        if frame.len() > crate::net::card::FRAME_MAX {
            return Err(CardError::TooLong(frame.len()));
        }
        let mut packet = Vec::with_capacity(32 + frame.len() + 3);
        packet.extend_from_slice(&eapol_txd(frame.len()));
        // Шифрует кадр бит PF (DW3, бит 1) дескриптора, а не запись WTBL
        // (gen4m `nic_txd_v1_compose`): без него после установки ключей
        // кадры уходили открытым текстом, и точка их отбрасывала (05.11 —
        // DHCP без ответа). Широковещательное у станции тоже идёт точке
        // одиночным кадром — тем же личным ключом.
        if KEYS.lock().is_some() {
            packet[12] |= 0x02;
        }
        packet.extend_from_slice(frame);
        while packet.len() % 4 != 0 {
            packet.push(0);
        }
        match transmit_nowait(TX_CMD, &packet) {
            Ok(()) => {
                self.stats.tx_frames += 1;
                self.stats.tx_bytes += frame.len() as u64;
                Ok(())
            }
            Err(err) => {
                self.stats.tx_dropped += 1;
                if self.stats.tx_dropped <= 5 || self.stats.tx_dropped % 100 == 0 {
                    crate::kprintln!("  wlan        : frame not sent ({err}), {} dropped so far", self.stats.tx_dropped);
                }
                Err(CardError::Busy)
            }
        }
    }

    fn receive(&mut self, frame: &mut [u8; crate::net::card::FRAME_MAX]) -> Option<usize> {
        if RADIO_BUSY.load(Ordering::Relaxed) || !is_ready() {
            return None;
        }
        self.summary();
        for ring in [RX_DATA, RX_EVENT] {
            // Кольцо событий — только пока есть что брать: события после
            // подключения стеку не нужны, но и копиться им незачем.
            let Some((_, bssid)) = CONNECTED.lock().clone() else { return None };
            while let Ok(Some(packet)) = receive(ring, 0) {
                if ring == RX_DATA {
                    self.seen_data += 1;
                } else {
                    self.seen_event += 1;
                }
                let from_ours = packet.windows(6).any(|w| w == bssid);
                if from_ours {
                    self.seen_ours += 1;
                }
                match ethernet_in(&packet, &bssid) {
                    Some(ethernet) if ethernet.len() <= frame.len() && ethernet.len() >= 14 => {
                        if DUMPED.load(Ordering::Relaxed) < 2 {
                            dump(&packet, "data");
                        }
                        frame[..ethernet.len()].copy_from_slice(&ethernet);
                        self.stats.rx_frames += 1;
                        self.stats.rx_bytes += ethernet.len() as u64;
                        return Some(ethernet.len());
                    }
                    Some(_) => self.stats.rx_dropped += 1,
                    None => {
                        // В журнал — только кадры своей точки (из любого кольца):
                        // чужие шифрованные рассылки разбирать и не надо.
                        // Маяки своей точки (каждые 100 мс) — мимо журнала.
                        let beacon = frame_in(&packet, 0x80).is_some();
                        if from_ours && !beacon {
                            self.stats.rx_dropped += 1;
                            dump(&packet, "not understood");
                        }
                    }
                }
            }
        }
        None
    }

    fn stats(&self) -> crate::net::card::Stats {
        self.stats
    }
}

/// Канал, защита и точка нынешнего подключения.
static LINK: SpinLock<Option<(u8, &'static str, [u8; 6])>> = SpinLock::new(None);

/// Сведения о подключении.
#[must_use]
pub fn link() -> Option<(u8, &'static str, [u8; 6])> {
    if CONNECTED.lock().is_none() {
        return None;
    }
    *LINK.lock()
}

/// Разорвать подключение: точке — «деаутентификация», карта перестаёт слать.
pub fn disconnect() {
    let Some((_, bssid)) = CONNECTED.lock().take() else { return };
    let mut deauth = mgmt_header(0xc0, &bssid);
    deauth.extend_from_slice(&3u16.to_le_bytes()); // «станция уходит»
    let _ = send_frame(mgmt_txd(deauth.len(), 0x0c), &deauth);
    remove_keys(&bssid);
    *LINK.lock() = None;
    crate::kprintln!("  wlan        : disconnected");
}

/// Отдать Wi-Fi сетевому стеку (после подключения).
pub fn attach() {
    static ATTACHED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    if ATTACHED.swap(true, Ordering::Relaxed) {
        return;
    }
    crate::net::attach_wifi(alloc::boxed::Box::new(Card {
        stats: crate::net::card::Stats::default(),
        seen_data: 0,
        seen_event: 0,
        seen_ours: 0,
        summary_at: 0,
    }));
}
