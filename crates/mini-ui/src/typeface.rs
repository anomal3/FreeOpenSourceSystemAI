//! Пропорциональный сглаженный текст.
//!
//! # Зачем он вместо шрифта 8×8
//!
//! Растровый шрифт 8×8, растянутый в целое число раз, — это не «ретро-стиль»,
//! это отсутствие выбора: другого способа нарисовать букву у системы не было.
//! Плата видна на любом снимке: буквы одной ширины, ступеньки на каждой
//! наклонной, невозможность отличить заголовок от подписи иначе, чем цветом.
//! Интерфейс, собранный из такого текста, выглядит консолью, чем бы он ни был.
//!
//! Здесь текст рисуется настоящим шрифтом: Inter для интерфейса и JetBrains
//! Mono для всего, что должно выравниваться по столбцам. Оба — SIL OFL, оба
//! лежат в `tools/font/ttf/`.
//!
//! # Почему растеризация — на машине разработчика
//!
//! Разбор TrueType внутри ядра означал бы парсер бинарного формата, растеризатор
//! кривых Безье и кэш глифов — три подсистемы, каждая со своими переполнениями,
//! ради задачи, ответ на которую известен заранее и не меняется. Поэтому глифы
//! растеризованы заранее, `tools/font/genfont.py` через FreeType, и в ядро
//! попадает таблица полутоновых картинок. Хинтинг и наплывы получаются те же,
//! что в любой другой программе на этой машине.
//!
//! # Почему четыре бита на точку
//!
//! Шестнадцать уровней покрытия на штрихе шириной в точку глаз не отличает от
//! двухсот пятидесяти шести, а образ ядра вдвое меньше. Полное покрытие — 15,
//! и умножение на 17 разворачивает его обратно в 0..255 без деления.
//!
//! # Почему два размерных ряда, а не масштабирование
//!
//! Растровый глиф, растянутый вдвое, — это ровно те ступеньки, ради избавления
//! от которых всё и делалось. Поэтому крупные кегли растеризованы отдельно, а
//! ряд выбирается по ширине экрана один раз при запуске.

pub mod data;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::{Color, Rect, Surface};

/// Один глиф: где его картинка лежит в общем массиве и куда её ставить.
///
/// `top` считается **вниз от верхней линии строки**, а не вверх от базовой.
/// Разница практическая: рисующий код складывает координату строки с `top` и
/// получает координату картинки, не храня базовую линию отдельно и не рискуя
/// перепутать знак.
pub struct Glyph {
    pub code: u16,
    pub adv: u8,
    pub left: i8,
    pub top: i8,
    pub w: u8,
    pub h: u8,
    pub off: u32,
}

/// Начертание: кегль, вес и семейство вместе.
pub struct Face {
    /// Высота строки: на столько опускается следующая строка.
    pub line: u8,
    /// Расстояние от верхней линии строки до базовой.
    pub ascent: u8,
    /// Ширина знака, если все они одинаковы, иначе ноль.
    ///
    /// Терминал имеет право считать по ячейкам только тогда, когда ячейка
    /// действительно одна и та же. Признак вычисляется генератором из самих
    /// ширин, а не объявляется руками: объявленное расходится с истиной ровно
    /// в тот день, когда в набор добавят знак пошире.
    pub mono: u8,
    pub glyphs: &'static [Glyph],
    /// Покрытие, на которое указывают `Glyph::off`: у встроенных рядов —
    /// общий [`data::COVERAGE`], у рядов из файла — свой (20.11).
    pub coverage: &'static [u8],
}

/// Назначение текста, а не его размер.
///
/// Роль, а не кегль, потому что кегль зависит от экрана, а назначение — нет.
/// Заголовок окна остаётся заголовком окна и на 1280, и на 3840; меняется
/// только то, сколько это точек.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// Подписи под элементами и мелкий пояснительный текст.
    Caption,
    /// Обычный текст: пункты меню, описания, содержимое списков.
    Body,
    /// Подписи значков, вкладки, кнопки панели задач.
    Label,
    /// Заголовок окна, выделенная строка списка.
    Title,
    /// Заголовок карточки.
    Strong,
    /// Заголовок раздела внутри окна.
    Heading,
    /// Пути, числа, терминал.
    Mono,
    /// Мелкие технические подписи.
    MonoSmall,
    /// ЗАГОЛОВКИ СЕКЦИЙ — всегда с разрядкой, см. [`draw_tracked`].
    MonoCaps,
}

