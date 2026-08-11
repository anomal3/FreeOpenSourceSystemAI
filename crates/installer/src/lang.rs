//! Строки интерфейса на двух языках.
//!
//! # Ограничение, которое видно прямо здесь
//!
//! Все русские строки набраны **только** буквами кириллицы и знаками ASCII: ни
//! длинного тире, ни угловых кавычек, ни неразрывного пробела. Причина
//! механическая — растровый шрифт 8x8 покрывает ASCII и кириллицу
//! ([`mini_ui::font`]), и любой другой знак вышел бы на экране вопросительным.
//! Проверить это глазами в исходнике проще, чем на экране установщика, поэтому
//! правило записано здесь.
//!
//! # Почему таблица строк, а не подстановка на месте
//!
//! Строк немного, и все они статические. Структура с полями даёт то, чего не
//! даёт словарь: забытый перевод не компилируется. Формат сообщения с числами
//! собирается на месте из отдельных слов — это дешевле, чем тащить в
//! UEFI-приложение подстановку по шаблону ради четырёх строк.

/// Язык интерфейса.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Russian,
}

impl Language {
    /// Как язык называется на самом себе — единственный способ подписать его
    /// в списке так, чтобы человек узнал свой.
    #[must_use]
    pub const fn endonym(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Russian => "Русский",
        }
    }

    /// Метка для файла настроек.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Russian => "ru",
        }
    }

    #[must_use]
    pub const fn strings(self) -> &'static Strings {
        match self {
            Language::English => &ENGLISH,
            Language::Russian => &RUSSIAN,
        }
    }
}

/// Всё, что установщик говорит человеку.
pub struct Strings {
    pub step: &'static str,
    pub of: &'static str,

    pub hint_select: &'static str,
    pub hint_next: &'static str,
    pub hint_fields: &'static str,
    pub hint_wait: &'static str,
    pub hint_finish: &'static str,

    pub language_heading: &'static str,
    pub language_body: &'static str,

    pub welcome_heading: &'static str,
    pub welcome_body: &'static str,
    pub welcome_payload: &'static str,

    pub disk_heading: &'static str,
    pub disk_body: &'static str,
    pub disk_none: &'static str,
    pub disk_install_media: &'static str,
    pub disk_read_only: &'static str,
    pub disk_too_small: &'static str,

    pub confirm_heading: &'static str,
    pub confirm_warning: &'static str,
    pub confirm_scheme: &'static str,
    pub confirm_esp: &'static str,
    pub confirm_root: &'static str,
    pub confirm_no: &'static str,
    pub confirm_yes: &'static str,

    pub account_heading: &'static str,
    pub account_body: &'static str,
    pub account_name: &'static str,
    pub account_password: &'static str,
    pub account_repeat: &'static str,
    pub account_err_name: &'static str,
    pub account_err_password: &'static str,
    pub account_err_mismatch: &'static str,

    pub keyboard_heading: &'static str,
    pub keyboard_body: &'static str,

    pub timezone_heading: &'static str,
    pub timezone_body: &'static str,

    pub install_heading: &'static str,
    pub step_wipe: &'static str,
    pub step_gpt: &'static str,
    pub step_format_esp: &'static str,
    pub step_copy: &'static str,
    pub step_format_root: &'static str,
    pub step_config: &'static str,
    pub step_flush: &'static str,

    pub done_heading: &'static str,
    pub done_body: &'static str,

    pub failed_heading: &'static str,
    pub failed_body: &'static str,

    pub error_no_payload: &'static str,
    pub error_disk: &'static str,
    pub error_memory: &'static str,
    pub error_root_fs: &'static str,
}

