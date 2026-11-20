// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! GPFIFO, заголовки методов пушбуфера, забор (семафор) и константы
//! классов канала (C56F) и копирования (C7B5).
//!
//! Сверено с Linux 6.12 `nouveau_dma.c` (`nv50_dma_push`), `nvc0_fence.c`
//! (`nvc0_fence_emit32`) и заголовками open-gpu-kernel-modules 570.144
//! `clc56f.h`, `clc7b5.h`.

/// Записей в кольце GPFIFO (0x2000 Б в пушбуфере канала, раздел 2.4).
pub const GPFIFO_ENTRIES: u32 = 1024;
/// `NV50_DMA_IB_MAX`: маска индекса и число полезных записей.
pub const IB_MAX: u32 = GPFIFO_ENTRIES - 1;
/// Байт на запись GPFIFO.
pub const GP_ENTRY_BYTES: u64 = 8;
/// Предел длины сегмента (поле LENGTH 30:10 — 21 бит двойных слов).
pub const MAX_SEGMENT_BYTES: u32 = (1 << 21) * 4 - 4;

/// Раскладка пушбуфера канала у nouveau (0x12000 Б): область ядра под
/// сегменты заборов и кольцо GPFIFO в конце.
pub const PUSHBUF_BYTES: u64 = 0x12000;
pub const PUSHBUF_KERNEL_AREA: u64 = 0x10000;
pub const PUSHBUF_RING_OFFSET: u64 = 0x10000;

/// USERD (`Nvc56fControl`), 0x200 Б на канал.
pub const USERD_BYTES: u64 = 0x200;
pub const USERD_GP_GET: u64 = 0x88;
pub const USERD_GP_PUT: u64 = 0x8c;

/// Дверной звонок и PTIMER в BAR0 (окно AMPERE_USERMODE_A, раздел 0 п.5).
pub const USERMODE_BASE: u32 = 0x00b8_0000 + 0x30000;
pub const DOORBELL: u32 = USERMODE_BASE + 0x90;
pub const PTIMER_LO: u32 = USERMODE_BASE + 0x80;
pub const PTIMER_HI: u32 = USERMODE_BASE + 0x84;

/// Жетон звонка у r535: `(runlist << 16) | chid` (сверять с
/// `NVC36F_CTRL_CMD_GPFIFO_GET_WORK_SUBMIT_TOKEN`).
pub const fn doorbell_token(runlist: u32, chid: u32) -> u32 {
    (runlist << 16) | chid
}

/// Запись GPFIFO: `dw0 = lo32(va)`, `dw1 = hi32(va) | len << 8 |
/// NO_PREFETCH << 31` (`len` в байтах, т.е. двойные слова в битах 30:10).
/// Возвращается как u64 в порядке памяти (dw0 — младшее слово).
pub const fn gp_entry(va: u64, len_bytes: u32, no_prefetch: bool) -> u64 {
    let dw0 = va as u32;
    let dw1 = ((va >> 32) as u32 & 0xff) | (len_bytes << 8) | if no_prefetch { 1 << 31 } else { 0 };
    (dw0 as u64) | ((dw1 as u64) << 32)
}

/// Разобранная запись GPFIFO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpEntry {
    pub va: u64,
    pub len_bytes: u32,
    pub no_prefetch: bool,
    /// Бит LEVEL (подпрограмма) — у нас не бывает.
    pub subroutine: bool,
}

pub const fn gp_entry_decode(e: u64) -> GpEntry {
    let dw0 = e as u32;
    let dw1 = (e >> 32) as u32;
    GpEntry {
        va: ((dw1 as u64 & 0xff) << 32) | (dw0 as u64 & !3),
        len_bytes: ((dw1 >> 10) & 0x1f_ffff) * 4,
        no_prefetch: dw1 & (1 << 31) != 0,
        subroutine: dw1 & (1 << 9) != 0,
    }
}

