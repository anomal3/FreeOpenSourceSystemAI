//! Оболочка OBSOS на egui: вкладки (сбоку или сверху), панель адреса,
//! полосы уведомлений над страницей, выдвижная панель справа (закладки,
//! история, пароли, синхронизация, оформление, настройки), диалоги страницы
//! (`alert`/`confirm`/`prompt`, `<select>`, контекстное меню, цвет, файл,
//! HTTP-вход) и всплывающие сообщения.
//!
//! Раскладка по мотивам Zervo (© Goddv, MPL-2.0); код свой. Значки рисуются
//! линиями — в шрифтах egui нет стрелок.

use egui::{Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use servo::{ContextMenuItem, RgbColor, SelectElementOptionOrOptgroup, SimpleDialog};

use crate::profile::{Look, Rgb, Search, now_ms};
use crate::shell::{Action, Infobar, Modal, Panel, Shell, TABSTRIP_H, TOOLBAR_H, TOOLBAR_H_COMPACT, page_origin};
use crate::vault::Login;
use browser_sync::remote::{Provider, oauth};

// Геометрия боковой панели — на неё целится сценарий `servo-window`.
pub const BRAND_H: f32 = 34.0;
pub const NEW_TAB_H: f32 = 30.0;
pub const TAB_ROW_H: f32 = 30.0;
const DRAWER_W: f32 = 390.0;

pub fn c(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

struct Palette {
    accent: Color32,
    accent2: Color32,
    sidebar: Color32,
    toolbar: Color32,
    text: Color32,
    dim: Color32,
    hover: Color32,
    active: Color32,
    field: Color32,
}

fn palette(look: &Look) -> Palette {
    let text = c(look.text);
    let sidebar = c(look.sidebar);
    let accent = c(look.accent);
    Palette {
        accent,
        accent2: c(look.accent2),
        sidebar,
        toolbar: c(look.toolbar),
        text,
        dim: mix(text, sidebar, 0.4),
        hover: mix(sidebar, text, 0.08),
        active: if look.accent_reach >= 1 { mix(sidebar, accent, 0.35) } else { mix(sidebar, text, 0.14) },
        field: mix(c(look.toolbar), if look.dark { Color32::BLACK } else { Color32::WHITE }, 0.35),
    }
}

pub fn style(ctx: &egui::Context, look: &Look) {
    let p = palette(look);
    let mut v = if look.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.toolbar;
    v.window_fill = p.sidebar;
    v.extreme_bg_color = p.field;
    v.override_text_color = Some(p.text);
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    v.hyperlink_color = p.accent2;
    let button = if look.accent_reach >= 2 { mix(p.toolbar, p.accent, 0.45) } else { mix(p.toolbar, p.text, 0.1) };
    v.widgets.inactive.weak_bg_fill = button;
    v.widgets.inactive.bg_fill = button;
    v.widgets.hovered.weak_bg_fill = mix(button, p.text, 0.12);
    v.widgets.active.weak_bg_fill = p.accent;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.window_corner_radius = CornerRadius::same(10);
    ctx.set_visuals(v);
}

// ── Значки ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Icon {
    Back,
    Forward,
    Reload,
    Star(bool),
    Menu,
    Key,
    Plus,
    Close,
}

fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = Stroke::new(1.8_f32, color);
    let ctr = rect.center();
    let r = 7.0;
    match icon {
        Icon::Back | Icon::Forward => {
            let d = if matches!(icon, Icon::Back) { -1.0 } else { 1.0 };
            painter.line_segment([ctr + vec2(-r * d, 0.0), ctr + vec2(r * d, 0.0)], s);
            painter.line_segment([ctr + vec2(r * d, 0.0), ctr + vec2(r * d * 0.2, -r * 0.8)], s);
            painter.line_segment([ctr + vec2(r * d, 0.0), ctr + vec2(r * d * 0.2, r * 0.8)], s);
        }
        Icon::Reload => {
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = -0.6 + i as f32 / 20.0 * 5.0;
                    ctr + vec2(a.cos() * r, a.sin() * r)
                })
                .collect();
            painter.add(egui::Shape::line(pts.clone(), s));
            let end = *pts.last().unwrap_or(&ctr);
            painter.line_segment([end, end + vec2(4.5, -1.0)], s);
            painter.line_segment([end, end + vec2(0.5, -4.5)], s);
        }
        Icon::Star(filled) => {
            let pts: Vec<Pos2> = (0..10)
                .map(|i| {
                    let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                    let rr = if i % 2 == 0 { r + 1.0 } else { r * 0.45 };
                    ctr + vec2(a.cos() * rr, a.sin() * rr)
                })
                .collect();
            if filled {
                painter.add(egui::Shape::convex_polygon(pts.clone(), color, Stroke::NONE));
                // Невыпуклую звезду закрашиваем лучами от центра.
                for i in 0..10 {
                    painter.add(egui::Shape::convex_polygon(vec![ctr, pts[i], pts[(i + 1) % 10]], color, Stroke::NONE));
                }
            } else {
                painter.add(egui::Shape::closed_line(pts, s));
            }
        }
        Icon::Menu => {
            for dy in [-5.0, 0.0, 5.0] {
                painter.line_segment([ctr + vec2(-r, dy), ctr + vec2(r, dy)], s);
            }
        }
        Icon::Key => {
            painter.circle_stroke(ctr + vec2(-3.5, 0.0), 3.5, s);
            painter.line_segment([ctr + vec2(0.0, 0.0), ctr + vec2(r, 0.0)], s);
            painter.line_segment([ctr + vec2(r - 1.0, 0.0), ctr + vec2(r - 1.0, 3.5)], s);
            painter.line_segment([ctr + vec2(r - 4.0, 0.0), ctr + vec2(r - 4.0, 3.0)], s);
        }
        Icon::Plus => {
            painter.line_segment([ctr + vec2(-r, 0.0), ctr + vec2(r, 0.0)], s);
            painter.line_segment([ctr + vec2(0.0, -r), ctr + vec2(0.0, r)], s);
        }
        Icon::Close => {
            let k = r * 0.7;
            painter.line_segment([ctr + vec2(-k, -k), ctr + vec2(k, k)], s);
            painter.line_segment([ctr + vec2(-k, k), ctr + vec2(k, -k)], s);
        }
    }
}

