//! virtio-net: сетевая карта.
//!
//! # Чем сеть отличается от диска
//!
//! Тремя вещами, и каждая из них меняет способ работы с очередью.
//!
//! **Очередей две, и они несимметричны.** Нулевая — приёмная, первая —
//! передающая. У диска очередь одна, и номер очереди в окне уведомлений всегда
//! совпадал с нулём по случайности; здесь эта случайность кончается, и толкать
//! надо ту очередь, в которую положили (см. [`Queue::kick`]).
//!
//! **Приёмные буферы выставляются заранее.** Устройство не спрашивает
//! разрешения перед тем, как отдать кадр: оно берёт первый попавшийся буфер из
//! приёмной очереди и пишет в него. Пустая приёмная очередь означает не
//! задержку, а потерянный кадр, поэтому все буферы выставлены с самого начала и
//! возвращаются в очередь сразу, как только их содержимое скопировано.
//!
//! **Ждать нечего.** Запрос к диску имеет ответ, и ждать его осмысленно. У сети
//! ответа нет: отправка завершается, когда устройство заберёт кадр, а приём
//! случается тогда, когда придёт кадр — то есть, может быть, никогда. Поэтому
//! обе операции здесь неблокирующие, а опрашивает очередь отдельная задача
//! ([`crate::net::service_task`]).
//!
//! # Заголовок перед каждым кадром
//!
//! Кадру предшествует `virtio_net_hdr_v1` — двенадцать байт про контрольные
//! суммы и сегментацию, которых мы не просили и не делаем. Их размер и есть
//! классическая ловушка: у **старого** интерфейса заголовок десять байт, а поле
//! `num_buffers` появляется только вместе с `VIRTIO_NET_F_MRG_RXBUF`. У
//! современного (а мы согласовали `VIRTIO_F_VERSION_1` и никак иначе работать не
//! умеем) поле присутствует всегда, и заголовок всегда двенадцать байт.
//! Ошибиться на два байта здесь — значит получать кадры, у которых MAC-адрес
//! назначения начинается на два байта раньше, чем на самом деле: разбор при этом
//! не падает, он просто читает мусор.

use super::{DESC_F_WRITE, FEATURE_VERSION_1, Queue, Transport, VirtioError};
use crate::mm::dma::{self, DmaBuffer};
use crate::pci::{self, Device};

/// Возможность: устройство сообщает свой аппаратный адрес в конфигурации.
///
/// Согласовать обязательно. Адрес, придуманный нами, не сделал бы карту
/// неработающей сразу — кадры уходили бы, — но ответы на них приходили бы на
/// адрес, которого на этой карте нет, а фильтр устройства их бы отбросил.
const FEATURE_MAC: u64 = 1 << 5;

/// Смещение MAC-адреса в конфигурации сетевого устройства.
const CONFIG_MAC: usize = 0;

/// Длина заголовка `virtio_net_hdr_v1`.
const HEADER: usize = 12;

/// Наибольший кадр Ethernet без контрольной суммы: заголовок и 1500 байт данных.
pub const FRAME_MAX: usize = 1514;

/// Сколько места отводится под один кадр вместе с заголовком virtio.
///
/// Два килобайта, а не 1526: буферы нарезаются из одного куска, и степень
/// двойки означает, что ни один из них не пересекает границу страницы.
const BUFFER: usize = 2048;

/// Номер приёмной очереди.
const QUEUE_RX: u16 = 0;
/// Номер передающей очереди.
const QUEUE_TX: u16 = 1;

/// Сколько раз опросить передающую очередь, ожидая свободный дескриптор.
///
/// Предел маленький и намеренно: занятая передающая очередь — повод потерять
/// исходящий кадр, а не остановить систему. Протоколы поверх переспросят, а
/// эмулируемая карта забирает кадр за единицы тысяч оборотов.
const TX_POLLS: u32 = 100_000;

/// Счётчики, по которым видно, что происходит на проводе.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub rx_frames: u64,
    pub rx_bytes: u64,
    /// Кадры, которые устройство отдало, а мы не смогли принять: слишком
    /// длинные или пришедшие с непонятной длиной.
    pub rx_dropped: u64,
    pub tx_frames: u64,
    pub tx_bytes: u64,
    /// Кадры, которые не удалось отправить: очередь не освободилась.
    pub tx_dropped: u64,
}

