//! Окно «Параметры»: то, что человек настраивает, не набирая команд.
//!
//! # Почему это окно, а не набор команд оболочки
//!
//! Все разделы и сейчас доступны из терминала: версию печатает `about`,
//! пакеты — `pkg list`, обновление — `sysupdate`, разрешение задаёт прошивка.
//! Разница не в возможностях, а в том, что человеку не приходится знать имена
//! команд, чтобы посмотреть, сколько памяти в машине и откуда она берёт
//! обновления. Окно ничего не умеет сверх команд — и это осознанно: две дороги к
//! одному действию расходятся ровно в тот день, когда одну из них поправят.
//!
//! Одно исключение из этого правила — тема. У неё нет команды оболочки, потому
//! что менять её вслепую бессмысленно: смотреть надо на то, что получилось, а
//! не на слово «dark» в ответе терминала.
//!
//! # Почему разделы слева, а содержимое справа
//!
//! Потому что так устроены «Параметры» везде, где человек их видел. Раскладка,
//! к которой не надо привыкать, — это раскладка, которую не надо объяснять.
//!
//! # Почему рисование и разбор щелчка — один проход
//!
//! Раскладка, посчитанная отдельно для отрисовки и отдельно для попадания,
//! расходится в первый же день, и расхождение не ловится ни сборкой, ни
//! снимком экрана: нарисовано верно, а нажимается мимо. Поэтому окно проходится
//! ровно один раз ([`Pass`]), и проход этот либо рисует, либо только запоминает,
//! куда можно нажать. Второму списку прямоугольников взяться неоткуда.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::theme;
use crate::input::KeyCode;
use crate::{arch, config, fs, kprintln};

/// Разделы окна — порядок сверху вниз.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Экран: режим, тема, масштаб.
    Display,
    /// Часовой пояс и то, который сейчас час.
    Clock,
    /// Адрес машины: получать по DHCP или задать постоянный.
    Network,
    /// Тома, свободное место и проверка файловой системы.
    Disks,
    /// Кто может входить в эту систему.
    Users,
    /// Что установлено пакетами.
    Programs,
    /// Откуда система берёт обновления и что с ними делать.
    Updates,
    /// Что это за машина: версия, процессор, память, время работы.
    System,
}

impl Section {
    const ALL: [Section; 8] = [
        Section::Display,
        Section::Clock,
        Section::Network,
        Section::Disks,
        Section::Users,
        Section::Programs,
        Section::Updates,
        Section::System,
    ];

    const fn title(self) -> &'static str {
        match self {
            Section::Display => "Экран",
            Section::Clock => "Дата и время",
            Section::Network => "Сеть",
            Section::Disks => "Диски",
            Section::Users => "Пользователи",
            Section::Programs => "Пакеты",
            Section::Updates => "Обновление",
            Section::System => "О системе",
        }
    }

    /// Значок раздела.
    ///
    /// Разделов ровно столько, сколько в системе есть того, что они
    /// настраивают. Строка бокового списка без содержимого за ней — это
    /// обещание, которого окно не выполнит.
    const fn icon(self) -> Icon {
        match self {
            Section::Display => Icon::Display,
            Section::Clock => Icon::Clock,
            Section::Network => Icon::Network,
            Section::Disks => Icon::Disk,
            Section::Users => Icon::User,
            Section::Programs => Icon::Package,
            Section::Updates => Icon::Update,
            Section::System => Icon::Info,
        }
    }

    /// Есть ли за разделом то, что он настраивает.
    ///
    /// Признак завёлся не про запас: «Сеть» на машине без сетевой карты — это
    /// ровно тот случай, ради которого серый цвет в словаре бокового списка и
    /// существует. Раздел при этом не прячется: спрятанный раздел выглядит как
    /// «в этой системе нет настроек сети», а серый — как «в этой машине нет
    /// сетевой карты», и это разные утверждения.
    fn available(self) -> bool {
        match self {
            Section::Network => crate::net::is_present(),
            _ => true,
        }
    }
}

/// Разрешения, которые можно попросить у прошивки.
///
/// Список, а не свободный ввод: прошивка предлагает свой набор режимов, и
/// написанное от руки «1234×567» она всё равно отвергнет. Здесь перечислены те,
/// которые предлагает всякая машина, на которой эта система вообще запускается.
pub const MODES: [(u32, u32); 5] = [
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1600, 900),
    (1920, 1080),
];

/// Что делает пункт, по которому нажали.
///
/// Действие лежит в самом пункте, а не выводится из его номера. Номер годился,
/// пока пункты были перечислением разрешений экрана; список установленных
/// пакетов меняется от машины к машине, и «пятый пункт означает удалить пятый
/// пакет» — это два места, обязанные считать одинаково, а разойдутся они в
/// первый же день.
#[derive(Clone, PartialEq, Eq)]
enum Deed {
    /// Попросить у прошивки этот режим экрана.
    Mode(u32, u32),
    /// Перейти на тёмную (`true`) или светлую тему.
    Theme(bool),
    /// Выбрать часовой пояс — смещение от UTC в минутах.
    Timezone(i32),
    /// Вернуть адрес во власть DHCP.
    NetDhcp,
    /// Перейти к правке постоянного адреса.
    NetStatic,
    /// Встать в поле ввода: адрес или шлюз.
    NetEdit(Field),
    /// Применить набранное и записать в `/etc/network.cfg`.
    NetApply,
    /// Проверить файловые системы.
    CheckDisks,
    /// Запустить `sysupdate`.
    CheckUpdates,
    /// Показать, что можно поставить.
    ChooseFile,
    /// Поставить пакет из этого файла.
    Install(String),
    /// Спросить, точно ли удалять этот пакет.
    AskRemove(String),
    /// Удалить его.
    Remove(String),
    /// Вернуться к списку установленного.
    BackToList,
}

/// Какое из двух полей раздела «Сеть» правится.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    /// Адрес с длиной префикса: `10.0.2.15/24`.
    Address,
    /// Шлюз: `10.0.2.2`. Пустой — «шлюза нет».
    Gateway,
}

/// Поля ввода раздела «Сеть».
///
/// # Почему ввод ограничен цифрами, точкой и косой
///
/// Не из осторожности, а потому что клавиатура ядра отдаёт **позицию клавиши**,
/// а не символ ([`KeyCode`]): чтобы получить букву, нужна раскладка, а раскладок
/// в системе две и выбираются они в установщике. Адрес состоит из цифр, точек и
/// одной косой — эти клавиши на всех раскладках стоят на одном месте, и никакой
/// таблицы для них не требуется. Поле, принимающее буквы, пришлось бы отложить
/// до фазы с раскладками; поле, принимающее адрес, работает сегодня.
///
/// Цена названа: имя узла отсюда не задать. Оно и не задаётся — DNS-имя себе
/// система не присваивает.
#[derive(Clone)]
struct NetDraft {
    address: String,
    gateway: String,
    /// В каком поле каретка. `None` — ни в каком, клавиши уходят разделам.
    editing: Option<Field>,
}

impl NetDraft {
    /// Начальное содержимое — то, что у машины сейчас.
    ///
    /// Не пустые поля: человек, открывший раздел, чаще уточняет адрес, чем
    /// придумывает его с нуля, а пустое поле вдобавок не показывает, какого
    /// вида ответ от него ждут.
    fn current() -> Self {
        let (address, gateway) = match crate::net::status() {
            Some(status) if !status.address.is_unspecified() => (
                format!("{}/{}", status.address, status.netmask.prefix().unwrap_or(24)),
                if status.gateway.is_unspecified() {
                    String::new()
                } else {
                    format!("{}", status.gateway)
                },
            ),
            _ => (String::new(), String::new()),
        };
        Self { address, gateway, editing: None }
    }

    fn text(&self, field: Field) -> &str {
        match field {
            Field::Address => &self.address,
            Field::Gateway => &self.gateway,
        }
    }

    fn text_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Address => &mut self.address,
            Field::Gateway => &mut self.gateway,
        }
    }
}

/// Наибольшая длина того, что можно набрать в поле адреса.
///
/// `255.255.255.255/32` — восемнадцать знаков; предел вдвое больше, чтобы
/// опечатка не упиралась молча в конец строки, но строка в куче не росла от
/// зажатой клавиши.
const FIELD_LIMIT: usize = 36;

/// Чем занят раздел «Пакеты».
///
/// Установка и удаление — два разговора, а не два нажатия: у первого надо
/// спросить, какой файл ставить, у второго — точно ли удалять. Оба разговора
/// идут в той же правой половине окна, потому что отдельное окно ради двух
/// вопросов — это ещё один вид окна, который придётся объяснять.
#[derive(Clone, PartialEq, Eq)]
enum Programs {
    /// Список установленного.
    List,
    /// Выбор файла для установки — вместе с тем, что нашлось.
    ///
    /// Список найденного лежит здесь, а не пересчитывается при рисовании, и это
    /// не кеш ради скорости. Содержимое раздела считается заново на каждое
    /// нажатие — и на разбор клавиши, и на перерисовку, — а поиск файлов
    /// пакетов означает обход трёх каталогов и чтение заголовка у каждого
    /// найденного. Внутри обработчика события ввода это те самые сотни
    /// миллисекунд, за которые теряется следующая клавиша.
    Choose(Vec<String>),
    /// Вопрос «точно удалить этот пакет?».
    Confirm(String),
}

/// Чем кончилось последнее действие.
///
/// Признак «получилось» хранится рядом с текстом, а не выводится из текста
/// потом: догадываться об исходе по словам ответа — это разбор естественного
/// языка ради выбора одного из двух цветов.
struct Report {
    ok: bool,
    text: String,
}

impl Report {
    fn ok(text: String) -> Self {
        Self { ok: true, text }
    }

    fn bad(text: String) -> Self {
        Self { ok: false, text }
    }
}

pub struct SettingsView {
    section: usize,
    /// Выбранный пункт справа — номер в [`SettingsView::deeds`].
    action: usize,
    /// Что ответило последнее действие.
    report: Option<Report>,
    /// Размер экрана: окно узнаёт его от стола, само оно экрана не видит.
    screen: (u32, u32),
    /// Фокус на левом списке разделов, а не на пунктах справа.
    on_sections: bool,
    /// Чем занят раздел «Пакеты».
    programs: Programs,
    /// Что набрано в разделе «Сеть».
    net: NetDraft,
    /// Тема сменилась, и перекрасить пора весь стол.
    theme_changed: bool,
}

impl SettingsView {
    #[must_use]
    pub fn new(screen: (u32, u32)) -> Self {
        Self {
            section: 0,
            action: 0,
            report: None,
            screen,
            on_sections: true,
            programs: Programs::List,
            net: NetDraft::current(),
            theme_changed: false,
        }
    }

