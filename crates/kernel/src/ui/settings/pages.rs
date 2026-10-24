//! Страницы «Параметров» — что на каждой написано (фаза 69b).
//!
//! Правило одно на все страницы: строка стоит здесь, только если за ней есть
//! то, что она называет. Переключатель, который ничего не переключает, и
//! сведение, которого система о себе не знает, хуже пустого места — поэтому
//! страницы короче, чем в макете, и это не недоделка, а честность.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::Fi;
use mini_ui::paint::{self, Tone, Weight};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect};

use crate::input::keymap::Layout as KeyLayout;
use crate::{arch, config, fs};

use super::kit::{self, Ctl, Pass, line_h, row};
use super::{Deed, Field, MODES, PanelPref, Programs, Section, SettingsView};

impl SettingsView {
    /// Тело страницы: от `y` (уже с учётом прокрутки) вниз.
    pub(super) fn page_body(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        match self.page {
            Section::Home => self.home(pass, x, y, w),
            Section::System => {
                let mut y = y;
                for section in [Section::Display, Section::Power, Section::Storage, Section::About] {
                    y = nav_row(pass, x, y, w, section);
                }
            }
            Section::Display => self.display(pass, x, y, w),
            Section::Power => power(pass, x, y, w),
            Section::Storage => self.storage(pass, x, y, w),
            Section::About => self.about(pass, x, y, w),
            Section::Devices => {
                let mut y = y;
                for section in [Section::Keyboard, Section::Usb, Section::Network] {
                    y = nav_row(pass, x, y, w, section);
                }
            }
            Section::Keyboard => keyboard(pass, x, y, w),
            Section::Usb => self.usb(pass, x, y, w),
            Section::Network => self.network(pass, x, y, w),
            Section::Personal => {
                let mut y = y;
                for section in [Section::Wallpaper, Section::Colors, Section::Dock] {
                    y = nav_row(pass, x, y, w, section);
                }
            }
            Section::Wallpaper => wallpaper(pass, x, y, w),
            Section::Colors => colors(pass, x, y, w),
            Section::Dock => dock(pass, x, y, w),
            Section::Explorer => explorer(pass, x, y, w),
            Section::Apps => self.apps(pass, x, y, w),
            Section::Packages => self.packages_page(pass, x, y, w),
            Section::Services => services(pass, x, y, w),
            Section::Accounts => accounts_page(pass, x, y, w),
            Section::Time => time(pass, x, y, w),
            Section::Privacy => privacy(pass, x, y, w),
            Section::Update => self.update(pass, x, y, w),
            Section::Recovery => self.recovery(pass, x, y, w),
        }
    }

    // ── Главная ──────────────────────────────────────────────────────────────

    fn home(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let gap = ctx.px(12);
        let wide = w >= ctx.px(600);
        let left_w = if wide { (w - gap) / 2 } else { w };
        let hero_h = ctx.px(176);

        // ── Карточка машины ─────────────────────────────────────────────────
        let card = Rect::new(x, y, left_w, hero_h);
        if kit::hero_card(pass, card) {
            let shot = Rect::new(card.x + ctx.px(16) as i32, card.y + ctx.px(36) as i32, ctx.px(150).min(left_w / 2), ctx.px(96));
            mini_desktop(pass, shot);
            let tx = shot.right() + ctx.px(16) as i32;
            let room = (card.right() - ctx.px(14) as i32 - tx).max(0) as u32;
            let mut ty = card.y + ctx.px(26) as i32;
            pass.text(Role::Title, tx, ty, room, "FossAi", p.ink);
            ty += line_h(ctx, Role::Title) + ctx.px(4) as i32;
            let version = format!("{} · {}", crate::version_text(), arch::ARCH_NAME);
            pass.text(Role::Caption, tx, ty, room, &version, p.ink3);
            ty += line_h(ctx, Role::Caption) + ctx.px(2) as i32;
            pass.text(Role::Caption, tx, ty, room, "UEFI", p.ink3);
            ty += line_h(ctx, Role::Caption) + ctx.px(2) as i32;
            let slot = match crate::slot::booted() {
                Some(slot) => format!("Активный слот {}", slot.name()),
                None => String::from("Живая система с носителя"),
            };
            pass.text(Role::Caption, tx, ty, room, &slot, p.ink3);
            ty += line_h(ctx, Role::Caption) + ctx.px(10) as i32;
            let link = "Сведения о системе";
            let lw = ctx.face(Role::Body).width(link).min(room);
            let rect = Rect::new(tx, ty, lw, line_h(ctx, Role::Body) as u32);
            let focused = pass.deed(rect, Deed::Open(Section::About));
            let ink = p.acc_ink;
            pass.text(Role::Body, tx, ty, room, link, ink);
            if focused {
                pass.on(|s| draw::hline(s, rect.x, rect.bottom(), rect.w, ink, 255));
            }
        }

        // ── Состояние и место ───────────────────────────────────────────────
        let (rx, ry, rw) = if wide { (x + (left_w + gap) as i32, y, w - left_w - gap) } else { (x, card.bottom() + gap as i32, w) };
        let half = (hero_h - gap) / 2;
        let state = Rect::new(rx, ry, rw, half);
        let installed = crate::slot::booted().is_some();
        let state_focused = pass.deed(state, Deed::Open(Section::Update));
        if kit::hero_card(pass, state) {
            if state_focused {
                pass.on(|s| draw::rounded_stroke(s, state, ctx.px(10), p.acc, 255));
            }
            let d = ctx.px(32);
            let cx = state.x + ctx.px(16) as i32 + d as i32 / 2;
            let cy = state.y + state.h as i32 / 2;
            kit::ok_badge(pass, cx, cy, d, Tone::Ok);
            let tx = cx + d as i32 / 2 + ctx.px(14) as i32;
            let room = (state.right() - ctx.px(14) as i32 - tx).max(0) as u32;
            let both = line_h(ctx, Role::Body) + ctx.px(3) as i32 + line_h(ctx, Role::Caption);
            let ty = state.y + (state.h as i32 - both) / 2;
            let (title, sub) = if installed {
                ("Система установлена", "Обновления ставятся во второй слот")
            } else {
                ("Работает с носителя", "На диски машины ничего не пишется")
            };
            pass.text(Role::Body, tx, ty, room, title, p.ink);
            pass.text(Role::Caption, tx, ty + line_h(ctx, Role::Body) + ctx.px(3) as i32, room, sub, p.ink4);
        }
        let space = Rect::new(rx, state.bottom() + gap as i32, rw, hero_h - half - gap);
        if kit::hero_card(pass, space) {
            let pad = ctx.px(16);
            let inner_w = rw.saturating_sub(pad * 2);
            let by = space.y + (space.h as i32 - (line_h(ctx, Role::Body) + ctx.px(14) as i32)) / 2;
            match self.cache.space.first() {
                Some((_, total, free)) => {
                    kit::usage_bar(pass, space.x + pad as i32, by, inner_w, "Хранилище", total - free, *total);
                }
                None => {
                    let (free, total) = super::super::memory_mib();
                    kit::usage_bar(pass, space.x + pad as i32, by, inner_w, "Память", (total - free) << 20, total << 20);
                }
            }
        }
        let mut y = if wide { card.bottom() } else { space.bottom() };

        // ── Рекомендуемые ───────────────────────────────────────────────────
        y = pass.group(x, y + ctx.px(8) as i32, w, "Рекомендуемые");
        let slot_desc = match crate::slot::booted() {
            Some(slot) => format!("Слот {} активен · откат на прежний слот включён", slot.name()),
            None => String::from("Обновлять можно установленную систему"),
        };
        y = row(pass, x, y, w, Some(Fi::Refresh), "Обновление системы", &slot_desc, Ctl::Nav, Some(Deed::Open(Section::Update)));
        let mounts = format!("Точек монтирования: {}", fs::mounted().len());
        y = row(pass, x, y, w, Some(Fi::Drive), "Хранилище", &mounts, Ctl::Nav, Some(Deed::Open(Section::Storage)));
        let packages = match &self.cache.packages {
            Some(Ok(list)) if list.is_empty() => String::from("Пока не установлено ни одного"),
            Some(Ok(list)) => format!("Установлено: {}", list.len()),
            _ => String::from("Реестр пакетов не прочитан"),
        };
        y = row(pass, x, y, w, Some(Fi::Package), "Пакеты", &packages, Ctl::Nav, Some(Deed::Open(Section::Packages)));

        // ── Быстрые настройки ───────────────────────────────────────────────
        y = pass.group(x, y + ctx.px(8) as i32, w, "Быстрые настройки");
        let dark = theme::is_dark();
        y = row(pass, x, y, w, Some(Fi::Moon), "Тёмная тема", "", Ctl::Toggle(dark), Some(Deed::Theme(!dark)));
        let uptime = super::super::panel::shows(PanelPref::Uptime);
        y = row(pass, x, y, w, Some(Fi::Clock), "Показывать аптайм на панели", "Время работы рядом с часами", Ctl::Toggle(uptime), Some(Deed::Panel(PanelPref::Uptime, !uptime)));
        row(pass, x, y, w, Some(Fi::Palette), "Акцентный цвет", "", Ctl::NavValue(theme::accent().title()), Some(Deed::Open(Section::Colors)));
    }

