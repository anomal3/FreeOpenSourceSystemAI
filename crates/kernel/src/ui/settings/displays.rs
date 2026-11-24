//! «Параметры → Дисплей» с несколькими мониторами (16.11), как в Windows.
//!
//! # Что на странице
//!
//! Сверху — холст: прямоугольники включённых мониторов в одном масштабе,
//! с номерами (номер — место выхода в реестре, как «1» и «2» у Windows).
//! Щелчок выбирает монитор, перетаскивание двигает его, и он прилипает к
//! краю соседнего: мониторы стоят вплотную, без щелей и без наложения —
//! иначе указатель не перешёл бы с одного на другой. Под холстом —
//! «Применить» и «Отмена», дальше — «Несколько дисплеев» (расширить или
//! показывать только на одном) и параметры выбранного монитора: основной
//! ли он, разрешение (родное помечено «рекомендуется») и частота — обе из
//! паспорта монитора (EDID).
//!
//! # Клавиатура
//!
//! Стрелки вверх-вниз ходят по пунктам, Enter на прямоугольнике выбирает
//! монитор, **Ctrl+стрелка** ставит выбранный монитор к этой стороне
//! остальных (вплотную, по верхнему или левому краю). Так раскладку
//! меняют без мыши — и так её меняет стенд.
//!
//! # Применить — и подтвердить
//!
//! «Применить» отдаёт раскладку столу ([`crate::ui::apply_layout`]) и
//! спрашивает «Сохранить эти параметры дисплея?» с отсчётом 15 секунд: если
//! на новом режиме ничего не видно, ответить человек не сможет, и через 15
//! секунд прежняя раскладка вернётся сама. «Сохранить» записывает раскладку
//! строкой `outputs=` в `desktop.cfg` (её берёт стол при следующем запуске и
//! драйвер NVIDIA), «Отменить изменения» возвращает прежнюю сразу.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use mini_ui::draw;
use mini_ui::iconset::Fi;
use mini_ui::paint::{self, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect};

use crate::display::outputs::{self, Backend};
use crate::input::KeyCode;
use crate::ui::LayoutEntry;

use super::kit::{self, Ctl, Pass, line_h, row};
use super::Deed;

/// Сколько ждать ответа на «Сохранить эти параметры дисплея?».
pub(super) const CONFIRM_MS: u64 = 15_000;

/// Монитор на странице — черновик: что выбрано, пока не применено.
#[derive(Clone)]
pub(super) struct Head {
    pub backend: Backend,
    pub key: String,
    pub name: String,
    pub monitor: Option<edid::Monitor>,
    pub modes: Vec<edid::Mode>,
    pub w: u32,
    pub h: u32,
    pub refresh: u32,
    pub x: i32,
    pub y: i32,
    pub enabled: bool,
    pub primary: bool,
}

/// Прямоугольник на рабочем столе: x, y, ширина, высота.
type Area = (i32, i32, i32, i32);

impl Head {
    fn area(&self) -> Area {
        (self.x, self.y, self.w as i32, self.h as i32)
    }

    /// Разрешения монитора, от большего к меньшему.
    fn resolutions(&self) -> Vec<(u32, u32)> {
        match &self.monitor {
            Some(m) => m.resolutions(),
            None => {
                let mut out: Vec<(u32, u32)> = Vec::new();
                for m in &self.modes {
                    if !out.contains(&(m.width, m.height)) {
                        out.push((m.width, m.height));
                    }
                }
                out
            }
        }
    }

    /// Частоты (мГц) для разрешения, от большей к меньшей.
    fn rates(&self, w: u32, h: u32) -> Vec<u32> {
        let mut out = match &self.monitor {
            Some(m) => m.refresh_rates(w, h),
            None => self.modes.iter().filter(|m| m.width == w && m.height == h).map(edid::Mode::refresh_mhz).collect(),
        };
        if out.is_empty() {
            out.push(self.refresh);
        }
        out
    }

    /// Родной режим монитора.
    fn native(&self) -> Option<(u32, u32)> {
        self.monitor.as_ref().and_then(edid::Monitor::preferred).map(|m| (m.width, m.height))
    }
}