/// Размерный ряд.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// До 2560 точек по ширине.
    Normal,
    /// От 2560 и выше.
    Large,
    /// Дробный масштаб стола (20.11): кегли обычного ряда × 125/150/175/200 %,
    /// из файла `ui-NNN.fnt` ([`install_pack`]); не загружен — ближайший
    /// встроенный ряд ([`Tier::Normal`] для 125 %, [`Tier::Large`] выше).
    Scaled(u16),
}

impl Tier {
    /// Ряд для масштаба стола. На телефоне — прежние два ряда: его 200 % —
    /// плотность экрана, а не выбор человека.
    #[must_use]
    pub fn for_scale(scale: crate::Scale, mobile: bool) -> Self {
        match scale.pct() {
            100 => Self::Normal,
            _ if mobile => Self::Large,
            pct => Self::Scaled(pct as u16),
        }
    }

    /// Ряд для экрана такой ширины.
    ///
    /// Порог один и высокий: на 1920 кегль 13 читается так же, как на 1280, —
    /// плотность точек у настольных экранов до 2560 отличается мало. Ставить
    /// порог ниже значило бы раздувать интерфейс там, где он и так удобен.
    #[must_use]
    pub const fn for_width(width: u32) -> Self {
        if width >= 2560 { Self::Large } else { Self::Normal }
    }
}

/// Начертание для роли в этом ряду.
#[must_use]
pub fn face(role: Role, tier: Tier) -> &'static Face {
    let tier = match tier {
        Tier::Scaled(pct) => {
            if let Some(set) = pack(u32::from(pct)) {
                return &set[role_index(role)];
            }
            if pct < 150 { Tier::Normal } else { Tier::Large }
        }
        other => other,
    };
    match (tier, role) {
        (Tier::Normal, Role::Caption) => &data::CAPTIONNORMAL,
        (Tier::Normal, Role::Body) => &data::BODYNORMAL,
        (Tier::Normal, Role::Label) => &data::LABELNORMAL,
        (Tier::Normal, Role::Title) => &data::TITLENORMAL,
        (Tier::Normal, Role::Strong) => &data::STRONGNORMAL,
        (Tier::Normal, Role::Heading) => &data::HEADINGNORMAL,
        (Tier::Normal, Role::Mono) => &data::MONONORMAL,
        (Tier::Normal, Role::MonoSmall) => &data::MONOSMALLNORMAL,
        (Tier::Normal, Role::MonoCaps) => &data::MONOCAPSNORMAL,
        (Tier::Large, Role::Caption) => &data::CAPTIONLARGE,
        (Tier::Large, Role::Body) => &data::BODYLARGE,
        (Tier::Large, Role::Label) => &data::LABELLARGE,
        (Tier::Large, Role::Title) => &data::TITLELARGE,
        (Tier::Large, Role::Strong) => &data::STRONGLARGE,
        (Tier::Large, Role::Heading) => &data::HEADINGLARGE,
        (Tier::Large, Role::Mono) => &data::MONOLARGE,
        (Tier::Large, Role::MonoSmall) => &data::MONOSMALLLARGE,
        (Tier::Large, Role::MonoCaps) => &data::MONOCAPSLARGE,
        (Tier::Scaled(_), _) => &data::BODYNORMAL,
    }
}

// ── Ряды дробного масштаба из файлов (20.11) ────────────────────────────────

/// Каталог файлов рядов в образе.
pub const PACK_DIR: &str = "/usr/share/fonts/fossai";
/// Масштабы, под которые есть файлы `ui-NNN.fnt`.
pub const PACK_PERCENTS: [u32; 4] = [125, 150, 175, 200];
/// Начертаний в ряду — по одному на [`Role`].
const ROLES: usize = 9;

type FaceSet = [Face; ROLES];

static PACKS: [AtomicPtr<FaceSet>; 4] = [const { AtomicPtr::new(core::ptr::null_mut()) }; 4];

fn pack_slot(pct: u32) -> Option<usize> {
    PACK_PERCENTS.iter().position(|&p| p == pct)
}

