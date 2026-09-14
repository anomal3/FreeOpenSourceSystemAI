//! Bochs VBE: стандартный VGA QEMU на x86-64.
//!
//! Интерфейс — пара портов: в `0x1CE` пишется номер регистра, через `0x1CF`
//! читается и пишется его значение, словами. Память кадра — BAR0 адаптера,
//! та же, в которую рисовал GOP: OVMF (`QemuVideoDxe`) отдаёт ядру именно её.

use boot_info::Framebuffer;

use super::DisplayError;
use crate::mm::{PAGE_SIZE, PageFlags};

const VENDOR: u16 = 0x1234;
const DEVICE: u16 = 0x1111;

const INDEX_ID: u16 = 0x0;
const INDEX_XRES: u16 = 0x1;
const INDEX_YRES: u16 = 0x2;
const INDEX_BPP: u16 = 0x3;
const INDEX_ENABLE: u16 = 0x4;
const INDEX_VIRT_WIDTH: u16 = 0x6;
const INDEX_VIRT_HEIGHT: u16 = 0x7;
const INDEX_X_OFFSET: u16 = 0x8;
const INDEX_Y_OFFSET: u16 = 0x9;
/// Объём памяти адаптера в блоках по 64 КиБ. Появился в версии `0xB0C2`.
const INDEX_VIDEO_MEMORY_64K: u16 = 0xA;

/// Версии интерфейса, с которыми мы согласны работать. Ниже `0xB0C2` адаптер
/// не сообщает объём памяти, и проверить, влезет ли режим, было бы нечем.
const ID_MIN: u16 = 0xB0C2;
const ID_MAX: u16 = 0xB0C5;

const ENABLED: u16 = 0x01;
const LFB_ENABLED: u16 = 0x40;

/// Порты — общий ресурс машины: две задачи, пишущие номер регистра вперемешку,
/// записали бы значение не в тот регистр.
static PORTS: crate::sync::SpinLock<()> = crate::sync::SpinLock::new(());

pub fn set_mode(current: &Framebuffer, width: u32, height: u32) -> Result<Framebuffer, DisplayError> {
    let rsdp = crate::acpi::rsdp();
    if rsdp == 0 {
        return Err(DisplayError::NoDriver("no ACPI tables, so no PCI"));
    }
    // SAFETY: адрес RSDP запомнен из хэндоффа, прямое отображение активно.
    let root = unsafe { crate::pci::Root::discover(rsdp) }
        .map_err(|_| DisplayError::NoDriver("no PCI"))?;
    // SAFETY: см. выше.
    let device = unsafe { crate::pci::find_by_id(&root, VENDOR, &[DEVICE]) }
        .ok_or(DisplayError::NoDriver("no QEMU standard VGA (1234:1111)"))?;
    let lfb = device
        .memory_bar(0)
        .ok_or(DisplayError::Refused("the adapter has no memory window"))?;

    let _ports = PORTS.lock();
    // SAFETY: порты 0x1CE/0x1CF у PC закреплены за Bochs VBE, адаптер найден
    // на шине; чтение номера версии побочных эффектов не имеет.
    let id = unsafe { crate::arch::vbe_read(INDEX_ID) };
    if !(ID_MIN..=ID_MAX).contains(&id) {
        return Err(DisplayError::Refused("no Bochs VBE interface behind the ports"));
    }
    // SAFETY: см. выше.
    let memory = u64::from(unsafe { crate::arch::vbe_read(INDEX_VIDEO_MEMORY_64K) }) * 64 * 1024;

    // Картинка на этом адаптере? Буфер от прошивки обязан лежать внутри его
    // памяти. Иначе на экране другое устройство — например, прошивка выбрала
    // вторую карту, — и переключать эту значит гасить не тот экран.
    //
    // Сравнивается физический адрес. Буфер от прошивки лежит по identity, то
    // есть его `base` физический и есть; буфер после нашей смены — в прямом
    // отображении, и первая версия сверяла его как есть: вторая смена подряд
    // отказывала словами «картинка не на этом адаптере», хотя он был ровно тем.
    let lfb_start = lfb.as_u64();
    let base = super::physical(current.base);
    if base < lfb_start || base >= lfb_start.saturating_add(memory) {
        return Err(DisplayError::NoDriver("the picture is not on the Bochs adapter"));
    }

    let needed = u64::from(width) * u64::from(height) * 4;
    if needed > memory {
        return Err(DisplayError::TooLarge { needed, available: memory });
    }

    // Отображается ровно столько, сколько нужно режиму. Повторная смена на тот
    // же адрес отображает те же страницы на те же кадры — это не ошибка.
    let len = (needed as usize).next_multiple_of(PAGE_SIZE);
    let virt = lfb.to_direct_map();
    // SAFETY: собственные таблицы ядра активны; диапазон — память устройства,
    // по которой ядро не исполняется, и получает семантику Device.
    unsafe {
        crate::arch::map_active(virt, lfb, len, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE)
    }
    .map_err(|_| DisplayError::OutOfMemory)?;

    // Порядок из спецификации интерфейса: выключить, задать геометрию,
    // включить с линейным буфером. Геометрия, записанная во включённый
    // адаптер, применяется по регистру, и между записями экран на миг был бы
    // чужого размера.
    let (w, h) = (width as u16, height as u16);
    // SAFETY: см. выше; значения проверены на вменяемость.
    unsafe {
        crate::arch::vbe_write(INDEX_ENABLE, 0);
        crate::arch::vbe_write(INDEX_XRES, w);
        crate::arch::vbe_write(INDEX_YRES, h);
        crate::arch::vbe_write(INDEX_BPP, 32);
        crate::arch::vbe_write(INDEX_VIRT_WIDTH, w);
        crate::arch::vbe_write(INDEX_VIRT_HEIGHT, h);
        crate::arch::vbe_write(INDEX_X_OFFSET, 0);
        crate::arch::vbe_write(INDEX_Y_OFFSET, 0);
        crate::arch::vbe_write(INDEX_ENABLE, ENABLED | LFB_ENABLED);
    }
    // Адаптер вправе округлить или отвергнуть режим — тогда он оставляет свой.
    // Рисовать по размеру, которого на экране нет, значит писать мимо строк.
    // SAFETY: см. выше.
    let (got_w, got_h) = unsafe { (crate::arch::vbe_read(INDEX_XRES), crate::arch::vbe_read(INDEX_YRES)) };
    if (got_w, got_h) != (w, h) {
        return Err(DisplayError::Refused("the adapter kept another mode"));
    }
    crate::devices::claim(device.address, "bochs vbe");

    Ok(Framebuffer {
        base: virt.as_usize() as u64,
        size: len as u64,
        width,
        height,
        stride: width,
        format: current.format,
    })
}