/// Какой выпадающий список открыт под строкой.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum List {
    Resolution,
    Refresh,
}

/// Перевод координат стола в точки холста и обратно. Масштаб — в 1/1024.
#[derive(Clone, Copy)]
struct Map {
    /// Точка холста, в которую ложится левый верхний угол раскладки.
    ox: i32,
    oy: i32,
    /// Левый верхний угол раскладки на столе.
    bx: i32,
    by: i32,
    scale: i64,
}

impl Map {
    fn to_px(self, x: i32, y: i32) -> (i32, i32) {
        (self.ox + (i64::from(x - self.bx) * self.scale / 1024) as i32, self.oy + (i64::from(y - self.by) * self.scale / 1024) as i32)
    }

    fn to_desk(self, px: i32, py: i32) -> (i32, i32) {
        (self.bx + (i64::from(px - self.ox) * 1024 / self.scale) as i32, self.by + (i64::from(py - self.oy) * 1024 / self.scale) as i32)
    }

    fn len(self, v: u32) -> u32 {
        (i64::from(v) * self.scale / 1024) as u32
    }
}

/// Монитор, который тянут мышью. Масштаб холста на время перетаскивания
/// заморожен: иначе холст пересчитывался бы под новую раскладку на каждом
/// шаге и уезжал бы из-под указателя.
struct Drag {
    index: usize,
    /// Где внутри монитора его взяли (координаты стола).
    grab: (i32, i32),
    map: Map,
    moved: bool,
}

/// Раскладка применена и ждёт ответа.
struct Confirm {
    /// Что было до «Применить» — туда возвращаться.
    previous: Vec<Head>,
    deadline: u64,
    /// Сколько секунд показано — перерисовывать раз в секунду, не чаще.
    shown: u64,
}

/// Черновик страницы.
pub(super) struct Displays {
    pub heads: Vec<Head>,
    pub selected: usize,
    pub open: Option<List>,
    drag: Option<Drag>,
    confirm: Option<Confirm>,
    /// Масштаб холста с последнего прохода — по нему щелчок становится
    /// местом на столе.
    map: Cell<Option<Map>>,
}

/// Мониторы из реестра — как они стоят сейчас.
fn from_registry() -> Vec<Head> {
    let list = outputs::list();
    let keys = outputs::keys(&list);
    list.into_iter()
        .zip(keys)
        .map(|(o, key)| {
            let (w, h, refresh) = outputs::default_mode(&o);
            Head {
                backend: o.backend,
                key,
                name: o.name.clone(),
                // Основной выход virtio-gpu заведён до реестра, и картинки у
                // него в реестре нет, — как у команды `outputs`.
                enabled: o.enabled && (o.surface.is_some() || o.primary),
                primary: o.primary,
                monitor: o.monitor,
                modes: o.modes,
                w,
                h,
                refresh,
                x: o.x,
                y: o.y,
            }
        })
        .collect()
}

/// Раскладка для стола: основной — в (0, 0), остальные вокруг.
fn plan(heads: &[Head]) -> Vec<LayoutEntry> {
    let (px, py) = heads.iter().find(|h| h.primary && h.enabled).map_or((0, 0), |h| (h.x, h.y));
    heads
        .iter()
        .map(|h| LayoutEntry {
            backend: h.backend,
            x: h.x - px,
            y: h.y - py,
            w: h.w,
            h: h.h,
            refresh_mhz: h.refresh,
            enabled: h.enabled,
            primary: h.primary && h.enabled,
        })
        .collect()
}

fn overlaps(a: Area, b: Area) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

/// Положение `v` отрезка длины `len` вдоль края соседа `[o, o + olen)`:
/// отрезки обязаны перекрываться хотя бы на четверть меньшего (касание
/// углом — не соседство: указатель через угол не пройдёт), а рядом с ровным
/// краем соседа отрезок к нему прилипает — как в Windows.
fn along(v: i32, len: i32, o: i32, olen: i32) -> i32 {
    let margin = (len.min(olen) / 4).max(1);
    let v = v.clamp(o - len + margin, o + olen - margin);
    let pull = (len.min(olen) / 12).max(16);
    if (v - o).abs() <= pull {
        o
    } else if (v + len - (o + olen)).abs() <= pull {
        o + olen - len
    } else {
        v
    }
}