    // ── Система ──────────────────────────────────────────────────────────────

    fn display(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let ctx = pass.ctx;
        let switchable = crate::display::can_switch();
        let mut y = pass.group(x, y, w, "Разрешение экрана");
        if switchable {
            for (width, height) in crate::display::modes(&MODES) {
                let label = format!("{width} × {height}");
                let current = (width, height) == self.screen;
                y = row(pass, x, y, w, None, &label, "", Ctl::Radio(current), Some(Deed::Mode(width, height)));
            }
            y = pass.note(x, y + ctx.px(4) as i32, w, "Меняется сразу и запоминается на следующий запуск.");
        } else {
            let label = format!("{} × {}", self.screen.0, self.screen.1);
            y = row(pass, x, y, w, Some(Fi::Monitor), "Разрешение", "Задаёт прошивка (UEFI GOP) до загрузки ядра", Ctl::Mono(&label), None);
            y = pass.note(x, y + ctx.px(4) as i32, w, "Сменить режим может только драйвер видеокарты — для этой его в системе пока нет.");
        }
        y = pass.group(x, y, w, "Масштаб и макет");
        let scale = if ctx.scale >= 2 { "200 %" } else { "100 %" };
        y = row(pass, x, y, w, Some(Fi::Sliders), "Масштаб", "Выбирается по ширине экрана", Ctl::Value(scale), None);
        row(pass, x, y, w, Some(Fi::Monitor), "Ориентация", "", Ctl::Value("Альбомная"), None);
    }

    fn storage(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let ctx = pass.ctx;
        let mut y = y;
        for (point, total, free) in &self.cache.space {
            let card = Rect::new(x, y, w, ctx.px(64));
            if kit::hero_card(pass, card) {
                let label = format!("Том {point}");
                kit::usage_bar(pass, card.x + ctx.px(16) as i32, card.y + ctx.px(16) as i32, w.saturating_sub(ctx.px(32)), &label, total - free, *total);
            }
            y = card.bottom() + ctx.px(8) as i32;
        }
        let disks: Vec<Vec<&str>> = self
            .cache
            .devices
            .lines()
            .filter(|line| line.starts_with("disk\t"))
            .map(|line| line.split('\t').collect())
            .collect();
        if !disks.is_empty() {
            y = pass.group(x, y, w, "Диски");
            for fields in &disks {
                let name = fields.get(1).copied().unwrap_or("?");
                let size = fields.get(2).copied().unwrap_or("");
                let mib = size.trim_end_matches(" MiB").parse::<u64>().ok();
                let size = mib.map_or_else(|| size.to_string(), |mib| kit::size_text(mib << 20));
                y = row(pass, x, y, w, Some(Fi::Drive), name, "Носитель найден при загрузке", Ctl::Mono(&size), None);
            }
        }
        y = pass.group(x, y, w, "Смонтировано");
        for (point, kind) in fs::mounted() {
            let point = if point.is_empty() { "/" } else { point };
            y = row(pass, x, y, w, Some(Fi::FolderOpen), point, "", Ctl::Mono(kind), None);
        }
        y = pass.group(x, y, w, "Проверка тома");
        y = row(pass, x, y, w, Some(Fi::ShieldCheck), "Проверить сейчас", "Только чтение; исправления — при следующей загрузке", Ctl::Button("Проверить", Weight::Normal), Some(Deed::CheckDisks));
        pass.note(x, y + ctx.px(4) as i32, w, "Чинить том под работающей системой нельзя: его держит редактор.");
    }

    fn about(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let mut y = pass.group(x, y, w, "Устройство");
        let cpus = crate::smp::online();
        let cpu = format!("{} · ядер: {cpus}", arch::ARCH_NAME);
        y = row(pass, x, y, w, Some(Fi::Cpu), "Процессор", "", Ctl::Value(&cpu), None);
        let (free, total) = super::super::memory_mib();
        let memory = format!("{} МиБ · свободно {} МиБ", total, free);
        y = row(pass, x, y, w, Some(Fi::Memory), "Память", "", Ctl::Value(&memory), None);
        let screen = format!("{} × {}", self.screen.0, self.screen.1);
        y = row(pass, x, y, w, Some(Fi::Monitor), "Экран", "", Ctl::Mono(&screen), None);
        let pci = self.cache.devices.lines().filter(|line| line.starts_with("pci\t")).count();
        let missing = self.cache.devices.lines().filter(|line| line.starts_with("pci\t") && line.split('\t').nth(5) == Some("none")).count();
        let bus = format!("{pci} · без драйвера {missing}");
        y = row(pass, x, y, w, Some(Fi::Layers), "Устройства PCI", "Подробно — в «Диспетчере устройств»", Ctl::Value(&bus), None);
        y = row(pass, x, y, w, Some(Fi::Info), "Загрузка", "", Ctl::Value("UEFI"), None);

        y = pass.group(x, y, w, "Система");
        y = row(pass, x, y, w, Some(Fi::Info), "Версия", "", Ctl::Mono(&crate::version_text()), None);
        y = row(pass, x, y, w, Some(Fi::FileCode), "Ядро", "Написано с нуля на Rust, без std", Ctl::Value("FossAi"), None);
        let uptime = crate::time::uptime_ms() / 1000;
        let up = format!("{} ч {:02} мин", uptime / 3600, (uptime / 60) % 60);
        y = row(pass, x, y, w, Some(Fi::Clock), "Время работы", "", Ctl::Value(&up), None);
        let root = fs::mounted().into_iter().find(|(point, _)| point.is_empty()).map_or("—", |(_, kind)| kind);
        row(pass, x, y, w, Some(Fi::Drive), "Корень", "", Ctl::Mono(root), None);
    }