fn icon_button(ui: &mut Ui, icon: Icon, enabled: bool, tip: &str, p: &Palette) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(30.0, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    if response.hovered() && enabled {
        ui.painter().rect_filled(rect, 6.0, p.hover);
    }
    let color = if enabled { p.text } else { mix(p.text, p.toolbar, 0.6) };
    let color = if matches!(icon, Icon::Star(true)) { p.accent2 } else { color };
    paint_icon(ui.painter(), rect, icon, color);
    response.on_hover_text(tip).clicked() && enabled
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn when(ms: u64) -> String {
    let (y, mo, d, h, mi, _) = browser_sync::remote::civil(ms / 1000);
    let age = now_ms().saturating_sub(ms) / 1000;
    if age < 60 {
        String::from("только что")
    } else if age < 3600 {
        format!("{} мин назад", age / 60)
    } else {
        format!("{d:02}.{mo:02}.{y} {h:02}:{mi:02}")
    }
}

// ── Проход интерфейса ───────────────────────────────────────────────────────

pub fn shell_ui(ui: &mut Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    actions.clear();
    let look = shell.look().clone();
    let p = palette(&look);
    if look.side_tabs {
        sidebar(ui, shell, &look, &p, actions);
    } else {
        tabstrip(ui, shell, &p, actions);
    }
    toolbar(ui, shell, &look, &p, actions);
    let ctx = ui.ctx().clone();
    let (ox, oy) = page_origin(&look);
    let screen = ctx.content_rect();
    let page = Rect::from_min_max(pos2(ox as f32, oy as f32), screen.max);
    overlays(&ctx, shell, page, &p, actions);
    if let Some(panel) = shell.panel {
        drawer(&ctx, shell, panel, page, &p, actions);
    }
    modal(&ctx, shell, page, &p, actions);
    toasts(&ctx, shell, page, &p);
}

fn sidebar(ui: &mut Ui, shell: &mut Shell, look: &Look, p: &Palette, actions: &mut Vec<Action>) {
    egui::Panel::left("obsos-tabs")
        .exact_size(look.sidebar_width as f32)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(8, 0)))
        .show_inside(ui, |ui| {
            let width = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(vec2(width, BRAND_H), Sense::hover());
            let painter = ui.painter();
            painter.circle_filled(pos2(rect.left() + 10.0, rect.center().y), 7.0, p.accent);
            painter.circle_filled(pos2(rect.left() + 20.0, rect.center().y - 5.0), 3.0, p.accent2);
            painter.text(pos2(rect.left() + 30.0, rect.center().y), Align2::LEFT_CENTER, "OBSOS", FontId::proportional(18.0), p.text);
            let new_tab = egui::Button::new(RichText::new("+  Новая вкладка").color(p.text)).fill(mix(p.sidebar, p.text, 0.1)).corner_radius(8);
            if ui.add_sized([width, NEW_TAB_H], new_tab).on_hover_text("Ctrl+T").clicked() {
                actions.push(Action::NewTab);
            }
            ui.add_space(6.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            let rows_h = ui.available_height() - 40.0;
            egui::ScrollArea::vertical().max_height(rows_h.max(60.0)).show(ui, |ui| {
                for i in 0..shell.tabs.len() {
                    tab_row(ui, shell, i, width, p, actions);
                }
            });
            // Внизу: вкладки других устройств и синхронизация.
            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let synced = shell.account().is_some();
                    let label = if shell.syncer.busy > 0 { "⟳ синхронизация…" } else if synced { "☁ синхронизация" } else { "☁ без синхронизации" };
                    if ui.add(egui::Button::new(RichText::new(label).size(12.0).color(p.dim)).frame(false)).clicked() {
                        actions.push(Action::Panel(Panel::Sync));
                    }
                });
            });
        });
}

fn tab_row(ui: &mut Ui, shell: &Shell, i: usize, width: f32, p: &Palette, actions: &mut Vec<Action>) {
    let tab = &shell.tabs[i];
    let (rect, response) = ui.allocate_exact_size(vec2(width, TAB_ROW_H), Sense::click());
    let close = Rect::from_center_size(pos2(rect.right() - 14.0, rect.center().y), vec2(20.0, 20.0));
    let over_close = response.hover_pos().is_some_and(|q| close.contains(q));
    let fill = if i == shell.active {
        p.active
    } else if response.hovered() {
        p.hover
    } else {
        p.sidebar
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 7.0, fill);
    if i == shell.active {
        painter.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 6.0), vec2(3.0, TAB_ROW_H - 12.0)), 2.0, p.accent2);
    }
    let mark = if tab.loading { "◌ " } else { "" };
    painter.text(
        pos2(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        format!("{mark}{}", clip(&tab.title, ((width - 50.0) / 8.0) as usize)),
        FontId::proportional(14.0),
        if i == shell.active { p.text } else { p.dim },
    );
    if response.hovered() || i == shell.active {
        if over_close {
            painter.rect_filled(close, 4.0, p.hover);
        }
        paint_icon(painter, close.shrink(3.0), Icon::Close, p.dim);
    }
    if response.clicked() {
        actions.push(if over_close { Action::Close(i) } else { Action::Activate(i) });
    }
    if response.middle_clicked() {
        actions.push(Action::Close(i));
    }
}

fn tabstrip(ui: &mut Ui, shell: &mut Shell, p: &Palette, actions: &mut Vec<Action>) {
    egui::Panel::top("obsos-strip")
        .exact_size(TABSTRIP_H as f32)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(p.sidebar).inner_margin(Margin::symmetric(6, 4)))
        .show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                let n = shell.tabs.len().max(1) as f32;
                let width = ((ui.available_width() - 40.0) / n).clamp(80.0, 220.0);
                ui.spacing_mut().item_spacing.x = 3.0;
                for i in 0..shell.tabs.len() {
                    tab_row(ui, shell, i, width, p, actions);
                }
                if icon_button(ui, Icon::Plus, true, "Новая вкладка (Ctrl+T)", p) {
                    actions.push(Action::NewTab);
                }
            });
        });
}

