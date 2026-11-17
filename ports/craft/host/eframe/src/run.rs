//! Цикл приложения eframe на FreeOS: одно окно стола, egui рисует процессор
//! (`egui_software_backend`) прямо в поверхность окна. Порядок вызовов — как
//! у eframe 0.37 (`epi_integration.rs`): `raw_input_hook` → `run_ui`
//! { `logic`, `ui` } → команды окна → закрытие с `CancelClose`.
//!
//! Дочерние окна (viewport) egui встраивает в главное (`embed_viewports`,
//! по умолчанию включено) — у Craft это окна книг/документов (craft-window).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use egui::{ViewportCommand, ViewportId};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};

use crate::freeos::{InputState, Window, clipboard_set, cursor_of, screen};
use crate::storage::FileStorage;
use crate::{App, AppCreator, CreationContext, Error, Frame, IntegrationInfo, NativeOptions, Storage};

/// Каталог настроек (`~/.config`) должен быть записываемым. Как у OBSOS: на
/// телефоне корень — FAT32 только на чтение, а записываемый том в памяти ядро
/// монтирует в `/home`.
fn ensure_home() {
    let writable = |dir: &std::path::Path| std::fs::create_dir_all(dir.join(".config")).is_ok();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if !home.is_some_and(|h| writable(&h)) && writable(std::path::Path::new("/home")) {
        log::warn!("eframe: HOME is not writable, using /home");
        // SAFETY: до запуска потоков приложения (окно ещё не открыто);
        // обновления/сеть приложения стартуют уже из `AppCreator`.
        unsafe { std::env::set_var("HOME", "/home") };
    }
}