/// Учёт кольца GPFIFO со стороны ядра.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Ring {
    /// Следующая свободная запись (`ib_put`).
    pub put: u32,
}

impl Ring {
    /// Сколько записей можно добавить при данном GP_GET (одна всегда пуста,
    /// чтобы отличить полное кольцо от пустого).
    pub const fn free(&self, gp_get: u32) -> u32 {
        (gp_get.wrapping_sub(self.put).wrapping_sub(1)) & IB_MAX
    }

    /// Занять запись; возвращает её индекс.
    pub fn advance(&mut self) -> u32 {
        let i = self.put;
        self.put = (self.put + 1) & IB_MAX;
        i
    }

    /// Смещение записи `i` от начала кольца.
    pub const fn offset(i: u32) -> u64 {
        i as u64 * GP_ENTRY_BYTES
    }
}

// ---------------------------------------------------------------------------
// Заголовки методов (формат Fermi+, SEC_OP в битах 31:29)

pub const SEC_OP_GRP0_USE_TERT: u32 = 0;
pub const SEC_OP_INC_METHOD: u32 = 1;
pub const SEC_OP_NON_INC_METHOD: u32 = 3;
pub const SEC_OP_IMMD_DATA_METHOD: u32 = 4;
pub const SEC_OP_ONE_INC: u32 = 5;
pub const SEC_OP_END_PB_SEGMENT: u32 = 7;

const fn hdr(op: u32, subc: u32, mthd: u32, count: u32) -> u32 {
    (op << 29) | ((count & 0x1fff) << 16) | ((subc & 7) << 13) | ((mthd >> 2) & 0xfff)
}

/// Возрастающие методы: `count` слов данных идут в `mthd`, `mthd+4`, ...
pub const fn mthd_inc(subc: u32, mthd: u32, count: u32) -> u32 {
    hdr(SEC_OP_INC_METHOD, subc, mthd, count)
}
/// Все `count` слов — в один и тот же метод.
pub const fn mthd_noninc(subc: u32, mthd: u32, count: u32) -> u32 {
    hdr(SEC_OP_NON_INC_METHOD, subc, mthd, count)
}
/// Первое слово в `mthd`, остальные — в `mthd+4`.
pub const fn mthd_1inc(subc: u32, mthd: u32, count: u32) -> u32 {
    hdr(SEC_OP_ONE_INC, subc, mthd, count)
}
/// Метод с 13-битным значением прямо в заголовке.
pub const fn mthd_immd(subc: u32, mthd: u32, data: u32) -> u32 {
    hdr(SEC_OP_IMMD_DATA_METHOD, subc, mthd, data)
}

/// Разобранный заголовок.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Header {
    Inc { subc: u32, mthd: u32, count: u32 },
    NonInc { subc: u32, mthd: u32, count: u32 },
    OneInc { subc: u32, mthd: u32, count: u32 },
    Immd { subc: u32, mthd: u32, data: u32 },
    EndSegment,
    /// GRP0/GRP2 (третичные операции) и запрещённые коды.
    Unsupported(u32),
}

pub const fn decode_header(h: u32) -> Header {
    let subc = (h >> 13) & 7;
    let mthd = (h & 0xfff) << 2;
    let count = (h >> 16) & 0x1fff;
    match h >> 29 {
        SEC_OP_INC_METHOD => Header::Inc { subc, mthd, count },
        SEC_OP_NON_INC_METHOD => Header::NonInc { subc, mthd, count },
        SEC_OP_ONE_INC => Header::OneInc { subc, mthd, count },
        SEC_OP_IMMD_DATA_METHOD => Header::Immd { subc, mthd, data: count },
        SEC_OP_END_PB_SEGMENT => Header::EndSegment,
        // GRP0 с нулевым словом — NOP-заполнитель; остальное не поддержано.
        SEC_OP_GRP0_USE_TERT if h == 0 => Header::Inc { subc: 0, mthd: 0, count: 0 },
        _ => Header::Unsupported(h),
    }
}

