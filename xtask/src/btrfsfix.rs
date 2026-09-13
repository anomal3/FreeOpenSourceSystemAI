//! btrfs и Linux: пересборка образца тома и проверка нашего `mkfs` и писателя
//! чужими руками.
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
//! # Проверка своего `mkfs` и писателя
//!
//! `cargo xtask btrfs-linux-check` идёт в обратную сторону: том создаёт и
//! наполняет наш код, а принимает Linux. Четыре круга, и в каждом `btrfs check`
//! смотрит на том дважды — до монтирования и после:
//!
//! 1. Наш [`btrfs::format`] и две транзакции [`btrfs::Writer`] → ядро сверяет
//!    всё записанное, пишет своё и удаляет одно наше.
//! 2. Наш читатель видит записанное Linux → наш писатель дописывает в этот том
//!    → ядро сверяет и наше, и своё.
//! 3. Наша запись поверх (своего экстента и экстента Linux), укорачивание,
//!    удаление, переименование → ядро сверяет и правит поверх, разрезая наш
//!    экстент.
//! 4. Наша правка рядом с этим разрезом → ядро сверяет.
//!
//! Тест в `cargo test` доказал бы только, что писатель согласен с читателем;
//! здесь с ними согласен формат, и в обе стороны.
//!
//! # Один вызов WSL на круг
//!
//! Образ уходит в WSL стандартным вводом, возвращается — дескриптором 3, а всё,
//! что печатают программы, идёт в поток ошибок и становится журналом. Так
//! сделано не из любви к трюкам: у дистрибутива `docker-desktop` рабочий каталог
//! бывает только в `/dev/shm` (см. [`WORKDIR`]), а его содержимое **не переживает
//! паузы** между вызовами `wsl` — проверено 2026-09-13, файл с суммой, записанный
//! одним вызовом, следующий уже не нашёл. Эталонное содержимое поэтому
//! передаётся суммами `sha256`, а не файлами.
//!
//! # Почему это не часть сборки и не часть `check`
//!
//! Потому что `mkfs.btrfs` на Windows взять негде. Здесь он берётся из WSL — то
//! есть команды работают на машине разработчика, а тесты обязаны идти везде.
//! Поэтому образ и лежит в репозитории готовым: пересобирается он руками и
//! редко, а читается на каждом `cargo test -p btrfs`.
//!
//! Это же и единственная причина, по которой рецепты написаны на `sh`, а не на
//! Rust: `mkfs.btrfs`, `mount` и `btrfs check` — программы Linux, и разговаривать
//! с ними всё равно придётся через оболочку Linux.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use osupdate::index::build::hash;

use crate::paths;

/// Рабочий каталог внутри WSL и разделение потоков.
///
/// Обычно каталог в `/tmp`, но не всегда: 2026-09-13 WSL сообщил, что диск
/// дистрибутива `docker-desktop` не смонтировался, и поднял корень **только на
/// чтение**. `mkfs.btrfs` в таком `/tmp` отказывает на обнулении хвоста образа,
/// и выглядит это как «I/O error» самого `mkfs`. `/dev/shm` — память, от диска
/// дистрибутива не зависит, и места в нём хватает на образ в сотню мегабайт.
///
/// `exec 3>&1 1>&2`: результат рецепт пишет в дескриптор 3, всё остальное
/// уходит в журнал.
const WORKDIR: &str = r#"
exec 3>&1 1>&2
work=/tmp/freeos-btrfs
if ! mkdir -p "$work" 2>/dev/null || ! touch "$work/.writable" 2>/dev/null; then
    work=/dev/shm/freeos-btrfs
    mkdir -p "$work"