fn toolbar(ui: &mut Ui, shell: &mut Shell, look: &Look, p: &Palette, actions: &mut Vec<Action>) {
    let height = if look.compact { TOOLBAR_H_COMPACT } else { TOOLBAR_H } as f32;
    let address_id = egui::Id::new("obsos-address");
    egui::Panel::top("obsos-bar")
        .exact_size(height)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(p.toolbar).inner_margin(Margin::symmetric(8, if look.compact { 4 } else { 7 })))
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let (back, forward) = shell.active_view().map(|w| (w.can_go_back(), w.can_go_forward())).unwrap_or((false, false));
                if icon_button(ui, Icon::Back, back, "Назад (Alt+Влево)", p) {
                    actions.push(Action::Back);
                }
                if icon_button(ui, Icon::Forward, forward, "Вперёд (Alt+Вправо)", p) {
                    actions.push(Action::Forward);
                }
                if icon_button(ui, Icon::Reload, true, "Обновить (F5)", p) {
                    actions.push(Action::Reload);
                }
                let url = shell.tabs.get(shell.active).and_then(|t| t.real_url.clone());
                let origin = url.as_ref().and_then(crate::vault::origin_of);
                let has_logins = origin.as_ref().is_some_and(|o| !shell.profile.logins_for(o).is_empty());
                let marked = url.as_ref().is_some_and(|u| shell.profile.is_bookmarked(u.as_str()));
                let right = 30.0 * (2.0 + has_logins as u8 as f32) + 16.0;
                if shell.focus_address {
                    ui.memory_mut(|m| m.request_focus(address_id));
                    shell.focus_address = false;
                }
                let secure = url.as_ref().is_some_and(|u| u.scheme() == "https" || u.scheme() == "obsos");
                let plain = secure || shell.address.is_empty();
                let hint = format!("Адрес или поиск в {}", shell.profile.settings.search.title());
                let edit = egui::TextEdit::singleline(&mut shell.address)
                    .id(address_id)
                    .hint_text(hint)
                    .font(FontId::proportional(15.0))
                    .margin(Margin::symmetric(10, 5))
                    .text_color(if plain { p.text } else { mix(p.text, Color32::from_rgb(0xe0, 0x90, 0x40), 0.5) })
                    .desired_width(ui.available_width() - right);
                let response = ui.add(edit);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(Action::Go(shell.address.clone()));
                }
                if has_logins && icon_button(ui, Icon::Key, true, "Подставить сохранённый пароль", p) {
                    if let Some(o) = &origin {
                        shell.infobars.retain(|b| !matches!(b, Infobar::ChooseLogin(_)));
                        shell.infobars.push(Infobar::ChooseLogin(o.clone()));
                    }
                }
                if icon_button(ui, Icon::Star(marked), url.is_some(), "Закладка (Ctrl+D)", p) {
                    actions.push(Action::ToggleBookmark);
                }
                if icon_button(ui, Icon::Menu, true, "Меню", p) {
                    actions.push(Action::Panel(Panel::Menu));
                }
            });
        });
}

/// Полоса загрузки, подсказка ссылки, полосы уведомлений.
fn overlays(ctx: &egui::Context, shell: &mut Shell, page: Rect, p: &Palette, actions: &mut Vec<Action>) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("obsos-overlay")));
    if let Some(tab) = shell.tabs.get(shell.active) {
        if tab.loading {
            let t = (ctx.input(|i| i.time) * 0.8).fract() as f32;
            let w = page.width() * 0.3;
            let x = page.left() + (page.width() + w) * t - w;
            painter.rect_filled(Rect::from_min_size(pos2(x.max(page.left()), page.top()), vec2(w, 3.0)), 0.0, p.accent2);
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        if let Some(status) = &tab.status {
            let text = clip(status, 90);
            let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), p.text);
            let r = Rect::from_min_size(pos2(page.left() + 4.0, page.bottom() - galley.size().y - 10.0), galley.size() + vec2(12.0, 6.0));
            painter.rect_filled(r, 5.0, p.toolbar);
            painter.galley(r.min + vec2(6.0, 3.0), galley, p.text);
        }
    }
    if shell.infobars.is_empty() {
        return;
    }
    let bars = shell.infobars.clone();
    egui::Area::new(egui::Id::new("obsos-infobars")).order(egui::Order::Foreground).fixed_pos(page.min + vec2(8.0, 6.0)).show(ctx, |ui| {
        ui.set_width(page.width() - 16.0);
        for (index, bar) in bars.iter().enumerate() {
            egui::Frame::new().fill(p.toolbar).corner_radius(8).inner_margin(Margin::symmetric(12, 8)).stroke(Stroke::new(1.0_f32, p.accent)).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| match bar {
                    Infobar::SaveLogin(login) => {
                        ui.label(format!("Сохранить пароль для {} ({})?", login.origin, if login.username.is_empty() { "без имени" } else { &login.username }));
                        if ui.button("Сохранить").clicked() {
                            shell.save_login(login.clone());
                            shell.infobars.remove(index);
                        } else if ui.button("Никогда для этого сайта").clicked() {
                            shell.profile.put_shared(&format!("never/{}", login.origin), "1");
                            shell.infobars.remove(index);
                        } else if ui.button("Не сейчас").clicked() {
                            shell.infobars.remove(index);
                        }
                    }
                    Infobar::ChooseLogin(origin) => {
                        ui.label(format!("Войти на {origin} как:"));
                        let id = shell.active_view().map(|w| w.id());
                        for login in shell.profile.logins_for(origin) {
                            let name = if login.username.is_empty() { String::from("(без имени)") } else { login.username.clone() };
                            if ui.button(name).clicked() {
                                if let Some(id) = id {
                                    shell.fill(id, &login);
                                }
                                shell.infobars.retain(|b| !matches!(b, Infobar::ChooseLogin(_)));
                            }
                        }
                        if ui.button("Закрыть").clicked() {
                            shell.infobars.retain(|b| !matches!(b, Infobar::ChooseLogin(_)));
                        }
                    }
                    Infobar::Unsupported { url, what } => {
                        let explain = if what == "MediaSource" {
                            String::from("Сайт хочет потоковое видео (Media Source Extensions) — в движке Servo его нет.")
                        } else {
                            format!("Servo не умеет: {what}.")
                        };
                        ui.label(explain);
                        if ui.button("Открыть в Chromium").clicked() {
                            actions.push(Action::Chromium(url.clone()));
                        }
                        if ui.button("Подробнее").clicked() {
                            actions.push(Action::OpenUrl(crate::pages::unsupported_url(url, what).to_string()));
                        }
                        if ui.button("Скрыть").clicked() {
                            shell.infobars.retain(|b| !matches!(b, Infobar::Unsupported { .. }));
                        }
                    }
                });
            });
            if index >= shell.infobars.len() {
                break;
            }
        }
    });
}

fn toasts(ctx: &egui::Context, shell: &mut Shell, page: Rect, p: &Palette) {
    let now = std::time::Instant::now();
    shell.toasts.retain(|(_, until)| *until > now);
    if shell.toasts.is_empty() {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(500));
    egui::Area::new(egui::Id::new("obsos-toasts")).order(egui::Order::Tooltip).anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0)).show(ctx, |ui| {
        for (text, _) in &shell.toasts {
            egui::Frame::new().fill(p.toolbar).corner_radius(8).inner_margin(Margin::symmetric(14, 9)).stroke(Stroke::new(1.0_f32, p.accent2)).show(ui, |ui| {
                ui.set_max_width(page.width().min(420.0));
                ui.label(text);
            });
        }
    });
}

// ── Выдвижная панель ────────────────────────────────────────────────────────