/// Методы класса канала (AMPERE_CHANNEL_GPFIFO_A, C56F) — любой подканал,
/// метод < 0x100.
pub mod host {
    pub const SET_OBJECT: u32 = 0x00;
    pub const NOP: u32 = 0x08;
    pub const SEMAPHOREA: u32 = 0x10;
    pub const SEMAPHOREB: u32 = 0x14;
    pub const SEMAPHOREC: u32 = 0x18;
    pub const SEMAPHORED: u32 = 0x1c;
    pub const NON_STALL_INTERRUPT: u32 = 0x20;
    pub const MEM_OP_A: u32 = 0x28;
    pub const MEM_OP_B: u32 = 0x2c;
    pub const MEM_OP_C: u32 = 0x30;
    pub const MEM_OP_D: u32 = 0x34;
    pub const SET_REFERENCE: u32 = 0x50;
    pub const SEM_ADDR_LO: u32 = 0x5c;
    pub const SEM_ADDR_HI: u32 = 0x60;
    pub const SEM_PAYLOAD_LO: u32 = 0x64;
    pub const SEM_PAYLOAD_HI: u32 = 0x68;
    pub const SEM_EXECUTE: u32 = 0x6c;
    pub const WFI: u32 = 0x78;
    /// Граница методов канала: всё ниже — хосту, выше — движку подканала.
    pub const LIMIT: u32 = 0x100;

    // SEMAPHORED
    pub const SEMD_OP_MASK: u32 = 0x1f;
    pub const SEMD_OP_ACQUIRE: u32 = 0x1;
    pub const SEMD_OP_RELEASE: u32 = 0x2;
    pub const SEMD_OP_ACQ_GEQ: u32 = 0x4;
    pub const SEMD_OP_ACQ_AND: u32 = 0x8;
    pub const SEMD_OP_REDUCTION: u32 = 0x10;
    /// RELEASE_WFI: 0 = EN (так у NVIDIA), 1 = DIS.
    pub const SEMD_RELEASE_WFI_DIS: u32 = 1 << 20;
    /// RELEASE_SIZE: 0 = 16 Б (значение, 0, время), 1 = 4 Б.
    pub const SEMD_RELEASE_SIZE_4BYTE: u32 = 1 << 24;

    // SEM_EXECUTE
    pub const SEMX_OP_MASK: u32 = 0x7;
    pub const SEMX_OP_ACQUIRE: u32 = 0;
    pub const SEMX_OP_RELEASE: u32 = 1;
    pub const SEMX_OP_ACQ_STRICT_GEQ: u32 = 2;
    pub const SEMX_OP_ACQ_CIRC_GEQ: u32 = 3;
    pub const SEMX_OP_ACQ_AND: u32 = 4;
    pub const SEMX_OP_ACQ_NOR: u32 = 5;
    pub const SEMX_OP_REDUCTION: u32 = 6;
    pub const SEMX_PAYLOAD_64BIT: u32 = 1 << 24;
    pub const SEMX_RELEASE_TIMESTAMP: u32 = 1 << 25;
    pub const SEMX_REDUCTION_SHIFT: u32 = 27;
}

/// Методы копировального движка AMPERE_DMA_COPY_A/B (C6B5/C7B5).
pub mod ce {
    pub const NOP: u32 = 0x100;
    pub const SET_SEMAPHORE_A: u32 = 0x240;
    pub const SET_SEMAPHORE_B: u32 = 0x244;
    pub const SET_SEMAPHORE_PAYLOAD: u32 = 0x248;
    pub const SET_SEMAPHORE_PAYLOAD_UPPER: u32 = 0x24c;
    pub const SET_SRC_PHYS_MODE: u32 = 0x260;
    pub const SET_DST_PHYS_MODE: u32 = 0x264;
    pub const LAUNCH_DMA: u32 = 0x300;
    pub const OFFSET_IN_UPPER: u32 = 0x400;
    pub const OFFSET_IN_LOWER: u32 = 0x404;
    pub const OFFSET_OUT_UPPER: u32 = 0x408;
    pub const OFFSET_OUT_LOWER: u32 = 0x40c;
    pub const PITCH_IN: u32 = 0x410;
    pub const PITCH_OUT: u32 = 0x414;
    pub const LINE_LENGTH_IN: u32 = 0x418;
    pub const LINE_COUNT: u32 = 0x41c;
    pub const SET_REMAP_CONST_A: u32 = 0x700;
    pub const SET_REMAP_CONST_B: u32 = 0x704;
    pub const SET_REMAP_COMPONENTS: u32 = 0x708;

