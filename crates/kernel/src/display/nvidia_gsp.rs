//! Видеокарты NVIDIA, этап 5 (15.11.2026): запустить GSP-RM — прошивку
//! NVIDIA, которая на картах Turing и новее и есть «драйвер внутри карты».
//!
//! # Что здесь
//!
//! Команда `gpu gsp`: после FWSEC-FRTS (этап 4, `nvidia.rs`) разобрать
//! прошивку linux-firmware 570.144 (`booter_load`, `bootloader`, `gsp`),
//! разложить её в памяти машины (radix3, журналы LIBOS, аргументы, очереди
//! сообщений, описание для загрузчика — GspFwWprMeta), положить в очередь
//! SET_SYSTEM_INFO и SET_REGISTRY, подготовить процессор GSP, выполнить
//! booter на процессоре SEC2 — он переносит всё в защищённую видеопамять и
//! запускает GSP — и ждать GSP_INIT_DONE, выполняя по дороге команды
//! RUN_CPU_SEQUENCER. Затем GET_GSP_STATIC_INFO: имя карты от неё самой.
//!
//! # Источники
//!
//! Только открытое: nova-core (`gsp/*`, `firmware/*`, `falcon/*`, `fb.rs`) и
//! nouveau (`nvkm/subdev/gsp/rm/r535`, `r570`, `tu102.c`, `ga102.c`) — порядок,
//! раскладки структур r570.144 и регистры; разбор с пометками «не уверен» —
//! в ROADMAP «Видеокарты NVIDIA».
//!
//! # Цена ошибки
//!
//! Прошивка карты (VBIOS) не меняется. Неудача оставляет карту в состоянии,
//! из которого её выводит только выключение питания: WPR2 уже задан.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use super::nvidia::{Bar0, spin_us};
use crate::mm::{FrameAllocator, PAGE_SIZE, PhysAddr};

pub(crate) type Log = Vec<String>;
pub(crate) type Res<T> = Result<T, String>;

const GSP: usize = 0x0011_0000;
const GSP2: usize = 0x0011_1000;
const SEC2: usize = 0x0084_0000;
const SEC2_2: usize = 0x0084_1000;

const FIRMWARE_DIR: [&str; 4] = ["/usb", "/usb1", "/usb2", "/usb3"];

/// Сообщения: по 63 ячейки на очередь, ячейка — страница GSP.
const SLOTS: u32 = 63;
const SLOT: usize = 0x1000;
const ELEMENT_HEADER: usize = 0x30;
const RPC_HEADER: usize = 0x20;

const FN_GET_GSP_STATIC_INFO: u32 = 65;
const FN_SET_SYSTEM_INFO: u32 = 72;
const FN_SET_REGISTRY: u32 = 73;
const EV_INIT_DONE: u32 = 0x1001;
const EV_RUN_CPU_SEQUENCER: u32 = 0x1002;
const STATIC_INFO_SIZE: usize = 0x678;

// --- память ----------------------------------------------------------------

/// Кусок физически непрерывной памяти машины: адрес для карты и указатель
/// для ядра. Страницы обнулены; память не возвращается — GSP пользуется ею
/// всю свою жизнь.
#[derive(Clone, Copy)]
pub(crate) struct Mem {
    pub(crate) phys: u64,
    ptr: *mut u8,
    len: usize,
}

// SAFETY: кусок памяти машины, принадлежащий драйверу; доступ — под замком
// сеанса GSP.
unsafe impl Send for Mem {}

impl Mem {
    pub(crate) fn new(pages: usize) -> Res<Self> {
        let phys = crate::mm::frame::with(|f| f.allocate_contiguous(pages))
            .flatten()
            .ok_or_else(|| format!("no {pages} contiguous pages of memory"))?;
        let ptr = phys.to_direct_map().as_usize() as *mut u8;
        Ok(Self { phys: phys.as_u64(), ptr, len: pages * PAGE_SIZE })
    }

    pub(crate) fn bytes(&self) -> &mut [u8] {
        // SAFETY: страницы выделены этим `Mem` и не возвращаются.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub(crate) fn w32(&self, at: usize, v: u32) {
        // SAFETY: внутри куска; общая с картой память — запись `volatile`.
        unsafe { (self.ptr.add(at) as *mut u32).write_volatile(v) }
    }
    pub(crate) fn w64(&self, at: usize, v: u64) {
        // SAFETY: см. `w32`.
        unsafe { (self.ptr.add(at) as *mut u64).write_volatile(v) }
    }
    pub(crate) fn r32(&self, at: usize) -> u32 {
        // SAFETY: см. `w32`.
        unsafe { (self.ptr.add(at) as *const u32).read_volatile() }
    }
    pub(crate) fn r64(&self, at: usize) -> u64 {
        // SAFETY: см. `w32`.
        unsafe { (self.ptr.add(at) as *const u64).read_volatile() }
    }
}

fn single_page() -> Res<u64> {
    crate::mm::frame::with(|f| f.allocate())
        .flatten()
        .map(PhysAddr::as_u64)
        .ok_or_else(|| "out of memory pages".into())
}

pub(crate) fn le32(b: &[u8], at: usize) -> Res<u32> {
    b.get(at..at + 4)
        .map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
        .ok_or_else(|| format!("firmware too short at {at:#x}"))
}

fn le64(b: &[u8], at: usize) -> Res<u64> {
    Ok(u64::from(le32(b, at)?) | (u64::from(le32(b, at + 4)?) << 32))
}

// --- файлы ------------------------------------------------------------------

fn open(name: &str) -> Res<alloc::boxed::Box<dyn crate::vfs::Node>> {
    for root in FIRMWARE_DIR {
        let path = format!("{root}/FREEOS/NVIDIA/{name}");
        if let Some(Ok(node)) =
            crate::fs::resolve_as(crate::vfs::perm::Credentials::ROOT, &path, crate::vfs::perm::Access::NONE)
        {
            return Ok(node);
        }
    }
    Err(format!("FREEOS/NVIDIA/{name} not found on the stick (/usb*)"))
}

fn read_at(node: &dyn crate::vfs::Node, mut offset: u64, out: &mut [u8]) -> Res<()> {
    let mut done = 0;
    while done < out.len() {
        let n = node.read_at(offset, &mut out[done..]).map_err(|e| format!("read failed: {e:?}"))?;
        if n == 0 {
            return Err("unexpected end of file".into());
        }
        done += n;
        offset += n as u64;
    }
    Ok(())
}

fn read_whole(name: &str) -> Res<Vec<u8>> {
    let node = open(name)?;
    let size = node.metadata().size as usize;
    let mut bytes = alloc::vec![0u8; size];
    read_at(&*node, 0, &mut bytes)?;
    Ok(bytes)
}

/// Наибольший значащий бит (`fls`): 0 для нуля.
fn fls(v: u32) -> u32 {
    32 - v.leading_zeros()
}

// --- процессоры Falcon карты -----------------------------------------------

