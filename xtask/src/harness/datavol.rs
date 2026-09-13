//! Том btrfs на диске сценария — глазами хоста, после того как гость погас.
//!
//! # Чего эта проверка не доказывает
//!
//! Том читает **наш** крейт `btrfs` — тот же, которым писало ядро. Совпадение
//! доказывает, что записанное дожило до диска и что читатель согласен с
//! писателем, но не то, что с ними согласен формат: `btrfs check` живёт в Linux,
//! а стенд обязан идти и там, где Linux нет. Чужой взгляд на тот же образ —
//! `cargo xtask btrfs-linux-check --image <образ>`, путь к образу шаг печатает.
//!
//! Нужна она всё равно. Гость может напечатать «записано» и ошибиться: например,
//! показывать новое из памяти, когда суперблок до диска не дошёл. Здесь читается
//! файл образа, а не память гостя, — и читается после выключения, то есть после
//! всего, что система успела или не успела довести до носителя.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use disk::gpt;

use super::scenarios::{Content, VolumeCheck};

/// Открыть раздел данных образа, обойти том со сверкой и сравнить файлы.
pub fn check(path: &Path, expect: &VolumeCheck) -> Result<()> {
    let data = std::fs::read(path).with_context(|| format!("не читается {}", path.display()))?;
    let mut dev =
        disk::MemDisk::from_vec(data).ok_or_else(|| anyhow!("длина образа не кратна сектору"))?;
    let table = gpt::read(&mut dev).map_err(|err| anyhow!("таблица разделов не читается: {err}"))?;
    let first_lba = table
        .find(gpt::FREEOS_DATA_TYPE)
        .ok_or_else(|| anyhow!("на диске нет раздела данных"))?
        .first_lba;

    let mut fs = btrfs::Btrfs::mount(&mut dev, first_lba)
        .map_err(|err| anyhow!("том не монтируется: {err}"))?;
    // Сначала обход со сверкой: файл, прочитанный из противоречивого тома, мог
    // сойтись случайно, и совпадение содержимого ничего бы не значило.
    let report = fs.check(&mut dev).map_err(|err| anyhow!("проверка тома отказала: {err}"))?;
    if !report.is_clean() {
        bail!("проверка тома нашла: {:?}", report.problems);
    }

    for (name, content) in expect.files {
        let node = fs.resolve(&mut dev, name).map_err(|err| anyhow!("{name}: {err}"))?;
        let bytes = fs.read_file(&mut dev, &node).map_err(|err| anyhow!("{name}: {err}"))?;
        let want = match content {
            Content::Text(text) => text.as_bytes().to_vec(),
            Content::Repeat(byte, count) => vec![*byte; *count],
        };
        if bytes != want {
            bail!(
                "{name}: на диске {} байт и не те, что записывал гость (ожидалось {} байт)",
                bytes.len(),
                want.len()
            );
        }
    }
    for name in expect.gone {
        match fs.resolve(&mut dev, name) {
            Err(btrfs::Error::NotFound) => {}
            Ok(_) => bail!("{name}: гость это удалил, а на диске оно есть"),
            Err(err) => bail!("{name}: {err}"),
        }
    }

    say!(
        "             том на диске: поколение {}, {} файлов, {} секторов сверено, \
         {} файлов совпали, {} удалённых нет",
        fs.generation(),
        report.files,
        report.sectors,
        expect.files.len(),
        expect.gone.len()
    );
    // Образ живёт до следующего сценария с томом btrfs на этой архитектуре: тот
    // развернёт образец заново по тому же пути. Проверенный так Linux видит
    // нетронутый образец и честно не находит записанного — так и случилось
    // 2026-09-13, когда за `btrfs-write` в той же цепочке шёл `btrfs-read`.
    say!(
        "             чужая проверка того же образа (до следующего сценария btrfs на этой \
         архитектуре): cargo xtask btrfs-linux-check --image {}",
        path.display()
    );
    Ok(())
}