/// Куда встать монитору `w`×`h`, которого хотят поставить в `want`: ближайшее
/// место вплотную к одному из `others`, не налезающее ни на кого. Соседей нет
/// — куда хотят.
fn snap(others: &[Area], w: i32, h: i32, want: (i32, i32)) -> (i32, i32) {
    let mut best: Option<(i64, (i32, i32))> = None;
    for &(ox, oy, ow, oh) in others {
        let y = along(want.1, h, oy, oh);
        let x = along(want.0, w, ox, ow);
        for spot in [(ox + ow, y), (ox - w, y), (x, oy + oh), (x, oy - h)] {
            if others.iter().any(|&o| overlaps((spot.0, spot.1, w, h), o)) {
                continue;
            }
            let (dx, dy) = (i64::from(spot.0 - want.0), i64::from(spot.1 - want.1));
            let d = dx * dx + dy * dy;
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, spot));
            }
        }
    }
    best.map_or(want, |(_, spot)| spot)
}

/// Частота словами: «60 Гц», «59,95 Гц» — сотые, только если они есть.
pub(super) fn hz_text(mhz: u32) -> String {
    let c = (mhz + 5) / 10;
    if c % 100 == 0 { format!("{} Гц", c / 100) } else { format!("{},{:02} Гц", c / 100, c % 100) }
}

impl Displays {
    pub fn load() -> Self {
        let heads = from_registry();
        let selected = heads.iter().position(|h| h.primary).unwrap_or(0);
        Self { heads, selected, open: None, drag: None, confirm: None, map: Cell::new(None) }
    }

    /// Перечитать реестр — если не тянут монитор и не ждут ответа.
    pub fn reload(&mut self) {
        if self.confirm.is_none() && self.drag.is_none() {
            let selected = self.selected;
            *self = Self::load();
            self.selected = selected.min(self.heads.len().saturating_sub(1));
        }
    }

    /// Страница — для нескольких мониторов. С одним остаётся прежняя: список
    /// разрешений, по которому ходят сценарии `gpu`, `vmsvga`, `display`.
    pub fn multi(&self) -> bool {
        self.heads.len() >= 2
    }

