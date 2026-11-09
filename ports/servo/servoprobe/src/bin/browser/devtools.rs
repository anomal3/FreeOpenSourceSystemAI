//! Инструменты разработчика OBSOS (F12, Ctrl+Shift+I): консоль, сеть, DOM.
//!
//! Сервер DevTools Servo (протокол Firefox) в сборку не входит, поэтому всё
//! своё и простое: консоль — сообщения `console.*` страницы плюс ошибки и
//! отклонённые обещания (их ловит `PAGE_SCRIPT`), строка ввода выполняет
//! JavaScript в странице; сеть — каждый запрос, который Servo показал
//! оболочке (`load_web_resource`): метод, адрес, назначение, заблокирован ли;
//! DOM — дерево документа скриптом, по щелчку — `outerHTML` и подсветка узла.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Instant;

use browser_sync::json::{self, Value};
use egui::{Color32, RichText, Ui};
use servo::WebViewId;

use crate::shell::{Action, Shell};

const MAX_ENTRIES: usize = 1000;

pub struct ConsoleEntry {
    pub webview: Option<WebViewId>,
    pub level: String,
    pub text: String,
    pub at: Instant,
}

pub struct NetEntry {
    pub webview: WebViewId,
    pub method: String,
    pub url: String,
    pub kind: String,
    pub main: bool,
    pub blocked: bool,
    pub at: Instant,
}

/// Журналы, которые пишет делегат Servo.
#[derive(Default)]
pub struct DevLog {
    pub console: VecDeque<ConsoleEntry>,
    pub network: VecDeque<NetEntry>,
}

impl DevLog {
    pub fn console(&mut self, webview: Option<WebViewId>, level: &str, text: String) {
        if self.console.len() >= MAX_ENTRIES {
            self.console.pop_front();
        }
        self.console.push_back(ConsoleEntry { webview, level: level.to_string(), text, at: Instant::now() });
    }

    pub fn request(&mut self, entry: NetEntry) {
        if self.network.len() >= MAX_ENTRIES {
            self.network.pop_front();
        }
        self.network.push_back(entry);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum DevTab {
    #[default]
    Console,
    Network,
    Dom,
}

/// Состояние панели (ответы скриптов приходят обратными вызовами).
pub struct DevUi {
    pub tab: DevTab,
    pub input: String,
    pub filter: String,
    pub only_errors: bool,
    pub dom: Rc<RefCell<Option<Value>>>,
    pub selected: Rc<RefCell<String>>,
    pub started: Instant,
}

impl Default for DevUi {
    fn default() -> Self {
        DevUi {
            tab: DevTab::Console,
            input: String::new(),
            filter: String::new(),
            only_errors: false,
            dom: Rc::default(),
            selected: Rc::default(),
            started: Instant::now(),
        }
    }
}

/// Дерево документа JSON: `t` — тег, `i`/`c` — id/class, `x` — текст, `k` —
/// дети, `n` — номер узла в `window.__obsosDom` (для `outerHTML`), `m` —
/// сколько детей не показано (глубже 14 уровней).
const DOM_SCRIPT: &str = r#"(function(){var all=[];function n(e,d){if(e.nodeType==3){return {t:'#text',x:e.nodeValue.trim().slice(0,100)};}
var o={t:e.tagName.toLowerCase(),n:all.length};all.push(e);if(e.id)o.i=String(e.id).slice(0,60);
if(typeof e.className=='string'&&e.className)o.c=e.className.slice(0,80);var ch=[];
if(d<14){for(var k=e.firstChild;k;k=k.nextSibling){if(k.nodeType==1||(k.nodeType==3&&k.nodeValue.trim()))ch.push(n(k,d+1));if(ch.length>300)break;}}
else if(e.children.length)o.m=e.children.length;o.k=ch;return o;}
var root=n(document.documentElement,0);window.__obsosDom=all;return JSON.stringify(root);})()"#;

fn node_script(n: u64) -> String {
    format!(
        "(function(){{var e=(window.__obsosDom||[])[{n}];if(!e)return '';try{{if(window.__obsosMark)window.__obsosMark.style.outline=window.__obsosMarkWas||'';\
window.__obsosMark=e;window.__obsosMarkWas=e.style.outline;e.style.outline='2px solid #ff8a3d';e.scrollIntoView({{block:'center'}});}}catch(x){{}}\
var h=e.outerHTML;return h.length>6000?h.slice(0,6000)+'\\n…':h;}})()"
    )
}