/// Сброс процессора Falcon (`reset_eng` GA102).
fn falcon_reset(bar: Bar0, base: usize) -> Res<()> {
    let _ = bar.rd(base + 0xf4);
    let _ = bar.wait(150, |b| b.rd(base + 0xf4) & 0x8000_0000 != 0);
    bar.wr(base + 0x3c0, bar.rd(base + 0x3c0) | 1);
    spin_us(10);
    bar.wr(base + 0x3c0, bar.rd(base + 0x3c0) & !1);
    if !bar.wait(20_000, |b| b.rd(base + 0xf4) & 0x1000 == 0) {
        return Err(format!("falcon {base:#x}: memory scrubbing did not finish (HWCFG2 {:#x})", bar.rd(base + 0xf4)));
    }
    Ok(())
}

/// Перенести `len` байт из памяти машины в IMEM/DMEM Falcon кусками по 256.
fn falcon_dma(bar: Bar0, base: usize, src: u64, src_offset: usize, dst: usize, len: usize, cmd: u32) -> Res<()> {
    bar.wr(base + 0x110, (src >> 8) as u32);
    bar.wr(base + 0x128, ((src >> 40) & 0x1ff) as u32);
    let mut pos = 0;
    while pos < len.div_ceil(256) * 256 {
        bar.wr(base + 0x114, (dst + pos) as u32);
        bar.wr(base + 0x11c, (src_offset + pos) as u32);
        bar.wr(base + 0x118, cmd);
        if !bar.wait(2_000_000, |b| b.rd(base + 0x118) & 2 != 0) {
            return Err(format!("falcon {base:#x}: DMA at {pos:#x} stuck (DMATRFCMD {:#x})", bar.rd(base + 0x118)));
        }
        pos += 256;
    }
    Ok(())
}

/// Выбрать ядро Falcon вместо RISC-V (`select` у nouveau, `select_core` у
/// nova): пока работает RISC-V, регистры DMA Falcon закрыты (`0xbadf…`).
fn falcon_select(bar: Bar0, base2: usize) -> Res<()> {
    if bar.rd(base2 + 0x668) & 0x10 != 0 {
        bar.wr(base2 + 0x668, 0);
        if !bar.wait(10_000, |b| b.rd(base2 + 0x668) & 1 != 0) {
            return Err(format!("falcon {base2:#x}: did not switch to the Falcon core"));
        }
    }
    Ok(())
}

fn falcon_start(bar: Bar0, base: usize) {
    if bar.rd(base + 0x100) & 0x40 != 0 {
        bar.wr(base + 0x130, 2);
    } else {
        bar.wr(base + 0x100, 2);
    }
}

// --- очереди сообщений -------------------------------------------------------

/// Общая с GSP память очередей: страница PTE, очередь команд, очередь ответов.
pub(crate) struct Queues {
    block: Mem,
    seq: u32,
}

impl Queues {
    const CMDQ: usize = 0x1000;
    const MSGQ: usize = 0x41000;

    fn new() -> Res<Self> {
        let block = Mem::new(0x81)?;
        for i in 0..0x81 {
            block.w64(i * 8, block.phys + (i * SLOT) as u64);
        }
        // Заголовок очереди команд (пишет процессор машины).
        let h = Self::CMDQ;
        block.w32(h, 0);
        block.w32(h + 0x04, 0x40000);
        block.w32(h + 0x08, SLOT as u32);
        block.w32(h + 0x0c, SLOTS);
        block.w32(h + 0x10, 0);
        block.w32(h + 0x14, 1);
        block.w32(h + 0x18, 0x20);
        block.w32(h + 0x1c, 0x1000);
        Ok(Self { block, seq: 0 })
    }

    fn slot_cmd(&self, i: u32) -> usize {
        Self::CMDQ + SLOT + i as usize * SLOT
    }
    fn slot_msg(&self, i: u32) -> usize {
        Self::MSGQ + SLOT + i as usize * SLOT
    }

    /// Положить команду. Звонок (`0x110c00`) — если GSP уже работает.
    pub(crate) fn send(&mut self, bar: Option<Bar0>, function: u32, payload: &[u8]) -> Res<()> {
        let total = ELEMENT_HEADER + RPC_HEADER + payload.len();
        let count = total.div_ceil(SLOT) as u32;
        let mut message = alloc::vec![0u8; count as usize * SLOT];
        message[0x24..0x28].copy_from_slice(&self.seq.to_le_bytes());
        message[0x28..0x2c].copy_from_slice(&count.to_le_bytes());
        let rpc = [0x0300_0000u32, 0x4350_5256, (RPC_HEADER + payload.len()) as u32, function, u32::MAX, u32::MAX, 0, 0];
        for (i, v) in rpc.iter().enumerate() {
            message[0x30 + i * 4..0x34 + i * 4].copy_from_slice(&v.to_le_bytes());
        }
        message[0x50..0x50 + payload.len()].copy_from_slice(payload);
        let sum = checksum(&message[..total]);
        message[0x20..0x24].copy_from_slice(&sum.to_le_bytes());

        let wptr = self.block.r32(Self::CMDQ + 0x10);
        let deadline = crate::time::uptime_ms() + 1000;
        loop {
            let rptr = self.block.r32(Self::MSGQ + 0x20);
            let free = (rptr + SLOTS - wptr - 1) % SLOTS;
            if free >= count {
                break;
            }
            if crate::time::uptime_ms() > deadline {
                return Err(format!("command queue full (w {wptr}, r {rptr})"));
            }
            core::hint::spin_loop();
        }
        for i in 0..count {
            let slot = self.slot_cmd((wptr + i) % SLOTS);
            self.block.bytes()[slot..slot + SLOT].copy_from_slice(&message[i as usize * SLOT..(i as usize + 1) * SLOT]);
        }
        fence(Ordering::SeqCst);
        self.block.w32(Self::CMDQ + 0x10, (wptr + count) % SLOTS);
        fence(Ordering::SeqCst);
        if let Some(bar) = bar {
            bar.wr(GSP + 0xc00, 0);
        }
        self.seq += 1;
        Ok(())
    }

    /// Указатели очередей — для журнала ожидания.
    fn pointers(&self) -> (u32, u32, u32, u32) {
        (
            self.block.r32(Self::CMDQ + 0x10),
            self.block.r32(Self::MSGQ + 0x20),
            self.block.r32(Self::MSGQ + 0x10),
            self.block.r32(Self::CMDQ + 0x20),
        )
    }