    pub fn confirming(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Остальные включённые мониторы.
    fn others(&self, index: usize) -> Vec<Area> {
        self.heads.iter().enumerate().filter(|(i, h)| *i != index && h.enabled).map(|(_, h)| h.area()).collect()
    }

    /// Поставить монитор `index` в ближайшее место вплотную к остальным.
    fn settle(&mut self, index: usize, want: (i32, i32)) -> bool {
        let others = self.others(index);
        let head = &self.heads[index];
        let spot = snap(&others, head.w as i32, head.h as i32, want);
        let head = &mut self.heads[index];
        let moved = (head.x, head.y) != spot;
        (head.x, head.y) = spot;
        moved
    }

    pub fn select(&mut self, index: usize) {
        if index < self.heads.len() {
            self.selected = index;
            self.open = None;
        }
    }

    /// Ctrl+стрелка: выбранный монитор — к этой стороне остальных.
    pub fn nudge(&mut self, code: KeyCode) -> bool {
        let index = self.selected;
        let Some(head) = self.heads.get(index).filter(|h| h.enabled) else { return false };
        let (w, h) = (head.w as i32, head.h as i32);
        let others = self.others(index);
        let Some(left) = others.iter().map(|o| o.0).min() else { return false };
        let top = others.iter().map(|o| o.1).min().unwrap_or(0);
        let right = others.iter().map(|o| o.0 + o.2).max().unwrap_or(0);
        let bottom = others.iter().map(|o| o.1 + o.3).max().unwrap_or(0);
        let want = match code {
            KeyCode::Right => (right, top),
            KeyCode::Left => (left - w, top),
            KeyCode::Down => (left, bottom),
            KeyCode::Up => (left, top - h),
            _ => return false,
        };
        self.settle(index, want)
    }

    /// Нажали на монитор на холсте: выбрать и взять, чтобы тянуть.
    pub fn grab(&mut self, index: usize, px: i32, py: i32) {
        self.select(index);
        let (Some(map), Some(head)) = (self.map.get(), self.heads.get(index)) else { return };
        let (dx, dy) = map.to_desk(px, py);
        self.drag = Some(Drag { index, grab: (dx - head.x, dy - head.y), map, moved: false });
    }

    /// Указатель с зажатой кнопкой в точке окна: монитор едет за ним.
    pub fn drag_to(&mut self, px: i32, py: i32) -> bool {
        let Some(drag) = &self.drag else { return false };
        let (index, grab, map) = (drag.index, drag.grab, drag.map);
        let (dx, dy) = map.to_desk(px, py);
        let moved = self.settle(index, (dx - grab.0, dy - grab.1));
        if moved {
            if let Some(drag) = &mut self.drag {
                drag.moved = true;
            }
        }
        moved
    }

    /// Кнопку отпустили. Возвращает монитор и место, если он сдвинулся.
    pub fn drop_drag(&mut self) -> Option<(usize, i32, i32)> {
        let drag = self.drag.take()?;
        let head = self.heads.get(drag.index)?;
        drag.moved.then_some((drag.index, head.x, head.y))
    }

    /// Что открытый список предлагает — строкой для журнала (стенд по ней
    /// видит, сколько строк вниз до нужного пункта).
    pub fn offer(&self) -> String {
        let Some(head) = self.heads.get(self.selected) else { return String::new() };
        let items: Vec<String> = match self.open {
            Some(List::Resolution) => head
                .resolutions()
                .into_iter()
                .map(|(w, h)| if head.native() == Some((w, h)) { format!("{w}x{h} (native)") } else { format!("{w}x{h}") })
                .collect(),
            Some(List::Refresh) => head.rates(head.w, head.h).into_iter().map(|r| format!("{}.{:03}", r / 1000, r % 1000)).collect(),
            None => Vec::new(),
        };
        items.join(", ")
    }

    /// Разрешение выбранного монитора; частота — ближайшая к прежней.
    pub fn set_resolution(&mut self, w: u32, h: u32) {
        let index = self.selected;
        let Some(head) = self.heads.get_mut(index) else { return };
        let rates = head.rates(w, h);
        let refresh = rates.iter().copied().min_by_key(|r| r.abs_diff(head.refresh)).unwrap_or(60_000);
        (head.w, head.h, head.refresh) = (w, h, refresh);
        self.open = None;
        // Монитор стал больше или меньше — он снова встаёт вплотную к
        // соседям: больше — не налезает, меньше — не оставляет щели.
        let at = (self.heads[index].x, self.heads[index].y);
        self.settle(index, at);
    }

    pub fn set_refresh(&mut self, mhz: u32) {
        if let Some(head) = self.heads.get_mut(self.selected) {
            head.refresh = mhz;
        }
        self.open = None;
    }

    pub fn make_primary(&mut self) {
        let index = self.selected;
        if !self.heads.get(index).is_some_and(|h| h.enabled) {
            return;
        }
        for (i, head) in self.heads.iter_mut().enumerate() {
            head.primary = i == index;
        }
    }

    /// «Несколько дисплеев»: `None` — расширить на все, `Some(i)` —
    /// показывать только на мониторе `i`.
    pub fn project(&mut self, only: Option<usize>) {
        match only {
            Some(index) if index < self.heads.len() => {
                for (i, head) in self.heads.iter_mut().enumerate() {
                    head.enabled = i == index;
                    head.primary = i == index;
                }
                self.selected = index;
            }
            Some(_) => {}
            None => {
                // Включаемые встают справа от уже включённых.
                for i in 0..self.heads.len() {
                    if self.heads[i].enabled {
                        continue;
                    }
                    let right = self.others(i).iter().map(|o| o.0 + o.2).max().unwrap_or(0);
                    let top = self.others(i).iter().map(|o| o.1).min().unwrap_or(0);
                    self.heads[i].enabled = true;
                    self.settle(i, (right, top));
                }
                if !self.heads.iter().any(|h| h.primary) {
                    self.heads[0].primary = true;
                }
            }
        }
        self.open = None;
    }

    /// Применить: раскладку — столу, прежнюю — запомнить для возврата.
    /// Возвращает раскладку строкой для журнала.
    pub fn apply(&mut self, now: u64) -> Result<String, String> {
        if !self.heads.iter().any(|h| h.enabled) {
            return Err(String::from("нужен хотя бы один включённый дисплей"));
        }
        if !self.heads.iter().any(|h| h.enabled && h.primary) {
            let first = self.heads.iter().position(|h| h.enabled).unwrap_or(0);
            for (i, head) in self.heads.iter_mut().enumerate() {
                head.primary = i == first;
            }
        }
        let plan = plan(&self.heads);
        // Черновик — в координатах стола: основной в (0, 0).
        for (head, entry) in self.heads.iter_mut().zip(&plan) {
            (head.x, head.y) = (entry.x, entry.y);
        }
        let text = describe(&plan);
        crate::ui::apply_layout(plan);
        self.confirm = Some(Confirm { previous: from_registry(), deadline: now + CONFIRM_MS, shown: CONFIRM_MS / 1000 });
        self.open = None;
        Ok(text)
    }

    /// «Сохранить»: раскладку — в `desktop.cfg`. Строка `outputs=` — и в
    /// ответе, и в ошибке: в памяти раскладка запомнена и без файла.
    pub fn keep(&mut self) -> Result<String, (String, String)> {
        self.confirm = None;
        let list: Vec<outputs::Saved> = self
            .heads
            .iter()
            .map(|h| outputs::Saved {
                key: h.key.clone(),
                w: h.w,
                h: h.h,
                refresh_mhz: h.refresh,
                x: h.x,
                y: h.y,
                enabled: h.enabled,
                primary: h.primary && h.enabled,
            })
            .collect();
        let primary = self.heads.iter().find(|h| h.primary && h.enabled).map(|h| (h.w, h.h));
        let text = outputs::format_saved(&list);
        super::super::prefs::store_outputs(list).map_err(|err| (text.clone(), format!("{err}")))?;
        // Режим основного — ещё и в DISPLAY.CFG, как раньше: его читает
        // загрузчик, выбирая режим прошивки (GOP) до ядра.
        if let Some((w, h)) = primary {
            if let Err(err) = crate::slot::request_screen_mode(w, h) {
                crate::kprintln!("  settings    : {w}x{h} not written for the loader: {err}");
            }
        }
        Ok(text)
    }

    /// Вернуть раскладку, что была до «Применить».
    pub fn revert(&mut self) -> bool {
        let Some(confirm) = self.confirm.take() else { return false };
        crate::ui::apply_layout(plan(&confirm.previous));
        self.heads = confirm.previous;
        self.selected = self.selected.min(self.heads.len().saturating_sub(1));
        self.open = None;
        true
    }

    /// Такт: `Some(true)` — отсчёт кончился и раскладка возвращена,
    /// `Some(false)` — сменилась секунда отсчёта, `None` — ничего.
    pub fn tick(&mut self, now: u64) -> Option<bool> {
        let confirm = self.confirm.as_mut()?;
        if now >= confirm.deadline {
            self.revert();
            return Some(true);
        }
        let left = (confirm.deadline - now).div_ceil(1000);
        if left == confirm.shown {
            return None;
        }
        confirm.shown = left;
        Some(false)
    }

    // ── Рисование ───────────────────────────────────────────────────────────

    /// Страница целиком, кроме масштаба. Возвращает `y` следующего.
    pub(super) fn page(&self, pass: &mut Pass, x: i32, y: i32, w: u32) -> i32 {
        if let Some(confirm) = &self.confirm {
            return confirm_card(pass, x, y, w, confirm);
        }
        let ctx = pass.ctx;
        let mut y = self.canvas(pass, x, y + ctx.px(6) as i32, w);
        y = self.actions(pass, x, y, w);

        let off: Vec<String> = self.heads.iter().enumerate().filter(|(_, h)| !h.enabled).map(|(i, h)| format!("{} — {}", i + 1, h.name)).collect();
        if !off.is_empty() {
            let text = format!("Не используются: {}", off.join(", "));
            y = pass.note(x, y, w, &text);
        }

        y = pass.group(x, y, w, "Несколько дисплеев");
        let enabled = self.heads.iter().filter(|h| h.enabled).count();
        y = row(pass, x, y, w, Some(Fi::Layers), "Расширить эти экраны", "Рабочий стол на всех мониторах", Ctl::Radio(enabled > 1), Some(Deed::Project(None)));
        for (i, head) in self.heads.iter().enumerate() {
            let label = format!("Показать только на {}", i + 1);
            y = row(pass, x, y, w, None, &label, &head.name, Ctl::Radio(enabled == 1 && head.enabled), Some(Deed::Project(Some(i))));
        }

        let Some(head) = self.heads.get(self.selected) else { return y };
        let title = format!("Дисплей {} · {}", self.selected + 1, head.name);
        y = pass.group(x, y, w, &title);
        if !head.enabled {
            return pass.note(x, y, w, "Этот дисплей выключен — включите его в «Несколько дисплеев».");
        }
        let desc = if head.primary { "На основном — панель задач и меню" } else { "" };
        y = row(pass, x, y, w, Some(Fi::Monitor), "Сделать основным дисплеем", desc, Ctl::Toggle(head.primary), Some(Deed::MakePrimary));

        let native = head.native();
        let label = |size: (u32, u32)| {
            if native == Some(size) { format!("{} × {} (рекомендуется)", size.0, size.1) } else { format!("{} × {}", size.0, size.1) }
        };
        let current = label((head.w, head.h));
        y = row(pass, x, y, w, Some(Fi::Sliders), "Разрешение дисплея", "", Ctl::NavValue(&current), Some(Deed::OutList(List::Resolution)));
        if self.open == Some(List::Resolution) {
            for size in head.resolutions() {
                let text = label(size);
                y = row(pass, x + ctx.px(24) as i32, y, w.saturating_sub(ctx.px(24)), None, &text, "", Ctl::Radio(size == (head.w, head.h)), Some(Deed::OutMode(size.0, size.1)));
            }
        }
        let rate = hz_text(head.refresh);
        y = row(pass, x, y, w, Some(Fi::Refresh), "Частота обновления", "", Ctl::NavValue(&rate), Some(Deed::OutList(List::Refresh)));
        if self.open == Some(List::Refresh) {
            for mhz in head.rates(head.w, head.h) {
                let text = hz_text(mhz);
                y = row(pass, x + ctx.px(24) as i32, y, w.saturating_sub(ctx.px(24)), None, &text, "", Ctl::Radio(mhz.abs_diff(head.refresh) < 500), Some(Deed::OutRate(mhz)));
            }
        }
        y
    }

    /// Холст с мониторами.
    fn canvas(&self, pass: &mut Pass, x: i32, y: i32, w: u32) -> i32 {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let canvas = Rect::new(x, y, w, ctx.px(230));
        let whole = pass.visible(canvas);
        // Холст, наполовину уехавший под заголовок, рисуется видимой частью
        // (строки страницы в таком случае не рисуются вовсе, но пустое место
        // в 230 точек на месте мониторов выглядит поломкой); нажимается он
        // только целиком — как строки.
        let band = Rect::new(x, pass.top, w, (pass.limit - pass.top).max(0) as u32);
        let seen = canvas.intersect(&band);
        if !seen.is_empty() {
            let fill = ctx.flat(p.sunk);
            let radius = if whole { ctx.px(10) } else { 0 };
            pass.on(|s| {
                draw::rounded(s, seen, radius, fill, 255);
                if whole {
                    draw::rounded_stroke(s, canvas, radius, p.line2.color, p.line2.alpha);
                }
            });
        }
        let inner = canvas.shrink(ctx.px(24));
        let map = match &self.drag {
            Some(drag) => drag.map,
            None => fit(&self.heads, inner, i64::from(ctx.scale.pct()) * 128 / 100),
        };
        self.map.set(Some(map));
        let gap = ctx.px(3);
        for (i, head) in self.heads.iter().enumerate() {
            if !head.enabled {
                continue;
            }
            let (px, py) = map.to_px(head.x, head.y);
            let full = Rect::new(px, py, map.len(head.w).saturating_sub(gap).max(4), map.len(head.h).saturating_sub(gap).max(4));
            // Монитор, утащенный за край холста, не рисуется поверх соседних строк.
            let rect = full.intersect(&canvas.shrink(ctx.px(2)));
            let focused = pass.deed_in(rect, canvas, Deed::PickOutput(i));
            let drawn = rect.intersect(&band);
            if drawn.is_empty() {
                continue;
            }
            let chosen = i == self.selected;
            let (fill, ink) = if chosen { (p.acc, Color::rgb(0x0F, 0x12, 0x18)) } else { (p.ink5, p.ink) };
            let radius = ctx.px(4);
            pass.on(|s| {
                draw::rounded(s, drawn, if drawn == rect { radius } else { 0 }, fill, 255);
                if focused && drawn == rect {
                    draw::rounded_stroke(s, rect, radius, p.ink, 255);
                    draw::rounded_stroke(s, rect.shrink(1), radius, p.ink, 255);
                }
            });
            if drawn != rect {
                continue;
            }
            let number = format!("{}", i + 1);
            let nw = ctx.face(Role::Heading).width(&number);
            let both = line_h(ctx, Role::Heading) + line_h(ctx, Role::Caption);
            let size = format!("{}×{}", head.w, head.h);
            if rect.h as i32 >= both + ctx.px(8) as i32 && ctx.face(Role::Caption).width(&size) + ctx.px(8) <= rect.w {
                let top = rect.y + (rect.h as i32 - both) / 2;
                pass.text(Role::Heading, rect.x + (rect.w as i32 - nw as i32) / 2, top, nw, &number, ink);
                let sw = ctx.face(Role::Caption).width(&size);
                pass.text(Role::Caption, rect.x + (rect.w as i32 - sw as i32) / 2, top + line_h(ctx, Role::Heading), sw, &size, ink);
            } else {
                pass.text(Role::Heading, rect.x + (rect.w as i32 - nw as i32) / 2, paint::baseline(ctx, Role::Heading, rect), nw, &number, ink);
            }
        }
        canvas.bottom() + ctx.px(8) as i32
    }

    /// Под холстом: подсказка слева, «Применить» и «Отмена» справа.
    fn actions(&self, pass: &mut Pass, x: i32, y: i32, w: u32) -> i32 {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let button_h = ctx.px(34);
        let strip = Rect::new(x, y, w, button_h);
        let buttons = [(Deed::DisplaysApply, "Применить", Weight::Primary), (Deed::DisplaysCancel, "Отмена", Weight::Normal)];
        let total: u32 = buttons.iter().map(|(_, label, _)| kit::button_width(ctx, label) + ctx.px(10)).sum();
        let mut bx = strip.right() - total as i32 + ctx.px(10) as i32;
        let shown = pass.visible(strip);
        // «Определить» (24.11, like Windows): the number of every monitor on
        // it for 3 s — the numbers of this canvas.
        let identify = "Определить";
        let iw = kit::button_width(ctx, identify);
        let irect = Rect::new(x, strip.y, iw, button_h);
        // A mouse target only: the keyboard order of the page (monitors,
        // «Применить», «Отмена», …) stays as scenarios and people know it.
        if shown {
            pass.hit(irect, kit::Spot::Deed(Deed::DisplaysIdentify));
            pass.on(|s| paint::button(ctx, s, irect, Weight::Normal, identify, false));
            let hint = "Выберите дисплей; перетащите его или Ctrl+стрелки";
            let hx = irect.right() + ctx.px(12) as i32;
            let room = (bx - hx - ctx.px(16) as i32).max(0) as u32;
            pass.text(Role::Caption, hx, paint::baseline(ctx, Role::Caption, strip), room, hint, p.ink4);
        }
        for (deed, label, weight) in buttons {
            let bw = kit::button_width(ctx, label);
            let rect = Rect::new(bx, strip.y, bw, button_h);
            bx += (bw + ctx.px(10)) as i32;
            let focused = pass.deed_in(rect, strip, deed);
            if shown {
                pass.on(|s| paint::button(ctx, s, rect, weight, label, focused));
            }
        }
        strip.bottom() + ctx.px(6) as i32
    }
}

/// Масштаб холста: раскладка целиком, по центру.
fn fit(heads: &[Head], inner: Rect, cap: i64) -> Map {
    let enabled = heads.iter().filter(|h| h.enabled);
    let left = enabled.clone().map(|h| h.x).min().unwrap_or(0);
    let top = enabled.clone().map(|h| h.y).min().unwrap_or(0);
    let right = enabled.clone().map(|h| h.x + h.w as i32).max().unwrap_or(1);
    let bottom = enabled.map(|h| h.y + h.h as i32).max().unwrap_or(1);
    let (bw, bh) = (i64::from((right - left).max(1)), i64::from((bottom - top).max(1)));
    // Не крупнее 1/8 (на экране с масштабом 200 % — 1/4): один монитор не
    // должен заполнять холст целиком — рядом должно быть видно, куда его
    // можно поставить.
    let scale = (i64::from(inner.w) * 1024 / bw).min(i64::from(inner.h) * 1024 / bh).clamp(1, cap);
    let ox = inner.x + ((i64::from(inner.w) - bw * scale / 1024) / 2) as i32;
    let oy = inner.y + ((i64::from(inner.h) - bh * scale / 1024) / 2) as i32;
    Map { ox, oy, bx: left, by: top, scale }
}

/// Карточка «Сохранить эти параметры дисплея?».
fn confirm_card(pass: &mut Pass, x: i32, y: i32, w: u32, confirm: &Confirm) -> i32 {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let card = Rect::new(x, y + ctx.px(6) as i32, w, ctx.px(150));
    let shown = kit::hero_card(pass, card);
    let pad = ctx.px(20) as i32;
    let room = card.w.saturating_sub(ctx.px(40));
    if shown {
        let mut ty = card.y + pad;
        pass.text(Role::Title, card.x + pad, ty, room, "Сохранить эти параметры дисплея?", p.ink);
        ty += line_h(ctx, Role::Title) + ctx.px(8) as i32;
        let now = crate::time::uptime_ms();
        let left = confirm.deadline.saturating_sub(now).div_ceil(1000);
        let text = format!("Возврат к прежним параметрам дисплея через {left} с.");
        pass.text(Role::Body, card.x + pad, ty, room, &text, p.ink3);
    }
    let button_h = ctx.px(34);
    let by = card.bottom() - pad - button_h as i32;
    let mut bx = card.x + pad;
    for (deed, label, weight) in [(Deed::DisplaysKeep, "Сохранить", Weight::Primary), (Deed::DisplaysRevert, "Отменить изменения", Weight::Normal)] {
        let bw = kit::button_width(ctx, label);
        let rect = Rect::new(bx, by, bw, button_h);
        bx += (bw + ctx.px(10)) as i32;
        let focused = pass.deed_in(rect, card, deed);
        if shown {
            pass.on(|s| paint::button(ctx, s, rect, weight, label, focused));
        }
    }
    card.bottom() + ctx.px(8) as i32
}

/// Раскладка строкой для журнала: `1: 1280x800@60.000 at 0,0 primary; 2: off`.
pub(super) fn describe(plan: &[LayoutEntry]) -> String {
    let mut out = String::new();
    for (i, e) in plan.iter().enumerate() {
        if !out.is_empty() {
            out.push_str("; ");
        }
        if e.enabled {
            out.push_str(&format!(
                "{}: {}x{}@{}.{:03} at {},{}{}",
                i + 1,
                e.w,
                e.h,
                e.refresh_mhz / 1000,
                e.refresh_mhz % 1000,
                e.x,
                e.y,
                if e.primary { " primary" } else { "" }
            ));
        } else {
            out.push_str(&format!("{}: off", i + 1));
        }
    }
    out
}
