//! Цели: что именно зовётся с испорченными байтами.
//!
//! # Как выбрана каждая цель
//!
//! Это точка, в которой байты, выбранные **не нами**, впервые превращаются в
//! числа, которыми потом что-то индексируется. Дальше по коду проверять уже
//! поздно: смещение, пришедшее из файла и сложенное без проверки, — это чтение
//! чужой памяти, а не неверный ответ.
//!
//! Порядок — по убыванию ценности, и он не наш: так его расставил разбор
//! проекта со стороны, и Роман его принял. Диск и сеть впереди дескрипторов,
//! потому что диск вставляют в машину, а сеть приходит сама.
//!
//! # Чего здесь нет
//!
//! **ELF.** Разбор образа программы живёт в `crates/kernel/src/user/elf.rs`, а
//! крейт ядра под хост не собирается: у него свои `arch`-модули с ассемблером
//! под две мишени. Вынести разборщик в отдельный крейт можно и, наверное,
//! нужно — но это правка ядра, а не фаззера, и делать её заодно значило бы
//! смешать две работы. Пока сказано вслух: **ELF не фаззится**.
//!
//! **Подпись и проверка цепочки.** `x509::verify` и проверка подписи пакета
//! считают криптографию, то есть тратят миллисекунды на вход; фаззер за то же
//! время успевает в тысячу раз меньше итераций, а ищет он ошибки разбора, а не
//! арифметики в поле. Цепочка проверяется своими тестами на настоящих
//! сертификатах.

use crate::seeds;
use crate::view::View;

/// Одна цель фаззера.
pub struct Target {
    /// Имя для `--target` и для отчёта.
    pub name: &'static str,
    pub about: &'static str,
    /// Правильные байты, с которых начинается порча.
    pub seeds: fn() -> Vec<Vec<u8>>,
    /// Сколько первых байт входа имеет смысл портить.
    ///
    /// Для сертификата это весь вход; для тома — область метаданных. Порча
    /// середины двухмегабайтного образа, куда разборщик и не заглянет, тратит
    /// итерацию впустую.
    pub hot_bytes: usize,
    /// Что позвать. Обязана вернуть на любых байтах.
    pub run: fn(&[u8]),
}