    /// Перейти на раздел экрана — им открывается меню стола.
    pub fn show_display(&mut self) {
        self.section = Self::index_of(Section::Display);
        self.on_sections = true;
        self.action = 0;
        self.report = None;
    }

    /// Тема сменилась и стол пора перекрасить целиком.
    ///
    /// Признак наружу, а не перерисовка отсюда: смена темы меняет обои, панель
    /// задач, значки и все прочие окна, а это окно о существовании соседей не
    /// знает и знать не должно. Читается признак один раз — тот, кто владеет
    /// поверхностями, обязан перекрасить стол ровно однажды на смену.
    pub fn take_theme_change(&mut self) -> bool {
        core::mem::take(&mut self.theme_changed)
    }

    fn index_of(section: Section) -> usize {
        Section::ALL
            .iter()
            .position(|other| *other == section)
            .unwrap_or(0)
    }

    fn current(&self) -> Section {
        Section::ALL[self.section.min(Section::ALL.len() - 1)]
    }

    /// Заголовок содержимого.
    ///
    /// Он не всегда равен названию раздела: разговоры «что поставить» и «точно
    /// удалить?» идут внутри «Пакетов», и оставить над ними прежний заголовок
    /// значило бы не сказать человеку, о чём его сейчас спрашивают.
    fn heading(&self) -> String {
        match (self.current(), &self.programs) {
            (Section::Programs, Programs::Choose(_)) => "Установка пакета".to_string(),
            (Section::Programs, Programs::Confirm(name)) => format!("Удалить {name}?"),
            (section, _) => section.title().to_string(),
        }
    }

    /// Одна строка пояснения под заголовком.
    fn about(&self) -> String {
        match (self.current(), &self.programs) {
            (Section::Display, _) => "Разрешение, тема и масштаб рабочего стола.".to_string(),
            (Section::Clock, _) => "Который сейчас час и от чего его отсчитывать.".to_string(),
            (Section::Network, _) => "Адрес этой машины: спрашивать или задать.".to_string(),
            (Section::Disks, _) => "Что смонтировано и в каком это состоянии.".to_string(),
            (Section::Users, _) => "Кто может входить в эту систему.".to_string(),
            (Section::Programs, Programs::List) => {
                "Что установлено на этой машине и что можно поставить.".to_string()
            }
            (Section::Programs, Programs::Choose(_)) => {
                "Файлы .fpk, которые видно с этой машины.".to_string()
            }
            (Section::Programs, Programs::Confirm(name)) => {
                format!("Каталог /opt/{name} и запись в реестре исчезнут.")
            }
            (Section::Updates, _) => {
                "Откуда система берёт обновления и как их поставить.".to_string()
            }
            (Section::System, _) => "Что это за машина и чем она сейчас занята.".to_string(),
        }
    }

    /// Пункты текущего раздела в том же порядке, в каком их рисует [`Pass`].
    ///
    /// Порядок — договор между этим списком и отрисовкой: по нему ходит
    /// клавиатура, и разойдись они, стрелка вниз выбирала бы не то, что
    /// подсвечено.
    fn deeds(&self) -> Vec<Deed> {
        let mut out = Vec::new();
        match self.current() {
            Section::Display => {
                for (width, height) in MODES {
                    out.push(Deed::Mode(width, height));
                }
                out.push(Deed::Theme(true));
                out.push(Deed::Theme(false));
            }
            Section::Programs => match &self.programs {
                Programs::List => {
                    for name in packages().unwrap_or_default() {
                        out.push(Deed::AskRemove(name));
                    }
                    out.push(Deed::ChooseFile);
                }
                Programs::Choose(found) => {
                    for path in found {
                        out.push(Deed::Install(path.clone()));
                    }
                    out.push(Deed::BackToList);
                }
                Programs::Confirm(name) => {
                    out.push(Deed::Remove(name.clone()));
                    out.push(Deed::BackToList);
                }
            },
            Section::Clock => {
                for minutes in crate::time::TIMEZONES {
                    out.push(Deed::Timezone(minutes));
                }
            }
            Section::Network => {
                out.push(Deed::NetDhcp);
                out.push(Deed::NetStatic);
                out.push(Deed::NetEdit(Field::Address));
                out.push(Deed::NetEdit(Field::Gateway));
                out.push(Deed::NetApply);
            }
            Section::Disks => out.push(Deed::CheckDisks),
            // Список учётных записей — чтение, и действий у него нет. Заводить
            // их здесь значило бы обещать создание пользователя, которого в
            // системе нет ни в каком виде: пароль умеет хешировать только
            // установщик. См. пояснение в `draw_users`.
            Section::Users => {}
            Section::Updates => out.push(Deed::CheckUpdates),
            Section::System => {}
        }
        out
    }

    /// Кнопки, прижатые к низу окна.
    ///
    /// Их две только там, где есть что подтверждать: у остальных разделов
    /// каждое действие срабатывает сразу, и «Применить» под ними означала бы
    /// кнопку, которая ничего не делает.
    fn buttons(&self) -> Vec<(Deed, &'static str, Weight)> {
        let mut out = Vec::new();
        // «Применить» у сети — в подвале, и это не украшение. Содержимое раздела
        // высокое (карточка состояния, выбор источника, два поля), и кнопка,
        // нарисованная под ними, оказывалась за нижним краем окна: клавиатурой
        // достижима, мышью — нет вовсе. Подвал прижат к низу и виден всегда.
        //
        // Она же — единственное действие в окне, которое не срабатывает сразу:
        // адрес меняется под тем, кто сидит по сети, и промах мимо строки не
        // должен обрывать ему соединение.
        if self.current() == Section::Network {
            out.push((Deed::NetApply, "Применить", Weight::Primary));
            return out;
        }
        if self.current() != Section::Programs {
            return out;
        }
        match &self.programs {
            Programs::List => {}
            Programs::Choose(_) => {
                out.push((Deed::BackToList, "Вернуться к списку", Weight::Normal));
            }
            Programs::Confirm(name) => {
                // Удаление красное, а не синее: цвет здесь — единственное, что
                // отличает эту пару кнопок от любой другой пары «да/нет».
                out.push((Deed::Remove(name.clone()), "Удалить", Weight::Danger));
                out.push((Deed::BackToList, "Оставить", Weight::Normal));
            }
        }
        out
    }

    /// Разобрать клавишу. `true` — окно её использовало.
    pub fn handle(&mut self, code: KeyCode) -> bool {
        // Поле ввода забирает клавиши целиком, и это не жадность: стрелка вниз
        // внутри поля обязана оставаться стрелкой вниз для списка разделов, но
        // цифра обязана попасть в поле, а не выбрать пункт с этим номером. Раз
        // граница проходит по клавишам, а не по областям окна, разбирать её
        // надо здесь и до всего остального.
        if let Some(field) = self.net.editing {
            if self.type_into(field, code) {
                return true;
            }
        }
        match code {
            KeyCode::Up => {
                if self.on_sections {
                    self.section = self.section.saturating_sub(1);
                    self.programs = Programs::List;
                } else {
                    self.action = self.action.saturating_sub(1);
                }
                self.report = None;
                true
            }
            KeyCode::Down => {
                if self.on_sections {
                    self.section = (self.section + 1).min(Section::ALL.len() - 1);
                    // Уход из раздела возвращает его в исходное состояние:
                    // вопрос «точно удалить?», оставленный без ответа, при
                    // возврате означал бы вопрос о том, чего человек уже не
                    // помнит.
                    self.programs = Programs::List;
                } else {
                    self.action = (self.action + 1).min(self.deeds().len().saturating_sub(1));
                }
                self.report = None;
                true
            }
            // Вправо — перейти к пунктам раздела, влево — вернуться к списку
            // разделов. Клавиатурой окно проходится целиком, включая образцы
            // темы: мышь на этой машине есть не всегда.
            KeyCode::Right | KeyCode::Tab => {
                let count = self.deeds().len();
                if count > 0 {
                    self.on_sections = false;
                    self.action = self.action.min(count - 1);
                }
                true
            }
            KeyCode::Left => {
                self.on_sections = true;
                true
            }
            KeyCode::Enter => {
                if self.on_sections {
                    if !self.deeds().is_empty() {
                        self.on_sections = false;
                        self.action = 0;
                    }
                } else {
                    self.activate();
                }
                true
            }
            _ => false,
        }
    }

    /// Набрать знак в поле. `true` — клавиша использована полем.
    ///
    /// Возвращает `false` на всём, чего в адресе быть не может, — и тогда
    /// клавиша достаётся окну как обычно. Так стрелки и Tab продолжают ходить
    /// по разделам, не выходя из поля: человек, набравший половину адреса и
    /// заглянувший в соседний раздел, возвращается к своей половине.
    fn type_into(&mut self, field: Field, code: KeyCode) -> bool {
        let digit = match code {
            KeyCode::Digit0 => Some('0'),
            KeyCode::Digit1 => Some('1'),
            KeyCode::Digit2 => Some('2'),
            KeyCode::Digit3 => Some('3'),
            KeyCode::Digit4 => Some('4'),
            KeyCode::Digit5 => Some('5'),
            KeyCode::Digit6 => Some('6'),
            KeyCode::Digit7 => Some('7'),
            KeyCode::Digit8 => Some('8'),
            KeyCode::Digit9 => Some('9'),
            KeyCode::Period => Some('.'),
            KeyCode::Slash => Some('/'),
            _ => None,
        };
        if let Some(ch) = digit {
            let text = self.net.text_mut(field);
            if text.len() < FIELD_LIMIT {
                text.push(ch);
            }
            self.report = None;
            return true;
        }
        match code {
            KeyCode::Backspace => {
                self.net.text_mut(field).pop();
                self.report = None;
                true
            }
            // Enter из поля — не «применить», а «поле готово». Применение
            // отдельной кнопкой намеренно: адрес меняется под тем, кто сидит по
            // сети, и лишний Enter не должен обрывать ему соединение.
            KeyCode::Enter | KeyCode::Escape => {
                self.net.editing = None;
                true
            }
            _ => false,
        }
    }

