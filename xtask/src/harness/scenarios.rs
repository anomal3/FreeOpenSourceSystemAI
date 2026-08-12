//! Сценарии стенда: что именно проверяется прогоном.
//!
//! # Правило проверок
//!
//! Утверждения делаются **только по серийной линии**. Снимок экрана
//! доказательством не является: он показывает последний нарисованный кадр, а не
//! текущее состояние, и после падения отстаёт на два-три экрана. Снимки здесь
//! отвечают на вопрос «как это выглядело», журнал — на вопрос «что произошло».
//!
//! # Почему сценарии — код, а не файлы данных
//!
//! Потому что их пишет тот же человек, который меняет ядро, и в тот же момент.
//! Формат файла пришлось бы разбирать и проверять, а его ошибки вылезали бы во
//! время прогона; здесь опечатку в имени клавиши ловит компилятор.

use super::aim::{self, Aim};
use crate::arch::Arch;

/// Носитель, с которого грузится сценарий.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Каталог хоста, выданный за FAT-раздел: самый быстрый цикл, но разметки
    /// не существует, и проверять на ней нечего, кроме самой системы.
    Live,
    /// Настоящий образ диска: наша разметка, наша файловая система.
    Image,
    /// Установочный носитель плюс чистый целевой диск.
    Installer,
    /// Диск, на который ставил установщик.
    Installed,
}

impl Target {
    pub const fn needs_installer(self) -> bool {
        matches!(self, Target::Installer)
    }

    pub const fn title(self) -> &'static str {
        match self {
            Target::Live => "каталог хоста через VVFAT",
            Target::Image => "образ диска GPT + FAT32",
            Target::Installer => "установочный носитель и чистый диск",
            Target::Installed => "диск после установки",
        }
    }
}

