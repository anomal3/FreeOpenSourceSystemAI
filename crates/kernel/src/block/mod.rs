//! Блочные устройства: всё, что умеет читать и писать сектора.
//!
//! # Зачем понадобился слой
//!
//! До Phase 26a слова «диск» и «virtio-blk» в этом ядре означали одно и то же.
//! Корень искался на одном устройстве одного типа, и монтирование было написано
//! так, что другого просто не предполагалось: `Ext2Fs::mount` принимал
//! `VirtioBlk` по значению. Пока система жила в QEMU, это было не упрощением, а
//! правдой — другого диска там и нет.
//!
//! Правда кончилась ровно там, где систему понесли на чужую машину. VirtualBox
//! с настройками по умолчанию даёт SATA, ноутбук — NVMe, и в обоих случаях
//! установщик успешно пишет диск через Block I/O прошивки, а установленная
//! система своего корня не находит: драйвера нет. Установка в гипервизор
//! оказывалась дорогой в один конец.
//!
//! Слой поэтому состоит ровно из двух вещей, и обе — про **множественное
//! число**: список найденных носителей вместо одного устройства и поиск раздела
//! **по всем** носителям вместо «на том единственном». Всё остальное уже было:
//! трейт [`disk::BlockDevice`] существует с фазы 8a и покрыт тестами на хосте,
//! `ext2` работает через `&mut dyn BlockDevice` с самого начала. Драйверу диска
//! достаточно реализовать этот трейт, чтобы GPT и ext2 заработали на нём без
//! единой строчки нового кода.
//!
//! # Чего здесь нет
//!
//! Ни кеша, ни очереди запросов, ни разделения на «устройство» и «раздел» как
//! на отдельные объекты. Раздел здесь — это пара «носитель и его первый
//! сектор», и этого достаточно, пока смонтированная файловая система одна.
//! Заготовка под таблицу монтирования была бы кодом без потребителя — а по
//! правилу дома непроверенный код, который пишет на диск, хуже отсутствующего.

pub mod ahci;
pub mod nvme;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::kprintln;
use crate::pci;
use crate::virtio;

/// Каким проводом подключён носитель.
///
/// Нужно не для логики, а для журнала: «корень найден на диске» и «корень
/// найден на **втором порту SATA**» — разные сообщения для того, кто выясняет,
/// почему система не загрузилась.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    VirtioBlk,
    Ahci,
    Nvme,
}

impl Kind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::VirtioBlk => "virtio-blk",
            Self::Ahci => "ahci",
            Self::Nvme => "nvme",
        }
    }
}

/// Найденный носитель.
pub struct Disk {
    pub kind: Kind,
    /// Номер внутри своего контроллера: порт у AHCI, ноль у virtio-blk.
    pub unit: usize,
    pub device: Box<dyn disk::BlockDevice + Send>,
}

impl Disk {
    /// Ёмкость в секторах — то, что стоит напечатать при загрузке.
    #[must_use]
    pub fn sectors(&self) -> u64 {
        self.device.sector_count()
    }
}

/// Перечислить все носители, какие есть на шине.
///
/// Порядок опроса — от того, что вероятнее в среде разработки, к тому, что
/// вероятнее на чужой машине; на результат он не влияет, потому что раздел
/// ищется по типу GUID, а не по номеру диска. Отсутствие устройств не ошибка:
/// живой ISO работает без единого диска, и это его штатный режим.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn probe_all(root: &pci::Root) -> Vec<Disk> {
    let mut disks = Vec::new();

    // SAFETY: контракт функции.
    match unsafe { virtio::blk::VirtioBlk::probe(root) } {
        Ok(device) => disks.push(Disk {
            kind: Kind::VirtioBlk,
            unit: 0,
            device: Box::new(device),
        }),
        Err(err) => kprintln!("  disk        : no virtio-blk ({err})"),
    }

    // SAFETY: контракт функции.
    for device in unsafe { ahci::probe(root) } {
        disks.push(Disk {
            kind: Kind::Ahci,
            unit: device.port_index(),
            device: device.into_block_device(),
        });
    }

    // SAFETY: контракт функции.
    for device in unsafe { nvme::probe(root) } {
        disks.push(Disk {
            kind: Kind::Nvme,
            unit: 0,
            device: device.into_block_device(),
        });
    }

    disks
}

/// Носитель со счётчиком обращений.
///
/// Счётчик был у virtio-blk и печатался при монтировании — не для красоты: это
/// единственное доказательство, что чтение действительно дошло до устройства, а
/// не было обслужено чем-то по дороге. Держать его в каждом драйвере значило бы
/// написать одно и то же трижды и получить три разных ответа на вопрос «что
/// считается запросом». Считает поэтому слой, одинаково для всех.
pub struct Counted {
    inner: Box<dyn disk::BlockDevice + Send>,
    requests: u64,
}

impl Counted {
    #[must_use]
    pub fn new(inner: Box<dyn disk::BlockDevice + Send>) -> Self {
        Self { inner, requests: 0 }
    }

    /// Сколько раз обращались к носителю.
    #[must_use]
    pub const fn requests(&self) -> u64 {
        self.requests
    }
}

impl disk::BlockDevice for Counted {
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.inner.sector_count()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        self.requests += 1;
        self.inner.read(lba, buf)
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        self.requests += 1;
        self.inner.write(lba, buf)
    }

    fn flush(&mut self) -> disk::Result<()> {
        self.inner.flush()
    }
}

/// Раздел: носитель и сектор, с которого он начинается.
pub struct Partition {
    pub device: Box<dyn disk::BlockDevice + Send>,
    pub first_lba: u64,
    /// Откуда он взялся — для журнала.
    pub source: &'static str,
    pub unit: usize,
}

/// Найти на любом из носителей раздел заданного в GPT типа.
///
/// Носители перебираются по очереди, и первый подошедший забирается **вместе с
/// устройством**: файловой системе нужно владеть диском, а раздавать один диск
/// двум владельцам незачем — точка монтирования пока одна.
///
/// Диск без таблицы разделов пропускается молча в том смысле, что это не
/// ошибка: у установочного носителя её и не должно быть. Сама причина при этом
/// печатается — молчаливый пропуск и есть то, из-за чего «система не видит
/// диск» превращается в вечер отладки.
pub fn find_partition(disks: Vec<Disk>, type_guid: disk::guid::Guid) -> Option<Partition> {
    for mut candidate in disks {
        let name = candidate.kind.name();
        let table = match disk::gpt::read(candidate.device.as_mut()) {
            Ok(table) => table,
            Err(err) => {
                kprintln!("  partitions  : {name} #{}: {err}", candidate.unit);
                continue;
            }
        };
        let sector = candidate.device.sector_size() as usize;
        kprintln!(
            "  partitions  : {name} #{}: GPT {}, {} entries, {sector}-byte sectors",
            candidate.unit,
            table.disk_guid,
            table.partitions.len(),
        );
        for partition in &table.partitions {
            kprintln!(
                "    part {}     : {} MiB at LBA {}, '{}'",
                partition.index + 1,
                partition.range().bytes(sector) / (1024 * 1024),
                partition.first_lba,
                partition.name_string(),
            );
        }
        if let Some(found) = table.find(type_guid) {
            return Some(Partition {
                first_lba: found.first_lba,
                source: name,
                unit: candidate.unit,
                device: candidate.device,
            });
        }
    }
    None
}
