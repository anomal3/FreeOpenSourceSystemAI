//! Ключи, которыми стенд входит в гостя по SSH.
//!
//! # Почему они не лежат в репозитории
//!
//! Потому что закрытый ключ в открытом репозитории — это закрытый ключ,
//! которого больше нет. Пара делается на машине разработчика при первом же
//! прогоне и живёт в `build/test/`, то есть в `.gitignore`.
//!
//! # Почему они не попадают в образ
//!
//! Открытая половина кладётся **не в initrd**, а прямо на раздел состояния уже
//! установленного гостя — тем же приёмом, которым в него попадает обновление
//! (см. [`crate::diskfile`]): открывается готовый образ диска, находится раздел,
//! и файл пишется тем же крейтом `ext2`, которым его записал установщик.
//!
//! Разница принципиальная. Файл, положенный в `initrd/`, уехал бы в выпущенный
//! ISO — и всякий, кто его скачал, получил бы систему, пускающую к себе
//! владельца ключа из `build/test/` на машине разработчика. Раздел состояния
//! существует только у конкретного установленного гостя внутри стенда, и
//! ничего, кроме этого гостя, не касается.
//!
//! # Заодно это проверяет то, что иначе не проверить
//!
//! Ключ ложится с настоящим владельцем (`uid` заведённой учётной записи) и
//! правами `0600`, а каталог `.ssh` — `0700`. Именно этого требует `sshd` от
//! файла ключей, и требование это проверяется на настоящей ext2 с настоящими
//! режимами доступа — на живой системе, где корень это FAT и всё на нём
//! `root:root 0755`, оно не значило бы ничего.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths;

/// Имя учётной записи, которую заводит сценарий `install`.
///
/// Оно же — имя домашнего каталога на разделе состояния и имя, под которым
/// стенд входит по SSH. Разойтись эти три места не могут: строка одна.
pub const ACCOUNT: &str = "roman";

/// Идентификатор первой заведённой учётной записи.
///
/// То же число, что в `crates/installer/src/account.rs::FIRST_UID`. Продублировать
/// его пришлось потому, что установщик собирается под UEFI и его константы
/// хосту не видны; расхождение выглядело бы как «ключ лежит, а sshd его не
/// берёт» — файл принадлежал бы не тому.
const FIRST_UID: u32 = 1000;

/// Пара, которой стенд входит в гостя: её открытая половина ляжет в
/// `authorized_keys`.
pub fn authorized() -> Result<PathBuf> {
    ensure_key("id_ed25519", "freeos-harness")
}

/// Пара, которой входить нельзя: она нигде не записана.
///
/// Нужна для проверки, без которой всё остальное ничего не доказывает: сервер,
/// пускающий по любому ключу, проходит проверку «вошли с правильным ключом» так
/// же успешно, как правильный.
pub fn stranger() -> Result<PathBuf> {
    ensure_key("id_stranger", "freeos-harness-stranger")
}

/// Сделать пару, если её ещё нет, и вернуть путь к закрытой половине.
///
/// Ключ переживает прогоны намеренно: он входит в содержимое образа гостя, и
/// новая пара на каждый прогон означала бы перезапись файла на диске, который
/// сценарии передают друг другу.
fn ensure_key(name: &str, comment: &str) -> Result<PathBuf> {
    let dir = paths::test_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
    let private = dir.join(name);
    let public = dir.join(format!("{name}.pub"));
    if private.is_file() && public.is_file() {
        return Ok(private);
    }
    // Половинка пары от прерванного прогона хуже отсутствующей: `ssh-keygen`
    // откажется писать поверх, и отказ будет про файл, а не про пару.
    let _ = std::fs::remove_file(&private);
    let _ = std::fs::remove_file(&public);

    let status = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        // Пустая парольная фраза: прогон идёт без человека, а ключ, который
        // просит пароль, повесил бы клиента до таймаута.
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg(comment)
        .arg("-f")
        .arg(&private)
        .arg("-q")
        .status()
        .context("не удалось запустить ssh-keygen; он нужен для проверки входа по ключу")?;
    if !status.success() {
        bail!("ssh-keygen отказался делать пару в {}", private.display());
    }
    say!("стенд: сделана пара ключей {}", private.display());
    Ok(private)
}

/// Положить открытый ключ пары стенда в `authorized_keys` гостя.
pub fn place_authorized_key(disk_path: &Path, private: &Path) -> Result<()> {
    authorize_public_key(disk_path, &private.with_extension("pub"))
}

