//! Тесты exFAT на образе, собранном чужой реализацией: `mkfs.exfat -L Z7S
//! -c 32K` и `exfat-fuse` (exfatprogs 1.2.2, fuse-exfat 1.4.0), разметка GPT
//! как у диска Романа — один раздел «Basic data» с сектора 2048.
//!
//! Записанное нами проверяется своим же чтением здесь и чужим `fsck.exfat`
//! снаружи: `EXFAT_OUT=<файл> cargo test -p disk exfat` сохраняет образ.

use std::io::Read as _;
use std::string::{String, ToString};
use std::vec::Vec;
use std::{format, vec};

use super::*;
use crate::MemDisk;
use crate::crc32::crc32;

const FIXTURE: &[u8] = include_bytes!("../../testdata/exfat-z7s.img.gz");
const BASE: u64 = 2048;
const LONG: &str = "Длинное имя файла с кириллицей и пробелами — проверка exFAT.txt";
const NOW: u64 = 1_795_000_000;

fn image() -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(FIXTURE).read_to_end(&mut out).unwrap();
    out
}

fn open(bytes: Vec<u8>) -> Volume<MemDisk> {
    let sectors = (bytes.len() / 512) as u64;
    Volume::mount(MemDisk::from_vec(bytes).unwrap(), BASE, sectors - BASE - 33, false).unwrap()
}

fn path(volume: &mut Volume<MemDisk>, path: &str) -> Node {
    let mut node = volume.root();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        node = volume.lookup(&node, part).unwrap();
    }
    node
}

fn cat(volume: &mut Volume<MemDisk>, p: &str) -> Vec<u8> {
    let node = path(volume, p);
    read_all(volume, &node)
}

fn read_all(volume: &mut Volume<MemDisk>, node: &Node) -> Vec<u8> {
    let mut buf = vec![0u8; node.size() as usize];
    let mut cursor = Cursor::default();
    // Кусками неровной длины — чтобы пересекать границы кластеров.
    let mut done = 0;
    while done < buf.len() {
        let take = (buf.len() - done).min(100_003);
        let got = volume.read(node, done as u64, &mut buf[done..done + take], &mut cursor).unwrap();
        assert_eq!(got, take);
        done += got;
    }
    buf
}

#[test]
fn reads_foreign_volume() {
    let mut v = open(image());
    assert_eq!(v.label(), "Z7S");
    assert!(!v.read_only());
    assert_eq!(v.cluster_size(), 32 * 1024);
    let root = v.root();
    let mut names: Vec<String> = v.list(&root).unwrap().into_iter().map(|n| n.name).collect();
    names.sort();
    assert_eq!(names, ["D2RLAN", "empty.txt", "frag_a.bin", "frag_b.bin", "many", "Документы"]);

    let data = path(&mut v, "/D2RLAN/Data/data.bin");
    assert_eq!(data.size(), 9_437_307);
    assert_eq!(crc32(&read_all(&mut v, &data)), 0x5fb2_b681);

    let frag = path(&mut v, "/frag_a.bin");
    assert_eq!(frag.size(), 640_000);
    std::println!("frag_a contiguous: {}, data.bin contiguous: {}", frag.data.contiguous, data.data.contiguous);
    assert_eq!(crc32(&read_all(&mut v, &frag)), 0x1e2a_ddfe);

    // Регистр не важен, и для кириллицы тоже (таблица прописных с тома).
    let long = path(&mut v, &format!("/d2rlan/{}", LONG.to_uppercase()));
    assert_eq!(long.name, LONG);
    assert_eq!(read_all(&mut v, &long), "Привет из exFAT!\n".as_bytes());

    let many = path(&mut v, "/many");
    let list = v.list(&many).unwrap();
    assert_eq!(list.len(), 400);
    assert!(many.size() > v.cluster_size());
    let f = path(&mut v, "/many/file399.txt");
    assert_eq!(read_all(&mut v, &f), b"file 399\n");
    assert_eq!(path(&mut v, "/empty.txt").size(), 0);
    let readme = path(&mut v, "/Документы/readme.txt");
    assert_eq!(read_all(&mut v, &readme), b"readme on Z7S\n");
    assert!(readme.modified > 1_700_000_000, "time {}", readme.modified);
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ (i >> 9) as u8).collect()
}

