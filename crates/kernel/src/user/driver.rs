// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Драйвер — программа: ядро отдаёт ей устройство PCI (веха «драйверы по
//! VID:PID», часть Д1).
//!
//! # Зачем программа, а не модуль ядра
//!
//! Упавшая программа снимается, как любая программа; упавший модуль ронял бы
//! машину. Договор с программой — системные вызовы (`docs/ABI.md`), договор с
//! модулем был бы внутренностями ядра. Цена — четыре вызова:
//! [`user_abi::SYS_DEVICE_OPEN`], [`user_abi::SYS_DEVICE_MAP`],
//! [`user_abi::SYS_DEVICE_WAIT`], [`user_abi::SYS_DMA_ALLOC`].
//!
//! # Чего это не защищает, и это надо знать
//!
//! IOMMU ядро не настраивает. Устройство, которому дали управление шиной,
//! пишет по любому физическому адресу, какой ему сообщат, — то есть программа
//! с правом `devices` равна ядру по силе. Поэтому право выдаётся только
//! подписанному пакету (часть Д2), а здесь проверяется лишь его наличие.
//!
//! # Прерывания: места, а не векторы
//!
//! Векторы MSI ядро выдаёт навсегда — освобождать их оно не умеет, и учить его
//! незачем. Драйверов-программ немного, поэтому у каждого из [`SLOTS`] мест
//! свой вектор, выделенный при первом использовании и оставшийся за местом.
//! Программа, занявшая место, получает его вектор; следующая — тот же. Иначе
//! драйвер, запущенный десять раз, унёс бы десять векторов из восьми.
//!
//! # Уход
//!
//! Всё, что программа взяла, отдаётся в [`Program::release_devices`] — на пути
//! выхода, в том числе снятой за отказ, **до** разбора пространства. Порядок
//! там не случаен: сначала устройству выключается управление шиной и MSI,
//! потом снимаются отображения, и только потом память для DMA возвращается в
//! окно ядра. Наоборот — и устройство успело бы записать в память, отданную
//! уже другому.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use alloc::vec::Vec;

use user_abi::{MAX_DEVICES, MAX_DMA_BYTES};

use super::{MmapError, Mapping, Program, Source};
use crate::mm::{PAGE_SIZE, PageFlags, VirtAddr};
use crate::{irq, kprintln, pci, sched};

/// Сколько драйверов-программ может работать одновременно.
pub const SLOTS: usize = 4;

/// Кто занимает место: номер задачи плюс один; ноль — свободно.
static SLOT_OWNER: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
/// Адрес и данные MSI места — ноль, пока вектор ещё не выделен.
static SLOT_MSI_ADDRESS: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static SLOT_MSI_DATA: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
/// Сколько прерываний пришло с прошлого вопроса программы.
static SLOT_PENDING: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];

/// Обработчики мест: у прерывания нет аргумента, а место знать надо.
static TRAMPOLINES: [fn(); SLOTS] = [|| fire(0), || fire(1), || fire(2), || fire(3)];

/// Прерывание места `slot`: отметить и разбудить. Снимать признак у
/// устройства — дело драйвера: ядро не знает его регистров. У MSI это и не
/// нужно — сообщение не уровень, и повторяться само оно не будет.
fn fire(slot: usize) {
    SLOT_PENDING[slot].fetch_add(1, Ordering::AcqRel);
    sched::wake_irq(source(slot));
}

/// Источник прерывания места — для планировщика и для команды `irq`.
const fn source(slot: usize) -> u32 {
    irq::source::DRIVER_FIRST + slot as u32
}

/// Устройство, взятое программой.
pub struct Held {
    device: pci::Device,
    slot: usize,
    msi_on: bool,
}

/// Почему не вышло.
#[derive(Debug, Clone, Copy)]
pub enum DriverError {
    NotFound,
    Taken,
    Limit,
    NoInterrupts,
    BadHandle,
    NoMemory,
}

impl Program {
    /// Взять устройство `vendor:device` (которое `index`-е по счёту).
    pub fn open_device(&mut self, vendor: u16, device: u16, index: usize) -> Result<usize, DriverError> {
        let handle = self.devices.iter().position(Option::is_none).ok_or(DriverError::Limit)?;

        let rsdp = crate::acpi::rsdp();
        // SAFETY: ядро давно на своих таблицах; `discover` читает таблицы
        // прошивки или мост из дерева — то же, что делает перепись устройств.
        let root = unsafe { pci::Root::discover(rsdp) }.map_err(|_| DriverError::NotFound)?;
        let mut seen = 0usize;
        let mut found = None;
        // SAFETY: см. выше.
        unsafe {
            pci::for_each(&root, |candidate| {
                if candidate.vendor == vendor && candidate.device == device {
                    if seen == index {
                        found = Some(*candidate);
                        return false;
                    }
                    seen += 1;
                }
                true
            });
        }
        let found = found.ok_or(DriverError::NotFound)?;
        if crate::devices::claimed_by(found.address).is_some() {
            return Err(DriverError::Taken);
        }

        let owner = sched::current().as_u32() + 1;
        let slot = SLOT_OWNER
            .iter()
            .position(|taken| taken.compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire).is_ok())
            .ok_or(DriverError::Limit)?;
        SLOT_PENDING[slot].store(0, Ordering::Release);
        crate::devices::claim(found.address, "a driver program");

