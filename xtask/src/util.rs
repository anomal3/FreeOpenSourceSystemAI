//! Мелкие хост-утилиты: запуск дочерних процессов, поиск исполняемых файлов,
//! копирование и дополнение файлов до нужного размера.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Собирает команду в строку, которую можно скопировать в терминал.
///
/// Печатать полную командную строку перед запуском QEMU/cargo — самый дешёвый
/// способ отладки: пользователь видит ровно то, что было выполнено.
pub fn render_command(cmd: &Command) -> String {
    let mut out = quote(&cmd.get_program().to_string_lossy());
    for arg in cmd.get_args() {
        out.push(' ');
        out.push_str(&quote(&arg.to_string_lossy()));
    }
    out
}

fn quote(value: &str) -> String {
    if value.is_empty() {
        "\"\"".to_string()
    } else if value.chars().any(char::is_whitespace) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

/// Запускает дочерний процесс, унаследовав stdin/stdout/stderr.
///
/// Наследование потоков (поведение `Command::status` по умолчанию) обязательно:
/// иначе вывод компилятора и серийной консоли QEMU не доходил бы до пользователя
/// в реальном времени.
///
/// Исключение — параллельный прогон стенда. Там потоков-воркеров несколько, а
/// stdout один: унаследованный вывод четырёх cargo сразу приходит вперемешку и
/// **без метки**, то есть непонятно, чей он. Поэтому под меткой вывод
/// перехватывается и печатается целиком, когда процесс закончил. Живого
/// «прямо сейчас компилируется» при этом не видно — и это не потеря: к моменту
/// развилки всё уже собрано, а cargo в воркере только подтверждает, что
/// пересобирать нечего.
pub fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let rendered = render_command(cmd);
    say!("> {rendered}");

    let status = if crate::out::tagged() {
        let output = cmd
            .output()
            .with_context(|| format!("не удалось запустить {what}\n  команда: {rendered}"))?;
        for stream in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(stream);
            if !text.trim().is_empty() {
                say!("{}", text.trim_end());
            }
        }
        output.status
    } else {
        cmd.status()
            .with_context(|| format!("не удалось запустить {what}\n  команда: {rendered}"))?
    };

    if !status.success() {
        let code = match status.code() {
            Some(code) => code.to_string(),
            None => "прерван сигналом".to_string(),
        };
        bail!("{what} завершился с ошибкой (код возврата: {code})\n  команда: {rendered}");
    }

    Ok(())
}

/// Имя исполняемого файла с платформенным расширением.
pub fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// Поиск исполняемого файла в `PATH` (аналог `where` / `which`).
///
/// Возвращается полный путь, а не просто имя: по каталогу бинарника QEMU мы
/// затем ищем каталог `share/` с прошивками edk2.
pub fn which(stem: &str) -> Option<PathBuf> {
    let file_name = exe_name(stem);

    // Явный путь, а не имя — проверяем как есть.
    let as_path = Path::new(stem);
    if as_path.components().count() > 1 && as_path.is_file() {
        return Some(as_path.to_path_buf());
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file_name))
        .find(|candidate| candidate.is_file())
}

/// Копирует `src` в `dst`, дополняя нулями до `size` байт.
///
/// Нулевое (а не 0xFF) дополнение выбрано намеренно: именно так QEMU собирает
/// свои `edk2-*-code.fd` (`truncate -s 64m`) и так же описано в инструкциях
/// Debian по подготовке flash-образов для `-machine virt`.
pub fn write_padded(src: &Path, dst: &Path, size: u64) -> Result<()> {
    let data = fs::read(src)
        .with_context(|| format!("не удалось прочитать файл прошивки {}", src.display()))?;

    if data.len() as u64 > size {
        bail!(
            "файл {} больше требуемого размера flash-устройства ({} > {size} байт)",
            src.display(),
            data.len()
        );
    }

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }

    let mut file = fs::File::create(dst)
        .with_context(|| format!("не удалось создать файл {}", dst.display()))?;
    file.write_all(&data)
        .with_context(|| format!("не удалось записать {}", dst.display()))?;
    if (data.len() as u64) < size {
        // set_len расширяет файл нулями.
        file.set_len(size)
            .with_context(|| format!("не удалось дополнить {} до {size} байт", dst.display()))?;
    }

    Ok(())
}

/// Копирует файл, создавая каталог назначения при необходимости.
pub fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    fs::copy(src, dst).with_context(|| {
        format!(
            "не удалось скопировать {} -> {}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(())
}

/// Копирует файл, только если получатель отличается от источника, и сообщает,
/// понадобилось ли копирование.
///
/// Заведено ради `initrd.img`: он в десятки мегабайт, и переливать его в ESP на
/// каждый запуск эмулятора — заметная пауза на ровном месте. Сравниваются
/// размер и время правки: побайтовое сравнение стоило бы примерно столько же,
/// сколько само копирование.
pub fn copy_file_if_stale(src: &Path, dst: &Path) -> Result<bool> {
    let up_to_date = match (fs::metadata(src), fs::metadata(dst)) {
        (Ok(src_meta), Ok(dst_meta)) => {
            src_meta.len() == dst_meta.len()
                && matches!(
                    (src_meta.modified(), dst_meta.modified()),
                    (Ok(src_time), Ok(dst_time)) if dst_time >= src_time
                )
        }
        _ => false,
    };

    if up_to_date {
        return Ok(false);
    }

    copy_file(src, dst)?;
    Ok(true)
}

pub fn file_len(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|meta| meta.len())
}

/// Превращает путь в строку, пригодную для аргумента QEMU.
///
/// Слэши нормализуются в `/`: QEMU на Windows понимает такие пути, а обратные
/// слэши в длинных `-drive`-строках легко спутать с экранированием.
/// Запятая в пути недопустима — `-drive` разбирает свои опции именно по ней.
pub fn qemu_path(path: &Path) -> Result<String> {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.contains(',') {
        bail!(
            "путь {} содержит запятую; QEMU использует запятую как разделитель опций -drive.\n\
             Перенесите проект в каталог без запятых в имени.",
            path.display()
        );
    }
    Ok(text)
}