/// Все цели.
pub static TARGETS: &[Target] = &[
    Target {
        name: "ext2",
        about: "Том ext2 с чужого диска: суперблок, группы, инод, каталог.",
        seeds: || vec![seeds::ext2_volume()],
        // Суперблок, дескрипторы групп, битовые карты и таблица инодов первой
        // группы целиком укладываются в первые шестьдесят четыре килобайта.
        hot_bytes: 64 * 1024,
        run: |bytes| {
            let mut disk = View::new(bytes);
            let Ok(volume) = ext2::Ext2::mount(&mut disk, 0) else {
                return;
            };
            // Монтирование — это только суперблок. Дальше идёт то, из-за чего
            // фаззинг файловой системы вообще нужен: чтение инода по номеру из
            // суперблока и разбор записей каталога, у которых длина записи
            // приходит из того же файла.
            let Ok(root) = volume.root(&mut disk) else {
                return;
            };
            if let Ok(entries) = volume.list(&mut disk, &root) {
                for entry in entries {
                    if let Ok(Some(found)) = volume.lookup(&mut disk, &root, &entry.name) {
                        if let Ok(inode) = volume.inode(&mut disk, found.inode) {
                            let _ = volume.read_file(&mut disk, &inode);
                        }
                    }
                }
            }
        },
    },
    Target {
        name: "ext2-fsck",
        about: "Проверка тома ext2: обход всего, что назвал испорченный суперблок.",
        seeds: || vec![seeds::ext2_volume()],
        hot_bytes: 64 * 1024,
        run: |bytes| {
            let mut disk = View::new(bytes);
            // Без починки: цель — разбор, а не запись. `fsck` здесь отдельной
            // целью, потому что он ходит по тому целиком, а не по одному пути,
            // — то есть доходит до структур, которых чтение файла не касается.
            let _ = ext2::check(&mut disk, 0, ext2::Fix::Nothing);
        },
    },
    Target {
        name: "btrfs",
        about: "Том btrfs с чужого диска: суперблок, дерево кусков, узлы B-дерева.",
        seeds: || vec![seeds::btrfs_volume()],
        // Суперблок лежит по смещению 64 КиБ, за ним — системные куски и
        // корни деревьев; двести килобайт накрывают их с запасом.
        hot_bytes: 200 * 1024,
        run: |bytes| {
            let mut disk = View::new(bytes);
            // `detect` смотрит только на подпись, `mount` — на всё остальное.
            let _ = btrfs::detect(&mut disk, 0);
            let Ok(mut volume) = btrfs::Btrfs::mount(&mut disk, 0) else {
                return;
            };
            let Ok(root) = volume.root(&mut disk) else {
                return;
            };
            if let Ok(entries) = volume.list(&mut disk, &root) {
                for entry in entries {
                    if let Ok(Some(found)) = volume.lookup(&mut disk, &root, &entry.name) {
                        if let Ok(inode) = volume.inode(&mut disk, found.inode) {
                            let _ = volume.read_file(&mut disk, &inode);
                        }
                    }
                }
            }
        },
    },
    Target {
        name: "x509",
        about: "Сертификат с чужого сервера: DER, имена, сроки, ключ.",
        seeds: seeds::certificates,
        // Сертификат — вход целиком, порча уместна где угодно.
        hot_bytes: usize::MAX,
        run: |bytes| {
            let Ok(certificate) = x509::Certificate::parse(bytes) else {
                return;
            };
            // Разбор отдал структуру — значит дальше её начнут спрашивать, и
            // спрашивать будут поля, посчитанные по тем же байтам.
            let _ = certificate.valid_at(1_700_000_000);
            let _ = certificate.matches("example.com");
        },
    },
    Target {
        name: "x509-pem",
        about: "Список доверенных корней из /etc: разбор PEM и base64 внутри.",
        seeds: || vec![seeds::roots_pem()],
        hot_bytes: usize::MAX,
        run: |bytes| {
            // Текст, а не байты: PEM — это текст, и не-UTF-8 обязан быть
            // отказом разбора, а не паникой на `from_utf8`.
            let Ok(text) = core::str::from_utf8(bytes) else {
                return;
            };
            let mut out = vec![0u8; 256 * 1024];
            let _ = x509::Store::parse_pem(text, &mut out);
        },
    },
    Target {
        name: "tls",
        about: "Записи TLS с чужого сервера: оболочка записи и рукопожатие.",
        seeds: seeds::tls_records,
        hot_bytes: usize::MAX,
        run: |bytes| {
            let mut io = tls::Buffers::new();
            // Пустое хранилище корней: цепочка всё равно не сойдётся, а
            // проверяется здесь разбор — он идёт раньше проверки доверия.
            let roots = x509::Store::empty();
            let random = [0x5a; 96];
            let Ok(mut session) = tls::Session::new(&mut io, roots, "example.com", 1_700_000_000, &random)
            else {
                return;
            };
            let _ = session.feed(bytes);
        },
    },
    Target {
        name: "usb-hid",
        about: "Дескриптор отчёта чужого устройства ввода.",
        seeds: seeds::hid_descriptors,
        hot_bytes: usize::MAX,
        run: |bytes| {
            // Разбор дескриптора отказа не возвращает вовсе — он возвращает то,
            // что понял. Значит единственное утверждение о нём: он возвращается.
            let descriptor = usb_hid::parse(bytes);
            // И то, что разобрано, тут же применяется к отчёту: поля с
            // разрядностью и смещением из того же дескриптора складываются в
            // индекс по буферу отчёта, и вот это уже про чтение за границей.
            let report = [0x5au8; 16];
            if let Some(pointer) = &descriptor.pointer {
                let _ = pointer.decode(&report);
            }
            if let Some(keyboard) = &descriptor.keyboard {
                let _ = keyboard.decode(&report);
            }
        },
    },
    Target {
        name: "fpk",
        about: "Пакет, скачанный по сети: заголовок, манифест, список файлов.",
        seeds: || vec![seeds::fpk_package()],
        hot_bytes: usize::MAX,
        run: |bytes| {
            let Ok(header) = fpk::Header::parse(bytes) else {
                return;
            };
            // Манифест берётся по смещению и длине из заголовка — то есть по
            // двум числам, которые выбрал не разбирающий.
            let from = header.manifest_offset() as usize;
            let to = from.saturating_add(header.manifest_len as usize);
            if to > bytes.len() {
                return;
            }
            let Ok(manifest) = fpk::Manifest::parse(&header, &bytes[from..to]) else {
                return;
            };
            let _ = manifest.name();
            let _ = manifest.version();
            for file in manifest.files() {
                let _ = file;
            }
        },
    },
    Target {
        name: "clr-meta",
        about: "Сборка .NET неизвестного происхождения: PE, метаданные, таблицы.",
        seeds: || vec![seeds::pe_image()],
        hot_bytes: usize::MAX,
        run: |bytes| {
            // Три слоя, и каждый складывает смещения предыдущего: образ PE,
            // корень метаданных, таблицы. Сборка целиком — один вызов.
            let _ = clr_meta::Assembly::parse(bytes);
        },
    },
    Target {
        name: "http",
        about: "Запрос из сети и ответ чужого сервера: голова сообщения и путь в ней.",
        seeds: seeds::http_messages,
        hot_bytes: usize::MAX,
        run: |bytes| {
            // Голова разбирается и как запрос, и как ответ: сервер читает
            // первое, тот же сервер в роли обратного прокси — второе, и
            // испорченные байты приходят к обоим из одного и того же места —
            // из сети.
            let head = http::head_end(bytes).unwrap_or(bytes.len());
            let head = &bytes[..head];
            if let Ok(request) = http::Request::parse(head) {
                let _ = request.body();
                let _ = request.keep_alive();
                let (path, query) = request.split_target();
                let _ = query;
                // Путь — самое опасное место сервера: здесь чужая строка
                // становится именем файла. Буфер ровно такой же, какой заводит
                // сервер, — и разбор обязан либо уместиться в него, либо
                // отказать.
                let mut room = [0u8; 256];
                if let Some(clean) = http::path::normalize(path, &mut room) {
                    let _ = http::mime::of(clean);
                    let _ = http::path::under(clean, "/up");
                }
            }
            if let Ok(answer) = http::Response::parse(head) {
                let _ = answer.body(http::Method::Get);
                let _ = answer.body(http::Method::Head);
                let _ = answer.keep_alive();
                let _ = answer.head.value("location");
            }
        },
    },
];
