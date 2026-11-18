//! NVK поверх GSP-RM (18.11.2026, WP-A): адресное пространство GPU.
//!
//! Как в Linux nouveau на GSP (`mmu/r535.c` `r535_mmu_promote_vmm`,
//! `vmmgp100.c`, `vmmgv100.c`, `vmmtu102.c`): таблицы страниц пишет
//! процессор, RM о них знает только корень и верхние уровни 512-МиБ
//! «серверного» куска (`COPY_SERVER_RESERVED_PDES`), куда он кладёт свои
//! отображения.
//!
//! # Формат (GP100+, 47 бит VA, только страницы 4 КиБ — как uvmm у nouveau)
//!
//! | уровень | биты VA | записей × размер |
//! |---------|---------|------------------|
//! | PD3 | 47..48 | 4 × 8 (0x20 B, под неё страница) |
//! | PD2 | 38..46 | 512 × 8 |
//! | PD1 | 29..37 | 512 × 8 |
//! | PD0 | 21..28 | 256 × 16: `data[0]` — большие страницы (у нас 0), `data[1]` — PT 4 КиБ |
//! | PT | 12..20 | 512 × 8 |
//!
//! PDE: `(phys >> 4) | aperture << 1` (видеопамять = 1). PTE: `VALID |
//! aperture << 1 (VRAM 0, SYS_COH 2) | VOL << 3 | PRIV << 5 | RO << 6 |
//! (phys >> 4) | kind << 56`; разреженная — только `VOL` без `VALID`.
//! Поправка ведущего к спецификации 2.5 (18.11): указатель на PT 4 КиБ в
//! записи PD0 — `data[1]` (байты 8..15).
//!
//! Кодирование и обход — `nvgpu::gmmu` (WP-B, проверен на хосте); здесь
//! только память таблиц ([`VramPt`]: CPU-арена через окно BAR1) и RM.
//!
//! # Где что в VA (NVK: `VM_INIT` даёт kernel-managed `[1<<39, 1<<40)`)
//!
//! * `[4096, 1<<38)` и `[1<<38, 1<<39)` — пользователь (PD2[0], PD2[1]);
//! * `[1<<39, 1<<39 + 512 МиБ)` — резерв RM (PD2[2]);
//! * ядро (pushbuf, GPFIFO, заборы, буферы контекста GR) — с `kernel_va + 1<<38`,
//!   то есть под PD2[3]: если RM перепишет PD2[2] своей таблицей, наши
//!   отображения это не заденет.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::nvidia_gsp::{Log, Res, Rm, hex_head};
use super::nvidia_rm_info::say;
use super::nvidia_rm_vram as vram;
use crate::sync::SpinLock;

pub(crate) const PAGE: u64 = 0x1000;
/// Размер резерва RM (nouveau: `0x20000000`, страница 0x1d).
pub(crate) const RSVD_SIZE: u64 = 0x2000_0000;

pub(crate) const H_DEVICE: u32 = 0xde1d_0000;
pub(crate) const H_SUBDEVICE: u32 = 0x5d1d_0000;
pub(crate) const H_VASPACE: u32 = 0x90f1_0000;

/// Клиенты RM, созданные NVK (снимаются при выгрузке GSP-RM).
pub(crate) static LIVE_CLIENTS: SpinLock<Vec<u32>> = SpinLock::new(Vec::new());

// --- память таблиц -------------------------------------------------------------

use nvgpu::gmmu::{self, Gmmu, PteAttrs, Target};

/// Таблицы страниц в CPU-арене видеопамяти (пишутся через окно BAR1).
pub(crate) struct VramPt;

impl gmmu::PtMem for VramPt {
    fn read64(&self, addr: u64) -> u64 {
        vram::r64(addr)
    }
    fn write64(&mut self, addr: u64, v: u64) {
        vram::w64(addr, v);
    }
    fn alloc_table(&mut self) -> Option<u64> {
        vram::cpu_alloc(PAGE, PAGE).ok()
    }
    fn free_table(&mut self, addr: u64) {
        vram::free(addr, PAGE);
    }
}

/// Где лежит отображаемая страница.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Aper {
    Vram,
    /// Память машины, согласованная (SYS_COH + VOL, как у nouveau uvmm).
    Sys,
}