    // ── Устройства ───────────────────────────────────────────────────────────

    fn usb(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let mut y = pass.group(x, y, w, "Подключено");
        let mut any = false;
        for line in self.cache.devices.lines().filter(|line| line.starts_with("usb\t")) {
            let fields: Vec<&str> = line.split('\t').collect();
            let port = fields.get(1).copied().unwrap_or("");
            let id = fields.get(2).copied().unwrap_or("");
            let kind = fields.get(3).copied().unwrap_or("usb device");
            let driver = fields.get(4).copied().unwrap_or("");
            let title = usb_kind_ru(kind);
            let desc = format!("{driver} · {port} · {id}");
            y = row(pass, x, y, w, Some(Fi::Usb), &title, &desc, Ctl::Chip("РАБОТАЕТ", Tone::Ok), None);
            any = true;
        }
        if !any {
            y = pass.note(x, y, w, "Ни одного устройства USB не подключено.");
        }
        y = pass.group(x, y, w, "Контроллеры");
        for line in self.cache.devices.lines().filter(|line| line.starts_with("pci\t") && line.contains("usb controller")) {
            let fields: Vec<&str> = line.split('\t').collect();
            let address = fields.get(1).copied().unwrap_or("");
            let id = fields.get(2).copied().unwrap_or("");
            let class = fields.get(3).copied().unwrap_or("");
            let driver = fields.get(4).copied().unwrap_or("-");
            let state = fields.get(5).copied().unwrap_or("");
            let title = class.trim_start_matches("usb controller").trim().trim_matches(|c| c == '(' || c == ')').to_uppercase();
            let title = if title.is_empty() { String::from("USB") } else { title };
            let desc = format!("{address} · {id}");
            let (chip, tone) = match state {
                "active" => (driver, Tone::Ok),
                "idle" => ("НЕ ЗАНЯТ", Tone::Muted),
                _ => ("НЕТ ДРАЙВЕРА", Tone::Warn),
            };
            let chip = chip.to_uppercase();
            y = row(pass, x, y, w, Some(Fi::Server), &title, &desc, Ctl::Chip(&chip, tone), None);
        }
    }

    fn network(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let ctx = pass.ctx;
        let Some(status) = crate::net::status() else {
            let y = row(pass, x, y, w, Some(Fi::Globe), "Сетевая карта", "Драйверы: virtio-net, e1000, atl1c", Ctl::Value("не найдена"), None);
            pass.note(x, y + ctx.px(4) as i32, w, "Адрес задавать нечему: раздел оживёт вместе с драйвером для вашей карты.");
            return;
        };
        let mut y = pass.group(x, y, w, "Сейчас");
        let mac = format!("{}", crate::net::eth::Display(status.mac));
        y = row(pass, x, y, w, Some(Fi::Globe), "Аппаратный адрес", "", Ctl::Mono(&mac), None);
        let address = if status.address.is_unspecified() {
            String::from("пока нет")
        } else {
            format!("{}/{}", status.address, status.netmask.prefix().unwrap_or(0))
        };
        y = row(pass, x, y, w, Some(Fi::Info), "Адрес", "", Ctl::Mono(&address), None);
        let gateway = if status.gateway.is_unspecified() { String::from("нет") } else { format!("{}", status.gateway) };
        y = row(pass, x, y, w, Some(Fi::ArrowRight), "Шлюз", "", Ctl::Mono(&gateway), None);
        let dns = if status.dns.is_unspecified() { String::from("нет") } else { format!("{}", status.dns) };
        y = row(pass, x, y, w, Some(Fi::Search), "Сервер имён", "", Ctl::Mono(&dns), None);

        y = pass.group(x, y, w, "Откуда адрес");
        let is_static = matches!(
            crate::net::persist::load().map(|settings| settings.mode),
            Some(crate::net::persist::Mode::Static)
        );
        y = row(pass, x, y, w, None, "Получать автоматически (DHCP)", "", Ctl::Radio(!is_static), Some(Deed::NetDhcp));
        y = row(pass, x, y, w, None, "Постоянный адрес", "Выбор запоминается на разделе состояния", Ctl::Radio(is_static), Some(Deed::NetStatic));

        y = pass.group(x, y, w, "Постоянный адрес");
        for (field, label, note) in [
            (Field::Address, "Адрес и префикс", "Например 10.0.2.15/24. Цифры, точка и косая."),
            (Field::Gateway, "Шлюз", "Пусто — шлюза нет."),
        ] {
            y = self.field_row(pass, x, y, w, field, label, note);
        }
    }

    /// Строка с полем ввода справа.
    fn field_row(&self, pass: &mut Pass, x: i32, y: i32, w: u32, field: Field, label: &str, note: &str) -> i32 {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let h = ctx.px(60);
        let rect = Rect::new(x, y, w, h);
        let fw = ctx.px(220).min(w / 2);
        let slot = Rect::new(rect.right() - ctx.px(18) as i32 - fw as i32, rect.y + (h as i32 - ctx.px(32) as i32) / 2, fw, ctx.px(32));
        let focused = pass.deed_in(slot, rect, Deed::NetEdit(field));
        let next = y + (h + ctx.px(4)) as i32;
        if !kit::hero_card(pass, rect) {
            return next;
        }
        let tx = rect.x + ctx.px(18) as i32;
        let room = (slot.x - tx - ctx.px(12) as i32).max(0) as u32;
        let both = line_h(ctx, Role::Body) + ctx.px(3) as i32 + line_h(ctx, Role::Caption);
        let ty = rect.y + (h as i32 - both) / 2;
        pass.text(Role::Body, tx, ty, room, label, p.ink);
        pass.text(Role::Caption, tx, ty + line_h(ctx, Role::Body) + ctx.px(3) as i32, room, note, p.ink4);
        text_field(pass, slot, self.net.text(field), self.net.editing == Some(field), focused);
        next
    }

