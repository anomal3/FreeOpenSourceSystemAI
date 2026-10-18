//! VMware SVGA II (фаза 68h): видеокарта VirtualBox (VMSVGA) и VMware.
//!
//! # Что это даёт
//!
//! До этой фазы в VirtualBox стол жил на буфере, который оставила прошивка
//! (1280×720 у `FreeOS-bench`), и разрешение не менялось: «Параметры»
//! честно говорили, что режим задаёт прошивка. VMSVGA — то, что VirtualBox
//! ставит по умолчанию, и то же самое устройство у VMware.
//!
//! # Как устроено
//!
//! - **Регистры** — пара портов в BAR0: номер регистра в `base`, значение в
//!   `base + 1`, двойными словами. Версия интерфейса согласуется записью
//!   `SVGA_ID_2` в `ID` и чтением обратно.
//! - **Режим** — `WIDTH`, `HEIGHT`, `BITS_PER_PIXEL` и `ENABLE`; картинка лежит
//!   в памяти адаптера с `FB_START + FB_OFFSET`, строка — `BYTES_PER_LINE`.
//!   Формат 32 бита — BGRX, тот же, что у буфера прошивки.
//! - **Очередь команд (FIFO)** — память в BAR2: заголовок из четырёх слов
//!   (`MIN`, `MAX`, `NEXT_CMD`, `STOP`) и кольцо команд. В режиме SVGA
//!   адаптер показывает изменённое по команде `UPDATE` — её пишет
//!   [`flush`] на каждый прямоугольник, собранный композитором, как у
//!   virtio-gpu. Место кончилось — `SYNC` и ожидание `BUSY`.
//!
//! # Чего нет
//!
//! Курсора адаптера (стрелку рисует стол), 3D, нескольких экранов,
//! прерываний (очередь не переполняется: команда — пять слов, кольцо —
//! сотни килобайт, и при нехватке драйвер ждёт).

use boot_info::Framebuffer;

use super::DisplayError;
use crate::mm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::sync::SpinLock;

const VENDOR: u16 = 0x15AD;
const DEVICE: u16 = 0x0405;

const SVGA_ID_2: u32 = 0x9000_0002;

const REG_ID: u32 = 0;
const REG_ENABLE: u32 = 1;
const REG_WIDTH: u32 = 2;
const REG_HEIGHT: u32 = 3;
const REG_MAX_WIDTH: u32 = 4;
const REG_MAX_HEIGHT: u32 = 5;
const REG_BITS_PER_PIXEL: u32 = 7;
const REG_BYTES_PER_LINE: u32 = 12;
const REG_FB_START: u32 = 13;
const REG_FB_OFFSET: u32 = 14;
const REG_VRAM_SIZE: u32 = 15;
const REG_MEM_START: u32 = 18;
const REG_MEM_SIZE: u32 = 19;
const REG_CONFIG_DONE: u32 = 20;
const REG_SYNC: u32 = 21;
const REG_BUSY: u32 = 22;

/// Слова заголовка очереди.
const FIFO_MIN: usize = 0;
const FIFO_MAX: usize = 1;
const FIFO_NEXT_CMD: usize = 2;
const FIFO_STOP: usize = 3;
/// Регистров очереди — столько, сколько отводит под заголовок спецификация
/// с расширенными регистрами; команды идут за ними.
const FIFO_NUM_REGS: u32 = 293;

const CMD_UPDATE: u32 = 1;

/// Адаптер, переведённый в режим SVGA: порты и очередь команд.
struct Svga {
    port: u16,
    fifo: VirtAddr,
}

/// `None` — адаптер в режиме прошивки (VGA/VBE), и показывать изменённое
/// ему не нужно.
static SVGA: SpinLock<Option<Svga>> = SpinLock::new(None);