fn drawer(ctx: &egui::Context, shell: &mut Shell, panel: Panel, page: Rect, p: &Palette, actions: &mut Vec<Action>) {
    let width = DRAWER_W.min(page.width());
    egui::Area::new(egui::Id::new("obsos-drawer"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos2(page.right() - width, page.top()))
        .show(ctx, |ui| {
            egui::Frame::new().fill(p.sidebar).inner_margin(Margin::same(14)).stroke(Stroke::new(1.0_f32, mix(p.sidebar, p.text, 0.15))).show(ui, |ui| {
                ui.set_width(width - 28.0);
                ui.set_height(page.height() - 28.0);
                ui.horizontal(|ui| {
                    if panel != Panel::Menu && icon_button(ui, Icon::Back, true, "Меню", p) {
                        shell.panel = Some(Panel::Menu);
                    }
                    ui.heading(panel_title(panel));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, true, "Закрыть", p) {
                            shell.panel = None;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match panel {
                    Panel::Menu => menu(ui, shell, actions),
                    Panel::Bookmarks => bookmarks(ui, shell, actions),
                    Panel::History => history(ui, shell, actions),
                    Panel::Passwords => passwords(ui, shell),
                    Panel::RemoteTabs => remote_tabs(ui, shell, actions),
                    Panel::Sync => sync_panel(ui, shell, actions),
                    Panel::Looks => looks(ui, shell),
                    Panel::Settings => settings(ui, shell),
                    Panel::Scripts => scripts(ui, shell),
                    Panel::Profiles => profiles(ui, shell),
                });
            });
        });
}

fn panel_title(panel: Panel) -> &'static str {
    match panel {
        Panel::Menu => "Меню",
        Panel::Bookmarks => "Закладки",
        Panel::History => "История",
        Panel::Passwords => "Пароли",
        Panel::RemoteTabs => "Вкладки других устройств",
        Panel::Sync => "Синхронизация",
        Panel::Looks => "Оформление",
        Panel::Settings => "Настройки",
        Panel::Scripts => "Скрипты и стили",
        Panel::Profiles => "Профили",
    }
}

fn menu(ui: &mut Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    let item = |ui: &mut Ui, text: &str| ui.add_sized([ui.available_width(), 30.0], egui::Button::new(text)).clicked();
    if item(ui, "Новая вкладка    Ctrl+T") {
        actions.push(Action::NewTab);
        shell.panel = None;
    }
    for (panel, text) in [
        (Panel::Bookmarks, "Закладки"),
        (Panel::History, "История"),
        (Panel::Passwords, "Пароли"),
        (Panel::RemoteTabs, "Вкладки других устройств"),
        (Panel::Sync, "Синхронизация"),
        (Panel::Looks, "Оформление"),
        (Panel::Settings, "Настройки"),
        (Panel::Scripts, "Скрипты и стили (вместо расширений)"),
        (Panel::Profiles, "Профили"),
    ] {
        if item(ui, text) {
            actions.push(Action::Panel(panel));
        }
    }
    ui.separator();
    ui.horizontal(|ui| {
        ui.label("Масштаб");
        let zoom = shell.tabs.get(shell.active).map(|t| t.zoom).unwrap_or(1.0);
        if ui.button(" − ").clicked() {
            actions.push(Action::Zoom(1.0 / 1.1));
        }
        if ui.button(format!("{:.0}%", zoom * 100.0)).clicked() {
            actions.push(Action::Zoom(0.0));
        }
        if ui.button(" + ").clicked() {
            actions.push(Action::Zoom(1.1));
        }
    });
    if item(ui, "Открыть страницу в Chromium") {
        actions.push(Action::Chromium(String::new()));
    }
    if item(ui, "О программе") {
        actions.push(Action::NewTabUrl(String::from("obsos://about")));
        shell.panel = None;
    }
    if item(ui, "Выход    Ctrl+Q") {
        actions.push(Action::Quit);
    }
    ui.separator();
    ui.label(RichText::new(format!("Профиль: {} · устройство {}", shell.profile.name, shell.profile.settings.device)).small());
}

fn filter_box(ui: &mut Ui, shell: &mut Shell, hint: &str) -> String {
    ui.add(egui::TextEdit::singleline(&mut shell.filter).hint_text(hint).desired_width(f32::INFINITY));
    shell.filter.to_lowercase()
}

fn bookmarks(ui: &mut Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    if ui.button("☆ Добавить текущую страницу").clicked() {
        actions.push(Action::ToggleBookmark);
    }
    let f = filter_box(ui, shell, "Найти в закладках");
    let mut remove = None;
    for b in shell.profile.bookmarks.iter().filter(|b| f.is_empty() || b.url.to_lowercase().contains(&f) || b.title.to_lowercase().contains(&f)) {
        ui.horizontal(|ui| {
            if ui.small_button("×").on_hover_text("Удалить").clicked() {
                remove = Some(b.url.clone());
            }
            let title = if b.title.is_empty() { &b.url } else { &b.title };
            if ui.link(clip(title, 40)).on_hover_text(&b.url).clicked() {
                actions.push(Action::OpenUrl(b.url.clone()));
            }
        });
    }
    if shell.profile.bookmarks.is_empty() {
        ui.label("Закладок пока нет — звёздочка в адресной строке или Ctrl+D.");
    }
    if let Some(url) = remove {
        shell.profile.remove_bookmark(&url);
        shell.update_page_data();
    }
}

fn history(ui: &mut Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    let f = filter_box(ui, shell, "Найти в истории");
    if ui.button("Очистить историю").clicked() {
        shell.modal = Some(Modal::Rollback(String::from("\u{0}history")));
    }
    let mut forget = None;
    for v in shell.profile.history.iter().filter(|v| f.is_empty() || v.url.to_lowercase().contains(&f) || v.title.to_lowercase().contains(&f)).take(300) {
        ui.horizontal(|ui| {
            if ui.small_button("×").clicked() {
                forget = Some(v.url.clone());
            }
            ui.label(RichText::new(when(v.last_ms)).small());
            let title = if v.title.is_empty() { &v.url } else { &v.title };
            if ui.link(clip(title, 34)).on_hover_text(format!("{} · {} раз", v.url, v.count)).clicked() {
                actions.push(Action::OpenUrl(v.url.clone()));
            }
        });
    }
    if let Some(url) = forget {
        shell.profile.forget(&url);
    }
}

fn passwords(ui: &mut Ui, shell: &mut Shell) {
    let Some(vault) = &shell.vault else {
        ui.label("Хранилище закрыто.");
        return;
    };
    ui.label(RichText::new(if vault.has_master_password() {
        "Хранилище защищено мастер-паролем."
    } else {
        "Без мастер-пароля пароли защищены ключом этого устройства (файлы профиля). Задать мастер-пароль — в «Настройках»."
    }).small());
    if shell.profile.foreign_logins > 0 {
        ui.label(RichText::new(format!(
            "Ещё {} пароль(ей) с других устройств зашифрованы другим паролем синхронизации — задайте такой же в «Синхронизации».",
            shell.profile.foreign_logins
        )).small());
    }
    ui.checkbox(&mut shell.profile.settings.save_passwords, "Предлагать сохранить пароль");
    ui.checkbox(&mut shell.profile.settings.autofill, "Подставлять сохранённые пароли");
    ui.separator();
    let f = filter_box(ui, shell, "Найти сайт");
    let mut delete = None;
    let entries: Vec<(String, Login)> = shell.profile.logins.iter().map(|(k, l)| (k.clone(), l.clone())).collect();
    for (record, login) in entries.iter().filter(|(_, l)| f.is_empty() || l.origin.to_lowercase().contains(&f) || l.username.to_lowercase().contains(&f)) {
        ui.group(|ui| {
            ui.label(RichText::new(&login.origin).strong());
            ui.horizontal(|ui| {
                ui.label(if login.username.is_empty() { "(без имени)" } else { &login.username });
                let shown = shell.show_passwords.contains(record);
                ui.label(if shown { login.password.clone() } else { "••••••••".into() });
                if ui.small_button(if shown { "скрыть" } else { "показать" }).clicked() {
                    if shown {
                        shell.show_passwords.remove(record);
                    } else {
                        shell.show_passwords.insert(record.clone());
                    }
                }
                if ui.small_button("копировать").clicked() {
                    *shell.delegate.clipboard.borrow_mut() = login.password.clone();
                }
                if ui.small_button("удалить").clicked() {
                    delete = Some(record.clone());
                }
            });
        });
    }
    if shell.profile.logins.is_empty() {
        ui.label("Сохранённых паролей нет. OBSOS предложит сохранить пароль, когда вы войдёте на сайт.");
    }
    if let Some(record) = delete {
        shell.profile.delete_login(&record);
    }
}

fn remote_tabs(ui: &mut Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    let devices = shell.profile.remote_tabs();
    if devices.is_empty() {
        ui.label("Других устройств пока нет: вкладки появятся после синхронизации с другого компьютера или телефона.");
    }
    for d in devices {
        ui.label(RichText::new(format!("{} · {}", d.device, when(d.hlc.wall))).strong());
        for t in &d.tabs {
            ui.horizontal(|ui| {
                if ui.small_button("×").on_hover_text("Закрыть на том устройстве").clicked() {
                    let _ = shell.profile.replica.close_remote_tab(&d.device, &t.id, now_ms());
                }
                if ui.link(clip(if t.title.is_empty() { &t.url } else { &t.title }, 40)).on_hover_text(&t.url).clicked() {
                    actions.push(Action::NewTabUrl(t.url.clone()));
                }
            });
        }
        ui.separator();
    }
}

fn sync_panel(ui: &mut Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    let active = shell.account();
    match &active {
        Some(a) => {
            ui.label(RichText::new(format!("Включена: {} · {}", a.provider.title(), a.repo)).strong());
        }
        None => {
            ui.label("Синхронизация выключена. Выберите, где хранить закладки, историю, оформление и пароли:");
        }
    }
    if !shell.sync.status.is_empty() {
        ui.label(RichText::new(&shell.sync.status).small());
    }
    if active.is_some() {
        ui.horizontal(|ui| {
            if ui.add_enabled(shell.syncer.busy == 0, egui::Button::new("Синхронизировать")).clicked() {
                actions.push(Action::SyncNow(None));
            }
            if ui.button("Выключить").clicked() {
                shell.sync_off();
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut shell.sync.label).hint_text("подпись точки").desired_width(170.0));
            if ui.add_enabled(shell.syncer.busy == 0, egui::Button::new("С меткой")).on_hover_text(if active.as_ref().is_some_and(|a| a.provider.is_git()) {
                "Синхронизировать и поставить тег obsos-дата-подпись: к нему можно откатиться"
            } else {
                "Синхронизировать и сохранить копию (points/…): к ней можно откатиться"
            }).clicked() {
                actions.push(Action::SyncNow(Some(std::mem::take(&mut shell.sync.label))));
            }
        });
        ui.checkbox(&mut shell.profile.settings.sync_auto, "Синхронизировать само");
        ui.horizontal(|ui| {
            ui.label("каждые");
            ui.add(egui::DragValue::new(&mut shell.profile.settings.sync_every_min).range(1..=1440).suffix(" мин"));
        });
        ui.checkbox(&mut shell.profile.settings.sync_points_auto, "Ставить точку (тег) при каждой синхронизации");
        ui.separator();
        ui.label(RichText::new("Точки синхронизации").strong());
        if ui.small_button("обновить список").clicked() {
            shell.load_points();
        }
        let points = shell.sync.points.clone();
        if points.is_empty() {
            ui.label(RichText::new("Пока нет — «С меткой» создаёт точку.").small());
        }
        for point in points.iter().take(50) {
            ui.horizontal(|ui| {
                if ui.small_button("откатить").on_hover_text("Вернуть закладки, историю, оформление и пароли к этой точке").clicked() {
                    shell.modal = Some(Modal::Rollback(point.name.clone()));
                }
                ui.label(&point.name);
                if !point.label.is_empty() {
                    ui.label(RichText::new(&point.label).small());
                }
            });
        }
        ui.separator();
    }
    ui.label(RichText::new("Площадка").strong());
    egui::ComboBox::from_id_salt("provider").selected_text(shell.sync.draft.provider.title()).width(260.0).show_ui(ui, |ui| {
        for provider in Provider::ALL {
            if ui.selectable_label(shell.sync.draft.provider == provider, provider.title()).clicked() {
                shell.select_provider(provider);
            }
        }
    });
    let provider = shell.sync.draft.provider;
    let draft = &mut shell.sync.draft;
    let field = |ui: &mut Ui, label: &str, value: &mut String, hint: &str, password: bool| {
        ui.label(RichText::new(label).small());
        ui.add(egui::TextEdit::singleline(value).hint_text(hint).password(password).desired_width(f32::INFINITY));
    };
    match provider {
        Provider::GitHub | Provider::GitLab | Provider::Bitbucket => {
            field(ui, "Репозиторий (создастся закрытым, если его нет)", &mut draft.repo, "имя/obsos-sync", false);
            field(ui, "Ветка", &mut draft.branch, "main", false);
            if provider == Provider::Bitbucket {
                field(ui, "Имя пользователя или почта (для пароля приложения)", &mut draft.user, "roman", false);
            }
            if provider == Provider::GitLab {
                field(ui, "Свой сервер GitLab (пусто — gitlab.com)", &mut draft.host, "https://gitlab.example.org", false);
            }
            field(ui, if provider == Provider::Bitbucket { "Пароль приложения или токен" } else { "Токен доступа" }, &mut draft.token, "", true);
        }
        Provider::YandexDisk => {
            field(ui, "Папка на Диске", &mut draft.repo, "OBSOS", false);
            field(ui, "Логин Яндекса", &mut draft.user, "roman@yandex.ru", false);
            field(ui, "Пароль приложения (раздел «Файлы»)", &mut draft.token, "", true);
        }
        Provider::WebDav => {
            field(ui, "Адрес WebDAV", &mut draft.host, "https://cloud.example.org/remote.php/dav/files/roman", false);
            field(ui, "Папка", &mut draft.repo, "OBSOS", false);
            field(ui, "Имя", &mut draft.user, "", false);
            field(ui, "Пароль приложения", &mut draft.token, "", true);
        }
        Provider::GoogleDrive => {
            field(ui, "Папка на Google Диске", &mut draft.repo, "OBSOS", false);
            ui.label(RichText::new("Google Диск — только вход через браузер: нужен OAuth-клиент типа «Телевизоры и устройства с ограниченным вводом».").small());
        }
        Provider::Folder => {
            field(ui, "Папка (флешка, сетевой диск)", &mut draft.repo, "/media/usb/OBSOS", false);
        }
    }
    if oauth::supported(provider) {
        egui::CollapsingHeader::new("Вход через браузер (OAuth Device Flow)").default_open(provider == Provider::GoogleDrive).show(ui, |ui| {
            field(ui, "client_id OAuth-приложения", &mut draft.client_id, "", false);
            if matches!(provider, Provider::GoogleDrive | Provider::YandexDisk) {
                field(ui, "client_secret", &mut draft.client_secret, "", true);
            }
        });
    }
    let token_page = provider.token_page();
    let can_device = oauth::supported(provider) && !shell.sync.draft.client_id.is_empty();
    ui.horizontal_wrapped(|ui| {
        if can_device && ui.button("Войти через браузер").clicked() {
            shell.login_start();
        }
        if !token_page.is_empty() && ui.button("Где взять токен").clicked() {
            actions.push(Action::NewTabUrl(token_page.to_string()));
        }
        if ui.button("Сохранить и включить").clicked() {
            shell.check_account();
        }
    });
    if let Some(flow) = &shell.sync.flow {
        ui.label(RichText::new(format!("Код: {} — введите его на {}", flow.code.user_code, flow.code.verification_uri)).strong());
    }
    ui.separator();
    ui.label(RichText::new("Пароль синхронизации паролей").strong());
    let shared = shell.vault.as_ref().is_some_and(|v| v.sync_key_shared);
    ui.label(RichText::new(if shared {
        "Задан: пароли расшифровываются на всех устройствах с тем же паролем."
    } else {
        "Не задан: пароли синхронизируются, но прочитать их может только это устройство. Задайте один и тот же пароль на всех устройствах."
    }).small());
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut shell.sync.sync_password).password(true).hint_text("пароль синхронизации").desired_width(200.0));
        if ui.button("Задать").clicked() {
            shell.set_sync_password();
        }
    });
    ui.label(RichText::new(format!("Это устройство: {} ({})", shell.profile.settings.device_name, shell.profile.settings.device)).small());
}