/// Значение JavaScript — строкой для консоли.
pub fn show_value(v: &servo::JSValue) -> String {
    match v {
        servo::JSValue::String(s) => format!("{s:?}"),
        other => format!("{other:?}"),
    }
}

pub fn ui(ui: &mut Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    let _ = actions;
    ui.horizontal(|ui| {
        for (tab, name) in [(DevTab::Console, "Консоль"), (DevTab::Network, "Сеть"), (DevTab::Dom, "DOM")] {
            ui.selectable_value(&mut shell.dev.tab, tab, name);
        }
        ui.separator();
        if ui.small_button("Очистить").clicked() {
            let mut log = shell.delegate.devlog.borrow_mut();
            match shell.dev.tab {
                DevTab::Console => log.console.clear(),
                DevTab::Network => log.network.clear(),
                DevTab::Dom => *shell.dev.dom.borrow_mut() = None,
            }
        }
    });
    let active = shell.active_view().map(|w| w.id());
    match shell.dev.tab {
        DevTab::Console => console(ui, shell, active),
        DevTab::Network => network(ui, shell, active),
        DevTab::Dom => dom(ui, shell),
    }
}

fn console(ui: &mut Ui, shell: &mut Shell, active: Option<WebViewId>) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut shell.dev.filter).hint_text("фильтр").desired_width(160.0));
        ui.checkbox(&mut shell.dev.only_errors, "только ошибки");
    });
    let input_h = 34.0;
    egui::ScrollArea::vertical().max_height(ui.available_height() - input_h).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
        let log = shell.delegate.devlog.borrow();
        let f = shell.dev.filter.to_lowercase();
        for e in log.console.iter().filter(|e| e.webview.is_none() || e.webview == active) {
            let error = e.level.contains("Error");
            let warn = e.level.contains("Warn");
            if shell.dev.only_errors && !error && !warn {
                continue;
            }
            if !f.is_empty() && !e.text.to_lowercase().contains(&f) {
                continue;
            }
            let color = if error {
                Color32::from_rgb(0xff, 0x8a, 0x80)
            } else if warn {
                Color32::from_rgb(0xff, 0xd0, 0x70)
            } else if e.level == "result" {
                Color32::from_rgb(0x9a, 0xd0, 0xff)
            } else {
                ui.visuals().text_color()
            };
            let secs = e.at.duration_since(shell.dev.started).as_secs_f32();
            ui.label(RichText::new(format!("{secs:>7.1}  {}", e.text)).monospace().size(12.0).color(color));
        }
    });
    ui.horizontal(|ui| {
        let edit = ui.add(egui::TextEdit::singleline(&mut shell.dev.input).hint_text("JavaScript — Enter выполнить").font(egui::TextStyle::Monospace).desired_width(ui.available_width()));
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !shell.dev.input.trim().is_empty() {
            let code = std::mem::take(&mut shell.dev.input);
            shell.delegate.devlog.borrow_mut().console(active, "input", format!("› {code}"));
            if let Some(w) = shell.active_view() {
                let delegate = shell.delegate.clone();
                let id = w.id();
                w.evaluate_javascript(code, move |result| {
                    let text = match result {
                        Ok(v) => show_value(&v),
                        Err(e) => format!("ошибка: {e:?}"),
                    };
                    delegate.devlog.borrow_mut().console(Some(id), "result", format!("← {text}"));
                });
            }
            edit.request_focus();
        }
    });
}

fn network(ui: &mut Ui, shell: &mut Shell, active: Option<WebViewId>) {
    ui.add(egui::TextEdit::singleline(&mut shell.dev.filter).hint_text("фильтр адресов").desired_width(220.0));
    let log = shell.delegate.devlog.borrow();
    let f = shell.dev.filter.to_lowercase();
    let rows: Vec<&NetEntry> = log.network.iter().filter(|e| Some(e.webview) == active && (f.is_empty() || e.url.to_lowercase().contains(&f))).collect();
    let blocked = rows.iter().filter(|e| e.blocked).count();
    ui.label(RichText::new(format!("Запросов: {} · заблокировано: {blocked}. Ответов Servo оболочке не показывает — видны запросы.", rows.len())).small());
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new("obsos-net").striped(true).num_columns(4).show(ui, |ui| {
            ui.label(RichText::new("время").strong());
            ui.label(RichText::new("метод").strong());
            ui.label(RichText::new("тип").strong());
            ui.label(RichText::new("адрес").strong());
            ui.end_row();
            for e in rows {
                let secs = e.at.duration_since(shell.dev.started).as_secs_f32();
                ui.label(RichText::new(format!("{secs:.1}")).monospace().size(12.0));
                ui.label(RichText::new(&e.method).monospace().size(12.0));
                let kind = if e.main { String::from("документ") } else { e.kind.clone() };
                ui.label(RichText::new(kind).size(12.0));
                let mut text = RichText::new(&e.url).monospace().size(12.0);
                if e.blocked {
                    text = text.strikethrough().color(Color32::from_rgb(0xff, 0x8a, 0x80));
                }
                if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text("Щелчок — скопировать адрес").clicked() {
                    crate::window::clipboard_set(&e.url);
                }
                ui.end_row();
            }
        });
    });
}