/// Найти адаптер на шине.
fn find() -> Option<crate::pci::Device> {
    let rsdp = crate::acpi::rsdp();
    if rsdp == 0 {
        return None;
    }
    // SAFETY: адрес RSDP запомнен из хэндоффа, прямое отображение активно;
    // обход шины только читает конфигурационное пространство.
    unsafe {
        crate::pci::Root::discover(rsdp)
            .ok()
            .and_then(|root| crate::pci::find_by_id(&root, VENDOR, &[DEVICE]))
    }
}

/// Стоит ли в машине этот адаптер.
#[must_use]
pub fn present() -> bool {
    find().is_some()
}

/// Отметить адаптер своим при загрузке: драйвер в ядре есть, и `drvd` незачем
/// искать его в репозитории (до смены режима экран — буфер прошивки на этом
/// же адаптере).
pub fn claim() {
    if let Some(device) = find() {
        crate::devices::claim(device.address, "vmsvga");
    }
}

/// Включён ли режим SVGA — то есть экран у этого драйвера.
#[must_use]
pub fn active() -> bool {
    SVGA.lock().is_some()
}

fn read(port: u16, index: u32) -> u32 {
    // SAFETY: порт — BAR ввода-вывода найденного адаптера; пара «номер —
    // значение» не пересекается: обращения идут под замком `SVGA` или до его
    // заполнения из единственного места (`set_mode`).
    unsafe {
        crate::arch::port_write32(port, index);
        crate::arch::port_read32(port + 1)
    }
}

fn write(port: u16, index: u32, value: u32) {
    // SAFETY: см. `read`.
    unsafe {
        crate::arch::port_write32(port, index);
        crate::arch::port_write32(port + 1, value);
    }
}