fn color_row(ui: &mut Ui, label: &str, rgb: &mut Rgb) -> bool {
    let mut arr = [rgb.0, rgb.1, rgb.2];
    let changed = ui.horizontal(|ui| {
        let r = ui.color_edit_button_srgb(&mut arr).changed();
        ui.label(label);
        r
    }).inner;
    if changed {
        *rgb = Rgb(arr[0], arr[1], arr[2]);
    }
    changed
}

fn looks(ui: &mut Ui, shell: &mut Shell) {
    let mut look = shell.profile.settings.look.clone();
    let before = look.clone();
    ui.label(RichText::new("Сохранённые оформления").strong());
    let saved = shell.profile.settings.looks.clone();
    let mut delete = None;
    for l in &saved {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(36.0, 16.0), Sense::hover());
            ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(12.0, 16.0)), 3.0, c(l.sidebar));
            ui.painter().rect_filled(Rect::from_min_size(r.min + vec2(12.0, 0.0), vec2(12.0, 16.0)), 3.0, c(l.accent));
            ui.painter().rect_filled(Rect::from_min_size(r.min + vec2(24.0, 0.0), vec2(12.0, 16.0)), 3.0, c(l.accent2));
            if ui.selectable_label(l.name == look.name, &l.name).clicked() {
                look = l.clone();
            }
            if saved.len() > 1 && ui.small_button("×").clicked() {
                delete = Some(l.name.clone());
            }
        });
    }
    if let Some(name) = delete {
        shell.profile.delete_look(&name);
    }
    ui.separator();
    ui.horizontal(|ui| {
        if ui.selectable_label(look.dark, "Тёмная основа").clicked() && !look.dark {
            look = Look { name: look.name.clone(), ..Look::dark() };
        }
        if ui.selectable_label(!look.dark, "Светлая основа").clicked() && look.dark {
            look = Look { name: look.name.clone(), ..Look::light() };
        }
    });
    ui.label(RichText::new("Палитра — каждый цвет свой").strong());
    color_row(ui, "Акцент", &mut look.accent);
    color_row(ui, "Второй акцент", &mut look.accent2);
    color_row(ui, "Боковая панель", &mut look.sidebar);
    color_row(ui, "Панель адреса", &mut look.toolbar);
    color_row(ui, "Текст", &mut look.text);
    ui.label(RichText::new("Куда заходит акцент").strong());
    ui.radio_value(&mut look.accent_reach, 0, "только выделение");
    ui.radio_value(&mut look.accent_reach, 1, "и активная вкладка");
    ui.radio_value(&mut look.accent_reach, 2, "и кнопки");
    ui.label(RichText::new("Раскладка").strong());
    ui.radio_value(&mut look.side_tabs, true, "вкладки сбоку (как Zervo)");
    ui.radio_value(&mut look.side_tabs, false, "вкладки сверху");
    if look.side_tabs {
        ui.add(egui::Slider::new(&mut look.sidebar_width, 160..=420).text("ширина панели"));
    }
    ui.checkbox(&mut look.compact, "Плотнее (ниже панель адреса)");
    ui.separator();
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut look.name).desired_width(160.0));
        if ui.button("Сохранить под этим именем").clicked() {
            shell.profile.save_look(&look);
            shell.toast(format!("Оформление «{}» сохранено и уйдёт на другие устройства", look.name));
        }
    });
    if look != before {
        shell.profile.settings.look = look;
        shell.profile.save_settings();
    }
}