#[test]
fn writes_and_reads_back() {
    let mut v = open(image());
    let (_, free_before) = v.space();
    let root = v.root();

    // Файл с длинным именем в корне: три записи имени.
    let name = "Новый файл, записанный FossAi — с очень длинным именем.bin";
    let file = v.create(&root, name, false, NOW).unwrap();
    let body = pattern(300_000, 7);
    let file = v.write(&file, 0, &body, NOW).unwrap();
    assert!(file.data.contiguous);
    // Дописать в конец и в середину.
    let tail = pattern(70_000, 9);
    let file = v.write(&file, 300_000, &tail, NOW).unwrap();
    let file = v.write(&file, 1000, b"MIDDLE", NOW).unwrap();
    let mut want = body.clone();
    want.extend_from_slice(&tail);
    want[1000..1006].copy_from_slice(b"MIDDLE");
    assert_eq!(read_all(&mut v, &file), want);

    // Каталог, в нём много файлов — каталог перерастает кластер.
    let dir = v.create(&root, "Сохранения", true, NOW).unwrap();
    for i in 0..500 {
        let f = v.create(&dir, &format!("save {i:03}.d2s"), false, NOW).unwrap();
        v.write(&f, 0, format!("save number {i}\n").as_bytes(), NOW).unwrap();
    }
    let dir = v.refresh(&dir).unwrap();
    assert!(dir.size() > v.cluster_size());
    assert_eq!(v.list(&dir).unwrap().len(), 500);

    // Два файла впритык; первый растёт — становится цепочкой FAT.
    let a = v.create(&root, "a.bin", false, NOW).unwrap();
    let a = v.write(&a, 0, &pattern(32 * 1024, 1), NOW).unwrap();
    let b = v.create(&root, "b.bin", false, NOW).unwrap();
    let _b = v.write(&b, 0, &pattern(10, 2), NOW).unwrap();
    let a = v.write(&a, 32 * 1024, &pattern(100_000, 3), NOW).unwrap();
    let mut want_a = pattern(32 * 1024, 1);
    want_a.extend_from_slice(&pattern(100_000, 3));
    assert_eq!(read_all(&mut v, &a), want_a);

    // Дыра: запись далеко за концом — между ними нули.
    let sparse = v.create(&root, "sparse.bin", false, NOW).unwrap();
    let sparse = v.write(&sparse, 200_000, b"END", NOW).unwrap();
    let got = read_all(&mut v, &sparse);
    assert_eq!(got.len(), 200_003);
    assert!(got[..200_000].iter().all(|b| *b == 0));
    // Длина больше без записи — дальше действительной длины нули.
    let sparse = v.truncate(&sparse, 500_000, NOW).unwrap();
    assert_eq!(sparse.valid, 200_003);
    assert_eq!(read_all(&mut v, &sparse)[200_003..].iter().filter(|b| **b != 0).count(), 0);
    let sparse = v.truncate(&sparse, 10, NOW).unwrap();
    assert_eq!(read_all(&mut v, &sparse), vec![0u8; 10]);

    // Переименование с переносом и замена существующего.
    let docs = path(&mut v, "/Документы");
    v.rename(&root, "b.bin", &docs, "перенесён.bin").unwrap();
    assert!(matches!(v.lookup(&root, "b.bin"), Err(Error::NotFound)));
    let moved = path(&mut v, "/Документы/перенесён.bin");
    assert_eq!(read_all(&mut v, &moved), pattern(10, 2));
    v.rename(&docs, "перенесён.bin", &docs, "readme.txt").unwrap();
    assert_eq!(cat(&mut v, "/Документы/README.TXT"), pattern(10, 2));
    // Только регистр.
    v.rename(&root, "a.bin", &root, "A.BIN").unwrap();
    assert_eq!(path(&mut v, "/a.bin").name, "A.BIN");

    // Удаление: файл, непустой каталог отказывает, пустой уходит.
    v.remove(&root, "sparse.bin", false).unwrap();
    assert_eq!(v.remove(&root, "Сохранения", true), Err(Error::NotEmpty));
    let empty = v.create(&root, "пусто", true, NOW).unwrap();
    let _ = empty;
    v.remove(&root, "пусто", true).unwrap();
    assert_eq!(v.create(&root, "bad?name", false, NOW).unwrap_err(), Error::BadName);
    assert_eq!(v.create(&root, "a.BIN", false, NOW).unwrap_err(), Error::Exists);

    // Корень перерастает свой кластер.
    for i in 0..400 {
        v.create(&root, &format!("root file {i}"), false, NOW).unwrap();
    }
    assert!(v.root().size() > v.cluster_size());
    v.clean().unwrap();
    let (_, free_after) = v.space();
    assert!(free_after < free_before);

    // Открыть заново — всё на месте, счёт свободного тот же.
    let bytes = v.into_device().into_vec();
    let mut v = open(bytes.clone());
    assert_eq!(v.space().1, free_after);
    assert!(!v.was_dirty());
    assert_eq!(cat(&mut v, &format!("/{name}")), want);
    assert_eq!(cat(&mut v, "/A.bin"), want_a);
    assert_eq!(cat(&mut v, "/Сохранения/save 499.d2s"), b"save number 499\n");
    assert_eq!(v.list(&v.root()).unwrap().len(), 6 + 3 + 400);
    assert_eq!(crc32(&cat(&mut v, "/D2RLAN/Data/data.bin")), 0x5fb2_b681);
    assert_eq!(cat(&mut v, "/frag_b.bin").len(), 640_000);

    if let Ok(out) = std::env::var("EXFAT_OUT") {
        std::fs::write(out, &bytes).unwrap();
    }
}

#[test]
fn full_volume_says_no_space() {
    let mut v = open(image());
    let root = v.root();
    let f = v.create(&root, "big", false, NOW).unwrap();
    let (_, free) = v.space();
    let err = v.truncate(&f, free + v.cluster_size(), NOW).unwrap_err();
    assert_eq!(err, Error::NoSpace);
    let f = v.truncate(&f, free, NOW).unwrap();
    assert_eq!(v.space().1, 0);
    assert_eq!(v.create(&root, "x", true, NOW).unwrap_err(), Error::NoSpace);
    v.remove(&root, "big", false).unwrap();
    let _ = f;
    assert_eq!(v.space().1, free);
}

#[test]
fn time_round_trip() {
    for t in [315_532_800u64, 1_795_000_000, 1_700_000_001] {
        let (stamp, ms, utc) = encode_time(t);
        assert_eq!(decode_time(stamp, ms, utc), t);
    }
    // 2026-11-20 12:00:00 +03:00 (Москва) == 09:00 UTC.
    let stamp = (46u32 << 25) | (11 << 21) | (20 << 16) | (12 << 11);
    assert_eq!(decode_time(stamp, 0, 0x80 | 12), 1_795_165_200);
    let _ = String::new().to_string();
}