    // ── Приложения ───────────────────────────────────────────────────────────

    fn apps(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let count = match &self.cache.packages {
            Some(Ok(list)) => format!("Установлено: {}", list.len()),
            _ => String::from(Section::Packages.blurb()),
        };
        let mut y = row(pass, x, y, w, Some(Fi::Package), "Пакеты", &count, Ctl::Nav, Some(Deed::Open(Section::Packages)));
        y = nav_row(pass, x, y, w, Section::Services);
        y = pass.group(x, y, w, "Установка");
        row(pass, x, y, w, Some(Fi::Download), "Установить пакет из файла", ".fpk раскладывается в /opt/<имя>, запись — в /var/lib/pkg", Ctl::Button("Обзор…", Weight::Primary), Some(Deed::ChooseFile));
    }

    fn packages_page(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let ctx = pass.ctx;
        match &self.programs {
            Programs::List => {
                // Живьём, а не из запаса: `pkg` ставит и удаляет в фоне, и
                // список обязан показать результат при первой же перерисовке.
                let list = packages();
                let mut y = y;
                match list {
                    Ok(names) => {
                        y = pass.group(x, y, w, &format!("Установлено · {}", names.len()));
                        if names.is_empty() {
                            y = pass.note(x, y, w, "Пока ничего не установлено.");
                        }
                        for name in names {
                            let desc = format!("/opt/{name}");
                            y = row(pass, x, y, w, Some(Fi::Package), &name, &desc, Ctl::Button("Удалить", Weight::Normal), Some(Deed::AskRemove(name.clone())));
                        }
                    }
                    Err(err) => {
                        y = row(pass, x, y, w, Some(Fi::Warning), "Реестр пакетов", &err, Ctl::Chip("ОШИБКА", Tone::Bad), None);
                    }
                }
                y = pass.group(x, y, w, "Установка");
                y = row(pass, x, y, w, Some(Fi::Download), "Установить пакет из файла", "Файлы .fpk с носителя, из домашнего каталога и со стола", Ctl::Button("Обзор…", Weight::Primary), Some(Deed::ChooseFile));
                y = pass.group(x, y, w, "Источник");
                y = row(pass, x, y, w, Some(Fi::ShieldCheck), "Подпись пакетов", "Проверяет pkg install", Ctl::Value("Проверяется"), None);
                let _ = y;
            }
            Programs::Choose(found) => {
                let mut y = pass.group(x, y, w, "Найденные файлы");
                if found.is_empty() {
                    pass.note(x, y, w, "В /media, в домашнем каталоге и на столе ничего не нашлось.");
                    return;
                }
                for path in found {
                    y = row(pass, x, y, w, Some(Fi::Package), &short_name(path), path, Ctl::Button("Установить", Weight::Primary), Some(Deed::Install(path.clone())));
                }
                pass.note(x, y + ctx.px(4) as i32, w, "Пакет ставит pkg install; ответ — в терминале.");
            }
            Programs::Confirm(name) => {
                let mut y = pass.group(x, y, w, "Что исчезнет");
                let dir = format!("/opt/{name}");
                let entry = format!("/var/lib/pkg/{name}.pkg");
                y = row(pass, x, y, w, Some(Fi::Folder), "Каталог", "", Ctl::Mono(&dir), None);
                y = row(pass, x, y, w, Some(Fi::FileText), "Запись в реестре", "", Ctl::Mono(&entry), None);
                pass.note(x, y + ctx.px(4) as i32, w, "Вернуть удалённое можно только установкой заново.");
            }
        }
    }

    // ── Обновление и восстановление ──────────────────────────────────────────

    fn update(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let card = Rect::new(x, y, w, ctx.px(96));
        let button = "Проверить обновления";
        let bw = kit::button_width(ctx, button);
        let brect = Rect::new(card.right() - ctx.px(20) as i32 - bw as i32, card.y + (card.h as i32 - ctx.px(34) as i32) / 2, bw, ctx.px(34));
        let focused = pass.deed_in(brect, card, Deed::CheckUpdates);
        if kit::hero_card(pass, card) {
            let d = ctx.px(52);
            let cx = card.x + ctx.px(22) as i32 + d as i32 / 2;
            let cy = card.y + card.h as i32 / 2;
            kit::ok_badge(pass, cx, cy, d, Tone::Ok);
            let tx = cx + d as i32 / 2 + ctx.px(18) as i32;
            let room = (brect.x - tx - ctx.px(12) as i32).max(0) as u32;
            let both = line_h(ctx, Role::Title) + ctx.px(4) as i32 + line_h(ctx, Role::Caption);
            let ty = card.y + (card.h as i32 - both) / 2;
            let title = format!("FossAi {}", crate::version_text());
            pass.text(Role::Title, tx, ty, room, &title, p.ink);
            let sub = match crate::slot::booted() {
                Some(slot) => format!("Загружена со слота {}", slot.name()),
                None => String::from("Живая система: обновляется установленная"),
            };
            pass.text(Role::Caption, tx, ty + line_h(ctx, Role::Title) + ctx.px(4) as i32, room, &sub, p.ink4);
            pass.on(|s| paint::button(ctx.on(ctx.flat(p.card)), s, brect, Weight::Primary, button, focused));
        }
        let mut y = card.bottom() + ctx.px(6) as i32;
        y = pass.group(x, y, w, "Параметры");
        if let Some(state) = &self.cache.slots {
            let active = format!("{}", state.active.name());
            y = row(pass, x, y, w, Some(Fi::Layers), "Активный слот", "Новая система пишется в другой, этот продолжает работать", Ctl::Mono(&active), None);
            let tries = format!("После {} неудачных загрузок", slots::DEFAULT_TRIES);
            y = row(pass, x, y, w, Some(Fi::Undo), "Автоматический откат", "Загрузчик возвращается к прежнему слоту сам", Ctl::Value(&tries), None);
            let confirmed = if state.confirmed { "Подтверждена" } else { "Не подтверждена" };
            y = row(pass, x, y, w, Some(Fi::Check), "Эта загрузка", "Подтверждается после запуска рабочего стола", Ctl::Value(confirmed), None);
        } else {
            y = row(pass, x, y, w, Some(Fi::Layers), "Слоты A/B", "Есть у установленной системы", Ctl::Value("Нет"), None);
        }
        y = row(pass, x, y, w, Some(Fi::ShieldCheck), "Подпись обновлений", "Ставится только образ с доверенной подписью", Ctl::Value("Обязательна"), None);
        y = row(pass, x, y, w, Some(Fi::Folder), "Что сохраняется", "/etc, /home, /var, /opt живут на разделе состояния", Ctl::Mono("state"), None);
        y = pass.group(x, y, w, "Серверы");
        let servers = update_servers();
        if servers.is_empty() {
            y = pass.note(x, y, w, "В update.cfg не задан ни один сервер.");
        }
        for server in servers {
            y = row(pass, x, y, w, Some(Fi::Globe), &server, "", Ctl::None, None);
        }
        pass.note(x, y + ctx.px(4) as i32, w, "Проверяет sysupdate; отвечает он в терминале.");
    }