fi
must() { "$@" || { echo "не сошлось: $*"; exit 1; }; }
same() { must [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$2" ]; }
"#;

/// Что кладётся в образец. Список повторяет таблицу в `crates/btrfs/src/tests.rs`
/// — и обязан меняться вместе с ней.
const RECIPE: &str = r#"
set -e
work="$work/fixture"
rm -rf "$work"
mkdir -p "$work/mnt"
cd "$work"
trap 'umount mnt 2>/dev/null || true' EXIT

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
gzip -9 -c fixture.img >&3
"#;

/// Общее начало кругов проверки: принять образ, проверить, смонтировать.
///
/// Жалобы ядра ищутся только среди строк, появившихся после монтирования:
/// журнал общий для всего дистрибутива, а очищать его ради своей проверки —
/// хозяйничать в чужом.
const CHECK_PRELUDE: &str = r#"
set -e
work="$work/linux-check"
rm -rf "$work"
mkdir -p "$work/mnt"
cd "$work"
trap 'umount mnt 2>/dev/null || true' EXIT
cat > v.img
echo "--- btrfs check: @STAGE@"
btrfs check v.img
before=$(dmesg | wc -l)
mount -o loop v.img mnt
"#;

const CHECK_COMPLAINTS: &str = r#"
sync
umount mnt
if dmesg | tail -n +$((before + 1)) | grep -iE "btrfs.*(error|warn|corrupt|critical)"; then
    echo "ядро Linux жаловалось на том"
    exit 1
fi
"#;

/// Первый круг: Linux сверяет записанное нами, пишет своё и удаляет одно наше.
const ROUND_ONE: &str = r#"
must [ "$(cat mnt/etc/hostname)" = "freeos" ]
must [ -f mnt/etc/empty ]
must [ ! -s mnt/etc/empty ]
same mnt/etc/mixed.bin @MIXED@
same mnt/home/roman/big.bin @BIG@
same mnt/twenty.bin @TWENTY@
must [ "$(ls mnt/many | wc -l)" = "300" ]
must [ "$(cat mnt/many/f299)" = "file 299" ]
must [ "$(cat mnt/second.txt)" = "second transaction" ]
must [ "$(stat -c '%a %u %g' mnt/home/roman)" = "700 1000 1000" ]
must [ "$(stat -c '%a %u %g %s' mnt/etc/empty)" = "600 1000 1000 0" ]
echo "--- Linux прочитал всё, что записал наш писатель"

printf "linux was here\n" > mnt/linux.txt
mkdir mnt/linuxdir
head -c 3000000 /dev/urandom > mnt/linuxdir/rand.bin
echo "RAND $(sha256sum mnt/linuxdir/rand.bin | cut -d' ' -f1)"
rm mnt/etc/mixed.bin
btrfs filesystem df mnt
"#;

/// Второй круг: наш писатель дописал в том, который менял Linux.
const ROUND_TWO: &str = r#"
same mnt/again/again.bin @AGAIN@
must [ "$(cat mnt/linuxdir/ours-in-linux-dir.txt)" = "ours, next to linux" ]
same mnt/linuxdir/rand.bin @RAND@
must [ "$(cat mnt/linux.txt)" = "linux was here" ]
same mnt/twenty.bin @TWENTY@
must [ ! -e mnt/etc/mixed.bin ]
echo "--- Linux прочитал и своё, и дописанное нами поверх"
"#;

/// Третий круг: Linux сверяет наши правки и правит поверх них сам.
const ROUND_THREE: &str = r#"
same mnt/twenty.bin @TWENTY_EDITED@
same mnt/linuxdir/rand.bin @RAND_EDITED@
same mnt/home/roman/big.bin @BIG_TRUNCATED@
must [ ! -e mnt/second.txt ]
must [ ! -e mnt/many/f000 ]
must [ "$(cat mnt/etc/renamed-from-many)" = "file 000" ]
must [ "$(ls mnt/many | wc -l)" = "299" ]
must [ ! -e mnt/gone ]
must [ "$(cat mnt/etc/hostname)" = "freeos-edited" ]
echo "--- Linux прочитал наши правки"

# Четыре байта посреди нашего экстента: ядро разрежет его, и у остатков будет
# общая ссылка со счётчиком 2 — на ней проверяется четвёртый круг.
printf "ZZZZ" | dd of=mnt/twenty.bin bs=1 seek=9000000 conv=notrunc 2>/dev/null
rm mnt/linuxdir/rand.bin
mv mnt/etc/renamed-from-many mnt/many/f000
truncate -s 100 mnt/home/roman/big.bin
"#;

/// Четвёртый круг: наша правка рядом с разрезом, который сделал Linux.
const ROUND_FOUR: &str = r#"
same mnt/twenty.bin @TWENTY_FINAL@
same mnt/home/roman/big.bin @BIG_FINAL@
must [ -e mnt/many/f000 ]
echo "--- Linux прочитал нашу правку поверх своей"
"#;

/// Том после сценария `btrfs-write`: Linux сверяет то, что записало ядро FreeOS.
///
/// Список обязан совпадать с шагом `DataVolume` сценария (`harness/scenarios.rs`):
/// там то же сверяет наш читатель, здесь — чужой.
const SCENARIO_ROUND: &str = r#"
must [ "$(cat mnt/notes/first.txt)" = "written by the freeos kernel" ]
must [ "$(cat mnt/hello.txt)" = "replaced by freeos" ]
same mnt/notes/mixed.bin @MIXED@
same mnt/mixed-renamed.bin @MIXED@
must [ ! -e mnt/mixed.bin ]
must [ ! -e mnt/many/f0000 ]
must [ ! -e mnt/dir/sub ]
must [ "$(cat mnt/many/f1999)" = "file 1999" ]
must [ "$(ls mnt/many | wc -l)" = "1999" ]
same mnt/dir/big.bin @BIG@
echo "--- Linux прочитал записанное ядром FreeOS, и нетронутое осталось целым"
"#;

/// Конец любого круга: проверить том после Linux и отдать его обратно.
const CHECK_AFTER: &str = r#"
echo "--- btrfs check: после того как Linux смонтировал и отпустил том"
btrfs check v.img
cat v.img >&3
"#;

/// Пересобрать `crates/btrfs/tests/fixture.img.gz`.
pub fn build() -> Result<()> {
    let distro = find_distro()?;
    say!("btrfs: образец собирается в WSL, дистрибутив «{distro}»");

    let (packed, log) = run_in(&distro, RECIPE, None).context("образец не собрался")?;
    // `btrfs check` печатает отчёт и при успехе — он и есть доказательство,
    // поэтому виден целиком, а не прячется за кодом возврата.
    show(&log);
    if packed.is_empty() {
        bail!("рецепт не вернул образец");
    }
    let target = paths::workspace_root().join("crates/btrfs/tests/fixture.img.gz");
    std::fs::write(&target, &packed).with_context(|| format!("не пишется {}", target.display()))?;
    say!("btrfs: {} ({} байт)", target.display(), packed.len());
    Ok(())
}

/// Создать том своим `mkfs`, наполнить своим писателем и отдать на суд Linux.
pub fn linux_check() -> Result<()> {
    let distro = find_distro()?;
    let dir = paths::build_dir().join("btrfs-linux-check");
    std::fs::create_dir_all(&dir)?;

    // --- круг первый ------------------------------------------------------------
    // Наименьший том, который создаёт наш mkfs: на нём ошибка в арифметике
    // размеров ближе всего к границе, а в `/dev/shm` он помещается всегда.
    let sectors = btrfs::MIN_VOLUME_BYTES / 512;
    let mut disk = disk::MemDisk::new(sectors).ok_or_else(|| anyhow!("не хватило памяти под образ"))?;
    let options = btrfs::FormatOptions { label: "FREEOS-CHECK", uuid: *b"FreeOS-btrfs-50b", time: now() };
    btrfs::format(&mut disk, 0, sectors, &options).map_err(broken("наш mkfs отказал"))?;

    let mixed = pattern(3000, 1);
    let big = pattern(1024 * 1024 + 123, 2);
    let twenty = pattern(20 * 1024 * 1024, 3);
    {
        let fail = broken("наш писатель отказал");
        let mut writer = btrfs::Writer::open(&mut disk, 0).map_err(&fail)?;
        let etc = writer.create_directory(256, "etc", &attrs(0o755)).map_err(&fail)?;
        let home = writer.create_directory(256, "home", &attrs(0o755)).map_err(&fail)?;
        let roman = writer.create_directory(home, "roman", &attrs(0o700)).map_err(&fail)?;
        writer.create_file(&mut disk, etc, "hostname", b"freeos\n", &attrs(0o644)).map_err(&fail)?;
        writer.create_file(&mut disk, etc, "empty", b"", &attrs(0o600)).map_err(&fail)?;
        writer.create_file(&mut disk, etc, "mixed.bin", &mixed, &attrs(0o644)).map_err(&fail)?;
        writer.create_file(&mut disk, roman, "big.bin", &big, &attrs(0o644)).map_err(&fail)?;
        // Двадцать мегабайт не помещаются в начальный кусок данных: писателю
        // приходится завести новый.
        writer.create_file(&mut disk, 256, "twenty.bin", &twenty, &attrs(0o644)).map_err(&fail)?;
        let many = writer.create_directory(256, "many", &attrs(0o755)).map_err(&fail)?;
        for number in 0..300 {
            let text = format!("file {number:03}\n");
            writer
                .create_file(&mut disk, many, &format!("f{number:03}"), text.as_bytes(), &attrs(0o644))
                .map_err(&fail)?;
        }
        writer.commit(&mut disk).map_err(&fail)?;
        writer
            .create_file(&mut disk, 256, "second.txt", b"second transaction\n", &attrs(0o644))
            .map_err(&fail)?;
        writer.commit(&mut disk).map_err(&fail)?;
    }
    let ours = dir.join("ours.img");
    std::fs::write(&ours, disk.as_bytes()).with_context(|| format!("не пишется {}", ours.display()))?;
    say!("btrfs: наш mkfs и писатель создали том на 128 МиБ, проверяет «{distro}»");

    let script = format!("{CHECK_PRELUDE}{ROUND_ONE}{CHECK_COMPLAINTS}\necho \"--- btrfs check: после записи и удаления ядром Linux\"\nbtrfs check v.img\ncat v.img >&3\n")
        .replace("@STAGE@", "наш mkfs и две транзакции нашего писателя")
        .replace("@MIXED@", &hex(&hash(&mixed)))
        .replace("@BIG@", &hex(&hash(&big)))
        .replace("@TWENTY@", &hex(&hash(&twenty)));
    let (after_linux, log) = run_in(&distro, &script, Some(&ours)).context("Linux не принял наш том")?;
    show(&log);
    let rand = log
        .lines()
        .find_map(|line| line.strip_prefix("RAND "))
        .map(str::trim)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("первый круг не назвал сумму файла Linux"))?;

    // --- круг второй ------------------------------------------------------------
    let mut disk = disk::MemDisk::from_vec(after_linux).ok_or_else(|| anyhow!("Linux вернул образ не кратный сектору"))?;
    {
        let fail = broken("наш читатель на томе после Linux");
        let mut fs = btrfs::Btrfs::mount(&mut disk, 0).map_err(&fail)?;
        let node = fs.resolve(&mut disk, "/linux.txt").map_err(&fail)?;
        if fs.read_file(&mut disk, &node).map_err(&fail)? != b"linux was here\n" {
            bail!("наш читатель прочитал файл Linux не тем");
        }
        let report = fs.check(&mut disk).map_err(&fail)?;
        if !report.is_clean() {
            bail!("наша проверка тома после Linux нашла {:?}", report.problems);
        }
        say!(
            "btrfs: наш читатель видит записанное Linux: поколение {}, {} файлов, {} секторов сверено",
            fs.generation(),
            report.files,
            report.sectors
        );
    }
    let again = pattern(500_000, 7);
    {
        let fail = broken("наш писатель поверх Linux отказал");
        let mut writer = btrfs::Writer::open(&mut disk, 0).map_err(&fail)?;
        let dir = writer.create_directory(256, "again", &attrs(0o755)).map_err(&fail)?;
        writer.create_file(&mut disk, dir, "again.bin", &again, &attrs(0o644)).map_err(&fail)?;
        let linuxdir = writer.resolve("/linuxdir").map_err(&fail)?;
        writer
            .create_file(&mut disk, linuxdir, "ours-in-linux-dir.txt", b"ours, next to linux\n", &attrs(0o644))
            .map_err(&fail)?;
        writer.commit(&mut disk).map_err(&fail)?;
    }
    let script = format!("{CHECK_PRELUDE}{ROUND_TWO}{CHECK_COMPLAINTS}{CHECK_AFTER}")
        .replace("@STAGE@", "наш писатель поверх тома, который менял Linux")
        .replace("@AGAIN@", &hex(&hash(&again)))
        .replace("@RAND@", &rand)
        .replace("@TWENTY@", &hex(&hash(&twenty)));
    let after_two = round(&distro, &dir, "ours-again.img", &disk, &script, "Linux не принял вторую запись")?;

    // --- круг третий: правки поверх ---------------------------------------------
    let mut disk = disk::MemDisk::from_vec(after_two).ok_or_else(|| anyhow!("Linux вернул образ не кратный сектору"))?;
    let fail = broken("наш писатель: правки поверх");
    let mut rand_edited = {
        let mut fs = btrfs::Btrfs::mount(&mut disk, 0).map_err(&fail)?;
        let node = fs.resolve(&mut disk, "/linuxdir/rand.bin").map_err(&fail)?;
        fs.read_file(&mut disk, &node).map_err(&fail)?
    };
    let patch = pattern(70_000, 21);
    let mut twenty_edited = twenty.clone();
    twenty_edited[9_000_001..9_070_001].copy_from_slice(&patch);
    let rand_patch = pattern(12_345, 22);
    rand_edited[1_500_000..1_512_345].copy_from_slice(&rand_patch);
    rand_edited.extend_from_slice(b"appended by freeos");
    let big_truncated = big[..5000].to_vec();
    {
        let time = now();
        let mut writer = btrfs::Writer::open(&mut disk, 0).map_err(&fail)?;
        // Посреди нашего экстента: он делится на остатки с общей ссылкой.
        let node = writer.resolve("/twenty.bin").map_err(&fail)?;
        writer.write_at(&mut disk, node, 9_000_001, &patch, time).map_err(&fail)?;
        // Экстенты этого файла сделало ядро Linux.
        let node = writer.resolve("/linuxdir/rand.bin").map_err(&fail)?;
        writer.write_at(&mut disk, node, 1_500_000, &rand_patch, time).map_err(&fail)?;
        writer.write_at(&mut disk, node, 3_000_000, b"appended by freeos", time).map_err(&fail)?;
        let node = writer.resolve("/home/roman/big.bin").map_err(&fail)?;
        writer.truncate(&mut disk, node, 5000, time).map_err(&fail)?;
        writer.unlink(256, "second.txt", time).map_err(&fail)?;
        let many = writer.resolve("/many").map_err(&fail)?;
        let etc = writer.resolve("/etc").map_err(&fail)?;
        writer.rename(many, "f000", etc, "renamed-from-many", time).map_err(&fail)?;
        let gone = writer.create_directory(256, "gone", &attrs(0o755)).map_err(&fail)?;
        writer.create_file(&mut disk, gone, "tmp", b"tmp", &attrs(0o644)).map_err(&fail)?;
        writer.unlink(gone, "tmp", time).map_err(&fail)?;
        writer.remove_directory(256, "gone", time).map_err(&fail)?;
        let node = writer.resolve("/etc/hostname").map_err(&fail)?;
        writer.write_at(&mut disk, node, 0, b"freeos-edited\n", time).map_err(&fail)?;
        writer.commit(&mut disk).map_err(&fail)?;
    }
    let script = format!("{CHECK_PRELUDE}{ROUND_THREE}{CHECK_COMPLAINTS}{CHECK_AFTER}")
        .replace("@STAGE@", "наша запись поверх, укорачивание, удаление и переименование")
        .replace("@TWENTY_EDITED@", &hex(&hash(&twenty_edited)))
        .replace("@RAND_EDITED@", &hex(&hash(&rand_edited)))
        .replace("@BIG_TRUNCATED@", &hex(&hash(&big_truncated)));
    let after_three = round(&distro, &dir, "ours-edited.img", &disk, &script, "Linux не принял наши правки")?;

    // --- круг четвёртый: наша правка рядом с разрезом, который сделал Linux -------
    // Здесь у экстента общая ссылка со счётчиком 2, и её обязан пережить вывод
    // ссылок при фиксации.
    let mut disk = disk::MemDisk::from_vec(after_three).ok_or_else(|| anyhow!("Linux вернул образ не кратный сектору"))?;
    let fail = broken("наш писатель поверх правок Linux");
    let mut twenty_final = twenty_edited;
    twenty_final[9_000_000..9_000_004].copy_from_slice(b"ZZZZ");
    twenty_final[8_999_990..9_000_000].copy_from_slice(b"QQQQQQQQQQ");
    let mut big_final = big_truncated[..100].to_vec();
    big_final[50..70].copy_from_slice(b"after linux truncate");
    {
        let time = now();
        let mut writer = btrfs::Writer::open(&mut disk, 0).map_err(&fail)?;
        let node = writer.resolve("/twenty.bin").map_err(&fail)?;
        writer.write_at(&mut disk, node, 8_999_990, b"QQQQQQQQQQ", time).map_err(&fail)?;
        let node = writer.resolve("/home/roman/big.bin").map_err(&fail)?;
        writer.write_at(&mut disk, node, 50, b"after linux truncate", time).map_err(&fail)?;
        writer.commit(&mut disk).map_err(&fail)?;
    }
    let script = format!("{CHECK_PRELUDE}{ROUND_FOUR}{CHECK_COMPLAINTS}{CHECK_AFTER}")
        .replace("@STAGE@", "наша правка рядом с разрезом, который сделал Linux")
        .replace("@TWENTY_FINAL@", &hex(&hash(&twenty_final)))
        .replace("@BIG_FINAL@", &hex(&hash(&big_final)));
    round(&distro, &dir, "ours-final.img", &disk, &script, "Linux не принял правку поверх своей")?;

    say!("btrfs: восемь раз btrfs check без нареканий — Linux и наш код читают и правят записанное друг другом");
    Ok(())
}

/// Отдать Linux диск, в который писало ядро FreeOS (сценарий `btrfs-write`).
///
/// Раздел вырезается на хосте нашим `disk::gpt`: смещение раздела внутри
/// образа — не то, о чём стоит договариваться с `losetup` в чужой системе.
pub fn scenario_check(image: &Path) -> Result<()> {
    let distro = find_distro()?;
    let data = std::fs::read(image).with_context(|| format!("не читается {}", image.display()))?;
    let mut dev = disk::MemDisk::from_vec(data).ok_or_else(|| anyhow!("длина образа не кратна сектору"))?;
    let table = disk::gpt::read(&mut dev).map_err(|err| anyhow!("таблица разделов не читается: {err}"))?;
    let part = table
        .find(disk::gpt::FREEOS_DATA_TYPE)
        .ok_or_else(|| anyhow!("на диске {} нет раздела данных", image.display()))?;
    let first = part.first_lba as usize * disk::DEFAULT_SECTOR_SIZE;
    let last = (part.last_lba as usize + 1) * disk::DEFAULT_SECTOR_SIZE;
    let bytes = dev.as_bytes();
    if last > bytes.len() {
        bail!("раздел данных выходит за пределы образа");
    }

    let dir = paths::build_dir().join("btrfs-linux-check");
    std::fs::create_dir_all(&dir)?;
    let volume = dir.join("scenario.img");
    std::fs::write(&volume, &bytes[first..last]).with_context(|| format!("не пишется {}", volume.display()))?;
    say!("btrfs: том после сценария {} проверяет «{distro}»", image.display());

    // Оба эталона — из рецепта образца (`RECIPE`): `mixed.bin` гость скопировал
    // и переименовал, а `big.bin` не трогал, и он обязан остаться целым.
    let mixed = vec![b'M'; 3000];
    let big = b"0123456789abcdef".repeat(4096 * 64);
    let script = format!(
        "{CHECK_PRELUDE}{SCENARIO_ROUND}{CHECK_COMPLAINTS}\n\
         echo \"--- btrfs check: после того как Linux смонтировал и отпустил том\"\n\
         btrfs check v.img\n"
    )
    .replace("@STAGE@", "том, в который писало ядро FreeOS")
    .replace("@MIXED@", &hex(&hash(&mixed)))
    .replace("@BIG@", &hex(&hash(&big)));
    let (_, log) = run_in(&distro, &script, Some(&volume)).context("Linux не принял том после сценария")?;
    show(&log);
    say!("btrfs: btrfs check дважды без нареканий, Linux прочитал записанное ядром FreeOS");
    Ok(())
}

/// Один круг: образ в файл, в WSL, журнал на экран, образ обратно.
fn round(distro: &str, dir: &Path, name: &str, disk: &disk::MemDisk, script: &str, what: &str) -> Result<Vec<u8>> {
    let image = dir.join(name);
    std::fs::write(&image, disk.as_bytes()).with_context(|| format!("не пишется {}", image.display()))?;
    let (back, log) = run_in(distro, script, Some(&image)).with_context(|| what.to_string())?;
    show(&log);
    if back.is_empty() {
        bail!("{what}: рецепт не вернул образ");
    }
    Ok(back)
}

/// Превратить отказ крейта в ошибку с объяснением, на каком шаге он случился.
fn broken(what: &'static str) -> impl Fn(btrfs::Error) -> anyhow::Error {
    move |err| anyhow!("{what}: {err}")
}

/// Узор, у которого соседние сектора различаются: сдвинутый на сектор экстент
/// прочитался бы не тем, а не тем же самым. Та же формула, что в тестах крейта.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| ((at / 4096) as u8).wrapping_mul(97) ^ (at as u8).wrapping_mul(31) ^ seed)
        .collect()
}