pub fn set_mode(current: &Framebuffer, width: u32, height: u32) -> Result<Framebuffer, DisplayError> {
    let device = find().ok_or(DisplayError::NoDriver("no VMware SVGA II (15ad:0405)"))?;
    let bar0 = device.config32(0x10);
    if bar0 & 1 == 0 {
        return Err(DisplayError::Refused("BAR0 is not an I/O window"));
    }
    let port = u16::try_from(bar0 & !0x3).map_err(|_| DisplayError::Refused("I/O window above 64 KiB"))?;
    // Ввод-вывод, память и управление шиной: прошивка обычно включает первые
    // два, третье нужно очереди команд (адаптер читает её сам).
    // SAFETY: регистр `Command` найденного устройства.
    unsafe { device.write_config8(0x04, device.config8(0x04) | 0x07) };

    let mut guard = SVGA.lock();
    write(port, REG_ID, SVGA_ID_2);
    if read(port, REG_ID) != SVGA_ID_2 {
        return Err(DisplayError::Refused("the adapter does not speak SVGA II"));
    }
    let fb_start = u64::from(read(port, REG_FB_START));
    let vram = u64::from(read(port, REG_VRAM_SIZE));
    let (max_w, max_h) = (read(port, REG_MAX_WIDTH), read(port, REG_MAX_HEIGHT));

    // Картинка на этом адаптере? Иначе на экране другая карта, и гасить эту
    // незачем (см. `bochs`).
    let base = super::physical(current.base);
    if guard.is_none() && (base < fb_start || base >= fb_start.saturating_add(vram)) {
        return Err(DisplayError::NoDriver("the picture is not on the SVGA adapter"));
    }
    if width > max_w || height > max_h {
        return Err(DisplayError::Refused("mode larger than the adapter allows"));
    }
    let needed = u64::from(width) * u64::from(height) * 4;
    if needed > vram {
        return Err(DisplayError::TooLarge { needed, available: vram });
    }

    // Очередь команд — один раз, при первом переходе в SVGA.
    if guard.is_none() {
        let fifo_phys = u64::from(read(port, REG_MEM_START));
        let fifo_size = read(port, REG_MEM_SIZE);
        if fifo_phys == 0 || fifo_size < FIFO_NUM_REGS * 4 + 4096 {
            return Err(DisplayError::Refused("no command FIFO"));
        }
        let fifo_len = (fifo_size as usize).next_multiple_of(PAGE_SIZE);
        let fifo = PhysAddr::new(fifo_phys).to_direct_map();
        // SAFETY: память устройства (BAR2), по которой ядро не исполняется.
        unsafe {
            crate::arch::map_active(fifo, PhysAddr::new(fifo_phys), fifo_len, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE)
        }
        .map_err(|_| DisplayError::OutOfMemory)?;
        let words = fifo.as_usize() as *mut u32;
        let min = FIFO_NUM_REGS * 4;
        // SAFETY: заголовок очереди — первые слова отображённой памяти.
        unsafe {
            words.add(FIFO_MIN).write_volatile(min);
            words.add(FIFO_MAX).write_volatile(fifo_size);
            words.add(FIFO_NEXT_CMD).write_volatile(min);
            words.add(FIFO_STOP).write_volatile(min);
        }
        write(port, REG_CONFIG_DONE, 1);
        *guard = Some(Svga { port, fifo });
    }

    write(port, REG_WIDTH, width);
    write(port, REG_HEIGHT, height);
    write(port, REG_BITS_PER_PIXEL, 32);
    write(port, REG_ENABLE, 1);
    if (read(port, REG_WIDTH), read(port, REG_HEIGHT)) != (width, height) {
        return Err(DisplayError::Refused("the adapter kept another mode"));
    }
    let pitch = read(port, REG_BYTES_PER_LINE);
    let offset = u64::from(read(port, REG_FB_OFFSET));
    if pitch % 4 != 0 || pitch < width * 4 {
        return Err(DisplayError::Refused("odd bytes per line"));
    }
    let len = (u64::from(pitch) * u64::from(height)) as usize;
    let len = len.next_multiple_of(PAGE_SIZE);
    let phys = PhysAddr::new(fb_start + offset);
    let virt = phys.to_direct_map();
    // SAFETY: память кадра адаптера; записи можно объединять.
    unsafe {
        crate::arch::map_active(virt, phys, len, PageFlags::READ | PageFlags::WRITE | PageFlags::WRITE_COMBINE)
    }
    .map_err(|_| DisplayError::OutOfMemory)?;
    drop(guard);
    crate::devices::claim(device.address, "vmsvga");

    Ok(Framebuffer {
        base: virt.as_usize() as u64,
        size: len as u64,
        width,
        height,
        stride: pitch / 4,
        format: current.format,
    })
}

/// Показать изменившийся прямоугольник: команда `UPDATE` в очередь.
pub fn flush(x: i32, y: i32, w: u32, h: u32) {
    let guard = SVGA.lock();
    let Some(svga) = guard.as_ref() else {
        return;
    };
    if w == 0 || h == 0 {
        return;
    }
    let command = [CMD_UPDATE, x.max(0) as u32, y.max(0) as u32, w, h];
    let words = svga.fifo.as_usize() as *mut u32;
    // SAFETY: заголовок и кольцо — в отображённой памяти очереди; смещения
    // проверяются по `MIN`/`MAX`, которые записали мы.
    unsafe {
        let min = words.add(FIFO_MIN).read_volatile();
        let max = words.add(FIFO_MAX).read_volatile();
        let mut next = words.add(FIFO_NEXT_CMD).read_volatile();
        for word in command {
            let mut after = next + 4;
            if after >= max {
                after = min;
            }
            // Кольцо полно: попросить адаптер разобрать очередь и дождаться.
            let mut spins = 0u32;
            while after == words.add(FIFO_STOP).read_volatile() {
                write(svga.port, REG_SYNC, 1);
                while read(svga.port, REG_BUSY) != 0 {
                    spins += 1;
                    if spins > 1_000_000 {
                        return;
                    }
                    core::hint::spin_loop();
                }
            }
            words.add(next as usize / 4).write_volatile(word);
            next = after;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        words.add(FIFO_NEXT_CMD).write_volatile(next);
    }
}