    // LAUNCH_DMA
    pub const DMA_TRANSFER_MASK: u32 = 0x3;
    pub const DMA_TRANSFER_NONE: u32 = 0;
    pub const DMA_TRANSFER_PIPELINED: u32 = 1;
    pub const DMA_TRANSFER_NON_PIPELINED: u32 = 2;
    pub const DMA_FLUSH_ENABLE: u32 = 1 << 2;
    pub const DMA_SEMAPHORE_SHIFT: u32 = 3;
    pub const DMA_SEMAPHORE_NONE: u32 = 0;
    pub const DMA_SEMAPHORE_ONE_WORD: u32 = 1;
    pub const DMA_SEMAPHORE_FOUR_WORD: u32 = 2;
    pub const DMA_SRC_PITCH: u32 = 1 << 7;
    pub const DMA_DST_PITCH: u32 = 1 << 8;
    pub const DMA_MULTI_LINE: u32 = 1 << 9;
    pub const DMA_REMAP: u32 = 1 << 10;
    pub const DMA_SRC_PHYSICAL: u32 = 1 << 12;
    pub const DMA_DST_PHYSICAL: u32 = 1 << 13;
    pub const DMA_SEMAPHORE_REDUCTION_SHIFT: u32 = 14;
    pub const DMA_SEMAPHORE_REDUCTION_ENABLE: u32 = 1 << 19;
    pub const DMA_SEMAPHORE_PAYLOAD_64BIT: u32 = 1 << 27;

    // SET_*_PHYS_MODE target
    pub const PHYS_LOCAL_FB: u32 = 0;
    pub const PHYS_COHERENT_SYSMEM: u32 = 1;
    pub const PHYS_NONCOHERENT_SYSMEM: u32 = 2;

    // SET_REMAP_COMPONENTS
    pub const REMAP_SRC_X: u32 = 0;
    pub const REMAP_CONST_A: u32 = 4;
    pub const REMAP_CONST_B: u32 = 5;
    pub const REMAP_NO_WRITE: u32 = 6;
    pub const REMAP_COMPONENT_SIZE_SHIFT: u32 = 16;
    pub const REMAP_NUM_SRC_SHIFT: u32 = 20;
    pub const REMAP_NUM_DST_SHIFT: u32 = 24;
}

/// Сегмент забора, который ядро дописывает после пушей EXEC
/// (`nvc0_fence_emit32`): SEMAPHOREA..D + NON_STALL_INTERRUPT одним
/// возрастающим заголовком, 6 слов. Выпуск 16 Б с WFI.
pub const FENCE_DWORDS: usize = 6;

pub const fn fence_release(sem_va: u64, seq: u32) -> [u32; FENCE_DWORDS] {
    [
        mthd_inc(0, host::SEMAPHOREA, 5),
        (sem_va >> 32) as u32 & 0xff,
        sem_va as u32,
        seq,
        host::SEMD_OP_RELEASE, // RELEASE_WFI_EN = 0, RELEASE_SIZE_16BYTE = 0
        0,
    ]
}

/// Расширить 32-битное значение семафора до 64-битного номера забора,
/// зная последний выданный номер `last` (значение не может обогнать его и
/// отстать больше чем на 2^31).
pub const fn widen_seq(last: u64, sem: u32) -> u64 {
    let behind = (last as u32).wrapping_sub(sem) as u64;
    last.saturating_sub(behind)
}