    fn recovery(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let ctx = pass.ctx;
        let mut y = pass.group(x, y, w, "Варианты восстановления");
        y = row(pass, x, y, w, Some(Fi::ShieldCheck), "Проверить файловые системы", "Порядок как в e2fsck: inode, каталоги, ссылки, битовые карты", Ctl::Button("Проверить", Weight::Normal), Some(Deed::CheckDisks));
        if let Some(state) = &self.cache.slots {
            let previous = format!("Слот {}", state.previous.name());
            y = row(pass, x, y, w, Some(Fi::Undo), "Предыдущая версия", "Загрузчик вернётся к ней сам, если новая не поднимется", Ctl::Value(&previous), None);
        }
        y = pass.group(x, y, w, "Меню загрузки");
        y = pass.note(x, y, w, "Нажмите любую клавишу, пока машина загружается, — откроется меню:");
        for (title, desc, key) in [
            ("Безопасный режим", "Без рабочего стола, корень только для чтения", "2"),
            ("Проверить том, затем загрузить", "Найденное чинится до монтирования", "3"),
            ("Безопасный режим с проверкой", "Оба пункта сразу", "4"),
            ("Режим экрана прошивки", "Если экран остаётся чёрным", "5"),
            ("Один процессор", "Если загрузка встаёт на «processors»", "6"),
        ] {
            y = row(pass, x, y, w, Some(Fi::History), title, desc, Ctl::Mono(key), None);
        }
        pass.note(x, y + ctx.px(4) as i32, w, "Консоль восстановления и fsck работают даже без корневого тома.");
    }
}

// ── Страницы без состояния окна ─────────────────────────────────────────────

fn nav_row(pass: &mut Pass, x: i32, y: i32, w: u32, section: Section) -> i32 {
    row(pass, x, y, w, Some(section.icon()), section.title(), section.blurb(), Ctl::Nav, Some(Deed::Open(section)))
}

fn power(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let ctx = pass.ctx;
    let limit = crate::power::screen_off_after();
    let mut y = pass.group(x, y, w, "Гасить экран без дела");
    for seconds in screen_off_options() {
        let label = if seconds == 0 { String::from("Никогда") } else { format!("Через {}", screen_off_text(seconds)) };
        y = row(pass, x, y, w, None, &label, "", Ctl::Radio(limit == seconds), Some(Deed::ScreenOff(seconds)));
    }
    y = pass.note(x, y + ctx.px(4) as i32, w, "Любая клавиша, мышь или касание зажигают экран обратно.");
    y = pass.group(x, y, w, "Действия");
    if crate::power::can_power_off() {
        y = row(pass, x, y, w, Some(Fi::Power), "Завершение работы", "Счётчики сбрасываются на диск, том помечается чистым", Ctl::Button("Выключить", Weight::Primary), Some(Deed::Shutdown));
    } else {
        y = row(pass, x, y, w, Some(Fi::Power), "Завершение работы", "Эта машина не умеет снять с себя питание", Ctl::Value("Недоступно"), None);
    }
    y = row(pass, x, y, w, Some(Fi::Restart), "Перезагрузка", "Загрузчик снова выберет активный слот", Ctl::Button("Перезагрузить", Weight::Normal), Some(Deed::Restart));
    pass.note(x, y + ctx.px(4) as i32, w, "Настоящего сна (ACPI S3) здесь нет — машина не засыпает, а перестаёт рисовать.");
}

fn keyboard(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let current = crate::input::keymap::layout();
    let mut y = pass.group(x, y, w, "Раскладка");
    for layout in [KeyLayout::Us, KeyLayout::Ru] {
        let desc = if layout == KeyLayout::Us { "QWERTY" } else { "ЙЦУКЕН" };
        y = row(pass, x, y, w, None, layout.name(), desc, Ctl::Radio(current == layout), Some(Deed::Layout(layout)));
    }
    y = pass.group(x, y, w, "Горячие клавиши");
    for (title, desc, keys) in [
        ("Переключить раскладку", "", "Alt+Shift"),
        ("Меню FossAi", "", "F1"),
        ("Закрыть окно", "", "Alt+F4"),
        ("Переключить окно", "", "Alt+Tab"),
        ("Сдвинуть окно", "", "Win+стрелки"),
        ("Прервать программу", "В терминале", "Ctrl+C"),
    ] {
        y = row(pass, x, y, w, Some(Fi::Keyboard), title, desc, Ctl::Mono(keys), None);
    }
}

fn wallpaper(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let gap = ctx.px(12);
    let count = theme::Wallpaper::ALL.len() as u32;
    let tile_w = (w.saturating_sub(gap * (count - 1)) / count).min(ctx.px(200));
    let tile_h = tile_w * 10 / 16;
    let label_h = line_h(ctx, Role::Caption) as u32 + ctx.px(6);
    for (index, wall) in theme::Wallpaper::ALL.into_iter().enumerate() {
        let rect = Rect::new(x + (index as u32 * (tile_w + gap)) as i32, y, tile_w, tile_h);
        let focused = pass.deed(rect, Deed::Wallpaper(wall));
        let current = theme::wallpaper() == wall;
        if pass.visible(Rect::new(rect.x, rect.y, rect.w, rect.h + label_h)) {
            let (top, bottom) = theme::wallpaper_colors(wall);
            color_swatch(pass, rect, top, Some(bottom), current, focused);
            let ink = if current { p.ink } else { p.ink3 };
            pass.text(Role::Caption, rect.x, rect.bottom() + ctx.px(6) as i32, rect.w, wall.title(), ink);
        }
    }
    let y = y + (tile_h + label_h) as i32 + ctx.px(8) as i32;
    pass.note(x, y, w, "Градиент и разметка точками; рисуются, а не хранятся.");
}

fn colors(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let ctx = pass.ctx;
    let mut y = pass.group(x, y, w, "Тема");
    let swatch_w = ctx.px(110);
    let swatch_h = ctx.px(6) * 3 + ctx.px(34) + line_h(ctx, Role::MonoCaps) as u32;
    let gap = ctx.px(12);
    for (index, dark) in [true, false].into_iter().enumerate() {
        let rect = Rect::new(x + (index as u32 * (swatch_w + gap)) as i32, y, swatch_w, swatch_h);
        let focused = pass.deed(rect, Deed::Theme(dark));
        if pass.visible(rect) {
            swatch(pass, rect, dark, theme::is_dark() == dark, focused);
        }
    }
    y += (swatch_h + ctx.px(6)) as i32;

    y = pass.group(x, y, w, "Акцентный цвет");
    let dot = ctx.px(40);
    let dot_gap = ctx.px(12);
    y += ctx.px(4) as i32;
    for (index, accent) in theme::Accent::ALL.into_iter().enumerate() {
        let rect = Rect::new(x + ctx.px(4) as i32 + (index as u32 * (dot + dot_gap)) as i32, y, dot, dot);
        let focused = pass.deed(rect, Deed::Accent(accent));
        if pass.visible(Rect::new(rect.x - 4, rect.y - 4, rect.w + 8, rect.h + 8)) {
            color_swatch(pass, rect, theme::accent_color(accent), None, theme::accent() == accent, focused);
        }
    }
    y += (dot + ctx.px(10)) as i32;
    y = pass.note(x, y, w, &format!("Сейчас: {}. Фокус, выделение и главное действие.", theme::accent().title()));

    y = pass.group(x, y, w, "Заголовок окна");
    for (index, height) in theme::TITLE_HEIGHTS.into_iter().enumerate() {
        let label = match index {
            0 => "Компактный",
            1 => "Обычный",
            _ => "Крупный",
        };
        let size = format!("{height} точек");
        y = row(pass, x, y, w, None, label, &size, Ctl::Radio(theme::title_h() == height), Some(Deed::TitleBar(height)));
    }
}