    /// Щелчок по окну: координаты внутри области содержимого.
    ///
    /// Возвращает `true`, если щелчок что-то изменил и окно надо перерисовать.
    /// Смена темы, кроме того, взводит [`SettingsView::take_theme_change`]:
    /// перерисовать в этом случае надо не одно окно.
    pub fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let hits = self.run(ctx, area, None);
        let Some(hit) = hits.into_iter().find(|hit| hit.rect.contains(x, y)) else {
            return false;
        };
        match hit.spot {
            Spot::Section(index) => {
                self.section = index;
                self.on_sections = true;
                self.action = 0;
                self.report = None;
                self.programs = Programs::List;
                true
            }
            Spot::Deed(deed) => {
                self.on_sections = false;
                // Номер пункта подтягивается за мышью, чтобы стрелки после
                // щелчка шли оттуда, куда человек нажал, а не с начала списка.
                if let Some(index) = self.deeds().iter().position(|other| *other == deed) {
                    self.action = index;
                }
                self.perform(deed);
                true
            }
        }
    }

    /// Применить набранный адрес и записать его.
    ///
    /// Порядок обязателен: сначала **применить**, потом записать. Записанный и
    /// не применённый адрес — это файл, который заработает после перезагрузки,
    /// и человек, набравший опечатку, узнает о ней не сейчас, а тогда, когда
    /// машина уже недоступна. Применённый первым отказывает немедленно и
    /// говорит, чем именно плох.
    fn apply_network(&mut self) {
        use crate::net::ipv4::Ipv4;
        use crate::net::persist::{Mode, Settings};

        let address = self.net.address.trim().to_string();
        let Some((host, bits)) = address.split_once('/') else {
            self.report = Some(Report::bad(String::from(
                "адрес пишется с длиной префикса: 10.0.2.15/24",
            )));
            return;
        };
        let (Some(host), Ok(bits)) = (Ipv4::parse(host), bits.parse::<u32>()) else {
            self.report = Some(Report::bad(format!("это не адрес: {address}")));
            return;
        };
        let Some(netmask) = Ipv4::from_prefix(bits) else {
            self.report = Some(Report::bad(format!("префикс — от 0 до 32 бит, а не {bits}")));
            return;
        };
        let gateway_text = self.net.gateway.trim().to_string();
        let gateway = if gateway_text.is_empty() {
            Ipv4::UNSPECIFIED
        } else {
            match Ipv4::parse(&gateway_text) {
                Some(gateway) => gateway,
                None => {
                    self.report = Some(Report::bad(format!("это не шлюз: {gateway_text}")));
                    return;
                }
            }
        };

        if let Err(err) = crate::net::configure(host, netmask, gateway) {
            self.report = Some(Report::bad(format!("адрес не принят: {err}")));
            return;
        }
        // Сервер имён сохраняется тот, что есть: спрашивать его отдельным полем
        // незачем — он приезжает с арендой, а при постоянном адресе остаётся
        // тем, который уже работает. Ноль здесь означает «не знаем», и таким он
        // в файл и уедет.
        let dns = crate::net::dns_server().unwrap_or(Ipv4::UNSPECIFIED);
        let settings = Settings { mode: Mode::Static, address: host, netmask, gateway, dns };
        self.net.editing = None;
        self.report = Some(match crate::net::persist::store(&settings) {
            Ok(()) => {
                kprintln!("  settings    : address {host}/{bits} static, saved");
                Report::ok(format!("адрес {host}/{bits} применён и запомнен"))
            }
            // Применён, но не записан — состояние, о котором обязано быть
            // сказано вслух: оно работает ровно до выключения.
            Err(err) => Report::bad(format!("{host}/{bits} применён, но не записан: {err}")),
        });
    }

    /// Выполнить выбранный пункт.
    fn activate(&mut self) {
        let Some(deed) = self.deeds().into_iter().nth(self.action) else {
            return;
        };
        self.perform(deed);
    }

    fn perform(&mut self, deed: Deed) {
        match deed {
            Deed::Mode(width, height) => {
                self.report = Some(match crate::slot::request_screen_mode(width, height) {
                    Ok(()) => Report::ok(format!(
                        "{width}×{height} будет использовано со следующего запуска"
                    )),
                    Err(err) => Report::bad(format!("выбор не сохранён: {err}")),
                });
            }
            // Тема меняется здесь и сейчас, без «Применить»: единственный способ
            // выбрать её — посмотреть на неё, а для этого её надо включить.
            Deed::Theme(dark) => {
                if theme::set_dark(dark) {
                    self.theme_changed = true;
                }
                // Записывается всегда, а не только при смене: человек, нажавший
                // на уже выбранную тему после отказа записи, вправе ожидать
                // второй попытки, а не молчания.
                self.report = Some(match super::prefs::store_theme(dark) {
                    Ok(()) => {
                        // В журнал, а не только в окно: снимок экрана
                        // доказательством не считается, и «тема запомнена»
                        // проверяется стендом по этой строке.
                        kprintln!(
                            "  settings    : theme {} saved",
                            if dark { "dark" } else { "light" }
                        );
                        Report::ok(String::from("тема запомнена"))
                    }
                    Err(err) => Report::bad(format!("тема применена, но не запомнена: {err}")),
                });
            }
            Deed::Timezone(minutes) => {
                let text = crate::time::zone_text(minutes);
                self.report = Some(match crate::time::set_timezone(minutes) {
                    Ok(()) => Report::ok(format!("часовой пояс {text} применён и запомнен")),
                    Err(err) => Report::bad(format!("{text} не записан: {err}")),
                });
            }
            Deed::NetDhcp => {
                self.report = Some(match crate::net::persist::forget() {
                    // Адрес не сбрасывается: он ещё работает, и обрывать связь
                    // раньше, чем её восстановит DHCP, значит отнимать у
                    // человека дорогу назад. Аренду возьмёт служба при
                    // следующей загрузке — об этом и сказано.
                    Ok(true) => {
                        kprintln!("  settings    : address back to DHCP, /etc/network.cfg removed");
                        Report::ok(String::from(
                            "адрес будет получаться по DHCP со следующего запуска",
                        ))
                    }
                    Ok(false) => Report::ok(String::from("адрес и так получается по DHCP")),
                    Err(err) => Report::bad(format!("не записано: {err}")),
                });
            }
            Deed::NetStatic => {
                // Не действие, а переход к правке: заполняем поля тем, что у
                // машины сейчас, и встаём в первое.
                self.net = NetDraft::current();
                self.net.editing = Some(Field::Address);
                self.report = None;
            }
            Deed::NetEdit(field) => {
                self.net.editing = Some(field);
                self.report = None;
            }
            Deed::NetApply => self.apply_network(),
            Deed::CheckDisks => {
                // Только проверка, без починки, и это не половина работы.
                // Чинить том, смонтированный на запись, нельзя: редактор
                // держит счётчики блоков в памяти, и `fsck`, поправивший их на
                // диске, разойдётся с ним молча. Ровно то же сказано в
                // `shell.rs` у команды `fsck`.
                let mut problems = 0usize;
                let mut volumes = 0usize;
                for (point, result) in crate::fs::check_all() {
                    match result {
                        Some(Ok(summary)) => {
                            volumes += 1;
                            // Не поместившиеся в список находки считаются тоже:
                            // «замечаний нет» на томе, где их было слишком
                            // много, чтобы перечислить, — самый вредный из
                            // возможных ответов.
                            problems += summary.problems.len() + summary.dropped;
                            kprintln!(
                                "  settings    : fsck {point}: {} problem(s)",
                                summary.problems.len() + summary.dropped
                            );
                        }
                        Some(Err(err)) => {
                            volumes += 1;
                            problems += 1;
                            kprintln!("  settings    : fsck {point}: {err}");
                        }
                        None => {}
                    }
                }
                self.report = Some(if problems == 0 {
                    Report::ok(format!("проверено томов: {volumes}, замечаний нет"))
                } else {
                    Report::bad(format!(
                        "замечаний: {problems}. Починить можно только при загрузке"
                    ))
                });
            }
            // Окно не качает обновление само и не ждёт его: `sysupdate` —
            // программа третьего кольца, у неё сеть, TLS и запись в раздел.
            // Окно только запускает её, а разговаривает она с человеком в
            // терминале — там же, где отвечала бы, набери он её имя руками.
            // Ждать её здесь нельзя: этот код работает внутри разбора события
            // ввода. То же самое и ниже, с `pkg`.
            Deed::CheckUpdates => self.report = Some(run("/bin/sysupdate", "sysupdate")),
            Deed::ChooseFile => {
                let found = available();
                let empty = found.is_empty();
                self.programs = Programs::Choose(found);
                self.action = 0;
                self.report = empty.then(|| {
                    Report::bad("файлов .fpk не нашлось ни в /media, ни дома".to_string())
                });
            }
            Deed::Install(path) => {
                let line = format!("/bin/pkg install {path}");
                self.report = Some(run(&line, "pkg install"));
                self.programs = Programs::List;
                self.action = 0;
            }
            Deed::AskRemove(name) => {
                self.programs = Programs::Confirm(name);
                self.action = 0;
                self.report = None;
            }
            Deed::Remove(name) => {
                let line = format!("/bin/pkg remove {name}");
                self.report = Some(run(&line, "pkg remove"));
                self.programs = Programs::List;
                self.action = 0;
            }
            Deed::BackToList => {
                self.programs = Programs::List;
                self.action = 0;
                self.report = None;
            }
        }
    }

    /// Нарисовать окно целиком.
    pub fn draw(&self, surface: &mut Surface, area: Rect, ctx: Ctx) {
        self.run(ctx, area, Some(surface));
    }

    // ── Один проход ──────────────────────────────────────────────────────────

    /// Пройти окно: нарисовать его и заодно собрать, куда можно нажать.
    fn run(&self, ctx: Ctx, area: Rect, surface: Option<&mut Surface>) -> Vec<Hit> {
        let mut pass = Pass {
            ctx,
            surface,
            hits: Vec::new(),
            focus: (!self.on_sections).then_some(self.action),
            counted: 0,
            limit: area.bottom(),
        };
        if area.w == 0 || area.h == 0 {
            return pass.hits;
        }
        let background = ctx.under;
        pass.on(|s| s.fill(area, background));

        let side_w = ctx.px(theme::SIDE_W).min(area.w / 2);
        self.sidebar(&mut pass, Rect::new(area.x, area.y, side_w, area.h));
        self.content(
            &mut pass,
            Rect::new(
                area.x + side_w as i32,
                area.y,
                area.w.saturating_sub(side_w),
                area.h,
            ),
        );
        pass.hits
    }

    /// Боковая колонка с разделами.
    fn sidebar(&self, pass: &mut Pass, area: Rect) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let fill = ctx.flat(p.panel);
        pass.on(|s| s.fill(area, fill));
        // Линия справа в одну точку, а не тень и не разница заливок: колонка и
        // содержимое отличаются друг от друга на несколько единиц яркости, и
        // держится граница между ними именно на ней.
        let edge = p.line;
        let edge_x = area.right() - 1;
        pass.on(|s| draw::vline(s, edge_x, area.y, area.h, edge.color, edge.alpha));

        // Всё в колонке лежит на панели, а не на окне: полупрозрачные токены
        // сводятся к разным цветам, и подложка обязана быть та, что под ними.
        let inner = ctx.on(fill);
        let pad = ctx.px(10);
        let row_h = ctx.px(theme::SIDE_ROW_H);
        let icon = ctx.px(15);
        let bottom = area.bottom() - ctx.px(16) as i32;
        let mut y = area.y + ctx.px(16) as i32;

        for (index, section) in Section::ALL.iter().enumerate() {
            let rect = Rect::new(
                area.x + pad as i32,
                y,
                area.w.saturating_sub(pad * 2),
                row_h,
            );
            y += (row_h + ctx.px(2)) as i32;
            if rect.bottom() > bottom || rect.w == 0 {
                continue;
            }
            let chosen = index == self.section;
            let ink = if !section.available() {
                p.ink6
            } else if chosen {
                p.ink
            } else {
                p.ink3
            };
            if chosen {
                pass.on(|s| paint::row(inner, s, rect, RowState::Selected));
                // Выбранный раздел выделен всегда, а обводкой акцентом — только
                // пока стрелки ходят по разделам: «где я сейчас» и «куда уедет
                // стрелка вниз» — разные вопросы, и один цвет на оба ответа
                // заставляет человека нажать, чтобы узнать.
                if self.on_sections {
                    let radius = ctx.px(theme::R_ROW);
                    pass.on(|s| draw::rounded_stroke(s, rect, radius, p.acc, 255));
                }
            }
            let glyph = section.icon();
            let icon_x = rect.x + ctx.px(12) as i32;
            let icon_y = rect.y + (rect.h as i32 - icon as i32) / 2;
            pass.on(|s| glyphicon::draw(s, glyph, icon_x, icon_y, icon, ink, 255));

            let text_x = icon_x + (icon + ctx.px(10)) as i32;
            let role = if chosen { Role::Title } else { Role::Body };
            let room = (rect.right() - ctx.px(10) as i32 - text_x).max(0) as u32;
            let baseline = paint::baseline(ctx, role, rect);
            pass.text_clipped(role, text_x, baseline, room, section.title(), ink);

            if section.available() {
                pass.section(rect, index);
            }
        }
    }

    /// Содержимое выбранного раздела.
    fn content(&self, pass: &mut Pass, area: Rect) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let pad = ctx.px(24);
        if area.w <= pad * 2 || area.h <= pad * 2 {
            return;
        }
        let inner = Rect::new(
            area.x + pad as i32,
            area.y + pad as i32,
            area.w - pad * 2,
            area.h - pad,
        );

        // Колонка предпросмотра появляется только там, где после неё остаётся
        // место на сами настройки: картинка, из-за которой список режимов сжался
        // до полоски, объясняет меньше, чем занимает.
        let wide = self.current() == Section::Display && inner.w > ctx.px(700);
        let preview = wide.then(|| {
            Rect::new(
                inner.right() - ctx.px(340) as i32,
                inner.y,
                ctx.px(340),
                inner.h,
            )
        });
        let main_w = match preview {
            Some(column) => (column.x - inner.x - ctx.px(24) as i32).max(0) as u32,
            None => inner.w,
        };
        let main = Rect::new(inner.x, inner.y, main_w, inner.h);

        // Низ занят ответом на последнее действие и кнопками разговора; тело
        // раздела заканчивается там, где начинается он.
        let buttons = self.buttons();
        let bottom_h = if buttons.is_empty() && self.report.is_none() {
            0
        } else {
            // Полоса низа одной высоты и под кнопками, и под одной отметкой:
            // иначе тело раздела подпрыгивало бы на несколько точек всякий раз,
            // когда действие что-то ответило.
            ctx.px(54)
        };
        pass.limit = main.bottom() - bottom_h as i32;

        let mut y = main.y;
        let heading = self.heading();
        pass.text_clipped(Role::Heading, main.x, y, main.w, &heading, p.ink);
        y += line_h(ctx, Role::Heading);
        let about = self.about();
        pass.text_clipped(Role::Body, main.x, y, main.w, &about, p.ink3);
        y += line_h(ctx, Role::Body) + ctx.px(16) as i32;

        match self.current() {
            Section::Display => self.draw_display(pass, main, y),
            Section::Clock => self.draw_clock(pass, main, y),
            Section::Network => self.draw_network(pass, main, y),
            Section::Disks => self.draw_disks(pass, main, y),
            Section::Users => Self::draw_users(pass, main, y),
            Section::Programs => self.draw_programs(pass, main, y),
            Section::Updates => self.draw_updates(pass, main, y),
            Section::System => self.draw_system(pass, main, y),
        }
        if let Some(column) = preview {
            draw_preview(pass, column);
        }
        pass.limit = main.bottom();
        self.draw_footer(pass, main, &buttons);
    }

    /// Раздел «Экран».
    fn draw_display(&self, pass: &mut Pass, main: Rect, y: i32) {
        let ctx = pass.ctx;
        let p = ctx.palette;

        // ── Разрешение ───────────────────────────────────────────────────────
        let row_h = ctx.px(30);
        let gap = ctx.px(2);
        let count = MODES.len() as u32;
        let list_w = ctx.px(230).min(main.w);
        let list_h = row_h * count + gap * count.saturating_sub(1);
        let (list, mut y) = setting(
            pass,
            main,
            y,
            "Разрешение",
            "Режим задаёт прошивка.",
            (list_w, list_h),
        );
        for (index, (width, height)) in MODES.iter().enumerate() {
            let rect = Rect::new(
                list.x,
                list.y + (index as u32 * (row_h + gap)) as i32,
                list.w,
                row_h,
            );
            let focused = pass.deed(rect, Deed::Mode(*width, *height));
            if !pass.visible(rect) {
                continue;
            }
            let current = (*width, *height) == self.screen;
            let state = if current {
                RowState::Selected
            } else if focused {
                RowState::Hover
            } else {
                RowState::Idle
            };
            pass.on(|s| paint::row(ctx, s, rect, state));
            if focused && current {
                pass.on(|s| draw::rounded_stroke(s, rect, ctx.px(theme::R_ROW), p.acc, 255));
            }
            let label = format!("{width} × {height}");
            let ink = paint::row_ink(ctx, state);
            let baseline = paint::baseline(ctx, Role::Mono, rect);
            let room = rect.w.saturating_sub(ctx.px(96));
            pass.text_clipped(Role::Mono, rect.x + ctx.px(12) as i32, baseline, room, &label, ink);
            if current {
                let chip_w = paint::chip_width(ctx, "СЕЙЧАС");
                let chip = Rect::new(
                    rect.right() - (chip_w + ctx.px(8)) as i32,
                    rect.y + (rect.h as i32 - ctx.px(20) as i32) / 2,
                    chip_w,
                    ctx.px(20),
                );
                pass.on(|s| paint::chip(ctx, s, chip, "СЕЙЧАС", Tone::Accent));
            }
        }

        // ── Тема ─────────────────────────────────────────────────────────────
        let swatch_w = ctx.px(80);
        let swatch_h = ctx.px(6) * 3 + ctx.px(24) + line_h(ctx, Role::MonoCaps) as u32;
        let gap = ctx.px(10);
        let (slot, next) = setting(
            pass,
            main,
            y,
            "Тема",
            "Две палитры, одна геометрия.",
            (swatch_w * 2 + gap, swatch_h),
        );
        for (index, dark) in [true, false].into_iter().enumerate() {
            let rect = Rect::new(
                slot.x + (index as u32 * (swatch_w + gap)) as i32,
                slot.y,
                swatch_w,
                swatch_h,
            );
            let focused = pass.deed(rect, Deed::Theme(dark));
            if pass.visible(rect) {
                swatch(pass, rect, dark, theme::is_dark() == dark, focused);
            }
        }
        y = next;

        // ── Масштаб интерфейса ───────────────────────────────────────────────
        //
        // Не переключатель: множитель выводится из ширины экрана — тот же порог,
        // что у размерного ряда шрифта, — и отдельного признака под ним нет.
        // Вкладка «200 %», которая не включается, объясняла бы человеку не то,
        // как устроен интерфейс, а то, что кнопка сломана.
        let scale_label = if ctx.scale >= 2 { "200 %" } else { "100 %" };
        let chip_w = paint::chip_width(ctx, scale_label);
        let (slot, next) = setting(
            pass,
            main,
            y,
            "Масштаб интерфейса",
            "Целые кратности: шрифт остаётся резким.",
            (chip_w, ctx.px(26)),
        );
        if pass.visible(slot) {
            pass.on(|s| paint::chip(ctx, s, slot, scale_label, Tone::Muted));
        }
        y = next;

        pass.note(
            main.x,
            y + ctx.px(16) as i32,
            main.w,
            "Режим экрана применяется при следующем запуске.",
        );
    }

    /// Раздел «Дата и время».
    ///
    /// Часы показываются, но не задаются, и это не недоделка. Время система
    /// однажды прочитала у прошивки (UEFI `GetTime` до выхода из boot services),
    /// а обратной операции после выхода не существует — писать в RTC напрямую
    /// значило бы завести драйвер часов на каждую машину отдельно. Поэтому
    /// раздел настраивает то единственное, что здесь настраивается: сдвиг.
    fn draw_clock(&self, pass: &mut Pass, main: Rect, y: i32) {
        let ctx = pass.ctx;
        let now = crate::time::now_local();
        // Две строки, а не три: «по Гринвичу» выводится из местного времени и
        // смещения, которые тут же рядом, и место в окне стоит дороже, чем
        // избавление читателя от вычитания.
        let rows = [
            (
                "Местное время".to_string(),
                now.map_or_else(|| "часов нет".to_string(), |t| format!("{t}")),
            ),
            ("Смещение".to_string(), crate::time::offset_text()),
        ];
        let y = fact_card(pass, main, y, "ЧАСЫ", &rows);

        // Список в два столбца: тринадцать поясов в одну колонку — это четыреста
        // точек, то есть больше, чем остаётся под содержимое. Столбцы, а не
        // прокрутка: прокрутка требует полосы, колеса и памяти о положении, а
        // тринадцать строк — это семь строк в два ряда.
        let row_h = ctx.px(30);
        let gap = ctx.px(2);
        let column_gap = ctx.px(8);
        let zones = crate::time::TIMEZONES;
        let rows_count = zones.len().div_ceil(2) as u32;
        let column_w = ctx.px(150);
        let list_w = (column_w * 2 + column_gap).min(main.w);
        let list_h = row_h * rows_count + gap * rows_count.saturating_sub(1);
        let (list, next) = setting(
            pass,
            main,
            y,
            "Часовой пояс",
            "Применяется сразу и запоминается.",
            (list_w, list_h),
        );
        let current = crate::time::offset_minutes();
        for (index, minutes) in zones.iter().enumerate() {
            // Заполняется **по столбцам**, сверху вниз и слева направо: так
            // порядок на экране совпадает с порядком, по которому ходят стрелки
            // клавиатуры. Заполнение по строкам развело бы их, и стрелка вниз
            // прыгала бы через полсписка.
            let column = index as u32 / rows_count;
            let row = index as u32 % rows_count;
            let rect = Rect::new(
                list.x + (column * (column_w + column_gap)) as i32,
                list.y + (row * (row_h + gap)) as i32,
                column_w,
                row_h,
            );
            let focused = pass.deed(rect, Deed::Timezone(*minutes));
            if !pass.visible(rect) {
                continue;
            }
            choice_row(pass, rect, &crate::time::zone_text(*minutes), *minutes == current, focused);
        }

        pass.note(
            main.x,
            next + ctx.px(8) as i32,
            main.w,
            "Часы идут от прошивки: перевести их эта система не умеет.",
        );
    }

    /// Раздел «Сеть».
    fn draw_network(&self, pass: &mut Pass, main: Rect, y: i32) {
        let ctx = pass.ctx;
        let Some(status) = crate::net::status() else {
            pass.note(main.x, y, main.w, "В этой машине нет сетевой карты.");
            return;
        };

        // Четыре строки, не пять: «что записано» видно ниже по отметке «СЕЙЧАС»
        // в списке выбора, и повторять это карточкой значит занимать высоту,
        // которой не хватает полю шлюза.
        let rows = [
            (
                "Аппаратный адрес".to_string(),
                format!("{}", crate::net::eth::Display(status.mac)),
            ),
            (
                "Адрес".to_string(),
                if status.address.is_unspecified() {
                    "пока нет".to_string()
                } else {
                    format!("{}/{}", status.address, status.netmask.prefix().unwrap_or(0))
                },
            ),
            (
                "Шлюз".to_string(),
                if status.gateway.is_unspecified() {
                    "нет".to_string()
                } else {
                    format!("{}", status.gateway)
                },
            ),
            (
                "Сервер имён".to_string(),
                if status.dns.is_unspecified() {
                    "нет".to_string()
                } else {
                    format!("{}", status.dns)
                },
            ),
        ];
        let y = fact_card(pass, main, y, "СЕЙЧАС", &rows);

        // ── Откуда брать адрес ───────────────────────────────────────────────
        let row_h = ctx.px(30);
        let gap = ctx.px(2);
        let list_w = ctx.px(260).min(main.w);
        let (list, mut y) = setting(
            pass,
            main,
            y,
            "Откуда адрес",
            "Выбор запоминается на разделе состояния.",
            (list_w, row_h * 2 + gap),
        );
        let is_static = matches!(
            crate::net::persist::load().map(|settings| settings.mode),
            Some(crate::net::persist::Mode::Static)
        );
        for (index, (deed, label, chosen)) in [
            (Deed::NetDhcp, "Получать автоматически (DHCP)", !is_static),
            (Deed::NetStatic, "Постоянный адрес", is_static),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = Rect::new(
                list.x,
                list.y + (index as u32 * (row_h + gap)) as i32,
                list.w,
                row_h,
            );
            let focused = pass.deed(rect, deed);
            if pass.visible(rect) {
                choice_row(pass, rect, label, chosen, focused);
            }
        }

        // ── Поля ─────────────────────────────────────────────────────────────
        let field_h = ctx.px(30);
        let field_w = ctx.px(200).min(main.w);
        for (field, label, note) in [
            (
                Field::Address,
                "Адрес и префикс",
                "Например 10.0.2.15/24. Цифры, точка и косая.",
            ),
            (Field::Gateway, "Шлюз", "Пусто — шлюза нет."),
        ] {
            let (slot, next) = setting(pass, main, y, label, note, (field_w, field_h));
            let focused = pass.deed(slot, Deed::NetEdit(field));
            if pass.visible(slot) {
                let active = self.net.editing == Some(field);
                text_field(pass, slot, self.net.text(field), active, focused);
            }
            y = next;
        }
        // Кнопка «Применить» рисуется в подвале окна, а не здесь — см.
        // [`SettingsView::buttons`]. Подвал прижат к низу и виден всегда, а
        // содержимое раздела кончается там, где кончается место.
        let _ = y;
    }

    /// Раздел «Диски».
    fn draw_disks(&self, pass: &mut Pass, main: Rect, y: i32) {
        let ctx = pass.ctx;
        let mut rows: Vec<(String, String)> = Vec::new();
        for (point, kind) in crate::fs::mounted() {
            // У корня приставка пустая — так его и хранит таблица монтирования,
            // потому что она приписывается к пути слева. Строка с пустым именем
            // выглядит как потерянная запись, поэтому корень называется здесь
            // тем, чем его называет человек.
            let point = if point.is_empty() { "/" } else { point };
            rows.push((point.to_string(), kind.to_string()));
        }
        if rows.is_empty() {
            rows.push(("—".to_string(), "ничего не смонтировано".to_string()));
        }
        // «Точки», а не «тома», и разница не словесная: пять из шести строк
        // ниже — ветки **одного** раздела состояния, и назвать их томами
        // значило бы обещать шесть дисков там, где их два. Сколько на самом
        // деле томов, говорит проверка: она обходит каждый по разу.
        let y = fact_card(pass, main, y, "ТОЧКИ МОНТИРОВАНИЯ", &rows);

        let button_h = ctx.px(30);
        let button_w = paint::chip_width(ctx, "Проверить файловые системы") + ctx.px(28);
        let (slot, next) = setting(
            pass,
            main,
            y,
            "Проверка",
            "Только чтение: чинить том под работающей системой нельзя.",
            (button_w.min(main.w), button_h),
        );
        let focused = pass.deed(slot, Deed::CheckDisks);
        if pass.visible(slot) {
            pass.on(|s| {
                paint::button(ctx, s, slot, Weight::Normal, "Проверить файловые системы", focused);
            });
        }

        pass.note(
            main.x,
            next + ctx.px(8) as i32,
            main.w,
            "Найденное чинит проверка при загрузке — том тогда ещё никто не правит.",
        );
    }

    /// Раздел «Пользователи».
    ///
    /// Только список, и это названо в самом окне, а не спрятано. Завести
    /// пользователя отсюда нельзя, потому что пароль в этой системе умеет
    /// хешировать ровно один код — установщика, — и он живёт вне ядра. Кнопка
    /// «Добавить», открывающая окно, которое ничего не создаёт, была бы хуже
    /// отсутствующей кнопки.
    fn draw_users(pass: &mut Pass, main: Rect, y: i32) {
        let rows = accounts();
        let mut cards: Vec<(String, String)> = Vec::new();
        for (name, uid, gid) in &rows {
            cards.push((name.clone(), format!("uid {uid}, gid {gid}")));
        }
        if cards.is_empty() {
            cards.push((
                "—".to_string(),
                "учётных записей нет: система загружена с носителя".to_string(),
            ));
        }
        let mut y = fact_card(pass, main, y, "УЧЁТНЫЕ ЗАПИСИ", &cards);

        let session = crate::user::session::credentials();
        let name = crate::user::session::with_name(|name| name.to_string());
        y = fact_card(pass, main, y, "ЭТОТ СЕАНС", &[(
            name,
            format!("uid {}, gid {}", session.uid, session.gid),
        )]);

        pass.note(
            main.x,
            y + pass.ctx.px(8) as i32,
            main.w,
            "Учётные записи заводит установщик: хешировать пароль умеет только он.",
        );
    }

    /// Раздел «Пакеты».
    fn draw_programs(&self, pass: &mut Pass, main: Rect, y: i32) {
        match &self.programs {
            Programs::List => self.draw_installed(pass, main, y),
            Programs::Choose(found) => {
                let mut y = pass.caps_line(main.x, y, "НАЙДЕННЫЕ ФАЙЛЫ");
                if found.is_empty() {
                    pass.note(
                        main.x,
                        y,
                        main.w,
                        "В /media, в домашнем каталоге и на столе ничего не нашлось.",
                    );
                    return;
                }
                y = list_card(pass, main, y, found.len(), |pass, index, rect| {
                    let path = &found[index];
                    let focused = pass.deed(rect, Deed::Install(path.clone()));
                    if !pass.visible(rect) {
                        return;
                    }
                    row_line(pass, rect, &short_name(path), focused, None);
                });
                pass.note(
                    main.x,
                    y,
                    main.w,
                    "Пакет ставит pkg install; он же проверяет подпись.",
                );
            }
            Programs::Confirm(name) => {
                let rows = [
                    ("Каталог".to_string(), format!("/opt/{name}")),
                    ("Реестр".to_string(), format!("/var/lib/pkg/{name}.pkg")),
                ];
                let y = fact_card(pass, main, y, "ЧТО ИСЧЕЗНЕТ", &rows);
                pass.note(
                    main.x,
                    y,
                    main.w,
                    "Удаляет pkg remove; вернуть удалённое можно только установкой заново.",
                );
            }
        }
    }

    fn draw_installed(&self, pass: &mut Pass, main: Rect, y: i32) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let mut y = pass.caps_line(main.x, y, "УСТАНОВЛЕНО");
        match packages() {
            Ok(names) if names.is_empty() => {
                y = pass.note(main.x, y, main.w, "Пока ничего не установлено.");
            }
            Ok(names) => {
                y = list_card(pass, main, y, names.len(), |pass, index, rect| {
                    let name = &names[index];
                    let focused = pass.deed(rect, Deed::AskRemove(name.clone()));
                    if !pass.visible(rect) {
                        return;
                    }
                    row_line(pass, rect, name, focused, Some(("УДАЛИТЬ", Tone::Muted)));
                });
            }
            // Отказ реестра — не пустой список: «пакетов нет» и «спросить не
            // удалось» человек обязан различать, иначе он поставит второй раз
            // то, что уже стоит.
            Err(err) => {
                let chip_w = paint::chip_width(ctx, "РЕЕСТР");
                let chip = Rect::new(main.x, y, chip_w, ctx.px(20));
                if pass.visible(chip) {
                    pass.on(|s| paint::chip(ctx, s, chip, "РЕЕСТР", Tone::Bad));
                }
                let text_x = chip.right() + ctx.px(10) as i32;
                let room = (main.right() - text_x).max(0) as u32;
                let baseline = paint::baseline(ctx, Role::Caption, chip);
                pass.text_clipped(Role::Caption, text_x, baseline, room, &err, p.ink4);
                y = chip.bottom() + ctx.px(12) as i32;
            }
        }

        let label = "Выбрать файл";
        let (slot, next) = setting(
            pass,
            main,
            y,
            "Установить пакет",
            "Файлы .fpk с носителя, из домашнего каталога и со стола.",
            (button_width(ctx, label), ctx.px(34)),
        );
        let focused = pass.deed(slot, Deed::ChooseFile);
        if pass.visible(slot) {
            pass.on(|s| paint::button(ctx, s, slot, Weight::Primary, label, focused));
        }
        pass.note(
            main.x,
            next + ctx.px(16) as i32,
            main.w,
            "Пакеты живут в /opt.",
        );
    }

    /// Раздел «Обновление».
    fn draw_updates(&self, pass: &mut Pass, main: Rect, y: i32) {
        let ctx = pass.ctx;
        let mut rows = Vec::new();
        rows.push(("Установлена".to_string(), crate::VERSION.to_string()));
        let servers = update_servers();
        if servers.is_empty() {
            rows.push((
                "Серверы".to_string(),
                "в update.cfg не задан ни один".to_string(),
            ));
        } else {
            // Порядок тот же, в каком их пробует `sysupdate`: список, в котором
            // сервера переставлены, отвечает не на тот вопрос, который задан.
            for server in servers {
                rows.push(("Сервер".to_string(), server));
            }
        }
        let y = fact_card(pass, main, y, "ОБНОВЛЕНИЕ", &rows);

        let label = "Проверить";
        let (slot, next) = setting(
            pass,
            main,
            y,
            "Проверить обновления",
            "Запускает sysupdate; отвечает он в терминале.",
            (button_width(ctx, label), ctx.px(34)),
        );
        let focused = pass.deed(slot, Deed::CheckUpdates);
        if pass.visible(slot) {
            pass.on(|s| paint::button(ctx, s, slot, Weight::Primary, label, focused));
        }
        pass.note(
            main.x,
            next + ctx.px(16) as i32,
            main.w,
            "Обновление ставится только с подписью, которой доверяет эта машина.",
        );
    }

    /// Раздел «О системе».
    fn draw_system(&self, pass: &mut Pass, main: Rect, y: i32) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let frames = crate::mm::frame::stats();
        let uptime = crate::time::uptime_ms() / 1000;
        let rows = [
            ("Версия".to_string(), crate::VERSION.to_string()),
            ("Архитектура".to_string(), arch::ARCH_NAME.to_string()),
            (
                "Экран".to_string(),
                format!("{}×{}", self.screen.0, self.screen.1),
            ),
            (
                "Время работы".to_string(),
                format!(
                    "{} ч {:02} мин {:02} с",
                    uptime / 3600,
                    (uptime / 60) % 60,
                    uptime % 60
                ),
            ),
            ("Смонтировано".to_string(), mounted_text()),
        ];
        let mut y = fact_card(pass, main, y, "МАШИНА", &rows);

        // Память показана полосой, а не двумя числами: «свободно 412 из 512»
        // требует деления в уме, а полоса отвечает на вопрос «много ли осталось»
        // до того, как человек дочитает подпись.
        y = pass.caps_line(main.x, y, "ПАМЯТЬ");
        let total_mib = (frames.total_bytes() / (1024 * 1024)) as u32;
        let free_mib = (frames.free_bytes() / (1024 * 1024)) as u32;
        let used_mib = total_mib.saturating_sub(free_mib);
        let bar = Rect::new(main.x, y, main.w, ctx.px(8));
        if pass.visible(bar) {
            let tone = if free_mib * 8 < total_mib {
                Tone::Warn
            } else {
                Tone::Accent
            };
            pass.on(|s| paint::progress(ctx, s, bar, used_mib, total_mib, tone));
        }
        y = bar.bottom() + ctx.px(8) as i32;
        let text = format!("занято {used_mib} МиБ из {total_mib} МиБ");
        pass.text_clipped(Role::Caption, main.x, y, main.w, &text, p.ink4);
        y += line_h(ctx, Role::Caption) + ctx.px(16) as i32;

        pass.note(main.x, y, main.w, "Написана с нуля на Rust.");
    }

    /// Низ окна: чем кончилось действие и кнопки разговора.
    fn draw_footer(&self, pass: &mut Pass, main: Rect, buttons: &[(Deed, &'static str, Weight)]) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let button_h = ctx.px(34);
        let top = main.bottom() - (ctx.px(20) + button_h) as i32;

        let mut widths = Vec::new();
        let mut total = 0;
        for (_, label, _) in buttons {
            let width = button_width(ctx, label);
            total += width;
            widths.push(width);
        }
        if !buttons.is_empty() {
            total += ctx.px(10) * (buttons.len() as u32 - 1);
        }
        let mut x = main.right() - total as i32;
        for ((deed, label, weight), width) in buttons.iter().zip(widths) {
            let rect = Rect::new(x, top, width, button_h);
            x += (width + ctx.px(10)) as i32;
            let focused = pass.deed(rect, deed.clone());
            if !pass.visible(rect) {
                continue;
            }
            let weight = *weight;
            pass.on(|s| paint::button(ctx, s, rect, weight, label, focused));
        }

        // Ответ последнего действия — внизу, отметкой: он относится ко всему
        // окну, а не к строке, по которой нажали.
        let Some(report) = &self.report else {
            return;
        };
        let (label, tone) = if report.ok {
            ("ГОТОВО", Tone::Ok)
        } else {
            ("ОШИБКА", Tone::Bad)
        };
        let chip_h = ctx.px(26);
        let chip = Rect::new(
            main.x,
            top + (button_h as i32 - chip_h as i32) / 2,
            paint::chip_width(ctx, label),
            chip_h,
        );
        if !pass.visible(chip) {
            return;
        }
        pass.on(|s| paint::chip(ctx, s, chip, label, tone));
        let text_x = chip.right() + ctx.px(10) as i32;
        let right = main.right() - total as i32 - ctx.px(16) as i32;
        let room = (right - text_x).max(0) as u32;
        let baseline = paint::baseline(ctx, Role::Caption, chip);
        pass.text_clipped(Role::Caption, text_x, baseline, room, &report.text, p.ink4);
    }
}

// ── Проход ───────────────────────────────────────────────────────────────────

/// Куда попадает щелчок.
#[derive(Clone, PartialEq, Eq)]
enum Spot {
    /// Строка бокового списка.
    Section(usize),
    /// Пункт содержимого.
    Deed(Deed),
}

/// Прямоугольник вместе с тем, что за ним стоит.
struct Hit {
    rect: Rect,
    spot: Spot,
}

/// Один проход по окну: он и рисует, и собирает то, по чему можно нажать.
struct Pass<'a> {
    ctx: Ctx,
    /// Куда рисовать. `None` — проход только считает.
    surface: Option<&'a mut Surface>,
    hits: Vec<Hit>,
    /// Номер пункта, на котором стоит клавиатурный фокус.
    focus: Option<usize>,
    /// Сколько пунктов уже выдано номеров.
    counted: usize,
    /// Ниже этой строки содержимое не рисуется и не нажимается.
    limit: i32,
}