fn attrs(mode: u16) -> btrfs::Attributes {
    btrfs::Attributes { mode, uid: 1000, gid: 1000, time: now() }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn show(log: &str) {
    for line in log.lines().filter(|line| !line.trim().is_empty()) {
        say!("  btrfs: {line}");
    }
}

/// Выполнить рецепт в WSL: вернуть то, что он записал в дескриптор 3, и журнал.
///
/// `input` уходит рецепту на стандартный ввод. Так образ попадает внутрь без
/// угадывания, где у дистрибутива смонтированы диски Windows (`/mnt/c` у
/// обычных, `/mnt/host/c` у того, что приезжает с Docker Desktop).
fn run_in(distro: &str, script: &str, input: Option<&Path>) -> Result<(Vec<u8>, String)> {
    let full = format!("{WORKDIR}\n{script}");
    let mut command = Command::new("wsl");
    command.args(["-d", distro, "-u", "root", "-e", "sh", "-c", &full]);
    command.stdin(match input {
        Some(path) => Stdio::from(File::open(path).with_context(|| format!("не открывается {}", path.display()))?),
        None => Stdio::null(),
    });
    let output = command.output().context("не запустился wsl — он вообще установлен?")?;
    let log = decode(&output.stderr);
    if !output.status.success() {
        bail!("рецепт в WSL закончился отказом:\n{log}");
    }
    Ok((output.stdout, log))
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

/// Строки WSL о его собственных бедах («mounted read-only as a fallback»)
/// приходят в UTF-16 посреди UTF-8 журнала; нули из них вычищаются, чтобы журнал
/// читался.
fn decode(raw: &[u8]) -> String {
    let cleaned: Vec<u8> = raw.iter().copied().filter(|&byte| byte != 0).collect();
    String::from_utf8_lossy(&cleaned).into_owned()
}

/// `wsl -l` печатает UTF-16, а всё остальное — обычный UTF-8.
fn decode_utf16(raw: &[u8]) -> String {
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}