        // SAFETY: устройство теперь наше; кольца и буферы программа построит
        // сама, а до того, как она сообщит их адреса, устройство не знает, куда
        // писать, — как и любое устройство сразу после включения.
        unsafe { found.enable_bus_master() };
        kprintln!(
            "  driver      : {} {:04x}:{:04x} given to {} (place {slot})",
            found.address,
            vendor,
            device,
            sched::current()
        );
        self.devices[handle] = Some(Held { device: found, slot, msi_on: false });
        // MSI — сразу, а не при первом ожидании. Первая версия включала его в
        // `SYS_DEVICE_WAIT`, и карта `edu` успевала досчитать раньше: её
        // прерывание уходило по линии INTx, которой никто не слушал, и
        // программа ждала вечно. Пришедшее раньше вопроса копится в счётчике
        // места. Устройство без MSI это не останавливает — ответ «прерываний
        // нет» даст ожидание.
        match self.arm_device(handle) {
            Ok(slot) => kprintln!(
                "  driver      : MSI -> {:#x} data {:#x}, interrupts arrive at place {slot}",
                SLOT_MSI_ADDRESS[slot].load(Ordering::Acquire),
                SLOT_MSI_DATA[slot].load(Ordering::Acquire)
            ),
            Err(err) => kprintln!("  driver      : no interrupts for this device ({err:?})"),
        }
        Ok(handle)
    }

    /// Отобразить BAR `bar` устройства `handle`; вернуть адрес.
    pub fn map_device(&mut self, handle: usize, bar: usize) -> Result<usize, DriverError> {
        let held = self.devices.get(handle).and_then(Option::as_ref).ok_or(DriverError::BadHandle)?;
        let phys = held.device.memory_bar(bar).ok_or(DriverError::NotFound)?;
        let size = held.device.memory_bar_size(bar).ok_or(DriverError::NotFound)?;
        // Больше шестнадцати мегабайт окна регистров у наших устройств не
        // бывает; память видеокарты — не наш случай.
        if size > 16 * 1024 * 1024 || phys.as_u64() % PAGE_SIZE as u64 != 0 {
            return Err(DriverError::NoMemory);
        }
        let pages = (size as usize).div_ceil(PAGE_SIZE);
        let base = self.reserve_device(pages)?;
        // SAFETY: адрес — BAR устройства, которое принадлежит программе; кадров
        // пула под ним нет, и разбор их туда не вернёт (область снимается
        // `unmap_range_keep`, см. `release_devices`).
        let mapped = unsafe {
            self.space.map_frames(
                VirtAddr::new(base),
                phys,
                pages,
                PageFlags::READ | PageFlags::WRITE | PageFlags::USER | PageFlags::DEVICE,
            )
        };
        mapped.map_err(|_| DriverError::NoMemory)?;
        self.remember(Mapping { base, pages, blocks: 0, source: Source::Device });
        Ok(base)
    }

    /// Ждать прерывания устройства `handle` не дольше `ms`.
    pub fn arm_device(&mut self, handle: usize) -> Result<usize, DriverError> {
        let held = self.devices.get_mut(handle).and_then(Option::as_mut).ok_or(DriverError::BadHandle)?;
        let slot = held.slot;
        if !held.msi_on {
            let msi = held.device.msi().ok_or(DriverError::NoInterrupts)?;
            if SLOT_MSI_ADDRESS[slot].load(Ordering::Acquire) == 0 {
                let (address, data) =
                    crate::arch::interrupts::alloc_msi(TRAMPOLINES[slot]).ok_or(DriverError::NoInterrupts)?;
                SLOT_MSI_DATA[slot].store(data, Ordering::Release);
                SLOT_MSI_ADDRESS[slot].store(address, Ordering::Release);
            }
            let address = SLOT_MSI_ADDRESS[slot].load(Ordering::Acquire);
            let data = SLOT_MSI_DATA[slot].load(Ordering::Acquire);
            // SAFETY: обработчик места стоит с первого выделения вектора;
            // устройство принадлежит программе.
            unsafe { held.device.set_msi_vector(&msi, address, data) };
            held.msi_on = true;
        }
        Ok(slot)
    }

    /// Выделить память для DMA; вернуть адрес в программе и физический.
    pub fn dma_alloc(&mut self, len: usize) -> Result<(usize, u64), DriverError> {
        if self.devices.iter().all(Option::is_none) {
            // Без устройства память для DMA незачем: отдавать её некому.
            return Err(DriverError::BadHandle);
        }
        let used: usize = self.dma.iter().map(crate::mm::dma::DmaBuffer::len).sum();
        let len = len.next_multiple_of(PAGE_SIZE);
        if len == 0 || used + len > MAX_DMA_BYTES {
            return Err(DriverError::Limit);
        }
        let buffer = crate::mm::dma::alloc(len).map_err(|_| DriverError::NoMemory)?;
        let pages = len / PAGE_SIZE;
        let base = match self.reserve_device(pages) {
            Ok(base) => base,
            Err(err) => {
                // SAFETY: буфер ещё никому не сообщён.
                unsafe { crate::mm::dma::free(&buffer) };
                return Err(err);
            }
        };
        // SAFETY: кадры окна DMA принадлежат буферу, пока он не отдан в
        // `release_devices`; атрибут тот же, что у отображения ядра, — второго
        // псевдонима с другим кешированием не появляется.
        let mapped = unsafe {
            self.space.map_frames(
                VirtAddr::new(base),
                buffer.phys(),
                pages,
                PageFlags::READ | PageFlags::WRITE | PageFlags::USER | PageFlags::DMA,
            )
        };
        if mapped.is_err() {
            // SAFETY: см. выше.
            unsafe { crate::mm::dma::free(&buffer) };
            return Err(DriverError::NoMemory);
        }
        let phys = buffer.phys().as_u64();
        self.remember(Mapping { base, pages, blocks: 0, source: Source::Device });
        self.dma.push(buffer);
        Ok((base, phys))
    }

    /// Место в области памяти по запросу под `pages` страниц — без кадров.
    fn reserve_device(&self, pages: usize) -> Result<usize, DriverError> {
        let bytes = pages.checked_mul(PAGE_SIZE).ok_or(DriverError::NoMemory)?;
        self.reserve(bytes, PAGE_SIZE).map(|(base, _)| base).map_err(|err| match err {
            MmapError::Limit => DriverError::Limit,
            _ => DriverError::NoMemory,
        })
    }

    /// Отдать всё, что программа взяла как драйвер. См. заголовок модуля.
    pub fn release_devices(&mut self) {
        let mut released = 0usize;
        for held in self.devices.iter_mut().filter_map(Option::take) {
            // SAFETY: устройство принадлежало программе, и она кончилась.
            unsafe { held.device.quiesce() };
            crate::devices::unclaim(held.device.address);
            SLOT_PENDING[held.slot].store(0, Ordering::Release);
            SLOT_OWNER[held.slot].store(0, Ordering::Release);
            kprintln!("  driver      : {} is free again", held.device.address);
            released += 1;
        }
        if released == 0 && self.dma.is_empty() {
            return;
        }
        let regions: Vec<(usize, usize)> = self
            .mappings
            .iter()
            .filter(|region| matches!(region.source, Source::Device))
            .map(|region| (region.base, region.pages))
            .collect();
        for (base, pages) in regions {
            // SAFETY: области `Device` отображены `map_frames` на чужие кадры
            // (окно регистров или окно DMA); в пул они не уезжают.
            unsafe { self.space.unmap_range_keep(VirtAddr::new(base), pages) };
        }
        self.mappings.retain(|region| !matches!(region.source, Source::Device));
        for buffer in self.dma.drain(..) {
            // SAFETY: устройству выключено управление шиной выше, отображение
            // в программе снято — буфер больше не видит никто.
            unsafe { crate::mm::dma::free(&buffer) };
        }
    }
}

/// Ждать прерывания места `slot` не дольше `ms`; вернуть, сколько пришло.
///
/// Вне замка программы: спать, держа его, значило бы остановить её потоки.
pub fn wait(slot: usize, ms: u64) -> u64 {
    let pending = SLOT_PENDING[slot].swap(0, Ordering::AcqRel);
    if pending > 0 {
        return pending;
    }
    sched::block_on_irq_until(source(slot), ms, || SLOT_PENDING[slot].load(Ordering::Acquire) > 0);
    SLOT_PENDING[slot].swap(0, Ordering::AcqRel)
}

/// Сколько устройств может держать программа — сверка с договором.
const _: () = assert!(MAX_DEVICES == 2);
