//! eMMC телефона (MSDC0) — только чтение (05.11).
//!
//! # Зачем
//!
//! Система на телефоне живёт в RAM-диске до 25 МиБ: настройки и запомненные
//! сети пропадают при перезагрузке, а модему нужны образы `md1img` и NVRAM
//! (`nvdata`), которые лежат на флеш-памяти. Первый шаг — читать eMMC.
//!
//! # «Усыновление» контроллера
//!
//! LK грузит нас с раздела `recovery` той же eMMC, и контроллер остаётся
//! настроенным: такт, DDR, шина 8 бит, режим PIO, карта выбрана (RCA 1) и
//! стоит в состоянии `tran` (проверено `oem peek 11230000`: `MSDC_CFG`
//! `0x02200199`, `SDC_CFG` `0x09020000`, последняя команда CMD13, ответ
//! `0x900`). Поэтому драйвер не поднимает карту заново, а только шлёт
//! команды чтения: CMD17 (один блок), CMD8 (EXT_CSD), CMD13 (состояние).
//! Данные — из FIFO словами (PIO), без DMA и прерываний; порядок шагов как у
//! `mtk-sd.c` U-Boot.
//!
//! # Чего здесь нет — намеренно
//!
//! Записи. Ни одной команды записи (CMD24/25, CMD6) драйвер не умеет:
//! на этой памяти загрузчик, NVRAM с IMEI и калибровкой радио, `preloader`
//! в загрузочных разделах — ошибка стоит аппарата. Запись — отдельным
//! решением.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use super::mtk_regs::{read32, write32};
use crate::sync::SpinLock;

const BASE: u64 = 0x1123_0000;

const MSDC_CFG: u64 = BASE + 0x00;
const MSDC_INT: u64 = BASE + 0x0c;
const MSDC_INTEN: u64 = BASE + 0x10;
const MSDC_FIFOCS: u64 = BASE + 0x14;
const MSDC_RXDATA: u64 = BASE + 0x1c;
const SDC_CFG: u64 = BASE + 0x30;
const SDC_CMD: u64 = BASE + 0x34;
const SDC_ARG: u64 = BASE + 0x38;
const SDC_STS: u64 = BASE + 0x3c;
const SDC_RESP0: u64 = BASE + 0x40;
const SDC_BLK_NUM: u64 = BASE + 0x50;

const CFG_RST: u32 = 1 << 2;
const CFG_PIO: u32 = 1 << 3;

const STS_SDCBUSY: u32 = 1 << 0;
const STS_CMDBUSY: u32 = 1 << 1;

const INT_CMDRDY: u32 = 1 << 8;
const INT_CMDTMO: u32 = 1 << 9;
const INT_RSPCRCERR: u32 = 1 << 10;
const INT_XFER_COMPL: u32 = 1 << 12;
const INT_DATTMO: u32 = 1 << 14;
const INT_DATCRCERR: u32 = 1 << 15;
const CMD_INTS: u32 = INT_CMDRDY | INT_CMDTMO | INT_RSPCRCERR;
const DATA_INTS: u32 = INT_XFER_COMPL | INT_DATTMO | INT_DATCRCERR;

const FIFOCS_RXCNT: u32 = 0xff;
const FIFOCS_CLR: u32 = 1 << 31;

/// Тип ответа R1 в поле `SDC_CMD[9:7]`.
const RSP_R1: u32 = 1;
/// Один блок данных в поле `SDC_CMD[12:11]`.
const DTYPE_SINGLE: u32 = 1;

/// Размер блока eMMC.
pub const BLOCK: usize = 512;

/// RCA карты: его назначил загрузчик (виден в `SDC_ARG` после его CMD13).
const RCA: u32 = 1;

/// Одна команда за раз: контроллер один, а спрашивать могут из разных задач.
static LOCK: SpinLock<()> = SpinLock::new(());

#[derive(Debug, Clone, Copy)]
pub enum Error {
    /// Регистры не отобразились.
    NoRegisters,
    /// Контроллер занят дольше предела.
    Busy,
    CmdTimeout,
    CmdCrc,
    DataTimeout,
    DataCrc,
    /// Ни ответа, ни ошибки за отведённое время.
    Stuck,
    /// Карта не в состоянии `tran` или выбран не раздел user.
    WrongState(u32),
    /// Адрес за концом устройства или раздела.
    OutOfRange,
    NoMemory,
    NoGpt,
    NoSuchPartition,
}

fn reg(address: u64) -> Result<u32, Error> {
    read32(address).ok_or(Error::NoRegisters)
}

fn set(address: u64, value: u32) -> Result<(), Error> {
    if write32(address, value) { Ok(()) } else { Err(Error::NoRegisters) }
}

fn deadline_ms(ms: u64) -> u64 {
    crate::time::uptime_ns() + ms * 1_000_000
}