fn dock(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let mut y = pass.group(x, y, w, "Часы");
    let uptime = super::super::panel::shows(PanelPref::Uptime);
    y = row(pass, x, y, w, Some(Fi::Clock), "Показывать аптайм", "Время и время работы отвечают на разные вопросы", Ctl::Toggle(uptime), Some(Deed::Panel(PanelPref::Uptime, !uptime)));
    let date = super::super::panel::shows(PanelPref::Date);
    y = row(pass, x, y, w, Some(Fi::Calendar), "Показывать дату", "", Ctl::Toggle(date), Some(Deed::Panel(PanelPref::Date, !date)));
    // Как «Объединять кнопки панели задач» в Windows 11: значок на программу
    // (окна стопкой) или кнопка с подписью на каждое окно.
    y = pass.group(x, y, w, "Кнопки окон");
    let labels = super::super::panel::shows(PanelPref::Labels);
    y = row(pass, x, y, w, Some(Fi::Dock), "Только значки", "Окна одной программы — в одной кнопке, как в Windows 11", Ctl::Radio(!labels), Some(Deed::Panel(PanelPref::Labels, false)));
    y = row(pass, x, y, w, Some(Fi::Dock), "Значки и подписи", "Кнопка с названием на каждое окно", Ctl::Radio(labels), Some(Deed::Panel(PanelPref::Labels, true)));
    row(pass, x, y, w, Some(Fi::Keyboard), "Раскладка в трее", "Щелчок переключает, как Alt+Shift", Ctl::Value("Показывается"), None);
}

fn explorer(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let mut y = row(pass, x, y, w, Some(Fi::FolderOpen), "Файловый менеджер", "Программа /bin/files", Ctl::Button("Открыть", Weight::Primary), Some(Deed::Launch("/bin/files")));
    y = pass.group(x, y, w, "Папки");
    let home = super::super::context::home_dir();
    y = row(pass, x, y, w, Some(Fi::Home), "Домашняя папка", "", Ctl::Mono(&home), None);
    let desktop = super::super::context::desktop_dir();
    y = row(pass, x, y, w, Some(Fi::Monitor), "Рабочий стол", "Файлы отсюда — значки на столе", Ctl::Mono(&desktop), None);
    row(pass, x, y, w, Some(Fi::Usb), "Носители USB", "Только чтение: FAT32 и ext2", Ctl::Mono("/usb"), None);
}

fn services(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let ctx = pass.ctx;
    let mut y = pass.group(x, y, w, "/etc/services");
    let text = config::read("services", 8 * 1024)
        .map(|(bytes, _)| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default();
    let mut any = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let (Some(name), Some(exec)) = (fields.next(), fields.next()) else { continue };
        let uid = fields.next().map_or_else(|| String::from("uid супервизора"), |uid| format!("uid {uid}"));
        let desc = format!("{exec} · {uid}");
        y = row(pass, x, y, w, Some(Fi::Server), name, &desc, Ctl::None, None);
        any = true;
    }
    if !any {
        y = pass.note(x, y, w, "Файл служб пуст или не найден.");
    }
    y = pass.group(x, y, w, "Супервизор");
    y = row(pass, x, y, w, Some(Fi::Restart), "Перезапуск после сбоя", "", Ctl::Value("Сразу"), None);
    y = row(pass, x, y, w, Some(Fi::X), "Остановка", "", Ctl::Value("После нескольких сбоев подряд"), None);
    pass.note(x, y + ctx.px(4) as i32, w, "Что работает сейчас — в «Диспетчере задач», вкладка «Службы».");
}

fn accounts_page(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let card = Rect::new(x, y, w, ctx.px(92));
    let name = session_name();
    let cred = crate::user::session::credentials();
    if kit::hero_card(pass, card) {
        let d = ctx.px(60);
        let avatar = Rect::new(card.x + ctx.px(18) as i32, card.y + (card.h - d) as i32 / 2, d, d);
        self::avatar(pass, avatar, &name);
        let tx = avatar.right() + ctx.px(18) as i32;
        let room = (card.right() - tx - ctx.px(14) as i32).max(0) as u32;
        let both = line_h(ctx, Role::Title) + ctx.px(4) as i32 + line_h(ctx, Role::Caption);
        let ty = card.y + (card.h as i32 - both) / 2;
        pass.text(Role::Title, tx, ty, room, &name, p.ink);
        let sub = format!("uid {} · gid {} · {}", cred.uid, cred.gid, if cred.uid == 0 { "администратор" } else { "локальная учётная запись" });
        pass.text(Role::Caption, tx, ty + line_h(ctx, Role::Title) + ctx.px(4) as i32, room, &sub, p.ink4);
    }
    let mut y = card.bottom() + ctx.px(6) as i32;
    y = pass.group(x, y, w, "Пользователи этой машины");
    let rows = accounts();
    if rows.is_empty() {
        y = pass.note(x, y, w, "Учётных записей нет: система загружена с носителя.");
    }
    for (user, uid, gid) in rows {
        let desc = format!("uid {uid} · gid {gid}");
        let ctl = if user == name {
            Ctl::Chip("ВЫ", Tone::Accent)
        } else if uid == 0 {
            Ctl::Value("Системная")
        } else {
            Ctl::None
        };
        y = row(pass, x, y, w, Some(Fi::User), &user, &desc, ctl, None);
    }
    pass.note(x, y + ctx.px(4) as i32, w, "Учётные записи заводит установщик: хешировать пароль умеет только он.");
}

