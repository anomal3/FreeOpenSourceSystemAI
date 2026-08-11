//! Буферы, разделяемые с устройствами: кольца дескрипторов и данные DMA.
//!
//! # Чем это отличается от кучи
//!
//! Тремя вещами, и каждая из них — требование железа, а не удобство.
//!
//! **Физический адрес.** Устройство адресует память физически: в регистр
//! контроллера уезжает не тот адрес, по которому к буферу обращается ядро.
//! [`Box`](alloc::boxed::Box) физический адрес своего содержимого не сообщает и
//! сообщить не может.
//!
//! **Физическая непрерывность.** Кольцо дескрипторов xHCI устройство читает
//! подряд, ничего не зная про таблицы страниц. Куча выдаёт непрерывность только
//! виртуальную — два соседних килобайта в ней запросто лежат в разных кадрах.
//!
//! **Атрибут памяти.** Кеш между процессором и устройством означает, что каждый
//! видит своё: см. [`PageFlags::DMA`]. Страницы кучи отображены как обычная
//! кешируемая память, и переотобразить их иначе нельзя — это задело бы чужие
//! аллокации, живущие на той же странице.
//!
//! # Почему нет освобождения
//!
//! Аллокатор — счётчик, растущий в одну сторону. Так сделано осознанно: всё, что
//! ядро сейчас выделяет под DMA, живёт до конца работы (кольца контроллера,
//! контексты устройств, буферы отчётов), а освобождение потребовало бы либо
//! списка свободных блоков, либо снятия отображений — то есть кода, у которого
//! пока нет ни одного вызывающего. Появится горячее подключение устройств —
//! появится и он; проектировать его заранее значит угадывать.
//!
//! Утечка при этом не бесшумна: окно ограничено [`DMA_SIZE`], и исчерпание
//! возвращает ошибку, а не тихо портит соседние страницы.
//!
//! # Двойное отображение и AArch64
//!
//! Кадры под буфер приходят из общего пула, а он уже отображён прямым
//! отображением как обычная кешируемая память. То есть у страницы появляется
//! второй псевдоним с другим атрибутом. Архитектура ARM такое допускает, но
//! требует, чтобы через кешируемый псевдоним обращений не было — иначе в кеше
//! окажутся данные, которых устройство не видит. Здесь это выполняется по
//! построению: адрес из прямого отображения наружу не отдаётся вовсе, а
//! [`DmaBuffer`] знает только свой некешируемый. Так же устроен
//! `dma_alloc_coherent` в Linux на arm64.

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch;
use crate::mm::{DMA_BASE, DMA_SIZE, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr, frame};

/// Почему буфер не удалось выделить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// Запрошено больше, чем осталось в окне [`DMA_SIZE`].
    WindowExhausted { requested: usize, left: usize },
    /// В пуле нет столько подряд идущих свободных кадров.
    NoContiguousFrames { pages: usize },
    /// Аллокатор кадров недоступен — [`frame::init`] ещё не вызывался.
    NoFrameAllocator,
    /// Отобразить выделенные кадры в окно не удалось.
    MapFailed(crate::mm::MapError),
}

impl core::fmt::Display for DmaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::WindowExhausted { requested, left } => {
                write!(f, "DMA window exhausted: {requested} bytes requested, {left} left")
            }
            Self::NoContiguousFrames { pages } => {
                write!(f, "no {pages} physically contiguous free frames")
            }
            Self::NoFrameAllocator => f.write_str("the frame allocator is not up yet"),
            Self::MapFailed(err) => write!(f, "mapping the buffer failed: {err}"),
        }
    }
}

/// Сколько байт окна уже роздано.
static USED: AtomicUsize = AtomicUsize::new(0);

/// Буфер, к которому обращаются и процессор, и устройство.
///
/// Владения не выражает: освобождения у аллокатора нет, поэтому и `Drop` тут
/// нечего делать. Тип существует, чтобы виртуальный и физический адреса всегда
/// ходили парой — перепутать их в драйвере значит записать в регистр устройства
/// адрес из верхней половины и получить молчание вместо работы.
#[derive(Clone, Copy)]
pub struct DmaBuffer {
    virt: VirtAddr,
    phys: PhysAddr,
    len: usize,
}