fn dom(ui: &mut Ui, shell: &mut Shell) {
    ui.horizontal(|ui| {
        if ui.button("Обновить дерево").clicked() || shell.dev.dom.borrow().is_none() {
            if let Some(w) = shell.active_view() {
                let slot = shell.dev.dom.clone();
                *slot.borrow_mut() = Some(Value::Null);
                w.evaluate_javascript(DOM_SCRIPT, move |result| {
                    let tree = match result {
                        Ok(servo::JSValue::String(text)) => json::parse(&text).unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    *slot.borrow_mut() = Some(tree);
                });
            }
        }
        ui.label(RichText::new("щелчок по узлу — его HTML и рамка на странице").small());
    });
    let height = ui.available_height();
    let selected_h = if shell.dev.selected.borrow().is_empty() { 0.0 } else { height * 0.35 };
    let mut pick: Option<u64> = None;
    egui::ScrollArea::both().id_salt("obsos-dom").max_height(height - selected_h).auto_shrink([false, false]).show(ui, |ui| {
        let tree = shell.dev.dom.borrow();
        match tree.as_ref() {
            Some(Value::Null) | None => {
                ui.label("Читаю документ…");
            }
            Some(root) => node(ui, root, 0, &mut pick),
        }
    });
    if let Some(n) = pick {
        if let Some(w) = shell.active_view() {
            let slot = shell.dev.selected.clone();
            w.evaluate_javascript(node_script(n), move |result| {
                if let Ok(servo::JSValue::String(html)) = result {
                    *slot.borrow_mut() = html;
                }
            });
        }
    }
    let html = shell.dev.selected.borrow().clone();
    if !html.is_empty() {
        ui.separator();
        egui::ScrollArea::vertical().id_salt("obsos-dom-html").show(ui, |ui| {
            let mut text = html;
            ui.add(egui::TextEdit::multiline(&mut text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
        });
    }
}

fn node(ui: &mut Ui, v: &Value, depth: usize, pick: &mut Option<u64>) {
    let tag = v.str_field("t").unwrap_or("?");
    if tag == "#text" {
        ui.label(RichText::new(format!("\"{}\"", v.str_field("x").unwrap_or(""))).monospace().size(12.0).weak());
        return;
    }
    let mut head = format!("<{tag}");
    if let Some(id) = v.str_field("i") {
        head.push_str(&format!(" id=\"{id}\""));
    }
    if let Some(c) = v.str_field("c") {
        head.push_str(&format!(" class=\"{c}\""));
    }
    head.push('>');
    let number = v.get("n").and_then(|n| match n {
        Value::Num(s) => s.parse::<u64>().ok(),
        _ => None,
    });
    let kids: &[Value] = match v.get("k") {
        Some(Value::Arr(a)) => a,
        _ => &[],
    };
    let more = v.get("m").is_some();
    let label = RichText::new(head).monospace().size(12.0).color(Color32::from_rgb(0x9a, 0xd0, 0xff));
    if kids.is_empty() && !more {
        if ui.add(egui::Label::new(label).sense(egui::Sense::click())).clicked() {
            *pick = number;
        }
        return;
    }
    let id = ui.make_persistent_id(("dom", number.unwrap_or(0), depth));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth < 2)
        .show_header(ui, |ui| {
            if ui.add(egui::Label::new(label).sense(egui::Sense::click())).clicked() {
                *pick = number;
            }
        })
        .body(|ui| {
            for k in kids {
                node(ui, k, depth + 1, pick);
            }
            if more {
                ui.label(RichText::new("… глубже — выберите узел").small().weak());
            }
        });
}