fn time(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let ctx = pass.ctx;
    let mut y = pass.group(x, y, w, "Дата и время");
    let now = crate::time::now_local().map_or_else(|| String::from("часов нет"), |t| format!("{t}"));
    y = row(pass, x, y, w, Some(Fi::Clock), "Текущее время", "", Ctl::Mono(&now), None);
    y = row(pass, x, y, w, Some(Fi::Calendar), "Источник времени", "Прошивка при загрузке и монотонный счётчик", Ctl::Value("UEFI GetTime"), None);
    y = row(pass, x, y, w, Some(Fi::Clock), "Установить время вручную", "Перевести часы прошивки эта система не умеет", Ctl::Value("Недоступно"), None);
    y = pass.group(x, y, w, "Часовой пояс");
    let current = crate::time::offset_minutes();
    for minutes in crate::time::TIMEZONES {
        let label = format!("UTC{}", crate::time::zone_text(minutes).trim_start_matches("UTC"));
        y = row(pass, x, y, w, None, &label, "", Ctl::Radio(minutes == current), Some(Deed::Timezone(minutes)));
    }
    y = pass.note(x, y + ctx.px(4) as i32, w, "Хранится в /etc/system.cfg; внутри системы всё — в UTC.");
    y = pass.group(x, y, w, "Язык и регион");
    y = row(pass, x, y, w, Some(Fi::Globe), "Язык интерфейса", "", Ctl::Value("Русский"), None);
    row(pass, x, y, w, Some(Fi::Keyboard), "Раскладка", "", Ctl::NavValue(crate::input::keymap::layout().name()), Some(Deed::Open(Section::Keyboard)));
}

fn privacy(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let mut y = pass.group(x, y, w, "Данные");
    y = row(pass, x, y, w, Some(Fi::EyeOff), "Телеметрия", "В системе нет кода, отправляющего данные о вас", Ctl::Value("Отсутствует"), None);
    y = row(pass, x, y, w, Some(Fi::Globe), "Обращения в сеть без вашей команды", "dhcp берёт адрес, drvd ищет драйверы для неизвестных устройств", Ctl::Value("dhcp, drvd"), None);
    y = pass.group(x, y, w, "Права доступа");
    y = row(pass, x, y, w, Some(Fi::Lock), "Проверка прав на системных вызовах", "mode, uid и gid на диске решают, что может прочитать программа", Ctl::Value("Включена"), None);
    y = row(pass, x, y, w, Some(Fi::ShieldCheck), "Проверка каждого каталога пути", "Нельзя прочитать файл 0644 внутри каталога 0700", Ctl::Value("Включена"), None);
    y = row(pass, x, y, w, Some(Fi::Shield), "W^X для программ", "Страница не бывает одновременно записываемой и исполняемой", Ctl::Value("Всегда"), None);
    row(pass, x, y, w, Some(Fi::Lock), "Вход по SSH", "Только по ключу; ключ живой системы лежит на носителе", Ctl::Value("По ключу"), None);
}

// ── Кирпичи ──────────────────────────────────────────────────────────────────

/// Круглая плитка с первой буквой имени.
pub(super) fn avatar(pass: &mut Pass, rect: Rect, name: &str) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let letter: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    pass.on(|s| draw::rounded_gradient(s, rect, rect.w / 2, p.acc, p.acc2, 255));
    let role = if rect.w >= ctx.px(44) { Role::Heading } else { Role::Title };
    let lw = ctx.face(role).width(&letter) as i32;
    pass.text(role, rect.x + (rect.w as i32 - lw) / 2, paint::baseline(ctx, role, rect), rect.w, &letter, Color::rgb(0xFF, 0xFF, 0xFF));
}

/// Кто вошёл. Пустое имя — сеанс без файла учётных записей (живой
/// носитель): это root, и «Пуск» называет его так же.
pub(super) fn session_name() -> String {
    crate::user::session::with_name(|name| if name.is_empty() { String::from("root") } else { name.to_string() })
}

/// Миниатюра стола: обои, окно, панель.
fn mini_desktop(pass: &mut Pass, picture: Rect) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let wall = theme::wall_average(p);
    let window = p.win.over(wall);
    pass.on(|s| {
        let r = ctx.px(6);
        draw::rounded_gradient(s, picture, r, p.wall_top, p.wall_bottom, 255);
        draw::rounded_stroke(s, picture, r, p.line3.color, p.line3.alpha);
        let frame = Rect::new(picture.x + picture.w as i32 / 7, picture.y + picture.h as i32 / 6, picture.w * 2 / 5, picture.h * 2 / 5);
        draw::rounded(s, frame, ctx.px(3), window, 255);
        draw::rounded_stroke(s, frame, ctx.px(3), p.line3.color, p.line3.alpha);
        draw::hline(s, frame.x, frame.y + ctx.px(6) as i32, frame.w, p.tbline.color, p.tbline.alpha);
        let bar = Rect::new(picture.x + ctx.px(4) as i32, picture.bottom() - ctx.px(12) as i32, picture.w.saturating_sub(ctx.px(8)), ctx.px(8));
        draw::rounded(s, bar, ctx.px(3), p.glass.over(wall), 255);
        let dot = Rect::new(bar.x + ctx.px(3) as i32, bar.y + ctx.px(2) as i32, ctx.px(10), ctx.px(4));
        draw::rounded(s, dot, ctx.px(2), p.acc, 255);
    });
}

/// Поле ввода с кареткой.
fn text_field(pass: &mut Pass, rect: Rect, text: &str, active: bool, focused: bool) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    pass.on(|s| paint::sunk(ctx, s, rect, ctx.px(theme::R_TAB)));
    if active || focused {
        let ink = if active { p.acc } else { p.ink4 };
        pass.on(|s| draw::rounded_stroke(s, rect, ctx.px(theme::R_TAB), ink, 255));
    }
    let baseline = paint::baseline(ctx, Role::Mono, rect);
    let x = rect.x + ctx.px(10) as i32;
    let room = rect.w.saturating_sub(ctx.px(24));
    let width = ctx.face(Role::Mono).width(text).min(room);
    pass.text(Role::Mono, x, baseline, room, text, p.ink);
    if active {
        let caret = Rect::new(x + width as i32 + ctx.px(1) as i32, rect.y + ctx.px(6) as i32, ctx.px(2), rect.h.saturating_sub(ctx.px(12)));
        pass.on(|s| s.fill(caret, p.acc));
    }
}

