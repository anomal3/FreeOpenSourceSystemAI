//! Окно выбора файла и окно сообщения на egui (отдельное окно стола FreeOS).

use std::path::{Path, PathBuf};

use crate::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel, Mode, cancel_of};

struct Entry {
    name: String,
    dir: bool,
    size: u64,
}

struct Browser {
    dialog: FileDialog,
    mode: Mode,
    dir: PathBuf,
    entries: Vec<Entry>,
    selected: Vec<String>,
    name: String,
    filter: usize,
    path_text: String,
    error: Option<String>,
    new_folder: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| PathBuf::from("/"))
}

impl Browser {
    fn new(dialog: FileDialog, mode: Mode) -> Self {
        let start = dialog.directory.clone().filter(|d| d.is_dir()).unwrap_or_else(home);
        let name = dialog.file_name.clone().unwrap_or_default();
        let mut b = Browser { dialog, mode, dir: PathBuf::new(), entries: Vec::new(), selected: Vec::new(), name, filter: 0, path_text: String::new(), error: None, new_folder: None };
        b.open(start);
        b
    }

    fn folders_only(&self) -> bool {
        matches!(self.mode, Mode::Folder | Mode::Folders)
    }

    fn open(&mut self, dir: PathBuf) {
        match std::fs::read_dir(&dir) {
            Ok(read) => {
                let mut entries: Vec<Entry> = read
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let meta = e.metadata().ok();
                        let dir = meta.as_ref().is_some_and(|m| m.is_dir());
                        Some(Entry { name, dir, size: meta.map_or(0, |m| m.len()) })
                    })
                    .collect();
                entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                self.entries = entries;
                self.path_text = dir.display().to_string();
                self.dir = dir;
                self.selected.clear();
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{}: {e}", dir.display())),
        }
    }

    fn visible(&self, e: &Entry) -> bool {
        if e.name.starts_with('.') {
            return false;
        }
        if e.dir {
            return true;
        }
        if self.folders_only() {
            return false;
        }
        let Some(filter) = self.dialog.filters.get(self.filter) else { return true };
        if filter.extensions.iter().any(|x| x == "*" || x.is_empty()) {
            return true;
        }
        let ext = Path::new(&e.name).extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        filter.extensions.iter().any(|x| *x == ext)
    }

    /// Готовый ответ по кнопке «Открыть»/«Сохранить».
    fn accept(&mut self) -> Option<Vec<PathBuf>> {
        match self.mode {
            Mode::Save => {
                let mut name = self.name.trim().to_string();
                if name.is_empty() {
                    self.error = Some(String::from("Введите имя файла"));
                    return None;
                }
                // Нет расширения — первое из фильтра (как у диалогов GTK/Windows).
                if Path::new(&name).extension().is_none()
                    && let Some(ext) = self.dialog.filters.get(self.filter).and_then(|f| f.extensions.first()).filter(|x| *x != "*")
                {
                    name = format!("{name}.{ext}");
                }
                Some(vec![self.dir.join(name)])
            }
            Mode::Folder | Mode::Folders => {
                if self.selected.is_empty() {
                    Some(vec![self.dir.clone()])
                } else {
                    Some(self.selected.iter().map(|n| self.dir.join(n)).collect())
                }
            }
            Mode::File | Mode::Files => {
                let files: Vec<PathBuf> = self.selected.iter().map(|n| self.dir.join(n)).filter(|p| p.is_file()).collect();
                if files.is_empty() {
                    self.error = Some(String::from("Выберите файл"));
                    None
                } else {
                    Some(files)
                }
            }
        }
    }

    fn ui(&mut self, root: &mut egui::Ui) -> Option<Option<Vec<PathBuf>>> {
        let mut answer = None;
        let mut go: Option<PathBuf> = None;
        egui::Panel::top("rfd-path").show(root, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("⬆").on_hover_text("Наверх").clicked()
                    && let Some(up) = self.dir.parent()
                {
                    go = Some(up.to_path_buf());
                }
                if ui.button("Дом").clicked() {
                    go = Some(home());
                }
                if ui.button("/").clicked() {
                    go = Some(PathBuf::from("/"));
                }
                let edit = ui.add(egui::TextEdit::singleline(&mut self.path_text).desired_width(f32::INFINITY));
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = Some(PathBuf::from(self.path_text.clone()));
                }
            });
            ui.add_space(4.0);
        });
        egui::Panel::bottom("rfd-buttons").show(root, |ui| {
            ui.add_space(6.0);
            if self.mode == Mode::Save {
                ui.horizontal(|ui| {
                    ui.label("Имя файла:");
                    let edit = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY));
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        answer = self.accept().map(Some);
                    }
                });
            }
            ui.horizontal(|ui| {
                if !self.dialog.filters.is_empty() && !self.folders_only() {
                    let names: Vec<String> = self.dialog.filters.iter().map(|f| format!("{} ({})", f.name, f.extensions.iter().map(|x| format!("*.{x}")).collect::<Vec<_>>().join(" "))).collect();
                    egui::ComboBox::from_id_salt("rfd-filter").width(260.0).selected_text(names.get(self.filter).cloned().unwrap_or_default()).show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut self.filter, i, n);
                        }
                    });
                }
                if self.dialog.can_create_directories && matches!(self.mode, Mode::Save | Mode::Folder | Mode::Folders) {
                    if let Some(name) = &mut self.new_folder {
                        ui.text_edit_singleline(name);
                        if ui.button("Создать").clicked() {
                            let path = self.dir.join(name.trim());
                            self.new_folder = None;
                            match std::fs::create_dir(&path) {
                                Ok(()) => go = Some(path),
                                Err(e) => self.error = Some(e.to_string()),
                            }
                        }
                    } else if ui.button("Новая папка").clicked() {
                        self.new_folder = Some(String::new());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Отмена").clicked() {
                        answer = Some(None);
                    }
                    let ok = match self.mode {
                        Mode::Save => "Сохранить",
                        Mode::Folder | Mode::Folders => "Выбрать папку",
                        _ => "Открыть",
                    };
                    if ui.button(ok).clicked() {
                        answer = self.accept().map(Some);
                    }
                    if let Some(e) = &self.error {
                        ui.colored_label(egui::Color32::from_rgb(230, 90, 80), e);
                    }
                });
            });
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(root, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                let multi = matches!(self.mode, Mode::Files | Mode::Folders);
                let mut chosen: Option<(String, bool, bool)> = None;
                for e in self.entries.iter().filter(|e| self.visible(e)) {
                    let picked = self.selected.iter().any(|s| *s == e.name);
                    let label = if e.dir { format!("📁 {}", e.name) } else { format!("📄 {}   {}", e.name, human(e.size)) };
                    let r = ui.selectable_label(picked, label);
                    if r.double_clicked() {
                        chosen = Some((e.name.clone(), e.dir, true));
                    } else if r.clicked() {
                        chosen = Some((e.name.clone(), e.dir, false));
                    }
                }
                if let Some((name, dir, double)) = chosen {
                    let ctrl = ui.input(|i| i.modifiers.ctrl);
                    if double && dir {
                        go = Some(self.dir.join(&name));
                    } else if double && !self.folders_only() {
                        self.selected = vec![name.clone()];
                        if self.mode == Mode::Save {
                            self.name = name;
                        }
                        answer = self.accept().map(Some);
                    } else {
                        if multi && ctrl {
                            if let Some(i) = self.selected.iter().position(|s| *s == name) {
                                self.selected.remove(i);
                            } else {
                                self.selected.push(name.clone());
                            }
                        } else {
                            self.selected = vec![name.clone()];
                        }
                        if self.mode == Mode::Save && !dir {
                            self.name = name;
                        }
                    }
                }
            });
        });
        if root.input(|i| i.key_pressed(egui::Key::Escape)) {
            answer = Some(None);
        }
        if let Some(dir) = go {
            self.open(dir);
        }
        answer
    }
}