pub static ENGLISH: Strings = Strings {
    step: "Step",
    of: "of",

    hint_select: "Up/Down  select      Enter  continue      Esc  back",
    hint_next: "Enter  continue      Esc  back",
    hint_fields: "Tab  next field      Enter  continue      Esc  back",
    hint_wait: "Installing, please wait",
    hint_finish: "Enter  reboot",

    language_heading: "Language",
    language_body: "Choose the language of the installer.",

    welcome_heading: "FreeOS installation",
    welcome_body: "This installer erases one disk of your choice, writes a GPT partition table \
                   with an EFI system partition and a root partition, and copies FreeOS onto \
                   the EFI partition.\nNothing is written until you confirm the disk.",
    welcome_payload: "To be installed:",

    disk_heading: "Target disk",
    disk_body: "Everything on the disk you pick will be destroyed.",
    disk_none: "No disk is available for installation. Attach one and restart the installer.",
    disk_install_media: "install media",
    disk_read_only: "read-only",
    disk_too_small: "too small",

    confirm_heading: "Confirm",
    confirm_warning: "The disk below will be erased completely. This cannot be undone.",
    confirm_scheme: "Partition layout:",
    confirm_esp: "EFI system partition, FAT32",
    confirm_root: "FreeOS root partition, ext2",
    confirm_no: "No, go back",
    confirm_yes: "Yes, erase this disk and install",

    account_heading: "User account",
    account_body: "The account is recorded on the installed system. Names are lowercase \
                   letters, digits, dash and underscore, up to 16 characters.",
    account_name: "User name",
    account_password: "Password",
    account_repeat: "Repeat password",
    account_err_name: "The name must be 1 to 16 characters: a-z, 0-9, dash, underscore.",
    account_err_password: "The password must not be empty.",
    account_err_mismatch: "The two passwords differ.",

    keyboard_heading: "Keyboard layout",
    keyboard_body: "Recorded in the configuration file. The kernel currently ships one keymap \
                    (US) and will use it whatever is chosen here.",

    timezone_heading: "Time zone",
    timezone_body: "Recorded in the configuration file as an offset from UTC.",

    install_heading: "Installing",
    step_wipe: "Erasing the old partition table",
    step_gpt: "Writing the GPT partition table",
    step_format_esp: "Formatting the EFI system partition",
    step_copy: "Copying",
    step_format_root: "Creating the root filesystem",
    step_config: "Writing the account and the configuration",
    step_flush: "Flushing the disk",

    done_heading: "Done",
    done_body: "FreeOS is installed. Remove the installation medium, then press Enter to \
                reboot into the installed system.",

    failed_heading: "Installation failed",
    failed_body: "Nothing more was written. The details are on the serial console.",

    error_no_payload: "The installation medium is incomplete: a required file is missing.",
    error_disk: "The disk refused the operation.",
    error_memory: "Not enough memory to read the file being installed.",
    error_root_fs: "The root filesystem could not be created on that partition.",
};

pub static RUSSIAN: Strings = Strings {
    step: "Шаг",
    of: "из",

    hint_select: "Вверх/вниз  выбор      Enter  далее      Esc  назад",
    hint_next: "Enter  далее      Esc  назад",
    hint_fields: "Tab  следующее поле      Enter  далее      Esc  назад",
    hint_wait: "Идет установка, подождите",
    hint_finish: "Enter  перезагрузка",

    language_heading: "Язык",
    language_body: "Выберите язык установщика.",

    welcome_heading: "Установка FreeOS",
    welcome_body: "Установщик стирает один выбранный вами диск, создает на нем таблицу \
                   разделов GPT с системным разделом EFI и корневым разделом, а затем \
                   переносит FreeOS на системный раздел.\nДо подтверждения выбора диска на \
                   него ничего не пишется.",
    welcome_payload: "Будет установлено:",

    disk_heading: "Целевой диск",
    disk_body: "Все, что есть на выбранном диске, будет уничтожено.",
    disk_none: "Ни одного пригодного диска не найдено. Подключите диск и запустите \
                установщик заново.",
    disk_install_media: "носитель установки",
    disk_read_only: "только чтение",
    disk_too_small: "слишком мал",

    confirm_heading: "Подтверждение",
    confirm_warning: "Диск ниже будет стерт полностью. Отменить это будет нельзя.",
    confirm_scheme: "Разметка:",
    confirm_esp: "системный раздел EFI, FAT32",
    confirm_root: "корневой раздел FreeOS, ext2",
    confirm_no: "Нет, вернуться назад",
    confirm_yes: "Да, стереть этот диск и установить",

    account_heading: "Учетная запись",
    account_body: "Запись сохраняется в установленной системе. Имя: строчные латинские буквы, \
                   цифры, дефис и подчеркивание, до 16 знаков.",
    account_name: "Имя пользователя",
    account_password: "Пароль",
    account_repeat: "Повторите пароль",
    account_err_name: "Имя: от 1 до 16 знаков, только a-z, 0-9, дефис и подчеркивание.",
    account_err_password: "Пароль не может быть пустым.",
    account_err_mismatch: "Введенные пароли различаются.",

    keyboard_heading: "Раскладка клавиатуры",
    keyboard_body: "Записывается в файл настроек. В ядре пока одна раскладка (US), и оно \
                    возьмет ее независимо от выбора здесь.",

    timezone_heading: "Часовой пояс",
    timezone_body: "Записывается в файл настроек как смещение от UTC.",

    install_heading: "Установка",
    step_wipe: "Стирание прежней таблицы разделов",
    step_gpt: "Запись таблицы разделов GPT",
    step_format_esp: "Форматирование системного раздела EFI",
    step_copy: "Копирование",
    step_format_root: "Создание корневой файловой системы",
    step_config: "Запись учетной записи и настроек",
    step_flush: "Сброс данных на диск",

    done_heading: "Готово",
    done_body: "FreeOS установлена. Извлеките носитель установки и нажмите Enter, чтобы \
                перезагрузиться в установленную систему.",

    failed_heading: "Установка не удалась",
    failed_body: "Больше ничего не записано. Подробности - в серийной консоли.",

    error_no_payload: "Носитель установки неполон: нужного файла на нем нет.",
    error_disk: "Диск отказал в операции.",
    error_memory: "Не хватило памяти, чтобы прочитать устанавливаемый файл.",
    error_root_fs: "Не удалось создать корневую файловую систему на этом разделе.",
};