impl DmaBuffer {
    /// Адрес, по которому к буферу обращается ядро.
    #[must_use]
    pub const fn virt(&self) -> VirtAddr {
        self.virt
    }

    /// Адрес, который надо сообщить устройству.
    #[must_use]
    pub const fn phys(&self) -> PhysAddr {
        self.phys
    }

    /// Размер в байтах — с учётом округления вверх до страницы.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Типизированный указатель на начало буфера.
    ///
    /// # Safety
    ///
    /// Вызывающий отвечает за то, что `T` не выходит за пределы буфера и что
    /// раскладка `T` совпадает с тем, чего ожидает устройство. Обращения к
    /// содержимому обязаны быть `volatile`: то же место правит устройство, и
    /// компилятору нельзя разрешать кешировать или выбрасывать доступы.
    #[must_use]
    pub const unsafe fn as_ptr<T>(&self) -> *mut T {
        self.virt.as_usize() as *mut T
    }

    /// Заполнить буфер нулями.
    ///
    /// Кольца xHCI обязаны начинаться с нулей: непрочитанный мусор контроллер
    /// истолковал бы как дескрипторы. Кадры от аллокатора уже обнулены, но
    /// полагаться на это в драйвере — значит связать его с деталью реализации
    /// пула.
    pub fn zero(&self) {
        let ptr = self.virt.as_usize() as *mut u8;
        for offset in 0..self.len {
            // SAFETY: смещение внутри буфера, отображённого на запись при
            // выделении. `volatile` — потому что через это же отображение
            // работает устройство, и запись обязана состояться на самом деле.
            unsafe { ptr.add(offset).write_volatile(0) };
        }
    }
}

/// Выделить буфер под DMA размером не меньше `bytes`.
///
/// Результат всегда выровнен на страницу — то есть удовлетворяет и требованию
/// xHCI о выравнивании на 64 байта, и требованию не пересекать границу 64 КиБ
/// (кольцо целиком лежит внутри своих страниц, а страницы физически подряд).
pub fn alloc(bytes: usize) -> Result<DmaBuffer, DmaError> {
    if bytes == 0 {
        return Ok(DmaBuffer { virt: VirtAddr::new(DMA_BASE), phys: PhysAddr::new(0), len: 0 });
    }
    let len = bytes.next_multiple_of(PAGE_SIZE);
    let pages = len / PAGE_SIZE;

    // Место в окне занимается до выделения кадров: если окно кончилось, кадры
    // трогать незачем.
    let offset = USED.fetch_add(len, Ordering::Relaxed);
    if offset + len > DMA_SIZE {
        // Счётчик не откатывается: конкурентный вызов мог уже занять место за
        // нами, и «вернуть» его значит отдать чужое. Окно всё равно исчерпано, а
        // потерянные байты в исчерпанном окне ничего не меняют.
        return Err(DmaError::WindowExhausted { requested: len, left: DMA_SIZE.saturating_sub(offset) });
    }

    let phys = frame::with(|frames| frames.allocate_contiguous(pages))
        .ok_or(DmaError::NoFrameAllocator)?
        .ok_or(DmaError::NoContiguousFrames { pages })?;

    let virt = VirtAddr::new(DMA_BASE + offset);
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DMA;
    // SAFETY: ядро исполняется на собственных таблицах (буферы DMA выделяются
    // сильно позже `take_over_memory`), а диапазон окна DMA не пересекается ни с
    // кодом, ни со стеком, ни с кучей — он отведён под это и только под это, и
    // каждый адрес в нём выдаётся ровно один раз.
    unsafe { arch::map_active(virt, phys, len, flags) }.map_err(DmaError::MapFailed)?;

    let buffer = DmaBuffer { virt, phys, len };
    buffer.zero();
    Ok(buffer)
}

/// Сколько байт окна роздано и сколько всего.
#[must_use]
pub fn stats() -> (usize, usize) {
    (USED.load(Ordering::Relaxed).min(DMA_SIZE), DMA_SIZE)
}