/// Сборщик пушбуфера в памяти (для проб, самопроверок и тестов).
#[derive(Clone, Debug, Default)]
pub struct PushBuilder {
    pub words: alloc::vec::Vec<u32>,
}

impl PushBuilder {
    pub fn new() -> PushBuilder {
        PushBuilder::default()
    }

    /// Возрастающая пачка методов с данными.
    pub fn inc(&mut self, subc: u32, mthd: u32, data: &[u32]) -> &mut Self {
        self.words.push(mthd_inc(subc, mthd, data.len() as u32));
        self.words.extend_from_slice(data);
        self
    }

    pub fn immd(&mut self, subc: u32, mthd: u32, data: u32) -> &mut Self {
        self.words.push(mthd_immd(subc, mthd, data));
        self
    }

    /// Привязать класс к подканалу.
    pub fn set_object(&mut self, subc: u32, class: u32) -> &mut Self {
        self.inc(subc, host::SET_OBJECT, &[class])
    }

    /// Линейное копирование `len` байт копировальным движком (подканал
    /// `subc` уже привязан к C7B5), с выпуском семафора `sem` = `payload`
    /// (один 32-битный, без времени), если задан.
    pub fn ce_copy(&mut self, subc: u32, src: u64, dst: u64, len: u32, sem: Option<(u64, u32)>) -> &mut Self {
        self.inc(subc, ce::OFFSET_IN_UPPER, &[(src >> 32) as u32, src as u32, (dst >> 32) as u32, dst as u32]);
        self.inc(subc, ce::LINE_LENGTH_IN, &[len, 1]);
        let mut launch = ce::DMA_TRANSFER_NON_PIPELINED | ce::DMA_FLUSH_ENABLE | ce::DMA_SRC_PITCH | ce::DMA_DST_PITCH;
        if let Some((va, payload)) = sem {
            self.inc(subc, ce::SET_SEMAPHORE_A, &[(va >> 32) as u32, va as u32, payload]);
            launch |= ce::DMA_SEMAPHORE_ONE_WORD << ce::DMA_SEMAPHORE_SHIFT;
        }
        self.inc(subc, ce::LAUNCH_DMA, &[launch])
    }

    /// Заполнить `len` байт (кратно 4) 32-битным значением через REMAP
    /// (так делает NVK для `vkCmdFillBuffer`).
    pub fn ce_fill(&mut self, subc: u32, dst: u64, len: u32, value: u32) -> &mut Self {
        self.inc(subc, ce::SET_REMAP_CONST_A, &[value]);
        // DST_X = CONST_A, COMPONENT_SIZE = 4, NUM_SRC = 1, NUM_DST = 1.
        self.inc(subc, ce::SET_REMAP_COMPONENTS, &[ce::REMAP_CONST_A | (3 << ce::REMAP_COMPONENT_SIZE_SHIFT) | (6 << 4) | (6 << 8) | (6 << 12)]);
        self.inc(subc, ce::OFFSET_OUT_UPPER, &[(dst >> 32) as u32, dst as u32]);
        self.inc(subc, ce::LINE_LENGTH_IN, &[len / 4, 1]);
        self.inc(subc, ce::LAUNCH_DMA, &[ce::DMA_TRANSFER_NON_PIPELINED | ce::DMA_FLUSH_ENABLE | ce::DMA_SRC_PITCH | ce::DMA_DST_PITCH | ce::DMA_REMAP])
    }

    /// Выпуск семафора хостом (как забор ядра, но произвольный адрес).
    pub fn host_release(&mut self, va: u64, payload: u32) -> &mut Self {
        self.inc(0, host::SEMAPHOREA, &[(va >> 32) as u32 & 0xff, va as u32, payload, host::SEMD_OP_RELEASE | host::SEMD_RELEASE_SIZE_4BYTE])
    }