fn pack(pct: u32) -> Option<&'static FaceSet> {
    let ptr = PACKS.get(pack_slot(pct)?)?.load(Ordering::Acquire);
    // SAFETY: в ячейку кладётся только `Box::leak` ([`install_pack`]), и
    // освобождать его некому: ряд живёт до конца программы.
    unsafe { ptr.as_ref() }
}

const fn role_index(role: Role) -> usize {
    match role {
        Role::Caption => 0,
        Role::Body => 1,
        Role::Label => 2,
        Role::Title => 3,
        Role::Strong => 4,
        Role::Heading => 5,
        Role::Mono => 6,
        Role::MonoSmall => 7,
        Role::MonoCaps => 8,
    }
}

/// Путь файла ряда для масштаба `pct` (`None` — такого ряда нет).
#[must_use]
pub fn pack_path(pct: u32) -> Option<String> {
    pack_slot(pct).map(|_| format!("{PACK_DIR}/ui-{pct}.fnt"))
}

/// Нужен ли файл ряда для этого масштаба: ряд есть и ещё не загружен.
#[must_use]
pub fn pack_wanted(pct: u32) -> bool {
    pack_slot(pct).is_some() && pack(pct).is_none()
}

/// Загрузить ряд из байт файла `ui-NNN.fnt` (формат — `tools/font/genfont.py
/// --packs`). Байты остаются за рядом навсегда. Возвращает проценты ряда;
/// файл испорчен — `None`, и рисуется встроенный ряд.
///
/// Файл — из образа, но читается как недоверенный: каждое смещение
/// проверено, глиф за краем покрытия отбрасывается целиком.
pub fn install_pack(bytes: &'static [u8]) -> Option<u32> {
    let u16_at = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if bytes.get(..4)? != b"FUI1" {
        return None;
    }
    let pct = u32::from(u16_at(4)?);
    let slot = pack_slot(pct)?;
    if usize::from(u16_at(6)?) != ROLES {
        return None;
    }
    let cov_len = u32_at(8)? as usize;
    let mut at = 12;
    let mut heads = [(0u8, 0u8, 0u8, 0usize); ROLES];
    for head in &mut heads {
        let b = bytes.get(at..at + 8)?;
        *head = (b[0], b[1], b[2], usize::from(u16::from_le_bytes([b[4], b[5]])));
        at += 8;
    }
    let glyph_bytes: usize = heads.iter().map(|h| h.3 * 12).sum();
    let coverage = bytes.get(at + glyph_bytes..at + glyph_bytes + cov_len)?;
    let mut faces: Vec<Face> = Vec::with_capacity(ROLES);
    for &(line, ascent, mono, count) in &heads {
        let mut glyphs = Vec::with_capacity(count);
        for _ in 0..count {
            let b = bytes.get(at..at + 12)?;
            at += 12;
            let glyph = Glyph {
                code: u16::from_le_bytes([b[0], b[1]]),
                adv: b[2],
                left: b[3] as i8,
                top: b[4] as i8,
                w: b[5],
                h: b[6],
                off: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
            };
            let size = usize::from(glyph.w).div_ceil(2) * usize::from(glyph.h);
            if glyph.off as usize + size > coverage.len() {
                return None;
            }
            glyphs.push(glyph);
        }
        faces.push(Face { line, ascent, mono, glyphs: glyphs.leak(), coverage });
    }
    let set: Box<FaceSet> = faces.into_boxed_slice().try_into().ok()?;
    let fresh = Box::into_raw(set);
    // Гонка двух загрузок одного ряда безвредна: проигравший ряд остаётся
    // висеть в памяти, но рисуют все одним.
    if PACKS[slot].compare_exchange(core::ptr::null_mut(), fresh, Ordering::AcqRel, Ordering::Acquire).is_err() {
        // SAFETY: `fresh` только что получен из `Box::into_raw` и никому не отдан.
        drop(unsafe { Box::from_raw(fresh) });
    }
    Some(pct)
}

impl Face {
    /// Найти глиф знака.
    ///
    /// Знак не из набора заменяется на `?`: молча пропустить его значит потерять
    /// текст, а нарисовать пустоту — сделать вид, что там ничего и не было.
    #[must_use]
    pub fn glyph(&self, ch: char) -> Option<&Glyph> {
        let code = u16::try_from(u32::from(ch)).unwrap_or(u16::from(b'?'));
        self.find(code).or_else(|| self.find(u16::from(b'?')))
    }