impl Pass<'_> {
    /// Сделать что-то с поверхностью, если она есть.
    fn on(&mut self, f: impl FnOnce(&mut Surface)) {
        if let Some(surface) = self.surface.as_deref_mut() {
            f(surface);
        }
    }

    /// Помещается ли элемент в отведённое содержимому место.
    const fn visible(&self, rect: Rect) -> bool {
        rect.bottom() <= self.limit
    }

    fn section(&mut self, rect: Rect, index: usize) {
        self.hits.push(Hit {
            rect,
            spot: Spot::Section(index),
        });
    }

    /// Завести пункт. Возвращает `true`, если на нём стоит клавиатурный фокус.
    ///
    /// Номер выдаётся всегда, даже пункту, который не поместился: это же его
    /// номер в [`SettingsView::deeds`], по которому ходят стрелки, и пропуск
    /// сдвинул бы весь дальнейший счёт. А вот попадание у невидимого пункта не
    /// заводится: щелчок по низу окна не должен выполнять то, чего не видно.
    fn deed(&mut self, rect: Rect, deed: Deed) -> bool {
        let index = self.counted;
        self.counted += 1;
        if self.visible(rect) && !rect.is_empty() {
            self.hits.push(Hit {
                rect,
                spot: Spot::Deed(deed),
            });
        }
        self.focus == Some(index)
    }

    fn text_clipped(&mut self, role: Role, x: i32, y: i32, room: u32, t: &str, color: Color) {
        let ctx = self.ctx;
        self.on(|s| {
            paint::text_clipped(ctx, s, role, x, y, room, t, color);
        });
    }

    /// Заголовок группы. Возвращает координату следующего элемента.
    fn caps_line(&mut self, x: i32, y: i32, t: &str) -> i32 {
        let ctx = self.ctx;
        let rect = Rect::new(x, y, 1, line_h(ctx, Role::MonoCaps) as u32);
        if self.visible(rect) {
            self.on(|s| {
                paint::caps(ctx, s, x, y, t);
            });
        }
        y + line_h(ctx, Role::MonoCaps) + ctx.px(10) as i32
    }

    /// Мелкое пояснение под блоком. Возвращает координату следующего элемента.
    fn note(&mut self, x: i32, y: i32, room: u32, t: &str) -> i32 {
        let ctx = self.ctx;
        let rect = Rect::new(x, y, room, line_h(ctx, Role::Caption) as u32);
        if self.visible(rect) {
            let color = ctx.palette.ink4;
            self.text_clipped(Role::Caption, x, y, room, t, color);
        }
        y + line_h(ctx, Role::Caption) + ctx.px(12) as i32
    }
}