pub(crate) fn run(app_name: &str, options: NativeOptions, egui_ctx: Option<egui::Context>, creator: AppCreator<'_>) -> crate::Result {
    ensure_home();
    let vp = &options.viewport;
    let title = vp.title.clone().unwrap_or_else(|| app_name.to_owned());
    let size = vp.inner_size.unwrap_or(egui::vec2(1024.0, 720.0));
    let resizable = vp.resizable.unwrap_or(true);
    // Окно сразу на весь экран (за вычетом рамки и панели), если просят
    // больше экрана: приложениям Craft нужны 1440×900, а экран QEMU 1024×768.
    let window = Window::open(&title, size.x.max(64.0) as u32, size.y.max(48.0) as u32, resizable)
        .ok_or_else(|| Error::Window(String::from("freeos_window_open failed")))?;
    println!("eframe: {title}: window {}x{}", window.width.get(), window.height.get());

    let ctx = egui_ctx.unwrap_or_default();
    // Дочерние окна — внутри главного (у FreeOS одно окно на программу).
    ctx.set_embed_viewports(true);
    let wake = Arc::new(AtomicBool::new(true));
    {
        let wake = wake.clone();
        ctx.set_request_repaint_callback(move |_| wake.store(true, Ordering::Release));
    }

    let app_id = vp.app_id.clone().unwrap_or_else(|| app_name.to_owned());
    let storage: Option<Box<dyn Storage>> = match &options.persistence_path {
        Some(path) => Some(Box::new(FileStorage::from_ron_filepath(path.clone()))),
        None => FileStorage::from_app_id(&app_id).map(|s| Box::new(s) as Box<dyn Storage>),
    };
    #[cfg(feature = "persistence")]
    if let Some(memory) = storage.as_deref().and_then(|s| crate::get_value::<egui::Memory>(s, crate::STORAGE_EGUI_MEMORY_KEY)) {
        ctx.memory_mut(|m| *m = memory);
    }

    let mut app: Box<dyn App + '_> = {
        let cc = CreationContext {
            egui_ctx: ctx.clone(),
            integration_info: IntegrationInfo { cpu_usage: None },
            storage: storage.as_deref(),
            #[cfg(feature = "glow")]
            gl: None,
            wgpu_render_state: None,
        };
        creator(&cc).map_err(Error::AppCreation)?
    };
    let mut frame = Frame {
        info: IntegrationInfo { cpu_usage: None },
        storage,
        egui_ctx: Some(ctx.clone()),
        #[cfg(feature = "glow")]
        gl: None,
    };

    let order = if window.swap_rb { ColorFieldOrder::Rgba } else { ColorFieldOrder::Bgra };
    let mut renderer = EguiSoftwareRender::new(order);
    let mut input = InputState::default();
    let mut events: Vec<egui::Event> = Vec::new();
    let started = Instant::now();
    let mut due = Some(Instant::now());
    let mut close_asked = false;
    let mut last_save = Instant::now();
    let mut frames = 0u64;
    let (sw, sh) = screen();

    'run: loop {
        let mut got_input = false;
        while let Some(ev) = window.next_event() {
            got_input = true;
            let said = input.translate(&ev, &mut events);
            if said.close {
                close_asked = true;
            }
            if let Some((w, h)) = said.resize {
                if window.resize(w, h) {
                    println!("eframe: resized {w}x{h}");
                } else {
                    log::warn!("eframe: resize to {w}x{h} failed");
                }
            }
        }
        if got_input || close_asked || wake.swap(false, Ordering::AcqRel) {
            due = Some(Instant::now());
        }
        let Some(when) = due else {
            std::thread::sleep(Duration::from_millis(8));
            continue;
        };
        if Instant::now() < when {
            std::thread::sleep(Duration::from_millis(4).min(when - Instant::now()));
            continue;
        }

        // ── Кадр egui ─────────────────────────────────────────────────────
        let t0 = Instant::now();
        let (ww, wh) = (window.width.get(), window.height.get());
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(ww as f32, wh as f32));
        let mut raw = egui::RawInput {
            screen_rect: Some(rect),
            time: Some(started.elapsed().as_secs_f64()),
            events: std::mem::take(&mut events),
            focused: true,
            ..Default::default()
        };
        {
            let info = raw.viewports.entry(ViewportId::ROOT).or_default();
            info.native_pixels_per_point = Some(1.0);
            info.inner_rect = Some(rect);
            info.outer_rect = Some(rect);
            info.monitor_size = (sw > 0).then(|| egui::vec2(sw as f32, sh as f32));
            info.focused = Some(true);
            info.minimized = Some(false);
            info.maximized = Some(false);
            info.fullscreen = Some(false);
            info.title = Some(title.clone());
            if close_asked {
                info.events.push(egui::ViewportEvent::Close);
            }
        }
        app.raw_input_hook(&ctx, &mut raw);
        let output = ctx.run_ui(raw, |ui| {
            app.logic(ui.ctx(), &mut frame);
            app.ui(ui, &mut frame);
        });

        // Платформа: буфер обмена, ссылки, указатель.
        for command in &output.platform_output.commands {
            match command {
                egui::OutputCommand::CopyText(text) => clipboard_set(text),
                egui::OutputCommand::OpenUrl(url) => open_url(&url.url),
                _ => {}
            }
        }
        window.set_cursor(cursor_of(output.platform_output.cursor_icon));

        // Команды главного окна.
        let mut quit = false;
        let mut repaint_now = false;
        if let Some(vo) = output.viewport_output.get(&ViewportId::ROOT) {
            let canceled = vo.commands.contains(&ViewportCommand::CancelClose);
            if close_asked {
                close_asked = false;
                quit = !canceled;
            }
            for command in &vo.commands {
                match command {
                    ViewportCommand::Close => quit = true,
                    ViewportCommand::Title(t) => window.set_title(t),
                    ViewportCommand::InnerSize(size) => {
                        let _ = window.resize(size.x.max(64.0) as u32, size.y.max(48.0) as u32);
                    }
                    ViewportCommand::Maximized(true) | ViewportCommand::Fullscreen(true) if sw > 0 => {
                        let _ = window.resize(sw.saturating_sub(40), sh.saturating_sub(80));
                    }
                    ViewportCommand::RequestCopy => events.push(egui::Event::Copy),
                    ViewportCommand::RequestCut => events.push(egui::Event::Cut),
                    ViewportCommand::RequestPaste => {
                        if let Some(text) = crate::freeos::clipboard_get() {
                            events.push(egui::Event::Paste(text));
                        }
                    }
                    _ => {}
                }
            }
            repaint_now = !events.is_empty();
            let delay = vo.repaint_delay;
            due = if repaint_now {
                Some(Instant::now())
            } else if delay < Duration::from_secs(3600) {
                Some(Instant::now() + delay.max(Duration::from_millis(16)))
            } else {
                None
            };
        } else {
            due = None;
        }

        // Рисование: фон окна цветом `clear_color`, холст egui поверх.
        let (ww, wh) = (window.width.get() as usize, window.height.get() as usize);
        if ww > 0 && wh > 0 {
            let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            renderer.render_to_canvas(ww, wh, &primitives, &output.textures_delta, output.pixels_per_point);
            let clear = app.clear_color(&ctx.global_style().visuals);
            let pixels = window.pixels();
            pixels.fill(clear_pixel(clear, window.swap_rb));
            let mut buffer = BufferMutRef::new(pixels, ww, wh);
            renderer.blit_canvas_to_buffer(&mut buffer);
            window.commit();
            frames += 1;
            if frames <= 3 || frames % 500 == 0 {
                println!("eframe: frame {frames} ({} primitives, {:?})", primitives.len(), t0.elapsed());
            }
        }
        frame.info.cpu_usage = Some(t0.elapsed().as_secs_f32());

        // Сохранение раз в `auto_save_interval` (как у eframe).
        if last_save.elapsed() >= app.auto_save_interval() {
            last_save = Instant::now();
            save(app.as_mut(), &mut frame, &ctx);
        }
        if quit {
            break 'run;
        }
        let _ = repaint_now;
    }
    println!("eframe: {title}: closing");
    save(app.as_mut(), &mut frame, &ctx);
    #[cfg(feature = "glow")]
    app.on_exit(None);
    #[cfg(not(feature = "glow"))]
    app.on_exit();
    drop(app);
    window.close();
    Ok(())
}

fn save(app: &mut dyn App, frame: &mut Frame, ctx: &egui::Context) {
    let Some(storage) = frame.storage_mut() else { return };
    #[cfg(feature = "persistence")]
    if app.persist_egui_memory() {
        ctx.memory(|m| crate::set_value(storage, crate::STORAGE_EGUI_MEMORY_KEY, m));
    }
    let _ = ctx;
    app.save(storage);
    storage.flush();
}

/// Цвет фона (линейные доли 0..1, как у eframe — гамма) → точка окна.
fn clear_pixel(rgba: [f32; 4], swap_rb: bool) -> [u8; 4] {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    // Прозрачный фон окна у нас не бывает — альфа всегда 255.
    if swap_rb { [c(rgba[0]), c(rgba[1]), c(rgba[2]), 255] } else { [c(rgba[2]), c(rgba[1]), c(rgba[0]), 255] }
}

/// Ссылка — в OBSOS (родной браузер), если он установлен.
fn open_url(url: &str) {
    let safe: String = url.chars().filter(|c| !c.is_whitespace()).collect();
    const OBSOS: &str = "/opt/obsos/bin/obsos";
    if std::path::Path::new(OBSOS).exists() {
        if let Err(e) = crate::freeos::spawn(&format!("{OBSOS} {safe}")) {
            log::warn!("eframe: open {safe}: spawn failed ({e})");
        }
    } else {
        log::warn!("eframe: open {safe}: no browser installed");
    }
}