/// Сетевая карта virtio.
pub struct VirtioNet {
    /// Окно регистров. Читается только при подключении, но держать его
    /// обязательно: в нём живёт отображение, через которое уведомляются очереди.
    #[allow(dead_code)]
    transport: Transport,
    rx: Queue,
    tx: Queue,
    /// Приёмные буферы: по одному на дескриптор, буфер `i` лежит по смещению
    /// `i * BUFFER`. Соответствие «номер дескриптора — номер буфера» прямое, и
    /// это единственное, что связывает завершение из кольца `used` с памятью,
    /// в которую устройство писало.
    rx_pool: DmaBuffer,
    tx_pool: DmaBuffer,
    mac: [u8; 6],
    stats: Stats,
}

impl VirtioNet {
    /// Найти карту на шине и подготовить её к работе.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц.
    pub unsafe fn probe(root: &pci::Root) -> Result<Self, VirtioError> {
        // SAFETY: контракт функции.
        let device = unsafe {
            pci::find_by_id(
                root,
                pci::VENDOR_VIRTIO,
                &[pci::DEVICE_VIRTIO_NET_LEGACY, pci::DEVICE_VIRTIO_NET_MODERN],
            )
        }
        .ok_or(VirtioError::NoCapabilities)?;

        // SAFETY: контракт функции.
        unsafe { Self::attach(&device) }
    }

    /// Подготовить найденное устройство.
    ///
    /// # Safety
    ///
    /// См. [`VirtioNet::probe`].
    unsafe fn attach(device: &Device) -> Result<Self, VirtioError> {
        // Ровно та же причина, что у диска: при сброшенном бите Memory Space
        // обращения к регистрам пропадают молча, и драйвер выглядит исправным
        // ровно до первого таймаута. Подробности — в `virtio::blk::attach`.
        //
        // SAFETY: bus master безопасен до того, как устройству сообщены адреса
        // колец и выставлен DRIVER_OK.
        unsafe { device.enable_bus_master() };

        // SAFETY: контракт функции.
        let transport = unsafe { Transport::open(device) }?;

        // Просим только соответствие virtio 1.0 и MAC-адрес. Всё остальное, что
        // предлагает virtio-net — контрольные суммы, сегментация, слияние
        // приёмных буферов, управляющая очередь, — требует кода, которого здесь
        // нет; согласовать возможность и не реализовать её значит получить
        // кадры, разобрать которые нечем.
        let negotiated = transport.negotiate(FEATURE_VERSION_1 | FEATURE_MAC)?;
        if negotiated & FEATURE_MAC == 0 {
            transport.set_failed();
            return Err(VirtioError::NoMac);
        }

        let rx = Queue::new(&transport, QUEUE_RX)?;
        let tx = Queue::new(&transport, QUEUE_TX)?;

        let rx_pool = dma::alloc(usize::from(rx.size()) * BUFFER).map_err(VirtioError::NoMemory)?;
        let tx_pool = dma::alloc(usize::from(tx.size()) * BUFFER).map_err(VirtioError::NoMemory)?;

        let mut mac = [0u8; 6];
        for (index, byte) in mac.iter_mut().enumerate() {
            // SAFETY: окно конфигурации отображено в `Transport::open`, а поле
            // MAC существует — возможность согласована выше.
            *byte = unsafe { transport.device_config8(CONFIG_MAC + index) };
        }

        // Кольца построены, адреса сообщены — теперь устройству можно ими
        // пользоваться. Приёмные буферы выставляются после этого, а не до:
        // до `DRIVER_OK` устройство не обязано смотреть в очередь вовсе, и
        // порядок «сначала разрешили, потом дали» не оставляет вопроса, увидело
        // ли оно первую партию.
        transport.set_driver_ok();

        let mut card = Self {
            transport,
            rx,
            tx,
            rx_pool,
            tx_pool,
            mac,
            stats: Stats::default(),
        };
        card.fill_receive_queue();

        Ok(card)
    }

    /// Аппаратный адрес карты.
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Выставить все приёмные буферы, какие есть.
    fn fill_receive_queue(&mut self) {
        while let Some(id) = self.rx.alloc_descriptor() {
            self.offer_receive_buffer(id);
        }
        self.rx.kick();
    }

    /// Отдать устройству один приёмный буфер.
    fn offer_receive_buffer(&mut self, id: u16) {
        let phys = self.rx_pool.phys().as_u64() + u64::from(id) * BUFFER as u64;
        // `DESC_F_WRITE`: в приёмный буфер пишет устройство, а не мы. Без флага
        // устройство сочтёт буфер исходящими данными и не отдаст в него ни
        // одного кадра — молча, потому что ошибкой это не является.
        self.rx.set_descriptor(id, phys, BUFFER as u32, DESC_F_WRITE, 0);
        self.rx.offer(id);
    }