// ── Кирпичи раскладки ────────────────────────────────────────────────────────

/// Высота строки этого начертания.
fn line_h(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

/// Ширина кнопки с такой подписью.
fn button_width(ctx: Ctx, label: &str) -> u32 {
    ctx.face(Role::Body).width(label) + ctx.px(40)
}

/// Строка настройки: слева подпись с пояснением, справа управляющий элемент.
///
/// Возвращает место под элемент и координату следующей строки, а не рисует его
/// сама. Элементы разные — тумблер, список, кнопка, — а разметка у них одна, и
/// считать её в каждом означало бы считать раскладку столько раз, сколько есть
/// видов управления.
fn setting(
    pass: &mut Pass,
    area: Rect,
    y: i32,
    label: &str,
    note: &str,
    control: (u32, u32),
) -> (Rect, i32) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let pad = ctx.px(16) as i32;
    let (control_w, control_h) = control;
    let strong = line_h(ctx, Role::Strong);
    let caption = line_h(ctx, Role::Caption);
    let gap = ctx.px(2) as i32;
    let text_h = if note.is_empty() {
        strong
    } else {
        strong + gap + caption
    };
    let body = text_h.max(control_h as i32);
    let top = y + pad;
    let rect = Rect::new(
        area.right() - control_w as i32,
        top + (body - control_h as i32) / 2,
        control_w,
        control_h,
    );

    if pass.visible(Rect::new(area.x, y, area.w, (pad + body) as u32)) {
        pass.on(|s| paint::separator(ctx, s, area.x, y, area.w));
        let room = area.w.saturating_sub(control_w + ctx.px(20));
        let text_y = top + (body - text_h) / 2;
        pass.text_clipped(Role::Strong, area.x, text_y, room, label, p.ink2);
        if !note.is_empty() {
            pass.text_clipped(
                Role::Caption,
                area.x,
                text_y + strong + gap,
                room,
                note,
                p.ink4,
            );
        }
    }
    (rect, top + body + pad)
}