    /// Место знака в таблице.
    ///
    /// Таблица идёт в том порядке, в каком знаки перечислены в генераторе:
    /// сплошная латиница, Ё, ё, сплошная кириллица, потом горсть отдельных
    /// знаков. Три сплошных куска считаются арифметикой — это весь текст,
    /// который система вообще печатает, — а перебором остаётся полтора десятка
    /// стрелок и псевдографики, встречающихся поштучно.
    ///
    /// Догадка арифметики **проверяется** сравнением кода: набор знаков живёт в
    /// генераторе и однажды изменится, и молча съехавшая таблица нарисовала бы
    /// не тот текст вместо того, чтобы сломаться заметно.
    fn find(&self, code: u16) -> Option<&Glyph> {
        const ASCII_LEN: usize = 0x7F - 0x20;
        let guess = match code {
            0x20..=0x7E => Some((code as usize) - 0x20),
            0x401 => Some(ASCII_LEN),
            0x451 => Some(ASCII_LEN + 1),
            0x410..=0x44F => Some(ASCII_LEN + 2 + (code as usize) - 0x410),
            _ => None,
        };
        if let Some(index) = guess {
            if let Some(glyph) = self.glyphs.get(index) {
                if glyph.code == code {
                    return Some(glyph);
                }
            }
        }
        self.glyphs.iter().find(|g| g.code == code)
    }

    /// Ширина строки в точках.
    #[must_use]
    pub fn width(&self, text: &str) -> u32 {
        text.chars()
            .filter_map(|ch| self.glyph(ch))
            .map(|g| u32::from(g.adv))
            .sum()
    }

    /// Ширина строки, набранной с разрядкой.
    #[must_use]
    pub fn width_tracked(&self, text: &str, tracking: u32) -> u32 {
        let count = text.chars().count() as u32;
        self.width(text) + tracking * count.saturating_sub(1)
    }

    /// Сколько знаков строки помещается в `room` точек.
    ///
    /// Нужно обрезке: подпись значка и путь в заголовке обязаны кончаться там,
    /// где кончается место, а не заезжать на соседа.
    #[must_use]
    pub fn fits(&self, text: &str, room: u32) -> usize {
        let mut used = 0;
        let mut taken = 0;
        for ch in text.chars() {
            let Some(glyph) = self.glyph(ch) else { continue };
            let next = used + u32::from(glyph.adv);
            if next > room {
                break;
            }
            used = next;
            taken += 1;
        }
        taken
    }
}

/// Нарисовать строку. Возвращает, на сколько сдвинулось перо.
///
/// `y` — **верхняя линия строки**, не базовая: так вызывающий выравнивает текст
/// по прямоугольнику, а не по невидимой линии внутри него.
///
/// Цвет смешивается с тем, что уже лежит в поверхности, — иначе сглаживание
/// нарисует серую кайму вокруг каждой буквы вместо мягкого края.
pub fn draw(
    surface: &mut Surface,
    face: &Face,
    x: i32,
    y: i32,
    text: &str,
    color: Color,
    alpha: u8,
) -> u32 {
    draw_tracked(surface, face, x, y, text, color, alpha, 0)
}

/// То же с разрядкой между знаками.
///
/// Разрядка — не украшение: заголовки секций в макете набраны прописными с
/// межбуквенным просветом, и без него прописные слипаются в сплошную полосу.
#[allow(clippy::too_many_arguments)]
pub fn draw_tracked(
    surface: &mut Surface,
    face: &Face,
    x: i32,
    y: i32,
    text: &str,
    color: Color,
    alpha: u8,
    tracking: u32,
) -> u32 {
    if alpha == 0 {
        return face.width_tracked(text, tracking);
    }
    let mut pen = 0;
    for ch in text.chars() {
        let Some(glyph) = face.glyph(ch) else { continue };
        blit(surface, face.coverage, glyph, x + pen, y, color, alpha);
        pen += i32::from(glyph.adv) + tracking as i32;
    }
    // Последний знак разрядку за собой не тянет: иначе строка, выровненная по
    // правому краю, встанет на просвет левее, чем нужно.
    (pen - tracking as i32).max(0) as u32
}