    /// Забрать один принятый кадр, если он есть.
    ///
    /// Возвращает длину кадра, скопированного в `frame`. Заголовок virtio при
    /// этом отброшен: выше по стеку о нём знать незачем.
    pub fn receive(&mut self, frame: &mut [u8; FRAME_MAX]) -> Option<usize> {
        let (id, len) = self.rx.take_used()?;

        let len = len as usize;
        let result = if len < HEADER || len > HEADER + FRAME_MAX {
            // Длина пришла от устройства. Кадр короче заголовка невозможен, а
            // длиннее буфера означал бы, что устройство написало за его
            // пределы, — и в обоих случаях единственное честное действие это
            // выбросить кадр и сказать об этом счётчиком.
            self.stats.rx_dropped += 1;
            None
        } else {
            let payload = len - HEADER;
            // SAFETY: буфер `id` лежит внутри пула (`id < rx.size()`), а
            // читаются ровно те байты, о которых сообщило устройство.
            unsafe {
                let src = self
                    .rx_pool
                    .as_ptr::<u8>()
                    .add(usize::from(id) * BUFFER + HEADER);
                core::ptr::copy_nonoverlapping(src, frame.as_mut_ptr(), payload);
            }
            self.stats.rx_frames += 1;
            self.stats.rx_bytes += payload as u64;
            Some(payload)
        };

        // Буфер возвращается в очередь в обоих случаях, и немедленно: приёмная
        // очередь, из которой забрали буфер и не вернули, кончается за десяток
        // кадров, а кончившись, начинает терять их молча.
        self.offer_receive_buffer(id);
        self.rx.kick();

        result
    }

    /// Отправить кадр.
    ///
    /// Кадр копируется в буфер DMA: срез вызывающего лежит в куче, которая не
    /// обязана быть ни физически непрерывной, ни видимой устройству — та же
    /// причина, по которой копирует диск.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), VirtioError> {
        if frame.len() > FRAME_MAX {
            return Err(VirtioError::TooLong(frame.len()));
        }

        let id = match self.take_transmit_descriptor() {
            Some(id) => id,
            None => {
                self.stats.tx_dropped += 1;
                return Err(VirtioError::QueueFull);
            }
        };

        let offset = usize::from(id) * BUFFER;
        // SAFETY: буфер `id` лежит внутри пула, а заголовок с кадром вместе
        // короче `BUFFER` — длина проверена выше.
        unsafe {
            let base = self.tx_pool.as_ptr::<u8>().add(offset);
            // Заголовок обнуляется целиком: нули означают «ни контрольных сумм,
            // ни сегментации», то есть ровно то, о чём мы договорились при
            // согласовании возможностей.
            core::ptr::write_bytes(base, 0, HEADER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), base.add(HEADER), frame.len());
        }

        let phys = self.tx_pool.phys().as_u64() + offset as u64;
        // Заголовок и кадр — один дескриптор без флага `WRITE`: устройство их
        // читает. Раскладка произвольная здесь законна именно потому, что
        // согласован `VIRTIO_F_VERSION_1`; у старого интерфейса пришлось бы
        // разделять на два дескриптора.
        self.tx
            .set_descriptor(id, phys, (HEADER + frame.len()) as u32, 0, 0);
        self.tx.offer(id);
        self.tx.kick();

        self.stats.tx_frames += 1;
        self.stats.tx_bytes += frame.len() as u64;
        Ok(())
    }

    /// Найти свободный дескриптор передающей очереди.
    fn take_transmit_descriptor(&mut self) -> Option<u16> {
        self.reclaim_transmitted();
        if let Some(id) = self.tx.alloc_descriptor() {
            return Some(id);
        }
        // Очередь занята целиком. Ждём — но недолго и с пределом: см. `TX_POLLS`.
        for _ in 0..TX_POLLS {
            self.reclaim_transmitted();
            if let Some(id) = self.tx.alloc_descriptor() {
                return Some(id);
            }
            core::hint::spin_loop();
        }
        None
    }

    /// Вернуть в оборот дескрипторы кадров, которые устройство уже забрало.
    fn reclaim_transmitted(&mut self) {
        while let Some((id, _)) = self.tx.take_used() {
            self.tx.free_descriptor(id);
        }
    }
}