    pub fn bytes(&self) -> alloc::vec::Vec<u8> {
        self.words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    pub fn len_bytes(&self) -> u32 {
        self.words.len() as u32 * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gp_entry_packing() {
        // 0x1_2345_6780, 0x100 байт = 0x40 двойных слов -> LENGTH 0x40 << 10.
        let e = gp_entry(0x1_2345_6780, 0x100, false);
        assert_eq!(e as u32, 0x2345_6780);
        assert_eq!((e >> 32) as u32, 0x0001_0001);
        let d = gp_entry_decode(e);
        assert_eq!(d, GpEntry { va: 0x1_2345_6780, len_bytes: 0x100, no_prefetch: false, subroutine: false });
        let e = gp_entry(0x80_0000_0000, 24, true);
        assert_eq!((e >> 32) as u32, 0x8000_1880);
        assert!(gp_entry_decode(e).no_prefetch);
        assert_eq!(gp_entry_decode(e).len_bytes, 24);
        assert_eq!(gp_entry_decode(gp_entry(0, MAX_SEGMENT_BYTES, false)).len_bytes, MAX_SEGMENT_BYTES);
    }

    #[test]
    fn ring_accounting() {
        let mut r = Ring::default();
        assert_eq!(r.free(0), IB_MAX);
        for _ in 0..IB_MAX {
            r.advance();
        }
        assert_eq!(r.put, IB_MAX);
        assert_eq!(r.free(0), 0, "полное кольцо");
        assert_eq!(r.free(5), 5);
        assert_eq!(r.advance(), IB_MAX);
        assert_eq!(r.put, 0, "переход через конец");
        assert_eq!(Ring::offset(3), 24);
    }

    #[test]
    fn headers() {
        // Mesa: NVC0_FIFO_PKHDR_SQ(subc 1, 0x0400, 4) = 0x20042100.
        assert_eq!(mthd_inc(1, 0x400, 4), 0x2004_2100);
        assert_eq!(decode_header(0x2004_2100), Header::Inc { subc: 1, mthd: 0x400, count: 4 });
        assert_eq!(decode_header(mthd_noninc(2, 0x1b0, 7)), Header::NonInc { subc: 2, mthd: 0x1b0, count: 7 });
        assert_eq!(decode_header(mthd_1inc(7, 0xffc, 2)), Header::OneInc { subc: 7, mthd: 0xffc, count: 2 });
        assert_eq!(decode_header(mthd_immd(3, 0x300, 0x1abc)), Header::Immd { subc: 3, mthd: 0x300, data: 0x1abc });
        assert_eq!(decode_header(0xe000_0000), Header::EndSegment);
        assert!(matches!(decode_header(0x4000_0001), Header::Unsupported(_)));
    }

    #[test]
    fn fence_segment() {
        let f = fence_release(0x80_0001_0000, 7);
        assert_eq!(f[0], 0x2005_0004, "SEMAPHOREA, 5 слов, подканал 0");
        assert_eq!(f[1], 0x80);
        assert_eq!(f[2], 0x0001_0000);
        assert_eq!(f[3], 7);
        assert_eq!(f[4], 2);
        assert_eq!(f[5], 0);
    }

    #[test]
    fn seq_widening() {
        assert_eq!(widen_seq(10, 10), 10);
        assert_eq!(widen_seq(10, 7), 7);
        assert_eq!(widen_seq(0x1_0000_0002, 0xffff_ffff), 0xffff_ffff);
        assert_eq!(widen_seq(0x1_0000_0002, 2), 0x1_0000_0002);
        assert_eq!(widen_seq(3, 0), 0);
    }

    #[test]
    fn doorbell() {
        assert_eq!(DOORBELL, 0x00bb_0090);
        assert_eq!(PTIMER_LO, 0x00bb_0080);
        assert_eq!(doorbell_token(1, 8), 0x1_0008);
    }
}