/// Нарисовать строку, обрезав её по ширине.
///
/// Обрезанное кончается многоточием, а не обрывается на середине буквы: обрыв
/// читается как ошибка отрисовки, многоточие — как «здесь есть ещё».
#[allow(clippy::too_many_arguments)]
pub fn draw_clipped(
    surface: &mut Surface,
    face: &Face,
    x: i32,
    y: i32,
    text: &str,
    room: u32,
    color: Color,
    alpha: u8,
) -> u32 {
    if face.width(text) <= room {
        return draw(surface, face, x, y, text, color, alpha);
    }
    let tail = face.width("…");
    let room = room.saturating_sub(tail);
    let taken = face.fits(text, room);
    let mut pen = 0;
    for ch in text.chars().take(taken) {
        let Some(glyph) = face.glyph(ch) else { continue };
        blit(surface, face.coverage, glyph, x + pen, y, color, alpha);
        pen += i32::from(glyph.adv);
    }
    if let Some(glyph) = face.glyph('…') {
        blit(surface, face.coverage, glyph, x + pen, y, color, alpha);
        pen += i32::from(glyph.adv);
    }
    pen.max(0) as u32
}

/// Нарисовать строку по центру прямоугольника.
pub fn draw_centered(
    surface: &mut Surface,
    face: &Face,
    area: Rect,
    text: &str,
    color: Color,
    alpha: u8,
) {
    let width = face.width(text);
    let x = area.x + (area.w as i32 - width as i32) / 2;
    let y = area.y + (area.h as i32 - i32::from(face.line)) / 2;
    draw(surface, face, x, y, text, color, alpha);
}

/// Нарисовать строку, прижав её к правому краю.
pub fn draw_right(
    surface: &mut Surface,
    face: &Face,
    right: i32,
    y: i32,
    text: &str,
    color: Color,
    alpha: u8,
) {
    let width = face.width(text) as i32;
    draw(surface, face, right - width, y, text, color, alpha);
}

/// Положить картинку одного глифа.
///
/// Отдельная функция, а не тело цикла: она же нужна и обрезке, и разрядке, и
/// терминалу, который ставит знаки по одному в свои ячейки.
fn blit(surface: &mut Surface, coverage: &[u8], glyph: &Glyph, x: i32, y: i32, color: Color, alpha: u8) {
    if glyph.w == 0 || glyph.h == 0 {
        return;
    }
    let stride = usize::from(glyph.w).div_ceil(2);
    let base = glyph.off as usize;
    let left = x + i32::from(glyph.left);
    let top = y + i32::from(glyph.top);
    for row in 0..u32::from(glyph.h) {
        let py = top + row as i32;
        if py < 0 || py >= surface.height() as i32 {
            continue;
        }
        let line = base + (row as usize) * stride;
        for column in 0..u32::from(glyph.w) {
            let px = left + column as i32;
            if px < 0 || px >= surface.width() as i32 {
                continue;
            }
            let Some(byte) = coverage.get(line + (column as usize) / 2) else {
                return;
            };
            // Младшая тетрада — левая точка: так упаковано генератором, и
            // порядок должен совпадать с ним, а не с интуицией.
            let level = if column % 2 == 0 { byte & 0x0F } else { byte >> 4 };
            if level == 0 {
                continue;
            }
            // Пятнадцать уровней разворачиваются в 0..255 умножением на 17 —
            // без деления и без потери верхнего уровня.
            let coverage = u32::from(level) * 17 * u32::from(alpha) / 255;
            let px = px as u32;
            let py = py as u32;
            let under = Color::from_pixel(surface.get(px, py));
            surface.put(px, py, under.mix(color, coverage as u8).pixel());
        }
    }
}

/// Одна ячейка моноширинной сетки: ширина и высота.
///
/// Терминалу нужны обе, и обе обязаны прийти из одного места, иначе курсор
/// встаёт не туда, где буква.
#[must_use]
pub fn cell(face: &Face) -> (u32, u32) {
    let width = if face.mono == 0 {
        // Начертание не моноширинное, а сетка всё равно нужна: берём ширину
        // цифры. Она у пропорциональных шрифтов табличная — колонки чисел
        // обязаны сходиться, — и потому подходит лучше любой буквы.
        face.glyph('0').map_or(8, |g| u32::from(g.adv))
    } else {
        u32::from(face.mono)
    };
    (width.max(1), u32::from(face.line).max(1))
}