fn attrs(aper: Aper, priv_: bool, ro: bool, kind: u8) -> PteAttrs {
    let mut a = PteAttrs::user(if aper == Aper::Vram { Target::Vram } else { Target::SysCoh }, kind);
    a.privileged = priv_;
    a.ro = ro;
    a
}

fn e2s(e: nvgpu::Errno) -> String {
    format!("gmmu errno {}", e.0)
}

/// Адресное пространство GPU: объекты RM и таблицы в CPU-арене.
pub(crate) struct Vmm {
    pub(crate) id: u32,
    pub(crate) client: u32,
    pub(crate) pd3: u64,
    gmmu: Option<Gmmu>,
    /// Kernel-managed диапазон VM_INIT: `[start, start+size)`.
    pub(crate) kernel: (u64, u64),
    /// Резерв RM `[lo, hi)`.
    pub(crate) rsvd: (u64, u64),
    kva_next: u64,
    /// Объекты RM созданы (клиент надо снимать).
    rm_alive: bool,
}

/// Номер клиента RM для `id`: `0xc1d0_0010 + id` (дисплей — `0xc1d0_0001`).
pub(crate) fn client_for(id: u32) -> u32 {
    0xc1d0_0010 + (id & 0xffff)
}

impl Vmm {
    /// client/device/subdevice/vaspace, PD3, резерв RM и
    /// COPY_SERVER_RESERVED_PDES. Ошибка — всё созданное снимается.
    pub(crate) fn create(rm: &mut Rm, id: u32, kernel: (u64, u64), log: &mut Log) -> Res<Self> {
        let client = client_for(id);
        let gmmu = Gmmu::new(&mut VramPt).map_err(e2s)?;
        let pd3 = gmmu.pd3();
        let mut vmm = Self { id, client, pd3, gmmu: Some(gmmu), kernel, rsvd: (0, 0), kva_next: 0, rm_alive: false };
        match vmm.create_rm(rm, log) {
            Ok(()) => Ok(vmm),
            Err(why) => {
                vmm.destroy(rm, log);
                Err(why)
            }
        }
    }