fn expired(deadline: u64) -> bool {
    crate::time::uptime_ns() >= deadline
}

/// Сбросить контроллер и FIFO после ошибки — как `msdc_reset_hw` U-Boot.
/// Карта при этом остаётся в своём состоянии (это сброс хоста, не карты).
fn reset_host() {
    if let Some(cfg) = read32(MSDC_CFG) {
        write32(MSDC_CFG, cfg | CFG_RST);
        let until = deadline_ms(10);
        while read32(MSDC_CFG).is_some_and(|v| v & CFG_RST != 0) && !expired(until) {}
    }
    if let Some(fifo) = read32(MSDC_FIFOCS) {
        write32(MSDC_FIFOCS, fifo | FIFOCS_CLR);
        let until = deadline_ms(10);
        while read32(MSDC_FIFOCS).is_some_and(|v| v & FIFOCS_CLR != 0) && !expired(until) {}
    }
    if let Some(int) = read32(MSDC_INT) {
        write32(MSDC_INT, int);
    }
}

/// Отправить команду и дождаться ответа R1. `data` — будет ли блок данных.
fn command(opcode: u32, arg: u32, data: bool) -> Result<u32, Error> {
    let until = deadline_ms(100);
    let busy = if data { STS_SDCBUSY | STS_CMDBUSY } else { STS_CMDBUSY };
    while reg(SDC_STS)? & busy != 0 {
        if expired(until) {
            return Err(Error::Busy);
        }
    }
    // Ни одна команда отсюда не пишет: проверка на случай опечатки в коде.
    if !matches!(opcode, 8 | 13 | 17) {
        return Err(Error::WrongState(opcode));
    }
    let mut raw = (opcode & 0x3f) | (RSP_R1 << 7);
    if data {
        raw |= (DTYPE_SINGLE << 11) | ((BLOCK as u32) << 16);
        set(SDC_BLK_NUM, 1)?;
    }
    let pending = reg(MSDC_INT)?;
    set(MSDC_INT, pending)?;
    set(SDC_ARG, arg)?;
    set(SDC_CMD, raw)?;

    let until = deadline_ms(100);
    loop {
        let int = reg(MSDC_INT)?;
        if int & CMD_INTS != 0 {
            set(MSDC_INT, int & CMD_INTS)?;
            if int & INT_CMDTMO != 0 {
                return Err(Error::CmdTimeout);
            }
            if int & INT_RSPCRCERR != 0 {
                return Err(Error::CmdCrc);
            }
            return reg(SDC_RESP0);
        }
        if expired(until) {
            return Err(Error::Stuck);
        }
    }
}

/// Принять блок из FIFO после команды с данными.
fn receive(out: &mut [u8; BLOCK]) -> Result<(), Error> {
    let mut at = 0;
    // «Передача кончилась» может прийти раньше, чем последние байты взяты из
    // FIFO: бит сбрасывается сразу, поэтому помнить его надо самому.
    let mut done = false;
    let until = deadline_ms(200);
    loop {
        let int = reg(MSDC_INT)?;
        if int & DATA_INTS != 0 {
            set(MSDC_INT, int & DATA_INTS)?;
        }
        done |= int & INT_XFER_COMPL != 0;
        if int & INT_DATCRCERR != 0 {
            return Err(Error::DataCrc);
        }
        if int & INT_DATTMO != 0 {
            return Err(Error::DataTimeout);
        }
        let mut ready = (reg(MSDC_FIFOCS)? & FIFOCS_RXCNT) as usize;
        while ready >= 4 && at < BLOCK {
            out[at..at + 4].copy_from_slice(&reg(MSDC_RXDATA)?.to_le_bytes());
            at += 4;
            ready -= 4;
        }
        if at >= BLOCK && done {
            return Ok(());
        }
        if expired(until) {
            return Err(Error::Stuck);
        }
    }
}

fn read_with(opcode: u32, arg: u32, out: &mut [u8; BLOCK]) -> Result<(), Error> {
    let _guard = LOCK.lock();
    let cfg = reg(MSDC_CFG)?;
    if cfg & CFG_PIO == 0 {
        set(MSDC_CFG, cfg | CFG_PIO)?;
    }
    set(MSDC_INTEN, 0)?;
    let result = command(opcode, arg, true).and_then(|_| receive(out));
    if result.is_err() {
        reset_host();
    }
    result
}

/// Прочитать блок `lba` раздела user.
pub fn read_block(lba: u32, out: &mut [u8; BLOCK]) -> Result<(), Error> {
    // Один повтор: после ошибки хост сброшен, и вторая попытка обычно проходит.
    read_with(17, lba, out).or_else(|_| read_with(17, lba, out))
}

/// Состояние карты (ответ CMD13).
pub fn status() -> Result<u32, Error> {
    let _guard = LOCK.lock();
    let result = command(13, RCA << 16, false);
    if result.is_err() {
        reset_host();
    }
    result
}