    /// Взять сообщение GSP, если есть: (функция, результат, полезная нагрузка).
    pub(crate) fn receive(&mut self) -> Res<Option<(u32, u32, Vec<u8>)>> {
        let wptr = self.block.r32(Self::MSGQ + 0x10);
        let rptr = self.block.r32(Self::CMDQ + 0x20);
        if wptr >= SLOTS {
            return Ok(None);
        }
        let available = (wptr + SLOTS - rptr) % SLOTS;
        if available == 0 {
            return Ok(None);
        }
        fence(Ordering::SeqCst);
        let first = self.slot_msg(rptr);
        let count = self.block.r32(first + 0x28).max(1);
        let length = self.block.r32(first + 0x38) as usize;
        if count > 16 || count > available || length < RPC_HEADER {
            return Err(format!("bad message: {count} slots, length {length:#x}, {available} available"));
        }
        let mut message = Vec::with_capacity(count as usize * SLOT);
        for i in 0..count {
            let slot = self.slot_msg((rptr + i) % SLOTS);
            message.extend_from_slice(&self.block.bytes()[slot..slot + SLOT]);
        }
        let total = ELEMENT_HEADER + length;
        let bad_sum = checksum(&message[..total.min(message.len())]) != 0;
        let function = le32(&message, 0x3c)?;
        let result = le32(&message, 0x40)?;
        let payload = message[0x50..total.min(message.len())].to_vec();
        fence(Ordering::SeqCst);
        self.block.w32(Self::CMDQ + 0x20, (rptr + count) % SLOTS);
        if bad_sum {
            return Err(format!("message {function:#x}: checksum mismatch"));
        }
        Ok(Some((function, result, payload)))
    }
}

/// Контрольная сумма сообщения: XOR слов по 8 байт, свёрнутый в 32 бита.
fn checksum(bytes: &[u8]) -> u32 {
    let mut x = 0u64;
    for chunk in bytes.chunks(8) {
        let mut w = [0u8; 8];
        w[..chunk.len()].copy_from_slice(chunk);
        x ^= u64::from_le_bytes(w);
    }
    ((x >> 32) as u32) ^ (x as u32)
}

// --- главное -----------------------------------------------------------------

/// Этап 5: запустить GSP-RM. Возвращает строки отчёта (они же — в журнал).
pub fn boot_gsp(argument: &str) -> Log {
    let mut log = Log::new();
    let dry = argument.trim() == "check";
    let heap_mib: u64 = argument.trim().parse().unwrap_or(128).clamp(88, 512);
    match run(&mut log, heap_mib, dry) {
        Ok(()) if dry => log.push("GSP-RM: firmware checked, nothing sent to the card".into()),
        Ok(()) => log.push("GSP-RM: running".into()),
        Err(why) => log.push(format!("GSP-RM: stopped - {why}")),
    }
    for line in &log {
        crate::kprintln!("  nvidia-gsp  : {line}");
    }
    log
}