fn human(size: u64) -> String {
    match size {
        s if s >= 1 << 30 => format!("{:.1} ГБ", s as f64 / (1u64 << 30) as f64),
        s if s >= 1 << 20 => format!("{:.1} МБ", s as f64 / (1u64 << 20) as f64),
        s if s >= 1 << 10 => format!("{:.0} КБ", s as f64 / 1024.0),
        s => format!("{s} Б"),
    }
}

pub(crate) fn pick(dialog: FileDialog, mode: Mode) -> Option<Vec<PathBuf>> {
    let title = dialog.title.clone().unwrap_or_else(|| {
        String::from(match mode {
            Mode::Save => "Сохранить как",
            Mode::Folder | Mode::Folders => "Выбор папки",
            _ => "Открыть файл",
        })
    });
    println!("rfd: {title} ({mode:?})");
    let mut browser = Browser::new(dialog, mode);
    let answer = eframe::freeos::run_modal(&title, 720, 520, |ui| browser.ui(ui)).flatten();
    println!("rfd: answer {answer:?}");
    answer
}

pub(crate) fn message(m: MessageDialog) -> MessageDialogResult {
    let title = if m.title.is_empty() { String::from("Сообщение") } else { m.title.clone() };
    let buttons: Vec<(String, MessageDialogResult)> = match &m.buttons {
        MessageButtons::Ok => vec![("OK".into(), MessageDialogResult::Ok)],
        MessageButtons::OkCancel => vec![("OK".into(), MessageDialogResult::Ok), ("Отмена".into(), MessageDialogResult::Cancel)],
        MessageButtons::YesNo => vec![("Да".into(), MessageDialogResult::Yes), ("Нет".into(), MessageDialogResult::No)],
        MessageButtons::YesNoCancel => vec![("Да".into(), MessageDialogResult::Yes), ("Нет".into(), MessageDialogResult::No), ("Отмена".into(), MessageDialogResult::Cancel)],
        MessageButtons::OkCustom(ok) => vec![(ok.clone(), MessageDialogResult::Custom(ok.clone()))],
        MessageButtons::OkCancelCustom(ok, cancel) => vec![(ok.clone(), MessageDialogResult::Custom(ok.clone())), (cancel.clone(), MessageDialogResult::Custom(cancel.clone()))],
        MessageButtons::YesNoCancelCustom(yes, no, cancel) => vec![
            (yes.clone(), MessageDialogResult::Custom(yes.clone())),
            (no.clone(), MessageDialogResult::Custom(no.clone())),
            (cancel.clone(), MessageDialogResult::Custom(cancel.clone())),
        ],
    };
    let icon = match m.level {
        MessageLevel::Info => "ℹ",
        MessageLevel::Warning => "⚠",
        MessageLevel::Error => "⛔",
    };
    let answer = eframe::freeos::run_modal(&title, 480, 200, |root| {
        let mut answer = None;
        egui::CentralPanel::default().show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading(icon);
                ui.label(&m.description);
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                for (label, result) in &buttons {
                    if ui.button(label).clicked() {
                        answer = Some(result.clone());
                    }
                }
            });
        });
        answer
    });
    answer.unwrap_or_else(|| cancel_of(&m.buttons))
}