/// Карточка с парами «подпись — значение».
///
/// Значение стоит во второй колонке, а не прижато к правому краю: длинный
/// список смонтированного, выровненный вправо, наезжал бы на собственную
/// подпись, и обрезать пришлось бы не хвост, а начало.
/// Строка списка, из которого выбирают одно.
///
/// Отдельной функцией, потому что таких списков стало четыре — разрешения,
/// пояса, откуда брать адрес — и переписанная в каждом разметка разошлась бы
/// первой же правкой отступа. Отметка «СЕЙЧАС» здесь та же, что у разрешений, и
/// это не совпадение: одинаковый смысл обязан выглядеть одинаково.
fn choice_row(pass: &mut Pass, rect: Rect, label: &str, current: bool, focused: bool) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let state = if current {
        RowState::Selected
    } else if focused {
        RowState::Hover
    } else {
        RowState::Idle
    };
    pass.on(|s| paint::row(ctx, s, rect, state));
    if focused && current {
        pass.on(|s| draw::rounded_stroke(s, rect, ctx.px(theme::R_ROW), p.acc, 255));
    }
    let ink = paint::row_ink(ctx, state);
    let baseline = paint::baseline(ctx, Role::Mono, rect);
    let chip_w = paint::chip_width(ctx, "СЕЙЧАС");
    // Место под отметку резервируется только там, где она есть. Резервировать
    // всегда было бы проще, но в узком столбце — а часовые пояса стоят в два
    // столбца по 150 точек — это съедало бы две трети строки, и «UTC+05:30»
    // обрезалось бы у каждого пояса, кроме выбранного.
    let room = if current {
        rect.w.saturating_sub(chip_w + ctx.px(28))
    } else {
        rect.w.saturating_sub(ctx.px(24))
    };
    pass.text_clipped(Role::Mono, rect.x + ctx.px(12) as i32, baseline, room, label, ink);
    if current {
        let chip = Rect::new(
            rect.right() - (chip_w + ctx.px(8)) as i32,
            rect.y + (rect.h as i32 - ctx.px(20) as i32) / 2,
            chip_w,
            ctx.px(20),
        );
        pass.on(|s| paint::chip(ctx, s, chip, "СЕЙЧАС", Tone::Accent));
    }
}