/// Сведения о памяти из EXT_CSD.
pub struct Info {
    pub msdc_cfg: u32,
    pub sdc_cfg: u32,
    pub card_status: u32,
    pub sectors: u32,
    pub rev: u8,
    pub partition_config: u8,
    pub boot_size_kib: u32,
    pub hs_timing: u8,
    pub bus_width: u8,
    pub card_type: u8,
}

/// EXT_CSD целиком (CMD8).
pub fn ext_csd() -> Result<[u8; BLOCK], Error> {
    let mut block = [0u8; BLOCK];
    read_with(8, 0, &mut block).or_else(|_| read_with(8, 0, &mut block))?;
    Ok(block)
}

/// Проверить, что читать можно: карта в `tran`, выбран раздел user.
fn check_ready() -> Result<[u8; BLOCK], Error> {
    let card = status()?;
    // Биты 12:9 — состояние; 4 = tran.
    if (card >> 9) & 0xf != 4 {
        return Err(Error::WrongState(card));
    }
    let ext = ext_csd()?;
    if ext[179] & 7 != 0 {
        return Err(Error::WrongState(u32::from(ext[179])));
    }
    Ok(ext)
}

pub fn info() -> Result<Info, Error> {
    let card_status = status()?;
    let ext = ext_csd()?;
    Ok(Info {
        msdc_cfg: reg(MSDC_CFG)?,
        sdc_cfg: reg(SDC_CFG)?,
        card_status,
        sectors: u32::from_le_bytes([ext[212], ext[213], ext[214], ext[215]]),
        rev: ext[192],
        partition_config: ext[179],
        boot_size_kib: u32::from(ext[226]) * 128,
        hs_timing: ext[185],
        bus_width: ext[183],
        card_type: ext[196],
    })
}

/// Раздел из GPT.
#[derive(Clone)]
pub struct Partition {
    pub name: String,
    pub first: u64,
    pub last: u64,
}

impl Partition {
    #[must_use]
    pub fn blocks(&self) -> u64 {
        self.last + 1 - self.first
    }
}

fn le64(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(word)
}

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Таблица разделов GPT (основная, с LBA 1).
pub fn gpt() -> Result<Vec<Partition>, Error> {
    check_ready()?;
    let mut header = [0u8; BLOCK];
    read_block(1, &mut header)?;
    if &header[..8] != b"EFI PART" {
        return Err(Error::NoGpt);
    }
    let entries_lba = le64(&header[72..]);
    let count = le32(&header[80..]).min(256) as usize;
    let size = le32(&header[84..]) as usize;
    if !(128..=BLOCK).contains(&size) || BLOCK % size != 0 || entries_lba > u64::from(u32::MAX) {
        return Err(Error::NoGpt);
    }
    let per_block = BLOCK / size;
    let mut parts = Vec::new();
    let mut block = [0u8; BLOCK];
    for index in 0..count {
        if index % per_block == 0 {
            read_block((entries_lba + (index / per_block) as u64) as u32, &mut block)?;
        }
        let entry = &block[(index % per_block) * size..][..size];
        if entry[..16].iter().all(|b| *b == 0) {
            continue;
        }
        let name: String = char::decode_utf16(
            entry[56..128].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0),
        )
        .map(|c| c.unwrap_or('?'))
        .collect();
        parts.push(Partition { name, first: le64(&entry[32..]), last: le64(&entry[40..]) });
    }
    Ok(parts)
}

/// Таблица разделов текстом: имя, первый блок, число блоков, размер.
pub fn gpt_text() -> Result<String, Error> {
    let parts = gpt()?;
    let mut text = String::new();
    for part in &parts {
        let _ = writeln!(
            text,
            "{:<20} first {:>10} blocks {:>10} = {} KiB",
            part.name,
            part.first,
            part.blocks(),
            part.blocks() / 2
        );
    }
    Ok(text)
}

/// Найти раздел по имени.
pub fn find(name: &str) -> Result<Partition, Error> {
    gpt()?.into_iter().find(|p| p.name == name).ok_or(Error::NoSuchPartition)
}

/// Дописать `count` блоков с `lba` в `out`.
pub fn read_blocks(lba: u64, count: u64, out: &mut Vec<u8>) -> Result<(), Error> {
    check_ready()?;
    let end = lba.checked_add(count).ok_or(Error::OutOfRange)?;
    if end > u64::from(u32::MAX) {
        return Err(Error::OutOfRange);
    }
    if out.try_reserve_exact(count as usize * BLOCK).is_err() {
        return Err(Error::NoMemory);
    }
    let mut block = [0u8; BLOCK];
    for at in lba..end {
        read_block(at as u32, &mut block)?;
        out.extend_from_slice(&block);
        if at % 256 == 255 {
            crate::sched::yield_now();
        }
    }
    Ok(())
}
