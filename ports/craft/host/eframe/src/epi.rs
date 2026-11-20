//! Типы eframe 0.37, которыми пользуются приложения: `App`, `CreationContext`,
//! `Frame`, `NativeOptions`, `Storage`. Состав и сигнатуры — как в eframe
//! (crates/eframe/src/epi.rs форка storytold/egui), без winit/wgpu/glow:
//! окна и рисование здесь свои (см. `run.rs`).

use std::time::Duration;

type DynError = Box<dyn core::error::Error + Send + Sync>;

/// Как в eframe: создаёт приложение, когда окно и egui готовы.
pub type AppCreator<'app> = Box<dyn 'app + FnOnce(&CreationContext<'_>) -> Result<Box<dyn 'app + App>, DynError>>;

/// Что получает приложение при создании.
pub struct CreationContext<'s> {
    /// Контекст egui (стили, шрифты, загрузчики картинок — настраивать здесь).
    pub egui_ctx: egui::Context,
    pub integration_info: IntegrationInfo,
    /// Хранилище `~/.config/<приложение>/app.ron`, если оно доступно.
    pub storage: Option<&'s dyn Storage>,
    /// У eframe с `glow` — контекст OpenGL; у нас OpenGL нет.
    #[cfg(feature = "glow")]
    pub gl: Option<std::sync::Arc<crate::glow::Context>>,
    /// У eframe с wgpu — устройство wgpu; у нас его нет (всегда `None`).
    pub wgpu_render_state: Option<crate::egui_wgpu::RenderState>,
}

impl CreationContext<'_> {
    /// Окна winit на FreeOS нет (всегда `None`).
    pub fn winit_window(&self) -> Option<&std::sync::Arc<winit::window::Window>> {
        None
    }

    #[doc(hidden)]
    pub fn _new_kittest(egui_ctx: egui::Context) -> Self {
        Self {
            egui_ctx,
            integration_info: IntegrationInfo::mock(),
            storage: None,
            #[cfg(feature = "glow")]
            gl: None,
            wgpu_render_state: None,
        }
    }
}

/// Приложение eframe 0.37: `logic` — до интерфейса, `ui` — сам интерфейс
/// (корневой `Ui` во всё окно).
pub trait App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        _ = (ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut Frame);

    fn save(&mut self, _storage: &mut dyn Storage) {}

    #[cfg(feature = "glow")]
    fn on_exit(&mut self, _gl: Option<&crate::glow::Context>) {}

    #[cfg(not(feature = "glow"))]
    fn on_exit(&mut self) {}

    fn auto_save_interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::from_rgba_unmultiplied(12, 12, 12, 180).to_normalized_gamma_f32()
    }

    fn persist_egui_memory(&self) -> bool {
        true
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, _raw_input: &mut egui::RawInput) {}
}

/// Рендерер окна. У eframe — выбор между OpenGL и wgpu; здесь оба значения
/// означают одно и то же: рисует процессор.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Renderer {
    Glow,
    #[default]
    Wgpu,
}

impl core::fmt::Display for Renderer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Glow => "glow".fmt(f),
            Self::Wgpu => "wgpu".fmt(f),
        }
    }
}

impl core::str::FromStr for Renderer {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, String> {
        match name.to_lowercase().as_str() {
            "glow" => Ok(Self::Glow),
            "wgpu" => Ok(Self::Wgpu),
            _ => Err(format!("eframe renderer {name:?} is not available")),
        }
    }
}

/// Настройки окна. Используются `viewport` (заголовок, размеры),
/// `persistence_path`; остальное принимается и ничего не меняет.
pub struct NativeOptions {
    pub viewport: egui::ViewportBuilder,
    pub multisampling: u16,
    pub depth_buffer: u8,
    pub stencil_buffer: u8,
    pub renderer: Renderer,
    pub run_and_return: bool,
    pub centered: bool,
    pub persist_window: bool,
    pub system_font_fallback: bool,
    pub persistence_path: Option<std::path::PathBuf>,
    pub dithering: bool,
    pub clipboard_shortcuts: bool,
}

impl Clone for NativeOptions {
    fn clone(&self) -> Self {
        Self { viewport: self.viewport.clone(), persistence_path: self.persistence_path.clone(), ..*self }
    }
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            viewport: Default::default(),
            multisampling: 0,
            depth_buffer: 0,
            stencil_buffer: 0,
            renderer: Renderer::default(),
            run_and_return: true,
            centered: false,
            persist_window: true,
            system_font_fallback: true,
            persistence_path: None,
            dithering: true,
            clipboard_shortcuts: true,
        }
    }
}