fn settings(ui: &mut Ui, shell: &mut Shell) {
    let s = &mut shell.profile.settings;
    let before = (s.search, s.home.clone());
    ui.label(RichText::new("Поиск").strong());
    egui::ComboBox::from_id_salt("search").selected_text(s.search.title()).show_ui(ui, |ui| {
        for e in Search::ALL {
            ui.selectable_value(&mut s.search, e, e.title());
        }
    });
    ui.label(RichText::new("Домашняя страница").strong());
    ui.add(egui::TextEdit::singleline(&mut s.home).desired_width(f32::INFINITY));
    ui.checkbox(&mut s.restore_session, "Открывать вкладки прошлого сеанса");
    ui.horizontal(|ui| {
        ui.label("Хранить историю");
        ui.add(egui::DragValue::new(&mut s.history_days).range(1..=3650).suffix(" дн."));
    });
    ui.label(RichText::new("Имя устройства").strong());
    ui.add(egui::TextEdit::singleline(&mut s.device_name).desired_width(f32::INFINITY));
    let after = (s.search, s.home.clone());
    if after != before {
        let (search, home) = after;
        shell.profile.put_shared("search", search.id());
        shell.profile.put_shared("home", &home);
        shell.update_page_data();
    }
    shell.profile.save_settings();
    ui.separator();
    ui.label(RichText::new("Мастер-пароль хранилища").strong());
    let has = shell.vault.as_ref().is_some_and(|v| v.has_master_password());
    ui.label(RichText::new(if has { "Задан: при запуске OBSOS спросит его." } else { "Не задан: хранилище открыто ключом устройства." }).small());
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut shell.new_profile).password(true).hint_text("новый мастер-пароль").desired_width(180.0));
        if ui.button(if has { "Сменить" } else { "Задать" }).clicked() {
            if let Some(v) = shell.vault.as_mut() {
                v.set_master(&shell.new_profile);
            }
            shell.new_profile.clear();
            shell.toast("Мастер-пароль сохранён");
        }
        if has && ui.button("Снять").clicked() {
            if let Some(v) = shell.vault.as_mut() {
                v.set_master("");
            }
        }
    });
}