/// Шаг сценария.
pub enum Step {
    /// Дождаться подстроки в серийной линии (мс).
    ///
    /// Поиск идёт от места, где закончилось прошлое ожидание: приглашение
    /// `freeos> ` встречается в выводе десятки раз, и поиск с начала считал бы
    /// выполненным то, что ещё не началось.
    Await(&'static str, u64),
    /// Проверить, что подстрока встречалась хоть раз за прогон.
    Expect(&'static str),
    /// Проверить, что подстроки не было ни разу.
    Absent(&'static str),
    /// Нажать клавишу на **настоящей** клавиатуре (имя в терминах QEMU).
    Key(&'static str),
    /// Нажать клавишу несколько раз.
    Repeat(&'static str, u32),
    /// Набрать текст на клавиатуре.
    Type(&'static str),
    /// Отправить строку в серийную линию, завершив переводом строки.
    Line(&'static str),
    /// Отправить в серийную линию байты как есть.
    Raw(&'static [u8]),
    /// Сдвинуть мышь на приращение. Абсолютных координат у мыши нет.
    Move(i32, i32),
    /// Навести мышь на то, что гость сам описал в журнале.
    Aim(aim::Aim),
    /// Нажать левую кнопку и отпустить её там же.
    Click,
    /// Нажать левую кнопку и **не** отпускать — начало перетаскивания.
    Press,
    /// Отпустить все кнопки.
    Release,
    /// Подождать (мс).
    Wait(u64),
    /// Снять экран.
    Shot(&'static str),
}

pub struct Scenario {
    pub name: &'static str,
    pub about: &'static str,
    pub target: Target,
    pub steps: &'static [Step],
    /// Дополнительные аргументы QEMU.
    pub extra: &'static [&'static str],
    /// Ввод должен идти именно через USB.
    ///
    /// На x86-64 к машине подключены и PS/2, и USB, а `sendkey` доходит ровно
    /// до одной клавиатуры — QEMU выбирает PS/2. Без отключения i8042 сценарий
    /// «проверяем USB HID» молча проверял бы совсем другой драйвер.
    pub usb_only: bool,
    /// Архитектуры, на которых сценарий имеет смысл. Пусто — все.
    pub arches: &'static [Arch],
}

impl Scenario {
    pub fn runs_on(&self, arch: Arch) -> bool {
        self.arches.is_empty() || self.arches.contains(&arch)
    }

    /// Аргументы QEMU, зависящие от архитектуры.
    pub fn qemu_args(&self, arch: Arch) -> &'static [&'static str] {
        match (self.usb_only, arch) {
            // Опции машины накапливаются: `-machine q35` уже задан, здесь
            // добавляется свойство. На `virt` PS/2 не существует вовсе.
            (true, Arch::X86_64) => &["-machine", "i8042=off"],
            _ => &[],
        }
    }
}

/// Сколько ждать первых слов прошивки.
///
/// Общий предел на обе архитектуры, и он велик не зря: edk2 для `virt`
/// добирается до приложения заметно дольше OVMF, а в release-сборке к этому
/// добавляется время на разметку носителя.
const BOOT: u64 = 120_000;

pub const ALL: &[Scenario] = &[
    Scenario {
        name: "boot",
        about: "Ядро грузится, композитор поднимается, оболочка отвечает на команды.",
        target: Target::Live,
        usb_only: false,
        arches: &[],
        extra: &[],
        steps: &[
            Step::Await("FreeOS kernel v", BOOT),
            Step::Await("FreeOS shell.", 60_000),
            Step::Await("freeos> ", 10_000),
            Step::Line("uptime"),
            Step::Await("timer ticks", 10_000),
            Step::Line("mem"),
            Step::Await("frames", 10_000),
            // Композитор обязан быть поднят: строка «windows,» бывает только у
            // работающего окна, а не у оболочки в серийной консоли.
            Step::Line("ui"),
            Step::Await("windows,", 10_000),
            Step::Line("echo harness-ok"),
            Step::Await("harness-ok", 10_000),
            Step::Shot("desktop"),
            Step::Line("exit"),
            Step::Await("finishing the session", 10_000),
            // Паника в любой момент прогона обесценивает всё остальное.
            Step::Absent("KERNEL PANIC"),
        ],
    },
    Scenario {
        name: "keyboard",
        about: "Клавиши доходят до ядра через xHCI и USB HID, а не через PS/2.",
        target: Target::Live,
        usb_only: true,
        arches: &[],
        extra: &[],
        steps: &[
            Step::Await("freeos> ", BOOT),
            // Ядро перечисляет только поднявшиеся источники. PS/2 на этой машине
            // выключен, значит клавиатура здесь может быть только USB.
            Step::Expect("Input: keyboard"),
            Step::Type("echo usb-ok"),
            Step::Key("ret"),
            Step::Await("usb-ok", 15_000),
            // Tab поднимает нижнее окно наверх и **передаёт ему ввод**: с этой
            // фазы фокус — это фокус, а не только порядок по глубине. Поэтому
            // второй Tab обязателен: без него «exit» ушёл бы в окно состояния,
            // а не в оболочку.
            Step::Key("tab"),
            Step::Wait(500),
            Step::Shot("raised"),
            Step::Key("tab"),
            Step::Wait(500),
            Step::Type("exit"),
            Step::Key("ret"),
            Step::Await("finishing the session", 15_000),
            Step::Absent("KERNEL PANIC"),
        ],
    },
    Scenario {
        name: "desktop",
        about: "Меню запуска открывает программу, окно двигается и закрывается, фокус возвращается.",
        target: Target::Live,
        usb_only: false,
        arches: &[],
        extra: &[],
        steps: &[
            Step::Await("freeos> ", BOOT),
            // Стол не печатает ничего сам по себе, поэтому все проверки здесь
            // опираются на строки, которые оконный менеджер пишет в журнал.
            // Иначе фазу мог бы проверить только человек, глядящий на экран.
            Step::Expect("window      : 'Terminal'"),
            Step::Key("f1"),
            Step::Await("desktop     : menu opened", 15_000),
            Step::Wait(400),
            Step::Shot("01-menu"),
            // Первый пункт — терминал, он уже открыт; второй — файловый
            // менеджер, окна которого ещё нет.
            Step::Key("down"),
            Step::Key("ret"),
            Step::Await("desktop     : opened 'Files'", 15_000),
            Step::Await("desktop     : focus 'Files'", 5_000),
            // Пауза щедрая не от неуверенности: снимок показывает последний
            // нарисованный кадр, а отладочная сборка перерисовывает окно в
            // эмуляторе заметно дольше, чем стенд успевает нажать следующую
            // клавишу. Слишком ранний снимок показал бы стол на шаг позади.
            Step::Wait(2_500),
            Step::Shot("02-files"),
            // Ввод уходит в активное окно, а не в оболочку: стрелки листают
            // список, Enter открывает. До этой фазы обе клавиши попали бы в
            // редактор строки.
            Step::Repeat("down", 2),
            Step::Key("ret"),
            Step::Wait(2_500),
            Step::Shot("03-files-open"),
            Step::Repeat("ctrl-right", 2),
            Step::Key("ctrl-down"),
            Step::Wait(400),
            Step::Shot("04-moved"),
            // Обойти окна по кругу и убедиться, что оболочка по-прежнему
            // принимает команды: сочетания оконного менеджера не должны
            // оставлять после себя зажатых модификаторов.
            Step::Repeat("tab", 2),
            Step::Await("desktop     : focus 'Terminal'", 5_000),
            Step::Line("echo after-move"),
            Step::Await("after-move", 10_000),
            Step::Key("tab"),
            Step::Await("desktop     : focus 'Files'", 5_000),
            Step::Key("ctrl-w"),
            Step::Await("desktop     : closed 'Files'", 15_000),
            // Закрытие возвращает фокус верхнему из оставшихся окон — им
            // оказывается терминал, и именно поэтому команда ниже доходит.
            Step::Await("desktop     : focus 'Terminal'", 5_000),
            // Дальше набираем с клавиатуры, а не в серийную линию, и это не
            // придирка к стилю. Клавиатура USB опрашивается задачей, а UART
            // приходит прерыванием; пока отладочная сборка перерисовывает
            // экран, отпускание Ctrl ждёт следующего опроса, а байты из линии
            // успевают приехать раньше — и команда попадает в систему как
            // Ctrl+U, Ctrl+I. С одного устройства порядок событий сохраняется
            // всегда, поэтому именно так это и проверяется.
            Step::Type("ui"),
            Step::Key("ret"),
            Step::Await("windows,", 30_000),
            Step::Shot("05-desktop"),
            Step::Type("exit"),
            Step::Key("ret"),
            Step::Await("finishing the session", 30_000),
            Step::Absent("KERNEL PANIC"),
        ],
    },
    Scenario {
        name: "mouse",
        about: "Курсор ездит, щелчок поднимает окно, окно тащится за заголовок и закрывается кнопкой.",
        target: Target::Live,
        usb_only: false,
        arches: &[],
        extra: &[],
        steps: &[
            Step::Await("freeos> ", BOOT),
            // Мышь — второе устройство на контроллере. Строка ниже доказывает,
            // что перечисление портов дошло до неё, а не остановилось на
            // клавиатуре.
            Step::Expect("boot mouse on interface"),
            Step::Expect("Mouse: click to focus"),
            // Доехать до угла можно только упёршись в край: если ограничение
            // не работает, курсор уедет за экран и следующие наводки промажут.
            Step::Aim(Aim::Corner),
            Step::Line("ui"),
            Step::Await("pointer  0,0 visible", 15_000),
            // Кнопка меню — в левом нижнем углу, где панель начинается на любом
            // экране.
            Step::Aim(Aim::MenuButton),
            Step::Click,
            Step::Await("desktop     : menu opened", 15_000),
            Step::Wait(1_500),
            Step::Shot("01-menu"),
            Step::Click,
            Step::Await("desktop     : menu closed", 15_000),
            // Щелчок по заголовку нижнего окна поднимает его и передаёт ему
            // ввод: до этой фазы того и другого можно было добиться только
            // клавишей Tab.
            Step::Aim(Aim::Title("System")),
            Step::Click,
            Step::Await("desktop     : focus 'System'", 15_000),
            // Перетаскивание: нажать, провезти, отпустить. Куда окно приехало,
            // говорит сам гость — снимок экрана этого не доказывает.
            Step::Press,
            Step::Await("desktop     : drag 'System'", 15_000),
            Step::Move(-160, 120),
            Step::Release,
            Step::Await("desktop     : moved 'System' to ", 15_000),
            Step::Wait(1_500),
            Step::Shot("02-dragged"),
            // Кнопка закрытия — у правого края заголовка. Прицел считается от
            // нового положения окна: строка о перетаскивании его и сообщила.
            Step::Aim(Aim::Close("System")),
            Step::Click,
            Step::Await("desktop     : closed 'System'", 15_000),
            Step::Aim(Aim::Middle("Terminal")),
            Step::Click,
            Step::Await("desktop     : focus 'Terminal'", 15_000),
            Step::Wait(1_500),
            Step::Shot("03-desktop"),
            Step::Type("exit"),
            Step::Key("ret"),
            Step::Await("finishing the session", 30_000),
            Step::Absent("KERNEL PANIC"),
        ],
    },
    Scenario {
        name: "serial-cr",
        about: "Терминал, присылающий один возврат каретки, работает как Enter.",
        target: Target::Live,
        usb_only: false,
        arches: &[],
        extra: &[],
        steps: &[
            Step::Await("freeos> ", BOOT),
            // Именно этот путь прежний стенд проверить не мог: канал Windows
            // съедал 0x0D, и до гостя не доезжало ничего.
            Step::Raw(b"echo cr-ok\r"),
            Step::Await("cr-ok", 15_000),
            Step::Line("exit"),
            Step::Await("finishing the session", 15_000),
            Step::Absent("KERNEL PANIC"),
        ],
    },
    Scenario {
        name: "image",
        about: "Система грузится с диска, размеченного нашим же кодом: GPT и FAT32.",
        target: Target::Image,
        usb_only: false,
        arches: &[],
        extra: &[],
        steps: &[
            // На VVFAT таблицы разделов не существует вовсе — QEMU синтезирует
            // её на лету. Значит, разметку проверяет только этот сценарий.
            Step::Await("FreeOS kernel v", BOOT),
            Step::Await("freeos> ", 60_000),
            Step::Line("echo image-ok"),
            Step::Await("image-ok", 15_000),
            Step::Line("exit"),
            Step::Await("finishing the session", 15_000),
            Step::Absent("KERNEL PANIC"),
        ],
    },
    Scenario {
        name: "install",
        about: "Установщик проходит все экраны и пишет систему на чистый диск.",
        target: Target::Installer,
        usb_only: false,
        arches: &[],
        extra: &[],
        steps: &[
            Step::Await("FreeOS installer", BOOT),
            Step::Await("[disk]", 30_000),
            // Список дисков отсортирован так, что установочный носитель уходит
            // вниз: под курсором по умолчанию — цель установки.
            Step::Wait(1_500),
            Step::Shot("01-language"),
            Step::Key("ret"),
            Step::Wait(700),
            Step::Shot("02-welcome"),
            Step::Key("ret"),
            Step::Wait(700),
            Step::Shot("03-disk"),
            Step::Key("ret"),
            Step::Wait(700),
            Step::Shot("04-account"),
            Step::Type("roman"),
            Step::Key("tab"),
            Step::Type("freeos"),
            Step::Key("tab"),
            Step::Type("freeos"),
            Step::Shot("05-account"),
            Step::Key("ret"),
            Step::Wait(700),
            Step::Shot("06-keyboard"),
            Step::Key("ret"),
            Step::Wait(700),
            // Пролистать список, а не согласиться с первым пунктом: выбор,
            // который никто не двигал, не доказывает, что он двигается.
            Step::Repeat("down", 3),
            Step::Shot("07-timezone"),
            Step::Key("ret"),
            Step::Wait(700),
            Step::Shot("08-confirm"),
            // Курсор стоит на «нет»: подтверждение стирания диска не должно
            // происходить от случайного Enter.
            Step::Key("down"),
            Step::Key("ret"),
            Step::Await("[install] finished", 240_000),
            Step::Expect("[install] root: /etc/passwd"),
            Step::Wait(1_000),
            Step::Shot("09-done"),
        ],
    },
    Scenario {
        name: "installed",
        about: "Установленная система находит свой диск, монтирует ext2 и читает /etc.",
        target: Target::Installed,
        usb_only: false,
        arches: &[],
        extra: &[],
        steps: &[
            Step::Await("root        : ext2 at LBA", BOOT),
            // Это и есть доказательство цепочки virtio-blk → GPT → ext2 → VFS:
            // ядро читает файл, который записал установщик.
            Step::Await("account     : /etc/passwd", 30_000),
            Step::Await("freeos> ", 30_000),
            Step::Line("ls /"),
            Step::Await("etc/", 15_000),
            Step::Line("cat /etc/system.cfg"),
            Step::Await("language=", 15_000),
            Step::Shot("installed"),
            Step::Line("exit"),
            Step::Await("finishing the session", 15_000),
            Step::Absent("KERNEL PANIC"),
        ],
    },
];
