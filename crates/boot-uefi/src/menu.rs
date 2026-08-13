//! Меню загрузчика: единственное место, где человек может вмешаться до того,
//! как система начнёт работать.
//!
//! # Почему оно здесь, а не в ядре
//!
//! Затем, что ядру может быть уже поздно. Безопасный режим нужен ровно тогда,
//! когда обычная загрузка не доходит до конца, — а всё, что умеет спрашивать в
//! ядре, к этому моменту ещё не поднялось. Клавиатура здесь прошивочная
//! (`Simple Text Input`), та же, которой пользуется установщик: она работает и
//! на машине, где наш USB-стек не завёлся вовсе.
//!
//! # Почему ожидание такое короткое
//!
//! Полсекунды — это полсекунды на **каждой** загрузке до конца времён. Меню,
//! которое стоит секунду и показывается раз в год, — плохая сделка, поэтому
//! ожидание короткое, а попасть в меню можно и клавишей, нажатой заранее:
//! прошивка копит нажатия в буфере, и загрузчик находит их сразу.
//!
//! Обратное свойство не менее важно: машина, на которой никто ничего не
//! нажимает, обязана грузиться ровно как раньше. Стенд именно так и грузится в
//! доброй сотне прогонов, и если бы меню требовало ответа, оно сломало бы их
//! все разом.

use core::time::Duration;

use boot_info::{BOOT_CHECK_DISK, BOOT_SAFE_MODE};
use uefi::proto::console::text::{Key, ScanCode};
use uefi::{boot, println, system};

/// Сколько ждать нажатия, прежде чем грузиться обычным порядком.
const WAIT: Duration = Duration::from_millis(500);

/// Шаг опроса клавиатуры.
const POLL: Duration = Duration::from_millis(25);

/// Сколько ждать выбора в открытом меню.
///
/// Здесь ожидание длинное и это правильно: меню открылось потому, что человек
/// об этом попросил, и захлопнуть его через секунду значило бы не показать
/// вовсе. Предел всё же есть — машина, которую попросили показать меню и
/// оставили, обязана в конце концов загрузиться сама.
const CHOICE_WAIT: Duration = Duration::from_secs(60);

/// Спросить, как грузиться, в начале работы. Возвращает флаги для
/// [`boot_info::BootInfo`].
pub fn choose() -> u64 {
    // Нажатие могло случиться и до нашего запуска: прошивка держит его в
    // буфере. Первое же чтение его и достанет — ждать ради этого не нужно.
    if wait_for_key(WAIT).is_none() {
        return 0;
    }
    ask()
}

/// Спросить ещё раз — перед самой точкой невозврата и **не ожидая**.
///
/// Нужно потому, что «нажать заранее» работает не везде одинаково. На x86-64
/// нажатие копит контроллер клавиатуры, и оно доживает до нашего запуска. На
/// машине, где клавиатура приходит по USB, до инициализации её прошивкой
/// нажимать некуда, а инициализация заканчивается позже, чем начинается наше
/// первое ожидание. Второй опрос ловит нажатие, сделанное **пока грузится
/// ядро**, и стоит он ровно одного чтения буфера: обычная загрузка не
/// задерживается ни на миллисекунду.
pub fn choose_late() -> u64 {
    if wait_for_key(Duration::ZERO).is_none() {
        return 0;
    }
    ask()
}

/// Показать меню и дождаться выбора.
fn ask() -> u64 {
    loop {
        show();
        let Some(key) = wait_for_key(CHOICE_WAIT) else {
            println!("  no choice made, booting normally");
            return 0;
        };
        match key {
            Key::Printable(character) => match char::from(character) {
                '1' | '\r' | '\n' => return chosen("normal boot", 0),
                '2' => return chosen("safe mode: no desktop, root read-only", BOOT_SAFE_MODE),
                '3' => return chosen("checking the root volume before mounting", BOOT_CHECK_DISK),
                '4' => {
                    return chosen(
                        "safe mode with a disk check",
                        BOOT_SAFE_MODE | BOOT_CHECK_DISK,
                    );
                }
                _ => println!("  unknown choice, try again"),
            },
            Key::Special(ScanCode::ESCAPE) => return chosen("normal boot", 0),
            Key::Special(_) => println!("  unknown choice, try again"),
        }
    }
}

fn chosen(what: &str, flags: u64) -> u64 {
    println!("  {what}");
    flags
}

fn show() {
    println!("");
    println!("---- boot menu --------------------------------------------------");
    println!("  1. Start FreeOS normally            (Enter)");
    println!("  2. Safe mode: no desktop, read-only root");
    println!("  3. Check the root volume, then start");
    println!("  4. Safe mode and check the volume");
    println!("");
    println!("  choose 1-4:");
}

/// Дождаться нажатия не дольше `limit`.
///
/// Опросом, а не ожиданием события: у `Simple Text Input` событие есть, но
/// ждать его вместе с таймером означает работать с массивом событий прошивки,
/// а выигрыш — доли процента одной загрузки. Опрос раз в двадцать пять
/// миллисекунд человек не отличит от мгновенного.
fn wait_for_key(limit: Duration) -> Option<Key> {
    let mut waited = Duration::ZERO;
    loop {
        let key = system::with_stdin(|stdin| stdin.read_key().ok().flatten());
        if key.is_some() {
            return key;
        }
        if waited >= limit {
            return None;
        }
        boot::stall(POLL);
        waited += POLL;
    }
}
