//! Пересборка образца тома btrfs — того самого, по которому идут тесты крейта
//! `btrfs`.
//!
//! # Зачем отдельная команда, если образ лежит в репозитории
//!
//! Затем, что иначе он превращается в данные, которые никто не может
//! воспроизвести. Файл в двести килобайт, про который известно только «его
//! кто-то когда-то сделал», — это не эталон, а магическая константа: через
//! полгода нельзя ни добавить в него файл, ни проверить, чем он создан.
//!
//! Команда держит рецепт рядом с результатом. Список того, что кладётся в том,
//! — это одновременно и список того, что проверяют тесты (`crates/btrfs/src/tests.rs`).
//!
//! # Почему это не часть сборки и не часть `check`
//!
//! Потому что `mkfs.btrfs` на Windows взять негде. Здесь он берётся из WSL — то
//! есть команда работает на машине разработчика, а тесты обязаны идти везде.
//! Поэтому образ и лежит в репозитории готовым: пересобирается он руками и
//! редко, а читается на каждом `cargo test -p btrfs`.
//!
//! Это же и единственная причина, по которой рецепт написан на `sh`, а не на
//! Rust: `mkfs.btrfs`, `mount` и `btrfs check` — программы Linux, и разговаривать
//! с ними всё равно придётся через оболочку Linux.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::paths;

/// Что кладётся в образец. Список повторяет таблицу в `crates/btrfs/src/tests.rs`
/// — и обязан меняться вместе с ней.
const RECIPE: &str = r#"
set -e
work=/tmp/freeos-btrfs-fixture
rm -rf "$work"
mkdir -p "$work/mnt"
cd "$work"

truncate -s 128M fixture.img
mkfs.btrfs -f -q -L FREEOS-FIXTURE -m single -d single \
    --nodesize 16384 --sectorsize 4096 \
    -U 11111111-2222-3333-4444-555555555555 fixture.img
mount -o loop fixture.img mnt
cd mnt

# Встроенный экстент: 17 байт лежат прямо в дереве, отдельного блока у них нет.
printf "hello from btrfs\n" > hello.txt

mkdir -p dir/sub
printf "small\n" > dir/sub/small.txt

# Четыре мегабайта повторяющегося узора. На диске они лежат как есть (сжатие
# выключено), а в gzip ужимаются почти в ничто — поэтому образец и помещается
# в репозиторий.
i=0; : > "$work/chunk64k"
while [ $i -lt 4096 ]; do printf "0123456789abcdef" >> "$work/chunk64k"; i=$((i+1)); done
i=0; : > dir/big.bin
while [ $i -lt 64 ]; do cat "$work/chunk64k" >> dir/big.bin; i=$((i+1)); done

# Дыра посередине: при NO_HOLES её не описывает ничто, экстента просто нет.
dd if=/dev/zero of=holes.bin bs=1 count=0 seek=1048576 2>/dev/null
printf "A%.0s" $(seq 1 4096) | dd of=holes.bin bs=4096 seek=0 conv=notrunc 2>/dev/null
printf "Z%.0s" $(seq 1 4096) | dd of=holes.bin bs=4096 seek=255 conv=notrunc 2>/dev/null

# 3000 байт: больше предела встраивания (2048), но меньше сектора.
printf "M%.0s" $(seq 1 3000) > mixed.bin

# Две тысячи файлов: дерево перестаёт помещаться в один лист, и обход начинает
# ходить через внутренние узлы. Без этого проверялась бы половина кода.
mkdir many
i=0
while [ $i -lt 2000 ]; do printf "file %04d\n" $i > many/$(printf "f%04d" $i); i=$((i+1)); done

# Предел длины имени в записи каталога.
printf "long name\n" > "$(printf 'n%.0s' $(seq 1 255))"

sync
cd "$work"
umount mnt

# Чужой верификатор по своему же образу: если mkfs и ядро оставили том
# противоречивым, тесты читателя проверяли бы не формат, а поломку.
btrfs check fixture.img
gzip -9 -c fixture.img > fixture.img.gz
"#;