    fn create_rm(&mut self, rm: &mut Rm, log: &mut Log) -> Res<()> {
        let client = self.client;
        // 1–3. Клиент, устройство, подустройство — как `probe_display`.
        let mut root = alloc::vec![0u8; 0x6c];
        root[0..4].copy_from_slice(&client.to_le_bytes());
        root[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        root[8..14].copy_from_slice(b"nvgpu\0");
        rm.alloc_in(client, client, client, 0x0000, &root)?;
        self.rm_alive = true;
        LIVE_CLIENTS.lock().push(client);
        let mut device = alloc::vec![0u8; 0x38];
        device[4..8].copy_from_slice(&client.to_le_bytes());
        rm.alloc_in(client, client, H_DEVICE, 0x0080, &device)?;
        rm.alloc_in(client, H_DEVICE, H_SUBDEVICE, 0x2080, &[0u8; 4])?;
        // 4. FERMI_VASPACE_A, index 0 (GPU_NEW), остальное 0.
        rm.alloc_in(client, H_DEVICE, H_VASPACE, 0x90f1, &[0u8; 0x30])?;
        say(log, format!("nvgpu: client {client:#x} dev/subdev/vaspace ok (vmm {})", self.id));

        // 5. Резерв RM в начале kernel-managed: 512 МиБ, выравнивание 4 ГиБ
        // (nouveau: `nvkm_vmm_get_locked(..., 0x1d, 32, 0x20000000)`).
        let lo = self.kernel.0.div_ceil(1 << 32) << 32;
        self.rsvd = (lo, lo + RSVD_SIZE);
        // Наше — под следующей записью PD2 (см. заголовок модуля).
        self.kva_next = (lo + (1 << 38)) & !((1 << 38) - 1);
        if self.kva_next + (1 << 30) > self.kernel.0 + self.kernel.1 {
            return Err(format!("kernel VA {:#x}+{:#x} too small for the RM reserve and kernel maps", self.kernel.0, self.kernel.1));
        }
        // Таблицы до PD1 для резерва (nouveau берёт ссылку до уровня 0x1d).
        let path = self.gmmu.as_mut().ok_or("no tables")?.ensure_path(&mut VramPt, lo, gmmu::PD1).map_err(e2s)?;
        let (pd2, pd1) = (path[1], path[2]);
        let pd2_index = (lo >> 38) & 0x1ff;
        // Три уровня — если у PD2[0] уже есть PD1 (nouveau: `pd->pde[0]->pde[0]`).
        let pd1_of_0 = gmmu::decode_pde(vram::r64(pd2)).map(|(t, _)| t);
        let three = pd1_of_0.is_some();
        let levels = if three { 3u32 } else { 2 };
        vram::flush(self.pd3);
        let before = vram::r64(pd2 + pd2_index * 8);
        let _ = pd1;

        let mut p = alloc::vec![0u8; 0xb8];
        let put64 = |p: &mut [u8], at: usize, v: u64| p[at..at + 8].copy_from_slice(&v.to_le_bytes());
        put64(&mut p, 0x08, RSVD_SIZE);
        put64(&mut p, 0x10, lo);
        put64(&mut p, 0x18, lo + RSVD_SIZE - 1);
        p[0x20..0x24].copy_from_slice(&levels.to_le_bytes());
        let level = |p: &mut [u8], i: usize, phys: u64, size: u64, shift: u8| {
            let at = 0x28 + i * 0x18;
            p[at..at + 8].copy_from_slice(&phys.to_le_bytes());
            p[at + 8..at + 16].copy_from_slice(&size.to_le_bytes());
            p[at + 16..at + 20].copy_from_slice(&1u32.to_le_bytes()); // видеопамять
            p[at + 20] = shift;
        };
        level(&mut p, 0, self.pd3, 0x20, 0x2f);
        level(&mut p, 1, pd2, 0x1000, 0x26);
        if let Some(t) = pd1_of_0 {
            level(&mut p, 2, t, 0x1000, 0x1d);
        }
        let status = if SKIP_PDES.load(core::sync::atomic::Ordering::Relaxed) {
            "skipped (gpu nvktest skip-pdes)".into()
        } else {
            match rm.control(client, H_VASPACE, 0x90f1_0106, &p) {
                Ok(_) => "ok".into(),
                Err(why) => format!("FAILED {why} (params {})", hex_head(&p, 0x58)),
            }
        };
        let after = vram::r64(pd2 + pd2_index * 8);
        say(
            log,
            format!(
                "nvgpu: vmm {} pd3 {:#x} rsvd {lo:#x}..{:#x} copy_reserved_pdes {status} levels {levels} \
                 (PD2[{pd2_index}] before {before:#x} after {after:#x}{}) kernel maps from {:#x}",
                self.id,
                self.pd3,
                lo + RSVD_SIZE,
                if before == after { "" } else { " - RM REWROTE IT" },
                self.kva_next
            ),
        );
        if status.starts_with("FAILED") {
            return Err(format!("COPY_SERVER_RESERVED_PDES {status}"));
        }
        Ok(())
    }

    /// Снять объекты RM (клиент с потомками) и вернуть таблицы.
    pub(crate) fn destroy(&mut self, rm: &mut Rm, log: &mut Log) {
        if self.rm_alive {
            match rm.free_in(self.client, 0, self.client) {
                Ok(()) => say(log, format!("nvgpu: vmm {} client {:#x} freed", self.id, self.client)),
                Err(why) => say(log, format!("nvgpu: vmm {} client {:#x} free - {why}", self.id, self.client)),
            }
            LIVE_CLIENTS.lock().retain(|c| *c != self.client);
            self.rm_alive = false;
        }
        if let Some(g) = self.gmmu.take() {
            g.destroy(&mut VramPt);
        }
        self.pd3 = 0;
    }

    fn check_range(&self, va: u64, len: u64) -> Res<()> {
        if va % PAGE != 0 || len % PAGE != 0 || len == 0 {
            return Err(format!("unaligned range {va:#x}+{len:#x}"));
        }
        let end = va.checked_add(len).ok_or("VA overflow")?;
        if va < self.rsvd.1 && end > self.rsvd.0 {
            return Err(format!("VA {va:#x}+{len:#x} overlaps the RM reserve {:#x}..{:#x}", self.rsvd.0, self.rsvd.1));
        }
        Ok(())
    }

    fn tables(&mut self) -> Res<&mut Gmmu> {
        self.gmmu.as_mut().ok_or_else(|| "address space destroyed".into())
    }

    /// Отобразить кадры памяти машины (по 4 КиБ) с `va`.
    pub(crate) fn map_sys(&mut self, va: u64, frames: &[u64], priv_: bool, ro: bool, kind: u8) -> Res<()> {
        let len = frames.len() as u64 * PAGE;
        self.check_range(va, len)?;
        self.tables()?.map(&mut VramPt, va, len, attrs(Aper::Sys, priv_, ro, kind), |i| frames[i as usize]).map_err(e2s)?;
        vram::flush(self.pd3);
        Ok(())
    }

    /// Отобразить непрерывную видеопамять `phys..phys+len` с `va`.
    pub(crate) fn map_vram(&mut self, va: u64, phys: u64, len: u64, priv_: bool, ro: bool, kind: u8) -> Res<()> {
        self.check_range(va, len)?;
        self.tables()?.map_contig(&mut VramPt, va, len, phys, attrs(Aper::Vram, priv_, ro, kind)).map_err(e2s)?;
        vram::flush(self.pd3);
        Ok(())
    }

    /// Снять отображение (`sparse` — оставить/сделать разреженным).
    pub(crate) fn unmap(&mut self, va: u64, len: u64, sparse: bool) -> Res<()> {
        self.check_range(va, len)?;
        self.tables()?.fill(&mut VramPt, va, len, sparse).map_err(e2s)?;
        vram::flush(self.pd3);
        Ok(())
    }

    /// Взять кусок kernel-managed VA (только растёт; v1).
    pub(crate) fn kva_alloc(&mut self, size: u64, align: u64) -> Res<u64> {
        let align = align.max(PAGE).next_power_of_two();
        let va = self.kva_next.div_ceil(align) * align;
        let end = va + size.div_ceil(PAGE) * PAGE;
        if end > self.kernel.0 + self.kernel.1 {
            return Err(format!("kernel VA exhausted ({va:#x}+{size:#x})"));
        }
        self.kva_next = end;
        Ok(va)
    }

    /// Вписать корень этого пространства в блок экземпляров канала
    /// (`gv100_vmm_join`/`gp100_vmm_join`): PDB c VER2 и 64-КиБ большими
    /// страницами, предел VA, таблица подконтекстов (только 0).
    pub(crate) fn join(&self, inst: u64) {
        gmmu::inst_block_join(|off, v| vram::w32(inst + u64::from(off), v), self.pd3, Target::Vram, gmmu::NVK_VA_LIMIT);
        vram::flush(inst + 0x200);
    }

    /// Сбросить TLB по этому корню (`tu102_vmm_flush`): 0xb830a0 = PDB >> 8,
    /// 0xb830a4 = 0, 0xb830b0 = 0x80000001 (PAGE_ALL), ждать бит 31 (2 с).
    pub(crate) fn flush_tlb(&self, log: &mut Log) -> bool {
        let Ok((_, bar)) = super::nvidia_rm_info::card() else { return false };
        let _guard = TLB.lock();
        for (reg, v) in gmmu::tlb::sequence(self.pd3) {
            bar.wr(reg as usize, v);
        }
        let ok = bar.wait(2_000_000, |b| b.rd(gmmu::tlb::TRIGGER as usize) & gmmu::tlb::PENDING == 0);
        if !ok || TLB_LOGGED.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 16 {
            say(
                log,
                format!("nvgpu: tlb flush pdb {:#x} {} (0xb830b0 {:#x})", self.pd3, if ok { "ok" } else { "TIMEOUT" }, bar.rd(0x00b8_30b0)),
            );
        }
        ok
    }

    /// Как GPU увидит `va` (обход таблиц в видеопамяти) — для журнала.
    pub(crate) fn translate(&self, va: u64) -> String {
        self.gmmu.as_ref().map_or_else(|| "no tables".into(), |g| format!("{:?}", g.translate(&VramPt, va)))
    }

    pub(crate) fn describe(&self) -> String {
        format!(
            "vmm {} client {:#x} pd3 {:#x} tables {} rsvd {:#x}",
            self.id,
            self.client,
            self.pd3,
            self.gmmu.as_ref().map_or(0, Gmmu::table_count),
            self.rsvd.0
        )
    }
}

/// Опыт (спецификация, риск 1): не слать COPY_SERVER_RESERVED_PDES. Ставит
/// `gpu nvktest skip-pdes`.
pub(crate) static SKIP_PDES: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Регистры сброса TLB — один на карту. Замок спящий: ожидание бита до 2 с
/// не должно идти с запрещёнными прерываниями.
static TLB: crate::sync::Mutex<()> = crate::sync::Mutex::new(());
static TLB_LOGGED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