/// Поле ввода с кареткой.
///
/// Каретка рисуется только у поля, в котором стоит ввод, и это единственное,
/// чем оно отличается от соседнего: рамка у обоих одна. Мигания нет намеренно —
/// мигающая каретка требует перерисовки по таймеру, то есть кадра в секунду на
/// пустом месте, а у окна, живущего в ядре, кадр стоит дороже, чем у программы.
fn text_field(pass: &mut Pass, rect: Rect, text: &str, active: bool, focused: bool) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    pass.on(|s| paint::sunk(ctx, s, rect, ctx.px(theme::R_ROW)));
    if active || focused {
        let ink = if active { p.acc } else { p.ink4 };
        pass.on(|s| draw::rounded_stroke(s, rect, ctx.px(theme::R_ROW), ink, 255));
    }
    let baseline = paint::baseline(ctx, Role::Mono, rect);
    let x = rect.x + ctx.px(10) as i32;
    let room = rect.w.saturating_sub(ctx.px(24));
    // Ширина берётся у шрифта, а не у отрисовки: рисование обрезает строку по
    // месту, и каретка, поставленная по нарисованному, уезжала бы к левому краю
    // ровно тогда, когда строка перестала помещаться.
    let width = ctx.face(Role::Mono).width(text).min(room);
    pass.text_clipped(Role::Mono, x, baseline, room, text, p.ink);
    if active {
        let caret = Rect::new(
            x + width as i32 + ctx.px(1) as i32,
            rect.y + ctx.px(6) as i32,
            ctx.px(2),
            rect.h.saturating_sub(ctx.px(12)),
        );
        pass.on(|s| s.fill(caret, p.acc));
    }
}

/// Учётные записи из `/etc/passwd`: имя, uid, gid.
///
/// Читается мимо проверки прав по той же причине, что и в `user::session`:
/// спрашивает ядро, а не программа, и файл `0640 root` иначе не открыть. Разбор
/// свой и минимальный — нужны три поля из восьми, а хеш пароля не нужен вовсе и
/// в окно не попадает.
fn accounts() -> Vec<(String, u32, u32)> {
    let Some((bytes, _)) = config::read("passwd", 8 * 1024) else {
        return Vec::new();
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split(':');
        let (Some(name), Some(uid), Some(gid)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(uid), Ok(gid)) = (uid.parse::<u32>(), gid.parse::<u32>()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        out.push((name.to_string(), uid, gid));
    }
    out
}

fn fact_card(pass: &mut Pass, area: Rect, y: i32, title: &str, rows: &[(String, String)]) -> i32 {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let pad = ctx.px(14);
    let row_h = line_h(ctx, Role::Mono).max(line_h(ctx, Role::Caption)) + ctx.px(10) as i32;
    let head_h = line_h(ctx, Role::MonoCaps) + ctx.px(10) as i32;
    let height = pad * 2 + head_h as u32 + row_h as u32 * rows.len() as u32;
    let card = Rect::new(area.x, y, area.w, height);
    if !pass.visible(card) || card.w <= pad * 2 {
        return card.bottom() + ctx.px(14) as i32;
    }
    pass.on(|s| paint::card(ctx, s, card));
    let x = card.x + pad as i32;
    pass.on(|s| {
        paint::caps(ctx, s, x, card.y + pad as i32, title);
    });

    // Колонка значений — сорок процентов ширины карточки: короче — и «1920×1080»
    // переносится, длиннее — и подпись рядом с ним теряется в пустоте.
    let column = (card.w - pad * 2) * 2 / 5;
    let value_x = card.right() - pad as i32 - column as i32;
    let mut row_y = card.y + pad as i32 + head_h;
    for (label, value) in rows {
        let label_room = (value_x - x - ctx.px(10) as i32).max(0) as u32;
        pass.text_clipped(
            Role::Caption,
            x,
            row_y + (row_h - line_h(ctx, Role::Caption)) / 2,
            label_room,
            label,
            p.ink4,
        );
        pass.text_clipped(
            Role::Mono,
            value_x,
            row_y + (row_h - line_h(ctx, Role::Mono)) / 2,
            column,
            value,
            p.ink2,
        );
        row_y += row_h;
    }
    card.bottom() + ctx.px(14) as i32
}

/// Карточка со списком строк по 30 точек.
///
/// Строки рисует вызывающий: у списка пакетов справа отметка «УДАЛИТЬ», у
/// списка файлов её нет, а раскладка одна и та же.
fn list_card(
    pass: &mut Pass,
    area: Rect,
    y: i32,
    count: usize,
    mut each: impl FnMut(&mut Pass, usize, Rect),
) -> i32 {
    let ctx = pass.ctx;
    let pad = ctx.px(10);
    let row_h = ctx.px(30);
    let gap = ctx.px(2);
    let rows = count as u32;
    let height = pad * 2 + row_h * rows + gap * rows.saturating_sub(1);
    let card = Rect::new(area.x, y, area.w, height);
    if pass.visible(card) {
        pass.on(|s| paint::card(ctx, s, card));
    }
    for index in 0..count {
        let rect = Rect::new(
            card.x + pad as i32,
            card.y + pad as i32 + (index as u32 * (row_h + gap)) as i32,
            card.w.saturating_sub(pad * 2),
            row_h,
        );
        each(pass, index, rect);
    }
    card.bottom() + ctx.px(14) as i32
}

/// Строка списка внутри карточки: подпись слева, необязательная отметка справа.
fn row_line(pass: &mut Pass, rect: Rect, label: &str, focused: bool, mark: Option<(&str, Tone)>) {
    let ctx = pass.ctx;
    // Строка лежит на карточке, а не на окне: выделение сводится к цвету по
    // подложке, и подложка тут другая.
    let inner = ctx.on(ctx.flat(ctx.palette.card));
    let state = if focused {
        RowState::Selected
    } else {
        RowState::Idle
    };
    pass.on(|s| paint::row(inner, s, rect, state));
    let mark_w = match mark {
        Some((text, _)) => paint::chip_width(ctx, text) + ctx.px(16),
        None => ctx.px(8),
    };
    let room = rect.w.saturating_sub(ctx.px(12) + mark_w);
    let baseline = paint::baseline(ctx, Role::Body, rect);
    let ink = paint::row_ink(inner, state);
    pass.text_clipped(Role::Body, rect.x + ctx.px(12) as i32, baseline, room, label, ink);
    if let Some((text, tone)) = mark {
        let width = paint::chip_width(ctx, text);
        let chip = Rect::new(
            rect.right() - (width + ctx.px(8)) as i32,
            rect.y + (rect.h as i32 - ctx.px(20) as i32) / 2,
            width,
            ctx.px(20),
        );
        pass.on(|s| paint::chip(inner, s, chip, text, tone));
    }
}

/// Образец темы: карточка с прямоугольником внутри и подписью под ним.
///
/// Цвета образца заданы числами, а не токенами палитры: образец показывает ту
/// тему, которая **не** включена, и взятый из действующей палитры он показывал
/// бы обе клетки одинаковыми.
fn swatch(pass: &mut Pass, rect: Rect, dark: bool, current: bool, focused: bool) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let radius = ctx.px(theme::R_ROW);
    let pad = ctx.px(6);
    let caps_h = line_h(ctx, Role::MonoCaps);
    pass.on(|s| {
        paint::card(ctx, s, rect);
        let inner = Rect::new(
            rect.x + pad as i32,
            rect.y + pad as i32,
            rect.w.saturating_sub(pad * 2),
            ctx.px(24),
        );
        let (fill, edge) = if dark {
            (Color::rgb(0x1B, 0x27, 0x40), Color::rgb(0x2E, 0x40, 0x5E))
        } else {
            (Color::rgb(0xFF, 0xFF, 0xFF), Color::rgb(0xC7, 0xD1, 0xE0))
        };
        draw::rounded(s, inner, ctx.px(theme::R_TAB), fill, 255);
        draw::rounded_stroke(s, inner, ctx.px(theme::R_TAB), edge, 255);

        let label = if dark { "ТЁМНАЯ" } else { "СВЕТЛАЯ" };
        let caption = Rect::new(rect.x, inner.bottom() + pad as i32, rect.w, caps_h as u32);
        paint::text_centered(ctx, s, Role::MonoCaps, caption, label, p.ink2);

        if current {
            // Действующий образец обведён дважды: внутренняя обводка отделяет
            // его от карточки, внешнее кольцо — от соседа. Одной обводки на
            // расстоянии в десять точек не хватает, чтобы сказать, какая из
            // двух клеток выбрана.
            draw::rounded_stroke(s, rect, radius, p.accedge, 255);
            let ring = Rect::new(rect.x - 1, rect.y - 1, rect.w + 2, rect.h + 2);
            draw::rounded_stroke(s, ring, radius + 1, p.acctint.color, p.acctint.alpha);
        }
        if focused {
            draw::rounded_stroke(s, rect, radius, p.acc, 255);
        }
    });
}