/// Кадр: хранилище и сведения о платформе.
pub struct Frame {
    pub(crate) info: IntegrationInfo,
    pub(crate) storage: Option<Box<dyn Storage>>,
    pub(crate) egui_ctx: Option<egui::Context>,
    #[cfg(feature = "glow")]
    pub(crate) gl: Option<std::sync::Arc<crate::glow::Context>>,
}

impl Frame {
    #[doc(hidden)]
    pub fn _new_kittest() -> Self {
        Self {
            info: IntegrationInfo::mock(),
            storage: None,
            egui_ctx: None,
            #[cfg(feature = "glow")]
            gl: None,
        }
    }

    pub fn is_web(&self) -> bool {
        false
    }

    pub fn info(&self) -> &IntegrationInfo {
        &self.info
    }

    /// У eframe форка storytold — язык интерфейса egui (`Context::set_language`
    /// есть только в форке; MailCraft собирается с egui crates.io). Никто из
    /// приложений Craft его не зовёт — здесь ничего не делает.
    pub fn set_ui_language(&self, _lang: Option<&str>) {}

    pub fn set_text_input_language(&mut self, _lang: Option<&str>) {}

    pub fn storage(&self) -> Option<&dyn Storage> {
        self.storage.as_deref()
    }

    pub fn storage_mut(&mut self) -> Option<&mut (dyn Storage + 'static)> {
        self.storage.as_deref_mut()
    }

    pub fn set_traffic_lights_position(&self, _egui_ctx: &egui::Context, _title_bar_y: egui::Rangef, _left_margin: f32) {}

    /// Окна winit на FreeOS нет (всегда `None`).
    pub fn winit_window(&self) -> Option<&std::sync::Arc<winit::window::Window>> {
        None
    }

    /// У eframe с wgpu — устройство wgpu; у нас его нет.
    pub fn wgpu_render_state(&self) -> Option<&crate::egui_wgpu::RenderState> {
        None
    }

    pub fn request_activation_token(&self) {}

    pub fn take_activation_token(&mut self) -> Option<String> {
        None
    }

    #[cfg(feature = "glow")]
    pub fn gl(&self) -> Option<&std::sync::Arc<crate::glow::Context>> {
        self.gl.as_ref()
    }
}

/// Сведения о платформе.
#[derive(Clone, Debug)]
pub struct IntegrationInfo {
    /// Сколько секунд процессора занял прошлый кадр.
    pub cpu_usage: Option<f32>,
}

impl IntegrationInfo {
    fn mock() -> Self {
        Self { cpu_usage: None }
    }
}

/// Хранилище строк по ключам (как у eframe).
pub trait Storage {
    fn get_string(&self, key: &str) -> Option<String>;
    fn set_string(&mut self, key: &str, value: String);
    fn remove_string(&mut self, key: &str);
    fn flush(&mut self);
}

/// Значение из хранилища (RON, как у eframe).
pub fn get_value<T: serde::de::DeserializeOwned>(storage: &dyn Storage, key: &str) -> Option<T> {
    let value = storage.get_string(key)?;
    match ron::from_str(&value) {
        Ok(value) => Some(value),
        Err(err) => {
            log::debug!("Failed to decode RON: {err}");
            None
        }
    }
}

/// Записать значение в хранилище (RON, как у eframe).
pub fn set_value<T: serde::Serialize + ?Sized>(storage: &mut dyn Storage, key: &str, value: &T) {
    match ron::ser::to_string(value) {
        Ok(string) => storage.set_string(key, string),
        Err(err) => log::error!("eframe failed to encode data using ron: {err}"),
    }
}

/// Ключ памяти egui в хранилище (как у eframe).
pub const STORAGE_EGUI_MEMORY_KEY: &str = "egui";
/// Ключ положения окна (как у eframe; здесь не пишется).
pub const STORAGE_WINDOW_KEY: &str = "window";
/// Обычный ключ данных приложения (как у eframe).
pub const APP_KEY: &str = "app";

// Дескрипторы окна (raw-window-handle): у окон FreeOS их нет — как у eframe
// без окна, `NotSupported`. Нужны, чтобы `rfd::…::set_parent(frame)` собирался.
impl raw_window_handle::HasWindowHandle for Frame {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::NotSupported)
    }
}

impl raw_window_handle::HasDisplayHandle for Frame {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::NotSupported)
    }
}

impl raw_window_handle::HasWindowHandle for CreationContext<'_> {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::NotSupported)
    }
}

impl raw_window_handle::HasDisplayHandle for CreationContext<'_> {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Err(raw_window_handle::HandleError::NotSupported)
    }
}