fn scripts(ui: &mut Ui, shell: &mut Shell) {
    let dir = shell.profile.dir.join("userscripts");
    ui.label(RichText::new(format!(
        "У Servo нет расширений. Вместо них OBSOS подключает к каждой странице ваши скрипты (*.js) и стили (*.css) из папки {} — изменения действуют на новых вкладках.",
        dir.display()
    )).small());
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    files.sort();
    if files.is_empty() {
        ui.label("Папка пуста.");
    }
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let enabled = name.ends_with(".js") || name.ends_with(".css");
        ui.horizontal(|ui| {
            let mut on = enabled;
            if ui.checkbox(&mut on, &name).changed() {
                let target = if on { path.with_extension("") } else { path.with_extension(format!("{}.off", path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default())) };
                let _ = std::fs::rename(&path, target);
            }
        });
    }
    if ui.button("Создать пример").clicked() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("example.css.off"), "/* Пример пользовательского стиля OBSOS: уберите .off из имени */\nbody { font-size: 105% !important; }\n");
        let _ = std::fs::write(dir.join("example.js.off"), "// Пример пользовательского скрипта OBSOS: уберите .off из имени\nconsole.log('OBSOS userscript on ' + location.host);\n");
    }
}

fn profiles(ui: &mut Ui, shell: &mut Shell) {
    ui.label(RichText::new("У каждого профиля свои куки, закладки, пароли и синхронизация.").small());
    for name in crate::profile::Profile::list() {
        ui.horizontal(|ui| {
            ui.label(if name == shell.profile.name { format!("● {name} (открыт)") } else { name.clone() });
            if name != shell.profile.name && ui.small_button("открыть").clicked() {
                let _ = crate::window::spawn(&format!("/opt/obsos/bin/obsos --profile {name}"));
            }
        });
    }
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut shell.new_profile).hint_text("имя нового профиля").desired_width(180.0));
        if ui.button("Создать и открыть").clicked() && !shell.new_profile.trim().is_empty() {
            let name: String = shell.new_profile.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
            let _ = crate::window::spawn(&format!("/opt/obsos/bin/obsos --profile {name}"));
            shell.new_profile.clear();
        }
    });
}

// ── Диалоги ─────────────────────────────────────────────────────────────────