/// Колонка предпросмотра: как стол выглядит в выбранной теме.
///
/// Картинка, а не список цветов: назвать тему можно и словом, но выбирают её
/// глазами, и единственный честный ответ на вопрос «что получится» — показать,
/// что получится.
fn draw_preview(pass: &mut Pass, area: Rect) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let y = pass.caps_line(area.x, area.y, "ПРЕДПРОСМОТР");
    let picture = Rect::new(area.x, y, area.w, ctx.px(190));
    if !pass.visible(picture) || picture.w < ctx.px(120) {
        return;
    }
    let radius = ctx.px(theme::R_ROW);
    let step = ctx.px(14) as i32;
    let wall = theme::wall_average(p);
    let window = p.win.over(wall);

    pass.on(|s| {
        draw::rounded_gradient(s, picture, radius, p.wall_top, p.wall_bottom, 255);
        // Точки разметки не заходят под скругление: точка, попавшая за угол,
        // торчала бы за краем картинки, потому что фигуры под ней там уже нет.
        let field = picture.shrink(radius);
        let mut dot_y = field.y;
        while dot_y < field.bottom() {
            let mut dot_x = field.x;
            while dot_x < field.right() {
                draw::blend_pixel(s, dot_x, dot_y, p.wall_dot.color, p.wall_dot.alpha);
                dot_x += step;
            }
            dot_y += step;
        }

        // Миниатюра окна: скругление, светлая кромка, полоса заголовка с тремя
        // кнопками и боковая колонка — те же четыре приметы, по которым окно
        // узнаётся в натуральную величину.
        let frame = Rect::new(
            picture.x + ctx.px(26) as i32,
            picture.y + ctx.px(22) as i32,
            picture.w.saturating_sub(ctx.px(52)),
            picture.h.saturating_sub(ctx.px(72)),
        );
        let small = ctx.px(8);
        draw::rounded(s, frame, small, window, 255);
        draw::rounded_stroke(s, frame, small, p.line3.color, p.line3.alpha);
        draw::crown(s, frame, small, p.crown.color, p.crown.alpha);

        let title_h = ctx.px(16) as i32;
        draw::hline(
            s,
            frame.x,
            frame.y + title_h,
            frame.w,
            p.tbline.color,
            p.tbline.alpha,
        );
        let dot_r = ctx.px(2);
        let mut button_x = frame.right() - ctx.px(10) as i32;
        for color in [p.bad, p.ink6, p.ink6] {
            draw::circle(s, button_x, frame.y + title_h / 2, dot_r, color, 255);
            button_x -= ctx.px(9) as i32;
        }

        let side = Rect::new(
            frame.x + ctx.px(5) as i32,
            frame.y + title_h + ctx.px(5) as i32,
            frame.w / 3,
            frame.h.saturating_sub((title_h + ctx.px(10) as i32) as u32),
        );
        draw::rounded(s, side, ctx.px(5), p.panel.over(window), 255);
        let mut line_y = side.y + ctx.px(6) as i32;
        for index in 0..3u32 {
            let bar = Rect::new(
                side.x + ctx.px(5) as i32,
                line_y,
                side.w.saturating_sub(ctx.px(10)),
                ctx.px(4),
            );
            if index == 0 {
                draw::rounded(s, bar, ctx.px(2), p.acc, 255);
            } else {
                draw::rounded(s, bar, ctx.px(2), p.line3.over(window), 255);
            }
            line_y += ctx.px(9) as i32;
        }

        let body_x = side.right() + ctx.px(7) as i32;
        let body_w = (frame.right() - ctx.px(6) as i32 - body_x).max(0) as u32;
        let mut text_y = side.y + ctx.px(4) as i32;
        for share in [10u32, 7, 8, 5] {
            let bar = Rect::new(body_x, text_y, body_w * share / 10, ctx.px(4));
            draw::rounded(s, bar, ctx.px(2), p.line3.over(window), 255);
            text_y += ctx.px(10) as i32;
        }

        // Панель задач плавает над обоями, а не приклеена к низу: отступ от края
        // — единственное, чем она отличается от полосы состояния.
        let panel_h = ctx.px(18);
        let panel_w = picture.w * 7 / 10;
        let panel = Rect::new(
            picture.x + (picture.w - panel_w) as i32 / 2,
            picture.bottom() - (panel_h + ctx.px(12)) as i32,
            panel_w,
            panel_h,
        );
        draw::rounded(s, panel, panel_h / 2, p.glass.over(wall), 255);
        draw::rounded_stroke(s, panel, panel_h / 2, p.line2.color, p.line2.alpha);
        let tile = ctx.px(10);
        let mut tile_x = panel.x + ctx.px(7) as i32;
        for index in 0..4u32 {
            let cell = Rect::new(
                tile_x,
                panel.y + (panel_h - tile) as i32 / 2,
                tile,
                tile,
            );
            let color = if index == 0 { p.acc } else { p.line3.over(p.glass.over(wall)) };
            draw::rounded(s, cell, ctx.px(3), color, 255);
            tile_x += ctx.px(13) as i32;
        }
    });

    pass.note(
        area.x,
        picture.bottom() + ctx.px(10) as i32,
        area.w,
        "Так стол выглядит в выбранной теме.",
    );
}

// ── Что окно рассказывает о машине ───────────────────────────────────────────

/// Что и куда смонтировано — одной строкой.
fn mounted_text() -> String {
    let mounts = fs::mounted();
    if mounts.is_empty() {
        return "ничего".to_string();
    }
    let mut text = String::new();
    for (index, (prefix, kind)) in mounts.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        text.push_str(prefix);
        text.push(' ');
        text.push_str(kind);
    }
    text
}

/// Имена установленных пакетов из реестра `/var/lib/pkg`.
///
/// Реестр — то же место, куда пишет `pkg`: два списка установленного
/// разошлись бы в первый же день.
fn packages() -> Result<Vec<String>, String> {
    const REGISTRY: &str = "/var/lib/pkg";
    let listing = fs::list(REGISTRY).ok_or_else(|| "файловой системы нет".to_string())?;
    let entries = listing.map_err(|err| format!("{err:?}"))?;
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.name.ends_with(".pkg"))
        .map(|entry| entry.name.trim_end_matches(".pkg").to_string())
        .collect();
    names.sort();
    Ok(names)
}

/// Запустить программу и рассказать человеку, чем это кончилось.
///
/// Окно не ждёт её конца и не может ждать: этот код работает внутри разбора
/// события ввода, а `pkg` читает файл, распаковывает его и пишет на диск. Всё,
/// что окно вправе сообщить, — что программа **начала** работу; говорит она
/// сама, в терминале.
fn run(line: &str, what: &str) -> Report {
    match crate::user::spawn(line, crate::user::session::credentials()) {
        Ok(id) => {
            // В журнал — чтобы снаружи было видно, что программу запустило
            // **окно**, а не человек в терминале. Без этой строки проверить
            // кнопку нечем: вывод самой программы одинаков в обоих случаях.
            crate::kprintln!("  settings    : started '{line}' as {id}");
            Report::ok(format!("{what} запущен как {id}; ответ — в терминале"))
        }
        Err(err) => {
            crate::kprintln!("  settings    : cannot start '{line}': {err}");
            Report::bad(format!("{what} не запустился: {err}"))
        }
    }
}

/// Файлы пакетов, которые видно с этой машины.
///
/// Смотрим в носитель, в домашний каталог и на стол — три места, куда пакет
/// попадает: с установочного носителя, из загрузки и рукой человека. Обходить
/// весь корень нельзя: это чтение каждого каталога тома внутри обработчика
/// события ввода.
fn available() -> Vec<String> {
    const MEDIA: &str = "/media";
    let home = super::context::home_dir();
    let desktop = super::context::desktop_dir();
    let mut found = Vec::new();
    for place in [MEDIA, home.as_str(), desktop.as_str()] {
        let Some(Ok(entries)) = fs::list(place) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .into_iter()
            .filter(|entry| entry.name.ends_with(".fpk"))
            .map(|entry| entry.name)
            .collect();
        names.sort();
        for name in names {
            if found.len() == MAX_FILES {
                return found;
            }
            let path = format!("{place}/{name}");
            if is_package(&path) {
                found.push(path);
            }
        }
    }
    found
}

/// Пакет ли это — или образ системы под тем же расширением.
///
/// Различать обязательно: в `/media` рядом с пакетами лежат контейнеры
/// обновления, у них то же расширение и тот же формат, но ставит их не `pkg`, а
/// `sysupdate`. Предложить образ системы в списке «что установить» значило бы
/// предложить действие, которое кончится отказом, — и человек решил бы, что
/// сломан он или файл.
///
/// Читается только заголовок: манифест с именем и версией лежит за ним и стоил
/// бы ещё одного чтения на каждый файл, а этот код работает внутри разбора
/// события ввода.
fn is_package(path: &str) -> bool {
    let Some(Ok((bytes, _))) = fs::read(path, fpk::HEADER_SIZE) else {
        return false;
    };
    matches!(fpk::Header::parse(&bytes), Ok(header) if header.kind == fpk::Kind::Package)
}

/// Сколько файлов показывается в списке установки.
///
/// Предел не косметический: список рисуется в окне, а имена приходят с
/// носителя. Каталог с сотней файлов означал бы сто строк, из которых видно
/// восемь, и прокрутки у этого списка нет.
const MAX_FILES: usize = 8;

/// Имя файла без каталога.
fn short_name(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[index + 1..].to_string(),
        None => path.to_string(),
    }
}

/// Серверы обновлений из `update.cfg` — в том порядке, в каком их пробует
/// `sysupdate`.
fn update_servers() -> Vec<String> {
    let Some((bytes, _)) = config::read("update.cfg", 4096) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes).to_string();
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("server="))
        .map(|value| value.to_string())
        .take(4)
        .collect()
}