#[allow(clippy::too_many_lines)]
fn run(log: &mut Log, heap_mib: u64, dry: bool) -> Res<()> {
    // Сухой прогон (`gpu gsp check`): прочитать и разобрать прошивку и
    // разложить образ по страницам — карту не трогать. Проверяется в QEMU.
    let card = super::nvidia::card();
    if card.is_none() && !dry {
        return Err("no NVIDIA card".into());
    }
    let (fb_end, vga_base, frts) = match card {
        Some((_, bar)) if !dry => layout(log, bar)?,
        _ => (0x3_0000_0000, 0x2_fff0_0000, 0x2_ffe0_0000),
    };

    // 2. Прошивка с флешки.
    let booter = read_whole("BOOTER.BIN")?;
    let loader = read_whole("BOOTLDR.BIN")?;
    let gsp = open("GSP.BIN")?;
    log.push(format!("firmware: booter {} B, bootloader {} B, gsp {} B", booter.len(), loader.len(), gsp.metadata().size));

    // 2a. booter_load.
    if le32(&booter, 0)? != 0x10de {
        return Err("BOOTER.BIN: not an NVIDIA firmware header".into());
    }
    let (b_hdr, b_data, b_size) = (le32(&booter, 0x0c)? as usize, le32(&booter, 0x10)? as usize, le32(&booter, 0x14)? as usize);
    let sig_offset = le32(&booter, b_hdr)? as usize;
    let sig_size = le32(&booter, b_hdr + 4)? as usize;
    let patch_loc = le32(&booter, le32(&booter, b_hdr + 8)? as usize)? as usize;
    let patch_sig = le32(&booter, le32(&booter, b_hdr + 12)? as usize)? as usize;
    let meta_at = le32(&booter, b_hdr + 16)? as usize;
    let (fuse_ver, engine_id, ucode_id) = (le32(&booter, meta_at)?, le32(&booter, meta_at + 4)?, le32(&booter, meta_at + 8)?);
    let num_sig = le32(&booter, le32(&booter, b_hdr + 24)? as usize)? as usize;
    let load = le32(&booter, b_hdr + 28)? as usize;
    let (os_data_off, os_data_size) = (le32(&booter, load + 8)? as usize, le32(&booter, load + 12)? as usize);
    let (app_off, app_size) = (le32(&booter, load + 20)? as usize, le32(&booter, load + 24)? as usize);
    if num_sig == 0 || sig_size / num_sig != 384 || engine_id & 1 == 0 || !(1..=16).contains(&ucode_id) {
        return Err(format!("BOOTER.BIN: unexpected header (sigs {num_sig}, size {sig_size}, engine {engine_id}, ucode {ucode_id})"));
    }
    let fuse = card.map_or(0, |(_, bar)| bar.rd(0x0082_4140 + (ucode_id as usize - 1) * 4) & 0xffff);
    let fuse_bits = fls(fuse);
    let sig_index = if fuse_bits == 0 {
        num_sig - 1
    } else {
        fuse_ver.checked_sub(fuse_bits).ok_or(format!("booter has no signature for fuse {fuse:#x}"))? as usize
    };
    if sig_index >= num_sig {
        return Err(format!("booter signature index {sig_index} out of {num_sig}"));
    }
    let booter_buf = Mem::new(b_size.div_ceil(PAGE_SIZE))?;
    booter_buf.bytes()[..b_size].copy_from_slice(booter.get(b_data..b_data + b_size).ok_or("BOOTER.BIN truncated")?);
    let sig_from = sig_offset + patch_sig + sig_index * 384;
    booter_buf.bytes()[patch_loc..patch_loc + 384].copy_from_slice(booter.get(sig_from..sig_from + 384).ok_or("booter signature truncated")?);
    let pkc = patch_loc - os_data_off;
    log.push(format!(
        "booter: IMEM {app_size:#x} from {app_off:#x}, DMEM {os_data_size:#x} from {os_data_off:#x}, fuse {fuse:#x} -> signature {sig_index}, PARAADDR {pkc:#x}"
    ));

    // 2b. Загрузчик GSP (RISC-V).
    let (l_hdr, l_data, l_size) = (le32(&loader, 0x0c)? as usize, le32(&loader, 0x10)? as usize, le32(&loader, 0x14)? as usize);
    let desc = |i: usize| le32(&loader, l_hdr + i * 4);
    let (app_version, manifest, monitor_data, monitor_code) = (desc(7)?, desc(8)?, desc(10)?, desc(12)?);
    let loader_buf = Mem::new(l_size.div_ceil(PAGE_SIZE))?;
    loader_buf.bytes()[..l_size].copy_from_slice(loader.get(l_data..l_data + l_size).ok_or("BOOTLDR.BIN truncated")?);

    // 2c. ELF GSP: разделы `.fwimage`, `.fwsignature_ga10x`, `.fwversion`.
    let mut head = [0u8; 64];
    read_at(&*gsp, 0, &mut head)?;
    if &head[..4] != b"\x7fELF" {
        return Err("GSP.BIN is not an ELF file".into());
    }
    let shoff = le64(&head, 0x28)?;
    let (shentsize, shnum, shstrndx) = (
        u16::from_le_bytes([head[0x3a], head[0x3b]]) as usize,
        u16::from_le_bytes([head[0x3c], head[0x3d]]) as usize,
        u16::from_le_bytes([head[0x3e], head[0x3f]]) as usize,
    );
    let mut shdrs = alloc::vec![0u8; shentsize * shnum];
    read_at(&*gsp, shoff, &mut shdrs)?;
    let section = |i: usize| -> Res<(u32, u64, u64)> {
        let at = i * shentsize;
        Ok((le32(&shdrs, at)?, le64(&shdrs, at + 0x18)?, le64(&shdrs, at + 0x20)?))
    };
    let (_, str_off, str_size) = section(shstrndx)?;
    let mut names = alloc::vec![0u8; str_size as usize];
    read_at(&*gsp, str_off, &mut names)?;
    let find = |wanted: &str| -> Res<(u64, u64)> {
        for i in 0..shnum {
            let (name, off, size) = section(i)?;
            let name = &names[name as usize..];
            let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
            if &name[..end] == wanted.as_bytes() {
                return Ok((off, size));
            }
        }
        Err(format!("GSP.BIN has no {wanted} section"))
    };
    let (ver_off, ver_size) = find(".fwversion")?;
    let mut version = alloc::vec![0u8; ver_size as usize];
    read_at(&*gsp, ver_off, &mut version)?;
    if !version.starts_with(b"570.144") {
        return Err(format!("GSP.BIN is version {:?}, only 570.144 is done", String::from_utf8_lossy(&version)));
    }
    let (img_off, img_size) = find(".fwimage")?;
    let (sig_off, sig_size) = find(".fwsignature_ga10x")?;
    if img_size % PAGE_SIZE as u64 != 0 {
        return Err("GSP image size is not whole pages".into());
    }
    let pages = (img_size / PAGE_SIZE as u64) as usize;
    let free = crate::mm::frame::stats().free;
    if free < pages + 4096 {
        return Err(format!("not enough memory: GSP needs {pages} pages, {free} free"));
    }

    // 3. Образ GSP — постранично (страницы вразброс, их адреса — в radix3).
    let lvl2_pages = (pages * 8).div_ceil(PAGE_SIZE);
    let lvl2 = Mem::new(lvl2_pages)?;
    let lvl1 = Mem::new(1)?;
    let lvl0 = Mem::new(1)?;
    lvl0.w64(0, lvl1.phys);
    for i in 0..lvl2_pages {
        lvl1.w64(i * 8, lvl2.phys + (i * PAGE_SIZE) as u64);
    }
    const CHUNK: usize = 1024 * 1024;
    let mut chunk = alloc::vec![0u8; CHUNK];
    let mut page = 0;
    while page < pages {
        let take = (pages - page).min(CHUNK / PAGE_SIZE);
        read_at(&*gsp, img_off + (page * PAGE_SIZE) as u64, &mut chunk[..take * PAGE_SIZE])?;
        for i in 0..take {
            let phys = single_page()?;
            let ptr = PhysAddr::new(phys).to_direct_map().as_usize() as *mut u8;
            // SAFETY: страница только что выделена.
            unsafe { core::ptr::copy_nonoverlapping(chunk[i * PAGE_SIZE..].as_ptr(), ptr, PAGE_SIZE) };
            lvl2.w64((page + i) * 8, phys);
        }
        page += take;
    }
    let signature = Mem::new((sig_size as usize).div_ceil(PAGE_SIZE))?;
    read_at(&*gsp, sig_off, &mut signature.bytes()[..sig_size as usize])?;
    log.push(format!("GSP image {img_size:#x} B in {pages} pages, radix3 {lvl2_pages} lvl2 pages, signature {sig_size:#x} B"));

    if dry {
        return Ok(());
    }
    let (device, bar) = card.ok_or("no NVIDIA card")?;

    // 4. LIBOS: журналы и аргументы GSP-RM.
    let logs: Vec<Mem> = (0..3).map(|_| Mem::new(16)).collect::<Res<_>>()?;
    for buffer in &logs {
        for i in 0..16 {
            buffer.w64(8 + i * 8, buffer.phys + (i * PAGE_SIZE) as u64);
        }
    }
    let mut queues = Queues::new()?;
    let rmargs = Mem::new(1)?;
    rmargs.w64(0x00, queues.block.phys);
    rmargs.w32(0x08, 0x81);
    rmargs.w64(0x10, Queues::CMDQ as u64);
    rmargs.w64(0x18, Queues::MSGQ as u64);
    rmargs.bytes()[0x30] = 1; // bDmemStack (r570)
    let libos = Mem::new(1)?;
    let ids: [u64; 4] = [0x4c4f_4749_4e49_54, 0x4c4f_4749_4e54_52, 0x4c_4f47_524d, 0x524d_4152_4753];
    let regions = [(logs[0].phys, 0x10000u64), (logs[1].phys, 0x10000), (logs[2].phys, 0x10000), (rmargs.phys, 0x1000)];
    for (i, (id, (pa, size))) in ids.iter().zip(regions).enumerate() {
        libos.w64(i * 32, *id);
        libos.w64(i * 32 + 8, pa);
        libos.w64(i * 32 + 16, size);
        libos.bytes()[i * 32 + 24] = 1; // непрерывный кусок
        libos.bytes()[i * 32 + 25] = 1; // в памяти машины
    }

    // 5. Раскладка WPR2 и описание для booter (GspFwWprMeta, r570.144).
    let elf_size = img_size;
    let boot = (frts - l_size as u64) & !0xfff;
    let fw_image = (boot - elf_size) & !0xffff;
    let heap_end = fw_image & !0xf_ffff;
    let heap = (fw_image - (heap_mib << 20)) & !0xf_ffff;
    let wpr2 = (heap - 0x100) & !0xf_ffff;
    let non_wpr = wpr2 - 0x10_0000;
    let meta = Mem::new(1)?;
    let fields: [(usize, u64); 26] = [
        (0x00, 0xdc3a_ae21_371a_60b3),
        (0x08, 1),
        (0x10, lvl0.phys),
        (0x18, img_size),
        (0x20, loader_buf.phys),
        (0x28, l_size as u64),
        (0x30, u64::from(monitor_code)),
        (0x38, u64::from(monitor_data)),
        (0x40, u64::from(manifest)),
        (0x48, signature.phys),
        (0x50, sig_size),
        (0x58, non_wpr),
        (0x60, non_wpr),
        (0x68, 0x10_0000),
        (0x70, wpr2),
        (0x78, heap),
        (0x80, heap_end - heap),
        (0x88, fw_image),
        (0x90, boot),
        (0x98, frts),
        (0xa0, 0x10_0000),
        (0xa8, vga_base & !0x1_ffff),
        (0xb0, fb_end),
        (0xb8, vga_base),
        (0xc0, fb_end - vga_base),
        (0xc8, 0),
    ];
    for (at, v) in fields {
        meta.w64(at, v);
    }
    log.push(format!(
        "WPR2 {wpr2:#x}..{:#x}: heap {heap:#x} ({heap_mib} MiB), image {fw_image:#x}, boot {boot:#x}, FRTS {frts:#x}, non-WPR heap {non_wpr:#x}",
        frts + 0x10_0000
    ));

    // 6. SET_SYSTEM_INFO и SET_REGISTRY — в очередь до запуска.
    let bar_phys = |i| device.memory_bar(i).map_or(0, |p: PhysAddr| p.as_u64());
    let mut info = alloc::vec![0u8; 0x3a0];
    let mut put64 = |at: usize, v: u64| info[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put64(0x00, bar_phys(0));
    put64(0x08, bar_phys(1));
    put64(0x10, bar_phys(3));
    let address = device.address;
    put64(0x20, (u64::from(address.bus) << 8) | (u64::from(address.device) << 3) | u64::from(address.function));
    // Картинка прошивки (GOP) лежит в начале видеопамяти: сказать GSP, что
    // консоль есть и её место занято (как делает CPU-RM при efifb; 32 МиБ —
    // с запасом на 4K). Этап 6, опыт E2.
    put64(0x40, 32 << 20);
    put64(0x48, (1u64 << 47) - 4096);
    let mut put32 = |at: usize, v: u32| info[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put32(0x50, 0x88000);
    put32(0x54, 0x1000);
    put32(0x58, device.config32(0));
    put32(0x5c, device.config32(0x2c));
    put32(0x60, u32::from(device.config8(8)));
    info[0x380] = 1; // bIsPrimary: эта карта показывает загрузочный экран
    queues.send(None, FN_SET_SYSTEM_INFO, &info)?;
    log.push(format!("before GSP: {}", snapshot(bar)));
    queues.send(None, FN_SET_REGISTRY, &registry())?;

    // 7. Процессор GSP: сброс, ядро RISC-V, адрес LIBOS в почтовых ящиках.
    bar.wr(GSP + 0x14, u32::MAX);
    bar.wr(GSP + 0x48, bar.rd(GSP + 0x48) & !3);
    falcon_reset(bar, GSP)?;
    bar.wr(GSP2 + 0x668, bar.rd(GSP2 + 0x668) | 0x111);
    bar.wr(GSP + 0x40, libos.phys as u32);
    bar.wr(GSP + 0x44, (libos.phys >> 32) as u32);
    bar.wr(GSP + 0x84, bar.rd(0));

    // 8. booter_load на SEC2.
    bar.wr(SEC2 + 0x14, u32::MAX);
    falcon_reset(bar, SEC2)?;
    if bar.rd(SEC2_2 + 0x668) & 0x10 != 0 {
        bar.wr(SEC2_2 + 0x668, 0);
        if !bar.wait(10_000, |b| b.rd(SEC2_2 + 0x668) & 1 != 0) {
            return Err("SEC2 did not switch to the Falcon core".into());
        }
    }
    bar.wr(SEC2 + 0x84, bar.rd(0));
    bar.wr(SEC2 + 0x624, bar.rd(SEC2 + 0x624) | 0x80);
    bar.wr(SEC2 + 0x10c, 0);
    bar.wr(SEC2 + 0x600, (bar.rd(SEC2 + 0x600) & !7) | 5);
    fence(Ordering::SeqCst);
    falcon_dma(bar, SEC2, booter_buf.phys, app_off, 0, app_size, 0x614)?;
    falcon_dma(bar, SEC2, booter_buf.phys + os_data_off as u64, 0, 0, os_data_size, 0x600)?;
    bar.wr(SEC2_2 + 0x210, pkc as u32);
    bar.wr(SEC2_2 + 0x19c, engine_id);
    bar.wr(SEC2_2 + 0x198, ucode_id);
    bar.wr(SEC2_2 + 0x180, 1);
    bar.wr(SEC2 + 0x104, app_off as u32);
    bar.wr(SEC2 + 0x40, meta.phys as u32);
    bar.wr(SEC2 + 0x44, (meta.phys >> 32) as u32);
    falcon_start(bar, SEC2);
    let halted = bar.wait(2_000_000, |b| b.rd(SEC2 + 0x100) & 0x10 != 0);
    let (m0, m1) = (bar.rd(SEC2 + 0x40), bar.rd(SEC2 + 0x44));
    log.push(format!(
        "booter {}: MAILBOX0 {m0:#x}, MAILBOX1 {m1:#x}, CPUCTL {:#x}, WPR2 {:#010x}..{:#010x}, meta verified {:#x}",
        if halted { "halted" } else { "did NOT halt" },
        bar.rd(SEC2 + 0x100),
        bar.rd(0x001f_a824),
        bar.rd(0x001f_a828),
        meta.r64(0xf8)
    ));
    if !halted || m0 != 0 {
        return Err("booter_load failed".into());
    }

    // 9. GSP RISC-V должен ожить.
    bar.wr(GSP + 0x80, app_version);
    let active = bar.wait(5_000_000, |b| b.rd(GSP2 + 0x388) & 0x80 != 0);
    log.push(format!(
        "GSP RISC-V {}: RISCV_CPUCTL {:#x}, HWCFG2 {:#x}, BCR {:#x}",
        if active { "active" } else { "NOT active" },
        bar.rd(GSP2 + 0x388),
        bar.rd(GSP + 0xf4),
        bar.rd(GSP2 + 0x668)
    ));
    if !active {
        return Err("GSP did not start".into());
    }

    // 10. Ждать GSP_INIT_DONE (до 30 с), выполняя секвенсор.
    let start = crate::time::uptime_ms();
    let mut next_report = start + 1000;
    let mut sequencer_runs = 0;
    loop {
        match queues.receive()? {
            Some((EV_INIT_DONE, _, _)) => break,
            Some((EV_RUN_CPU_SEQUENCER, _, payload)) => {
                sequencer_runs += 1;
                let line = run_sequencer(bar, &payload, libos.phys, app_version)?;
                log.push(line);
            }
            Some((function, result, payload)) => {
                log.push(format!("event {function:#x} (result {result:#x}, {} B)", payload.len()));
            }
            None => {
                let now = crate::time::uptime_ms();
                if now > start + 30_000 {
                    let (cw, gr, mw, cr) = queues.pointers();
                    return Err(format!(
                        "no GSP_INIT_DONE in 30 s (cmdq w {cw} / GSP r {gr}, msgq w {mw} / CPU r {cr}, RISCV_CPUCTL {:#x}, log puts {:#x} {:#x} {:#x})",
                        bar.rd(GSP2 + 0x388),
                        logs[0].r64(0),
                        logs[1].r64(0),
                        logs[2].r64(0)
                    ));
                }
                if now > next_report {
                    next_report = now + 5000;
                    let (cw, gr, mw, cr) = queues.pointers();
                    crate::kprintln!(
                        "  nvidia-gsp  : waiting {} s: cmdq {cw}/{gr}, msgq {mw}/{cr}, RISCV_CPUCTL {:#x}, LOGINIT put {:#x}",
                        (now - start) / 1000,
                        bar.rd(GSP2 + 0x388),
                        logs[0].r64(0)
                    );
                }
                spin_us(500);
            }
        }
    }
    log.push(format!("GSP_INIT_DONE after {} ms, {sequencer_runs} sequencer run(s)", crate::time::uptime_ms() - start));

    // 11. GET_GSP_STATIC_INFO: имя карты от самой карты.
    queues.send(Some(bar), FN_GET_GSP_STATIC_INFO, &alloc::vec![0u8; STATIC_INFO_SIZE])?;
    let deadline = crate::time::uptime_ms() + 5000;
    let internal = loop {
        match queues.receive()? {
            Some((FN_GET_GSP_STATIC_INFO, result, payload)) => {
                if result != 0 {
                    return Err(format!("GET_GSP_STATIC_INFO failed: {result:#x}"));
                }
                let name = payload.get(0x4ec..0x52c).unwrap_or(&[]);
                let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
                let fb_length = le64(&payload, 0x4c8).unwrap_or(0);
                log.push(format!("GSP says: '{}', FB {} MiB", String::from_utf8_lossy(&name[..end]), fb_length >> 20));
                // Области видеопамяти, как их видит RM.
                let regions = le32(&payload, 0x158).unwrap_or(0).min(16) as usize;
                for i in 0..regions {
                    let at = 0x160 + i * 48;
                    log.push(format!(
                        "FB region {i}: {:#x}..{:#x} reserved {:#x} perf {} compr {} iso {} protected {}",
                        le64(&payload, at).unwrap_or(0),
                        le64(&payload, at + 8).unwrap_or(0),
                        le64(&payload, at + 16).unwrap_or(0),
                        le32(&payload, at + 24).unwrap_or(0),
                        payload.get(at + 28).copied().unwrap_or(0),
                        payload.get(at + 29).copied().unwrap_or(0),
                        payload.get(at + 30).copied().unwrap_or(0)
                    ));
                }
                log.push(format!(
                    "BAR1 PDE {:#x}, BAR2 PDE {:#x}, internal client {:#x} device {:#x} subdevice {:#x}",
                    le64(&payload, 0x600).unwrap_or(0),
                    le64(&payload, 0x608).unwrap_or(0),
                    le32(&payload, 0x640).unwrap_or(0),
                    le32(&payload, 0x644).unwrap_or(0),
                    le32(&payload, 0x648).unwrap_or(0)
                ));
                break (le32(&payload, 0x640).unwrap_or(0), le32(&payload, 0x648).unwrap_or(0));
            }
            Some((function, _, _)) => log.push(format!("event {function:#x} while waiting for static info")),
            None if crate::time::uptime_ms() > deadline => return Err("no GET_GSP_STATIC_INFO reply in 5 s".into()),
            None => spin_us(500),
        }
    };
    bar.wr(GSP + 0x04, 0x40);

    // 12. Этап 6, опыт E1: GSP-RM переводит BAR1 в виртуальный режим со своими
    // таблицами, и запись процессора в кадр прошивки больше не доходит до
    // видеопамяти — картинка «замерзает». Вернуть BAR1 в физический режим
    // (то, что пишет сам RM в `kbusBar1InstBlkBind_TU102`).
    log.push(format!("after GSP: {}", snapshot(bar)));
    bar.wr(0x00b8_0f40, 0);
    let bound = bar.wait(2_000_000, |b| b.rd(0x00b8_0f50) & 3 == 0);
    log.push(format!("BAR1 back to physical mode: {} ({})", if bound { "bound" } else { "bind did NOT finish" }, snapshot(bar)));

    // 13. Этап 6: объекты дисплея GSP-RM и опрос выходов (только чтение).
    let mut rm = Rm { queues: &mut queues, bar, log: Vec::new() };
    rm.probe_display();
    log.extend(rm.log);

    // Сеанс живёт дальше: дисплей (`nvidia_disp`) продолжает разговор с RM.
    // Память GSP (журналы, очереди, образ) не возвращается никогда.
    *SESSION.lock() = Some(Session { queues, bar, internal_client: internal.0, internal_subdevice: internal.1 });
    Ok(())
}

/// Запущенный GSP-RM: очереди сообщений и внутренние хэндлы RM.
pub(crate) struct Session {
    pub(crate) queues: Queues,
    pub(crate) bar: Bar0,
    pub(crate) internal_client: u32,
    pub(crate) internal_subdevice: u32,
}

// SAFETY: очереди — память машины, отданная GSP; ходит один владелец под замком.
unsafe impl Send for Session {}

pub(crate) static SESSION: crate::sync::SpinLock<Option<Session>> = crate::sync::SpinLock::new(None);

/// Работает ли GSP-RM.
#[must_use]
pub fn running() -> bool {
    SESSION.lock().is_some()
}

/// Состояние BAR1 и дисплейных головок — для журнала.
fn snapshot(bar: Bar0) -> String {
    let mut out = format!(
        "BAR1_BLOCK {:#x} BAR2_BLOCK {:#x} BIND {:#x} 0x1704 {:#x} 0x1714 {:#x}",
        bar.rd(0x00b8_0f40),
        bar.rd(0x00b8_0f48),
        bar.rd(0x00b8_0f50),
        bar.rd(0x0000_1704),
        bar.rd(0x0000_1714)
    );
    for head in 0..4usize {
        let raster = bar.rd(0x0068_a064 + head * 0x400);
        let line0 = bar.rd(0x0061_6330 + head * 0x800) & 0xffff;
        spin_us(1000);
        let line1 = bar.rd(0x0061_6330 + head * 0x800) & 0xffff;
        out.push_str(&format!(" | head{head} raster {raster:#x} line {line0}->{line1}"));
    }
    out
}

// --- RM: объекты и управление (этап 6) --------------------------------------

const FN_FREE: u32 = 10;
const FN_RM_CONTROL: u32 = 76;
const FN_RM_ALLOC: u32 = 103;

pub(crate) const H_CLIENT: u32 = 0xc1d0_0001;
pub(crate) const H_DEVICE: u32 = 0xde1d_0000;
const H_SUBDEVICE: u32 = 0x5d1d_0000;
pub(crate) const H_DISP_COMMON: u32 = 0x0073_0000;

/// Разговор с GSP-RM: выделить объект, вызвать управление.
pub(crate) struct Rm<'a> {
    pub(crate) queues: &'a mut Queues,
    pub(crate) bar: Bar0,
    pub(crate) log: Log,
}

impl Rm<'_> {
    /// Послать RPC и дождаться ответа с той же функцией (события — мимо).
    pub(crate) fn call(&mut self, function: u32, payload: &[u8]) -> Res<Vec<u8>> {
        self.queues.send(Some(self.bar), function, payload)?;
        let deadline = crate::time::uptime_ms() + 5000;
        loop {
            match self.queues.receive()? {
                Some((f, result, reply)) if f == function => {
                    if result != 0 {
                        return Err(format!("RPC {function} result {result:#x}"));
                    }
                    return Ok(reply);
                }
                Some((f, _, _)) => self.log.push(format!("event {f:#x} during RPC {function}")),
                None if crate::time::uptime_ms() > deadline => return Err(format!("no reply to RPC {function} in 5 s")),
                None => spin_us(200),
            }
        }
    }

    /// GSP_RM_ALLOC: объект `class` с хэндлом `object` под `parent`.
    pub(crate) fn alloc(&mut self, parent: u32, object: u32, class: u32, params: &[u8]) -> Res<()> {
        let mut p = alloc::vec![0u8; 0x20 + params.len()];
        for (at, v) in [(0, H_CLIENT), (4, parent), (8, object), (12, class), (0x14, params.len() as u32)] {
            p[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        p[0x20..].copy_from_slice(params);
        let reply = self.call(FN_RM_ALLOC, &p)?;
        let status = le32(&reply, 0x10)?;
        if status != 0 {
            return Err(format!("alloc class {class:#x} status {status:#x}"));
        }
        Ok(())
    }

    /// GSP_RM_CONTROL: команда `cmd` объекту; возвращает ответные параметры.
    pub(crate) fn control(&mut self, client: u32, object: u32, cmd: u32, params: &[u8]) -> Res<Vec<u8>> {
        let mut p = alloc::vec![0u8; 0x18 + params.len()];
        for (at, v) in [(0, client), (4, object), (8, cmd), (0x10, params.len() as u32)] {
            p[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        p[0x18..].copy_from_slice(params);
        let reply = self.call(FN_RM_CONTROL, &p)?;
        let status = le32(&reply, 0x0c)?;
        if status != 0 {
            return Err(format!("control {cmd:#x} status {status:#x}"));
        }
        Ok(reply.get(0x18..).unwrap_or(&[]).to_vec())
    }

    pub(crate) fn disp(&mut self, cmd: u32, params: &[u8]) -> Res<Vec<u8>> {
        self.control(H_CLIENT, H_DISP_COMMON, cmd, params)
    }

    /// Создать клиента, устройство, подустройство и общий дисплей, затем
    /// спросить у RM о выходах и мониторах. Только чтение — режимы не
    /// меняются. Каждый шаг отчитывается сам; ошибка не обрывает остальные.
    fn probe_display(&mut self) {
        let mut root = alloc::vec![0u8; 0x6c];
        root[0..4].copy_from_slice(&H_CLIENT.to_le_bytes());
        root[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        root[8..14].copy_from_slice(b"fossai");
        let mut device = alloc::vec![0u8; 0x38];
        device[4..8].copy_from_slice(&H_CLIENT.to_le_bytes());
        let steps: [(&str, u32, u32, u32, Vec<u8>); 4] = [
            ("client", H_CLIENT, H_CLIENT, 0x0000, root),
            ("device", H_CLIENT, H_DEVICE, 0x0080, device),
            ("subdevice", H_DEVICE, H_SUBDEVICE, 0x2080, alloc::vec![0u8; 4]),
            ("display common", H_DEVICE, H_DISP_COMMON, 0x0073, Vec::new()),
        ];
        for (name, parent, object, class, params) in steps {
            match self.alloc(parent, object, class, &params) {
                Ok(()) => self.log.push(format!("RM: {name} {object:#x} allocated")),
                Err(why) => {
                    self.log.push(format!("RM: {name} failed - {why}"));
                    return;
                }
            }
        }

        let u32s = |words: &[u32]| words.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>();
        match self.disp(0x0073_0102, &u32s(&[0, 0, 0])) {
            Ok(r) => self.log.push(format!("RM: {} head(s)", le32(&r, 8).unwrap_or(0))),
            Err(why) => self.log.push(format!("RM: GET_NUM_HEADS - {why}")),
        }
        let supported = match self.disp(0x0073_0107, &u32s(&[0, 0, 0])) {
            Ok(r) => {
                let (mask, ddc) = (le32(&r, 4).unwrap_or(0), le32(&r, 8).unwrap_or(0));
                self.log.push(format!("RM: outputs supported {mask:#x}, with DDC {ddc:#x}"));
                mask
            }
            Err(why) => {
                self.log.push(format!("RM: GET_SUPPORTED - {why}"));
                return;
            }
        };
        let connected = match self.disp(0x0073_0108, &u32s(&[0, 0, supported, 0])) {
            Ok(r) => {
                let mask = le32(&r, 8).unwrap_or(0);
                self.log.push(format!("RM: outputs connected {mask:#x}"));
                mask
            }
            Err(why) => {
                self.log.push(format!("RM: GET_CONNECT_STATE - {why}"));
                0
            }
        };
        for bit in 0..32 {
            let id = 1u32 << bit;
            if supported & id == 0 {
                continue;
            }
            let mut info = u32s(&[0, id]);
            info.resize(0x38, 0);
            let kind = match self.disp(0x0073_028b, &info) {
                Ok(r) => {
                    let (index, kind, protocol, location) =
                        (le32(&r, 8).unwrap_or(0), le32(&r, 12).unwrap_or(0), le32(&r, 16).unwrap_or(0), le32(&r, 28).unwrap_or(0));
                    let lit = r.get(0x30).copied().unwrap_or(0);
                    self.log.push(format!(
                        "RM: output {id:#x}: OR index {index} type {kind} protocol {protocol} location {location} lit by VBIOS {lit}{}",
                        if connected & id != 0 { ", CONNECTED" } else { "" }
                    ));
                    protocol
                }
                Err(why) => {
                    self.log.push(format!("RM: OR_GET_INFO {id:#x} - {why}"));
                    continue;
                }
            };
            let _ = kind;
            if connected & id == 0 {
                continue;
            }
            // EDID: у HDMI/DVI его отдаёт RM; у DisplayPort — только AUX (позже).
            let mut edid = u32s(&[0, id, 2048, 0]);
            edid.resize(16 + 2048, 0);
            match self.disp(0x0073_0245, &edid) {
                Ok(r) => {
                    let size = le32(&r, 8).unwrap_or(0) as usize;
                    let bytes = r.get(16..16 + size.min(2048)).unwrap_or(&[]);
                    self.log.push(format!("RM: EDID of {id:#x}: {size} bytes, {}", describe_edid(bytes)));
                }
                Err(why) => self.log.push(format!("RM: EDID of {id:#x} - {why} (DisplayPort needs AUX)")),
            }
        }
        for head in 0..4u32 {
            if let Ok(r) = self.disp(0x0073_010c, &u32s(&[0, head, 0, 0])) {
                let id = le32(&r, 12).unwrap_or(0);
                if id != 0 {
                    self.log.push(format!("RM: head {head} drives output {id:#x}"));
                }
            }
        }
        let _ = FN_FREE;
    }
}

/// Производитель, модель и родной режим из EDID — для журнала.
fn describe_edid(e: &[u8]) -> String {
    if e.len() < 128 || e[0..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
        return "not a valid EDID".into();
    }
    let maker = u16::from_be_bytes([e[8], e[9]]);
    let letters: String = [(maker >> 10) & 31, (maker >> 5) & 31, maker & 31]
        .iter()
        .map(|c| char::from(b'@' + *c as u8))
        .collect();
    let mut name = String::new();
    for d in 0..4 {
        let at = 54 + d * 18;
        if e[at] == 0 && e[at + 1] == 0 && e[at + 3] == 0xfc {
            name = String::from_utf8_lossy(&e[at + 5..at + 18]).trim().into();
        }
    }
    // Первый подробный режим — родной.
    let d = &e[54..72];
    let clock = u32::from(u16::from_le_bytes([d[0], d[1]])) * 10;
    let w = u32::from(d[2]) | (u32::from(d[4] & 0xf0) << 4);
    let h = u32::from(d[5]) | (u32::from(d[7] & 0xf0) << 4);
    format!("{letters} '{name}', native {w}x{h} pixel clock {clock} kHz")
}

/// FWSEC-FRTS (если в эту загрузку ещё не было) и раскладка верха
/// видеопамяти — та же, что у FWSEC: (конец VRAM, рабочая область VGA, FRTS).
fn layout(log: &mut Log, bar: Bar0) -> Res<(u64, u64, u64)> {
    if bar.rd(0x001f_a828) >> 4 == 0 {
        log.push("WPR2 not set yet - running FWSEC-FRTS first".into());
        for line in super::nvidia::boot_fwsec() {
            log.push(line);
        }
        if bar.rd(0x001f_a828) >> 4 == 0 {
            return Err("FWSEC-FRTS did not set WPR2".into());
        }
    }
    let usable = bar.rd(0x0011_83a4);
    if usable == 0 || usable == u32::MAX {
        return Err(format!("VRAM size unknown (0x1183a4 = {usable:#x})"));
    }
    let fb_end = u64::from(usable) << 20;
    let base_nopdisp = fb_end - 0x10_0000;
    let display = bar.rd(0x0082_0c04) & 1 == 0;
    let vga = bar.rd(0x0062_5f04);
    let vga_base = if display && vga & 8 != 0 {
        let at = u64::from(vga >> 8) << 16;
        if at < base_nopdisp { fb_end - 0x2_0000 } else { at }
    } else {
        base_nopdisp
    };
    Ok((fb_end, vga_base, (vga_base & !0x1_ffff) - 0x10_0000))
}

/// Таблица реестра GSP-RM: три параметра, как у nova и nouveau.
fn registry() -> Vec<u8> {
    let keys: [(&str, u32); 3] = [("RMSecBusResetEnable", 1), ("RMForcePcieConfigSave", 1), ("RMDevidCheckIgnore", 1)];
    let mut out = alloc::vec![0u8; 8 + 16 * keys.len()];
    let mut strings = Vec::new();
    for (i, (key, value)) in keys.iter().enumerate() {
        let name_offset = (8 + 16 * keys.len() + strings.len()) as u32;
        let at = 8 + 16 * i;
        out[at..at + 4].copy_from_slice(&name_offset.to_le_bytes());
        out[at + 4] = 1;
        out[at + 8..at + 12].copy_from_slice(&value.to_le_bytes());
        out[at + 12..at + 16].copy_from_slice(&4u32.to_le_bytes());
        strings.extend_from_slice(key.as_bytes());
        strings.push(0);
    }
    out.extend_from_slice(&strings);
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_le_bytes());
    out[4..8].copy_from_slice(&(keys.len() as u32).to_le_bytes());
    out
}

/// RUN_CPU_SEQUENCER: GSP просит процессор машины потрогать регистры.
fn run_sequencer(bar: Bar0, payload: &[u8], libos: u64, app_version: u32) -> Res<String> {
    let dwords = le32(payload, 0)? as usize;
    let end = (le32(payload, 4)? as usize).min(dwords);
    let word = |i: usize| le32(payload, 0x28 + i * 4);
    let available = payload.len().saturating_sub(0x28) / 4;
    let end = end.min(available);
    let mut ops: Vec<String> = Vec::new();
    let mut i = 0;
    while i < end {
        let op = word(i)?;
        let args = match op {
            0 => 2,
            1 => 3,
            2 => 5,
            3 => 1,
            4 => 2,
            5..=8 => 0,
            _ => return Err(format!("sequencer: unknown opcode {op} at {i}")),
        };
        let a = |k: usize| word(i + 1 + k);
        let mut text = format!("{op}");
        for k in 0..args {
            text.push_str(&format!(":{:#x}", a(k).unwrap_or(0)));
        }
        ops.push(text);
        let fail = |why: String, ops: &Vec<String>| Err(format!("{why}; sequence so far [{}]", ops.join(" ")));
        match op {
            0 => bar.wr(a(0)? as usize, a(1)?),
            1 => {
                let (addr, mask, val) = (a(0)? as usize, a(1)?, a(2)?);
                bar.wr(addr, (bar.rd(addr) & !mask) | val);
            }
            2 => {
                let (addr, mask, val, timeout) = (a(0)? as usize, a(1)?, a(2)?, a(3)?);
                let us = if timeout == 0 { 4_000_000 } else { u64::from(timeout) };
                if !bar.wait(us, |b| b.rd(addr) & mask == val) {
                    return fail(format!("sequencer: poll {addr:#x} & {mask:#x} == {val:#x} timed out ({:#x})", bar.rd(addr)), &ops);
                }
            }
            3 => spin_us(u64::from(a(0)?)),
            4 => {
                let _ = bar.rd(a(0)? as usize);
            }
            5 => {
                // `nvkm_falcon_reset` / nova `reset`: сброс, ядро Falcon, RM.
                falcon_reset(bar, GSP)?;
                falcon_select(bar, GSP2)?;
                bar.wr(GSP + 0x84, bar.rd(0));
                bar.wr(GSP + 0x624, bar.rd(GSP + 0x624) | 0x80);
                bar.wr(GSP + 0x10c, 0);
            }
            6 => falcon_start(bar, GSP),
            7 => {
                if !bar.wait(2_000_000, |b| b.rd(GSP + 0x100) & 0x10 != 0) {
                    return fail("sequencer: GSP core did not halt".into(), &ops);
                }
            }
            8 => {
                // `ga102_gsp_reset`: сброс и снова RISC-V.
                falcon_reset(bar, GSP)?;
                bar.wr(GSP2 + 0x668, bar.rd(GSP2 + 0x668) | 0x111);
                bar.wr(GSP + 0x40, libos as u32);
                bar.wr(GSP + 0x44, (libos >> 32) as u32);
                falcon_start(bar, SEC2);
                if !bar.wait(2_000_000, |b| b.rd(0x0011_80f8) & 0x0400_0000 != 0) {
                    return Err("sequencer: SEC2 resume timed out".into());
                }
                if bar.rd(SEC2 + 0x40) != 0 {
                    return Err(format!("sequencer: SEC2 resume returned {:#x}", bar.rd(SEC2 + 0x40)));
                }
                bar.wr(GSP + 0x80, app_version);
            }
            _ => {}
        }
        i += 1 + args;
    }
    Ok(format!("sequencer: {} op(s) [{}]", ops.len(), ops.join(" ")))
}
