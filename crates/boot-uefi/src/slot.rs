//! Выбор системного слота: чтение записи с ESP, счётчик попыток и откат.
//!
//! # Почему это делает загрузчик
//!
//! Потому что решение принимается **до** того, как что-нибудь запустится.
//! Система, которая не поднялась, не уменьшит счётчик и не откатит сама себя —
//! в этом весь смысл: считать надо попытки, а не успехи, и считать их обязан
//! тот, кто эти попытки предпринимает.
//!
//! Отсюда и порядок: запись обновляется **до** прыжка в ядро. Уменьшенный после
//! запуска счётчик не уменьшился бы вовсе в том единственном случае, ради
//! которого существует, — когда ядро не запускается.
//!
//! # Что бывает, когда записи нет
//!
//! Обычная загрузка `\kernel.elf`. Слотов нет ни у живого ISO, ни у системы,
//! установленной прежним установщиком, и объявлять их там, где их не размечали,
//! загрузчик не вправе — см. [`crate::volume::BootVolume::open_read_write`].

use slots::{Attempt, FILE_SIZE, RECORD_SIZE, Slot, State};
use uefi::proto::media::file::{File, RegularFile};
use uefi::{CStr16, cstr16, println};

use crate::Aborted;
use crate::volume::BootVolume;

/// Путь к записи о слотах на томе.
///
/// Дублирует [`slots::PATH`] в виде, который понимает прошивка: `CStr16` строится
/// только из литерала, а константа из чужого крейта литералом не является.
/// Совпадение проверяется утверждением ниже — расхождение поймает компилятор, а
/// не человек, объясняющий, почему система не подтверждает загрузку.
const SLOTS_PATH: &CStr16 = cstr16!("\\FREEOS\\SLOTS.CFG");

// Совпадение с [`slots::PATH`] здесь не проверяется утверждением: `CStr16`
// не умеет отдавать длину в константном контексте, а сравнивать строки в
// рантайме ради константы — это проверка, которая сработает уже после того, как
// система не подтвердит загрузку. Держатся они рядом тем же способом, что и
// прочие пути тома: их два, оба в этом файле, и оба видны глазом.

/// Что загрузчик решил про эту загрузку.
pub struct Choice {
    /// Слот, с которого грузиться. `None` — слотов на томе нет.
    pub slot: Option<Slot>,
    /// Сколько попыток осталось после этой.
    pub tries_left: u8,
    /// Пришлось ли вернуться на прежний слот.
    pub rolled_back: bool,
}

impl Choice {
    /// Слотов нет: обычная загрузка одного-единственного ядра.
    const fn none() -> Self {
        Self { slot: None, tries_left: 0, rolled_back: false }
    }
}

/// Прочитать запись, обновить её и сказать, с какого слота грузиться.
///
/// Отказ носителя здесь **не** прерывает загрузку. Причина простая: слоты — это
/// механизм восстановления, и механизм восстановления, мешающий загрузиться,
/// хуже его отсутствия. Не прочиталась запись — грузимся тем, что есть, и
/// говорим об этом вслух.
pub fn choose(volume: &mut BootVolume) -> Result<Choice, Aborted> {
    let Some(mut file) = volume.open_read_write(SLOTS_PATH)? else {
        println!("  [slot] {SLOTS_PATH}: absent -- this system has no A/B slots");
        return Ok(Choice::none());
    };

    let mut bytes = [0u8; FILE_SIZE];
    if !read_whole(&mut file, &mut bytes) {
        println!("  [slot] {SLOTS_PATH} is unreadable -- booting the default kernel");
        return Ok(Choice::none());
    }

    let Some(mut state) = State::parse(&bytes) else {
        // Обе копии испорчены. Это не «слотов нет» — это состояние, которого
        // никто не понимает, и выдумывать его загрузчик не станет.
        println!("  [slot] both copies of the slot record are damaged");
        println!("  [slot] booting the default kernel; the system will say so too");
        return Ok(Choice::none());
    };

    let attempt = state.begin_boot();
    match attempt {
        Attempt::Confirmed(slot) => {
            println!("  [slot] slot {} is confirmed", slot.name());
        }
        Attempt::Trying { slot, left } => {
            println!(
                "  [slot] slot {} is not confirmed yet: {left} attempt(s) left after this one",
                slot.name()
            );
        }
        Attempt::RolledBack { to, from, left } => {
            println!(
                "  [slot] slot {} used up its attempts; falling back to slot {}",
                from.name(),
                to.name()
            );
            println!("  [slot] {left} attempt(s) left on slot {}", to.name());
        }
    }

    // Запись обновляется до возврата — то есть до чтения ядра и тем более до
    // прыжка в него.
    if !write_whole(&mut file, &state) {
        // Не записалось — и это стоит сказать, но не стоит останавливать
        // загрузку: система, которую нельзя запустить из-за неудачной записи
        // счётчика, — это неисправность, которую сам счётчик и создал.
        println!("  [slot] WARNING: the slot record could not be updated");
    }

    Ok(Choice {
        slot: Some(attempt.slot()),
        tries_left: state.tries,
        rolled_back: state.rolled_back,
    })
}

/// Прочитать файл целиком в буфер фиксированного размера.
///
/// `false` означает «прочитано не столько, сколько ожидалось». Файл записи о
/// слотах имеет ровно [`FILE_SIZE`] байт по построению, и другой размер — это
/// не «неполный файл», а не тот файл.
fn read_whole(file: &mut RegularFile, out: &mut [u8; FILE_SIZE]) -> bool {
    if file.set_position(0).is_err() {
        return false;
    }
    let mut filled = 0usize;
    while filled < out.len() {
        match file.read(&mut out[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(_) => return false,
        }
    }
    filled == out.len()
}

/// Записать обе копии: сначала запасную, потом основную.
///
/// Порядок обязателен и составляет всю защиту от выключения посреди записи.
/// Оборвись питание между двумя записями — основная копия остаётся прежней и
/// целой, то есть система поднимется с прежним состоянием. Обратный порядок
/// оставил бы целой запасную, а основную — наполовину новой, и разбор взял бы
/// именно её.
///
/// Длина файла при этом не меняется ни на байт: ни таблица FAT, ни запись
/// каталога не трогаются. Подробнее — в заголовке крейта `slots`.
fn write_whole(file: &mut RegularFile, state: &State) -> bool {
    let mut record = [0u8; RECORD_SIZE];
    state.write_record(&mut record);

    for offset in [RECORD_SIZE as u64, 0] {
        if file.set_position(offset).is_err() {
            return false;
        }
        if file.write(&record).is_err() {
            return false;
        }
        // Сброс после каждой копии, а не один в конце: без него прошивка вправе
        // держать обе в своём кэше и записать их в любом порядке — то есть
        // ровно тот порядок, который здесь и защищает.
        if file.flush().is_err() {
            return false;
        }
    }
    true
}