/// Пересобрать `crates/btrfs/tests/fixture.img.gz`.
pub fn build() -> Result<()> {
    let distro = find_distro()?;
    say!("btrfs: образец собирается в WSL, дистрибутив «{distro}»");

    let output = Command::new("wsl")
        .args(["-d", &distro, "-u", "root", "-e", "sh", "-c", RECIPE])
        .output()
        .context("не запустился wsl — он вообще установлен?")?;
    let log = decode(&output.stdout) + &decode(&output.stderr);
    if !output.status.success() {
        bail!("образец не собрался:\n{log}");
    }
    // `btrfs check` печатает отчёт и при успехе — он и есть доказательство,
    // поэтому виден целиком, а не прячется за кодом возврата.
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        say!("  btrfs: {line}");
    }

    let target = paths::workspace_root().join("crates/btrfs/tests/fixture.img.gz");
    copy_out(&distro, "/tmp/freeos-btrfs-fixture/fixture.img.gz", &target)?;
    let size = std::fs::metadata(&target)?.len();
    say!("btrfs: {} ({size} байт)", target.display());
    Ok(())
}

/// Первый дистрибутив WSL, где есть `mkfs.btrfs`.
///
/// Если его нет нигде — пробуем поставить пакет тем менеджером, который в
/// дистрибутиве нашёлся. Это не самодеятельность ради удобства: рецепт без
/// `btrfs-progs` не выполним вовсе, и человеку всё равно пришлось бы набрать
/// ровно эту команду, прочитав подсказку.
fn find_distro() -> Result<String> {
    let listed = Command::new("wsl")
        .args(["-l", "-q"])
        .output()
        .context("не запустился wsl — он вообще установлен?")?;
    if !listed.status.success() {
        bail!("`wsl -l -q` отказал: WSL не установлен или не настроен");
    }
    let names: Vec<String> = decode_utf16(&listed.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect();
    if names.is_empty() {
        bail!("в WSL нет ни одного дистрибутива: поставьте любой (`wsl --install -d Ubuntu`)");
    }

    for name in &names {
        if has_mkfs(name) {
            return Ok(name.clone());
        }
    }

    let first = &names[0];
    say!("btrfs: в «{first}» нет btrfs-progs, ставлю");
    for install in [
        "apk add --no-cache btrfs-progs",
        "apt-get update && apt-get install -y btrfs-progs",
        "dnf install -y btrfs-progs",
    ] {
        let done = Command::new("wsl")
            .args(["-d", first, "-u", "root", "-e", "sh", "-c", install])
            .output();
        if matches!(done, Ok(ref out) if out.status.success()) && has_mkfs(first) {
            return Ok(first.clone());
        }
    }
    Err(anyhow!(
        "ни в одном дистрибутиве WSL нет mkfs.btrfs, и поставить его не вышло.\n\
         Поставьте btrfs-progs руками, например: wsl -d {first} -u root apk add btrfs-progs"
    ))
}

fn has_mkfs(distro: &str) -> bool {
    Command::new("wsl")
        .args(["-d", distro, "-u", "root", "-e", "sh", "-c", "command -v mkfs.btrfs"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Вынести файл из WSL наружу.
///
/// Через `cat`, а не через `/mnt/<буква>`: у разных дистрибутивов диски Windows
/// подмонтированы по-разному (`/mnt/c` у обычных, `/mnt/host/c` у того, что
/// приезжает с Docker Desktop), и угадывать это — лишний способ ошибиться.
fn copy_out(distro: &str, inside: &str, outside: &Path) -> Result<()> {
    let output = Command::new("wsl")
        .args(["-d", distro, "-u", "root", "-e", "cat", inside])
        .output()
        .context("не запустился wsl")?;
    if !output.status.success() {
        bail!("не вышло забрать {inside}: {}", decode(&output.stderr));
    }
    if let Some(parent) = outside.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(outside, &output.stdout)
        .with_context(|| format!("не пишется {}", outside.display()))?;
    Ok(())
}

fn decode(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

/// `wsl -l` печатает UTF-16, а всё остальное — обычный UTF-8.
fn decode_utf16(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}