/// Образец темы. Цвета заданы числами: образец показывает и ту тему, что
/// сейчас **не** включена.
fn swatch(pass: &mut Pass, rect: Rect, dark: bool, current: bool, focused: bool) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let radius = ctx.px(theme::R_ROW);
    let pad = ctx.px(6);
    let caps_h = line_h(ctx, Role::MonoCaps);
    pass.on(|s| {
        paint::card(ctx, s, rect);
        let inner = Rect::new(rect.x + pad as i32, rect.y + pad as i32, rect.w.saturating_sub(pad * 2), ctx.px(34));
        let (fill, edge, bar) = if dark {
            (Color::rgb(0x1B, 0x1E, 0x24), Color::rgb(0x2E, 0x33, 0x3C), Color::rgb(0x2A, 0x2E, 0x36))
        } else {
            (Color::rgb(0xFF, 0xFF, 0xFF), Color::rgb(0xC7, 0xD1, 0xE0), Color::rgb(0xE6, 0xEA, 0xF0))
        };
        draw::rounded(s, inner, ctx.px(theme::R_TAB), fill, 255);
        draw::rounded_stroke(s, inner, ctx.px(theme::R_TAB), edge, 255);
        let strip = Rect::new(inner.x + ctx.px(6) as i32, inner.y + ctx.px(8) as i32, inner.w / 3, ctx.px(18));
        draw::rounded(s, strip, ctx.px(3), bar, 255);
        let label = if dark { "ТЁМНАЯ" } else { "СВЕТЛАЯ" };
        let caption = Rect::new(rect.x, inner.bottom() + pad as i32, rect.w, caps_h as u32);
        paint::text_centered(ctx, s, Role::MonoCaps, caption, label, p.ink2);
        if current {
            draw::rounded_stroke(s, rect, radius, p.accedge, 255);
            let ring = Rect::new(rect.x - 1, rect.y - 1, rect.w + 2, rect.h + 2);
            draw::rounded_stroke(s, ring, radius + 1, p.acctint.color, p.acctint.alpha);
        }
        if focused {
            draw::rounded_stroke(s, rect, radius, p.acc, 255);
        }
    });
}

/// Образец цвета: заливка или градиент, кольцо у выбранного, рамка у фокуса.
fn color_swatch(pass: &mut Pass, rect: Rect, top: Color, bottom: Option<Color>, current: bool, focused: bool) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let radius = if bottom.is_none() { rect.w / 2 } else { ctx.px(theme::R_ROW) };
    pass.on(|s| {
        match bottom {
            Some(bottom) => draw::rounded_gradient(s, rect, radius, top, bottom, 255),
            None => draw::rounded(s, rect, radius, top, 255),
        }
        draw::rounded_stroke(s, rect, radius, p.line3.color, p.line3.alpha);
        if current {
            let ring = Rect::new(rect.x - 2, rect.y - 2, rect.w + 4, rect.h + 4);
            draw::rounded_stroke(s, ring, radius + 2, p.ink, 255);
        }
        if focused {
            let ring = Rect::new(rect.x - 4, rect.y - 4, rect.w + 8, rect.h + 8);
            draw::rounded_stroke(s, ring, radius + 4, p.acc, 255);
        }
    });
}

/// Вид устройства USB по-русски.
fn usb_kind_ru(kind: &str) -> String {
    let lower = kind.to_lowercase();
    let word = if lower.contains("keyboard") {
        "Клавиатура USB"
    } else if lower.contains("mouse") {
        "Мышь USB"
    } else if lower.contains("tablet") || lower.contains("pointer") {
        "Планшет USB"
    } else if lower.contains("storage") || lower.contains("mass") {
        "Накопитель USB"
    } else if lower.contains("hub") {
        "Концентратор USB"
    } else if lower.contains("net") || lower.contains("rndis") || lower.contains("ecm") {
        "Сетевой адаптер USB"
    } else {
        return format!("USB: {kind}");
    };
    String::from(word)
}

// ── Что окно рассказывает о машине ───────────────────────────────────────────

/// Сроки гашения экрана — вместе с тем, что стоит сейчас (его могла задать
/// команда `power` любым числом секунд).
pub(super) fn screen_off_options() -> Vec<u32> {
    let mut out: Vec<u32> = alloc::vec![60, 300, 600, 1800];
    let now = crate::power::screen_off_after();
    if now != 0 && !out.contains(&now) {
        out.push(now);
        out.sort_unstable();
    }
    out.push(0);
    out
}

/// Срок гашения словами.
pub(super) fn screen_off_text(seconds: u32) -> String {
    match seconds {
        0 => String::from("никогда"),
        s if s < 60 => format!("{s} с"),
        s if s % 60 == 0 => minutes_text(s / 60),
        s => format!("{} мин {} с", s / 60, s % 60),
    }
}

/// «1 минута», «5 минут», «22 минуты».
fn minutes_text(count: u32) -> String {
    let word = match (count % 10, count % 100) {
        (1, 11) => "минут",
        (1, _) => "минуту",
        (2..=4, 12..=14) => "минут",
        (2..=4, _) => "минуты",
        _ => "минут",
    };
    format!("{count} {word}")
}

/// Учётные записи из `/etc/passwd`: имя, uid, gid. Хеш пароля не читается.
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
        let (Some(name), Some(uid), Some(gid)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(uid), Ok(gid)) = (uid.parse::<u32>(), gid.parse::<u32>()) else {
            continue;
        };
        if !name.is_empty() {
            out.push((name.to_string(), uid, gid));
        }
    }
    out
}

/// Имена установленных пакетов из реестра `/var/lib/pkg` — того же места,
/// куда пишет `pkg`.
pub(super) fn packages() -> Result<Vec<String>, String> {
    const REGISTRY: &str = "/var/lib/pkg";
    let listing = fs::list(REGISTRY).ok_or_else(|| "файловой системы нет".to_string())?;
    let entries = match listing {
        Ok(entries) => entries,
        // Каталога реестра нет — значит, ничего ещё не ставили (живая система,
        // свежая установка). Это пустой список, а не отказ.
        Err(crate::vfs::VfsError::NotFound) => return Ok(Vec::new()),
        Err(err) => return Err(format!("{err:?}")),
    };
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.name.ends_with(".pkg"))
        .map(|entry| entry.name.trim_end_matches(".pkg").to_string())
        .collect();
    names.sort();
    Ok(names)
}

/// Запустить программу и сказать, что она **начала** работу: ждать её
/// внутри разбора события ввода нельзя.
pub(super) fn run(line: &str, what: &str) -> super::Report {
    match crate::user::spawn(line, crate::user::session::credentials()) {
        Ok(id) => {
            crate::kprintln!("  settings    : started '{line}' as {id}");
            super::Report::ok(format!("{what} запущен как {id}; ответ — в терминале"))
        }
        Err(err) => {
            crate::kprintln!("  settings    : cannot start '{line}': {err}");
            super::Report::bad(format!("{what} не запустился: {err}"))
        }
    }
}

/// Сколько файлов показывается в списке установки.
const MAX_FILES: usize = 8;

/// Файлы пакетов в носителе, домашнем каталоге и на столе.
pub(super) fn available() -> Vec<String> {
    const MEDIA: &str = "/media";
    let home = super::super::context::home_dir();
    let desktop = super::super::context::desktop_dir();
    let mut found = Vec::new();
    for place in [MEDIA, home.as_str(), desktop.as_str()] {
        let Some(Ok(entries)) = fs::list(place) else {
            continue;
        };
        let mut names: Vec<String> =
            entries.into_iter().filter(|entry| entry.name.ends_with(".fpk")).map(|entry| entry.name).collect();
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
fn is_package(path: &str) -> bool {
    let Some(Ok((bytes, _))) = fs::read(path, fpk::HEADER_SIZE) else {
        return false;
    };
    matches!(fpk::Header::parse(&bytes), Ok(header) if header.kind == fpk::Kind::Package)
}

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