fn modal(ctx: &egui::Context, shell: &mut Shell, page: Rect, p: &Palette, actions: &mut Vec<Action>) {
    let Some(m) = shell.modal.take() else { return };
    let mut put_back: Option<Modal> = None;
    // Затемнение страницы — щелчок мимо закрывает меню и списки.
    let backdrop = egui::Area::new(egui::Id::new("obsos-backdrop")).order(egui::Order::Foreground).fixed_pos(page.min).show(ctx, |ui| {
        let (r, resp) = ui.allocate_exact_size(page.size(), Sense::click());
        let dim = matches!(m, Modal::Select(..) | Modal::Context(..));
        if !dim {
            ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(90));
        }
        resp.clicked()
    });
    let center = page.center();
    let window = |title: &str| {
        egui::Window::new(title.to_string())
            .collapsible(false)
            .resizable(false)
            .order(egui::Order::Tooltip)
            .pivot(Align2::CENTER_CENTER)
            .fixed_pos(center)
            .frame(egui::Frame::window(&ctx.global_style()).fill(p.sidebar))
    };
    match m {
        Modal::Dialog(dialog, mut value) => {
            let mut done: Option<bool> = None;
            let title = match &dialog {
                SimpleDialog::Alert(_) => "Сообщение страницы",
                SimpleDialog::Confirm(_) => "Подтверждение",
                SimpleDialog::Prompt(_) => "Вопрос страницы",
            };
            window(title).show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.label(dialog.message());
                if matches!(dialog, SimpleDialog::Prompt(_)) {
                    let r = ui.add(egui::TextEdit::singleline(&mut value).desired_width(380.0));
                    r.request_focus();
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        done = Some(true);
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        done = Some(true);
                    }
                    if !matches!(dialog, SimpleDialog::Alert(_)) && ui.button("Отмена").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    let mut dialog = dialog;
                    if let SimpleDialog::Prompt(p) = &mut dialog {
                        p.set_current_value(&value);
                    }
                    dialog.confirm();
                }
                Some(false) => dialog.dismiss(),
                None => put_back = Some(Modal::Dialog(dialog, value)),
            }
        }
        Modal::Auth { request, host, mut user, mut pass, mut remember } => {
            let mut done: Option<bool> = None;
            window("Вход на сайт").show(ctx, |ui| {
                ui.label(format!("Сайт {host} просит имя и пароль."));
                ui.add(egui::TextEdit::singleline(&mut user).hint_text("имя").desired_width(300.0));
                let r = ui.add(egui::TextEdit::singleline(&mut pass).password(true).hint_text("пароль").desired_width(300.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                }
                ui.checkbox(&mut remember, "Запомнить");
                ui.horizontal(|ui| {
                    if ui.button("Войти").clicked() {
                        done = Some(true);
                    }
                    if ui.button("Отмена").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    if remember {
                        if let Some(origin) = crate::vault::origin_of(request.url()) {
                            shell.save_login(Login { origin, username: user.clone(), password: pass.clone(), updated_ms: now_ms() });
                        }
                    }
                    request.authenticate(user, pass);
                }
                Some(false) => drop(request),
                None => put_back = Some(Modal::Auth { request, host, user, pass, remember }),
            }
        }
        Modal::Select(mut select, at) => {
            let mut chosen: Option<usize> = None;
            let current = select.selected_options();
            egui::Area::new(egui::Id::new("obsos-select")).order(egui::Order::Tooltip).fixed_pos(at).show(ctx, |ui| {
                egui::Frame::popup(&ctx.global_style()).fill(p.sidebar).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        ui.set_min_width(180.0);
                        let mut option = |ui: &mut Ui, o: &servo::SelectElementOption| {
                            let r = ui.add_enabled(!o.is_disabled, egui::Button::selectable(current.contains(&o.id), &o.label));
                            if r.clicked() {
                                chosen = Some(o.id);
                            }
                        };
                        for item in select.options() {
                            match item {
                                SelectElementOptionOrOptgroup::Option(o) => option(ui, o),
                                SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                                    ui.label(RichText::new(label).strong());
                                    for o in options {
                                        option(ui, o);
                                    }
                                }
                            }
                        }
                    });
                });
            });
            if let Some(id) = chosen {
                select.select(vec![id]);
                select.submit();
                println!("browser: select option {id}");
            } else if backdrop.inner {
                select.submit();
            } else {
                put_back = Some(Modal::Select(select, at));
            }
        }
        Modal::Context(menu, at) => {
            let mut chosen = None;
            egui::Area::new(egui::Id::new("obsos-context")).order(egui::Order::Tooltip).fixed_pos(at).show(ctx, |ui| {
                egui::Frame::popup(&ctx.global_style()).fill(p.sidebar).show(ui, |ui| {
                    ui.set_min_width(200.0);
                    for item in menu.items() {
                        match item {
                            ContextMenuItem::Item { label, action, enabled } => {
                                if ui.add_enabled(*enabled, egui::Button::new(label).frame(false)).clicked() {
                                    chosen = Some(*action);
                                }
                            }
                            ContextMenuItem::Separator => {
                                ui.separator();
                            }
                        }
                    }
                });
            });
            if let Some(action) = chosen {
                menu.select(action);
            } else if backdrop.inner {
                menu.dismiss();
            } else {
                put_back = Some(Modal::Context(menu, at));
            }
        }
        Modal::Color(mut picker, mut rgb) => {
            let mut done: Option<bool> = None;
            window("Цвет").show(ctx, |ui| {
                ui.color_edit_button_srgb(&mut rgb);
                ui.horizontal(|ui| {
                    if ui.button("Выбрать").clicked() {
                        done = Some(true);
                    }
                    if ui.button("Отмена").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    picker.select(Some(RgbColor { red: rgb[0], green: rgb[1], blue: rgb[2] }));
                    picker.submit();
                }
                Some(false) => picker.submit(),
                None => put_back = Some(Modal::Color(picker, rgb)),
            }
        }
        Modal::File(mut picker, mut path) => {
            let mut done: Option<bool> = None;
            window("Выбор файла").show(ctx, |ui| {
                ui.label("Путь к файлу:");
                let r = ui.add(egui::TextEdit::singleline(&mut path).desired_width(380.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                }
                let dir = std::path::Path::new(&path);
                let dir = if dir.is_dir() { dir.to_path_buf() } else { dir.parent().map(|d| d.to_path_buf()).unwrap_or_default() };
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    let mut entries: Vec<_> = std::fs::read_dir(&dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
                    entries.sort();
                    for e in entries.iter().take(200) {
                        let name = e.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let label = if e.is_dir() { format!("{name}/") } else { name };
                        if ui.selectable_label(false, label).clicked() {
                            path = if e.is_dir() { format!("{}/", e.display()) } else { e.display().to_string() };
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.button("Открыть").clicked() {
                        done = Some(true);
                    }
                    if ui.button("Отмена").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) => {
                    picker.select(&[std::path::PathBuf::from(path.clone())]);
                    picker.submit();
                }
                Some(false) => picker.dismiss(),
                None => put_back = Some(Modal::File(picker, path)),
            }
        }
        Modal::Master { mut text, error } => {
            let mut unlock = false;
            window("OBSOS — хранилище паролей").show(ctx, |ui| {
                ui.label("Введите мастер-пароль, чтобы открыть пароли и синхронизацию.");
                let r = ui.add(egui::TextEdit::singleline(&mut text).password(true).desired_width(300.0));
                r.request_focus();
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    unlock = true;
                }
                if !error.is_empty() {
                    ui.colored_label(Color32::from_rgb(0xe0, 0x60, 0x60), &error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Открыть").clicked() {
                        unlock = true;
                    }
                });
            });
            if unlock {
                shell.unlock(&text);
            } else {
                put_back = Some(Modal::Master { text, error });
            }
        }
        Modal::Rollback(point) => {
            let mut done: Option<bool> = None;
            let history = point == "\u{0}history";
            window(if history { "Очистить историю" } else { "Откат к точке синхронизации" }).show(ctx, |ui| {
                ui.set_max_width(400.0);
                if history {
                    ui.label("Удалить всю историю на всех синхронизированных устройствах?");
                } else {
                    ui.label(format!(
                        "Вернуть закладки, историю, оформление и пароли к состоянию точки {point}? Изменения после неё уйдут на все устройства как новые — сама история площадки не переписывается, к нынешнему состоянию можно вернуться откатом к более поздней точке."
                    ));
                }
                ui.horizontal(|ui| {
                    if ui.button(if history { "Очистить" } else { "Откатить" }).clicked() {
                        done = Some(true);
                    }
                    if ui.button("Отмена").clicked() {
                        done = Some(false);
                    }
                });
            });
            match done {
                Some(true) if history => shell.profile.clear_history(),
                Some(true) => shell.rollback(point),
                Some(false) => {}
                None => put_back = Some(Modal::Rollback(point)),
            }
        }
    }
    let _ = actions;
    if shell.modal.is_none() {
        shell.modal = put_back;
    }
}