/// Дописать открытый ключ (файл `.pub`) в `authorized_keys` гостя на разделе
/// состояния.
///
/// # Дописать, а не положить
///
/// До фазы 38c ключ был один — стенда, — и файл с ним либо был, либо нет. Для
/// `cargo xtask run --installed --ssh --key` ключей два: стенда, положенный
/// прогоном, и свой ключ человека. Отказ «файл уже лежит» оставил бы человека с
/// системой, пускающей стенд и не пускающей его самого, — а выглядело бы это
/// как испорченный ключ. Поэтому ключ, которого в файле нет, дописывается в
/// конец; тот, что есть, не пишется второй раз.
pub fn authorize_public_key(disk_path: &Path, public: &Path) -> Result<()> {
    use disk::BlockDevice as _;

    let mut line = std::fs::read(public)
        .with_context(|| format!("не удалось прочитать {}", public.display()))?;
    // `sshd` этой системы проверяет только Ed25519 — ключ другого типа лёг бы
    // в файл и молча не пустил бы никого.
    let text = String::from_utf8_lossy(&line).into_owned();
    let mut fields = text.split_whitespace();
    let (Some("ssh-ed25519"), Some(blob)) = (fields.next(), fields.next()) else {
        bail!(
            "{} — не открытый ключ ssh-ed25519; sshd FreeOS принимает только такие \
             (сделать: ssh-keygen -t ed25519)",
            public.display()
        );
    };
    if !line.ends_with(b"\n") {
        line.push(b'\n');
    }

    let mut dev = crate::diskfile::DiskFile::open(disk_path, 512)?;

    // Что уже лежит — читается до правки и тем же крейтом, которым прочтёт
    // система.
    let existing: Option<Vec<u8>> = {
        let table = disk::gpt::read(&mut dev)
            .map_err(|err| anyhow::anyhow!("на образе {} нет GPT: {err}", disk_path.display()))?;
        let state = table
            .find(disk::gpt::FREEOS_STATE_TYPE)
            .ok_or_else(|| anyhow::anyhow!("на образе нет раздела состояния"))?;
        let fs = ext2::Ext2::mount(&mut dev, state.first_lba)
            .map_err(|err| anyhow::anyhow!("раздел состояния не монтируется: {err}"))?;
        match fs.resolve(&mut dev, &format!("/home/{ACCOUNT}/.ssh/authorized_keys")) {
            Ok(inode) => Some(
                fs.read_file(&mut dev, &inode)
                    .map_err(|err| anyhow::anyhow!("authorized_keys не читается: {err}"))?,
            ),
            Err(_) => None,
        }
    };
    if let Some(existing) = &existing {
        let known = String::from_utf8_lossy(existing)
            .lines()
            .any(|entry| entry.split_whitespace().nth(1) == Some(blob));
        if known {
            say!("стенд: ключ {} уже в authorized_keys гостя", public.display());
            return Ok(());
        }
    }
    let table = disk::gpt::read(&mut dev)
        .map_err(|err| anyhow::anyhow!("на образе {} нет GPT: {err}", disk_path.display()))?;
    // Домашние каталоги живут на разделе состояния, а не на корневом: корень
    // смонтирован только на чтение и заменяется целиком при обновлении.
    let state = table
        .find(disk::gpt::FREEOS_STATE_TYPE)
        .ok_or_else(|| anyhow::anyhow!("на образе нет раздела состояния"))?;

    let mut fs = ext2::Editor::open(&mut dev, state.first_lba)
        .map_err(|err| anyhow::anyhow!("раздел состояния не открывается: {err}"))?;
    // Том помечается используемым на время правки и чистым в конце — так же,
    // как это делает установщик. Без этого система при следующей загрузке
    // объявила бы состояние грязным и проверила бы его целиком.
    fs.mark_dirty(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том используемым: {err}"))?;

    // `.ssh` — `0700` и владелец тот же, что у домашнего каталога: `sshd`
    // отказывается доверять файлу ключей, лежащему там, куда может писать
    // кто-то ещё, и проверка эта настоящая, а не наша выдумка.
    let ssh_dir = format!("home/{ACCOUNT}/.ssh");
    let dir = fs
        .create_dir_path(&mut dev, &ssh_dir, 0o700, FIRST_UID, FIRST_UID)
        .map_err(|err| anyhow::anyhow!("не удалось создать /{ssh_dir}: {err}"))?;

    let target = format!("{ssh_dir}/authorized_keys");
    let present = fs
        .lookup(&mut dev, dir, "authorized_keys")
        .map_err(|err| anyhow::anyhow!("не удалось заглянуть в /{ssh_dir}: {err}"))?;
    match (present, existing) {
        (Some((number, _)), Some(existing)) => {
            // Файл есть, ключа в нём нет — дописать в конец. Если последняя
            // строка без перевода, он ставится первым: иначе новый ключ
            // склеился бы с последним в одну негодную строку.
            let mut tail = Vec::new();
            if !existing.is_empty() && !existing.ends_with(b"\n") {
                tail.push(b'\n');
            }
            tail.extend_from_slice(&line);
            fs.write_at(&mut dev, number, existing.len() as u64, &tail)
                .map_err(|err| anyhow::anyhow!("не удалось дописать /{target}: {err}"))?;
            say!("стенд: ключ {} дописан в /{target}", public.display());
        }
        _ => {
            fs.write_file_path(&mut dev, &target, &line, 0o600, FIRST_UID, FIRST_UID)
                .map_err(|err| anyhow::anyhow!("не удалось записать /{target}: {err}"))?;
            say!("стенд: ключ положен в /{target} ({} байт)", line.len());
        }
    }

    fs.flush_everywhere(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось сбросить раздел состояния: {err}"))?;
    fs.mark_clean(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том чистым: {err}"))?;
    dev.flush()
        .map_err(|err| anyhow::anyhow!("не удалось сбросить образ: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod check {
    /// Укладка ключа проверяется на настоящем образе и **без эмулятора**.
    ///
    /// Ошибиться здесь можно молча: файл ляжет не с тем владельцем или не с тем
    /// режимом, `sshd` откажется ему доверять, и снаружи это будет выглядеть
    /// как «ключ не подошёл» — то есть неотличимо от чужого ключа. Прогон в
    /// эмуляторе такую разницу тоже показал бы, но минутами позже и строкой в
    /// журнале, а не здесь.
    ///
    /// Проверка пропускается, если образа нет: его делает сценарий `install`, и
    /// на свежей копии репозитория его ещё не существует.
    ///
    /// Пропускается она и тогда, когда образ есть, но он **4Kn**. Это не
    /// осторожность на всякий случай, а разбор случившегося: сценарий
    /// `install4k` пересоздаёт тот же файл с сектором 4096 и оставляет его
    /// таким до следующего `install`. После полного прогона `cargo xtask check`
    /// падал здесь — «на образе нет GPT», — хотя GPT там есть, просто в другом
    /// месте. Проверять укладку ключа на таком образе нечего: сценарии,
    /// которым он нужен, идут на 512-байтовой шине.
    #[test]
    fn places_the_key() {
        let disk = crate::paths::target_disk(crate::arch::Arch::X86_64, false);
        if !disk.is_file() {
            eprintln!("нет образа {}, проверка пропущена", disk.display());
            return;
        }
        {
            let mut probe =
                crate::diskfile::DiskFile::open(&disk, 512).expect("образ открывается");
            if disk::gpt::read(&mut probe).is_err() {
                eprintln!(
                    "на образе {} нет 512-байтовой GPT (скорее всего он 4Kn после install4k), \
                     проверка пропущена",
                    disk.display()
                );
                return;
            }
        }
        let key = super::authorized().expect("пара делается");
        super::place_authorized_key(&disk, &key).expect("ключ ложится");
        super::place_authorized_key(&disk, &key).expect("второй раз — тоже без отказа");

        // И читается обратно тем же кодом, которым его прочтёт система: важно
        // не только содержимое, но и владелец с режимом — `sshd` откажется
        // доверять файлу с чужим владельцем или открытым на запись каталогом.
        let mut dev = crate::diskfile::DiskFile::open(&disk, 512).expect("образ открывается");
        let table = disk::gpt::read(&mut dev).expect("GPT читается");
        let state = table
            .find(disk::gpt::FREEOS_STATE_TYPE)
            .expect("раздел состояния есть");
        let fs = ext2::Ext2::mount(&mut dev, state.first_lba).expect("том монтируется");
        for (path, mode, kind) in [
            ("/home/roman/.ssh", 0o700u16, ext2::FileType::Directory),
            ("/home/roman/.ssh/authorized_keys", 0o600, ext2::FileType::Regular),
        ] {
            let inode = fs.resolve(&mut dev, path).expect("узел находится");
            assert_eq!(inode.kind, kind, "{path}");
            assert_eq!(inode.mode & 0o777, mode, "{path}");
            assert_eq!(inode.uid, super::FIRST_UID, "{path}");
            assert_eq!(inode.gid, super::FIRST_UID, "{path}");
        }
        let inode = fs
            .resolve(&mut dev, "/home/roman/.ssh/authorized_keys")
            .expect("файл находится");
        let data = fs.read_file(&mut dev, &inode).expect("файл читается");
        assert!(data.starts_with(b"ssh-ed25519 "), "это ключ ssh-ed25519");
    }
}
