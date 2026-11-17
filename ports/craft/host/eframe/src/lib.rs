//! eframe для FossAi (FreeOS): то же API, что у eframe 0.37 (то подмножество,
//! которым пользуются GridCraft, WordCraft, PdfCraft, MailCraft), поверх
//! родного окна стола FreeOS. Ни winit, ни wgpu/OpenGL: egui рисует
//! процессор (`egui_software_backend`, копия в ../egui-software-backend).
//!
//! Подставляется вместо настоящего eframe через `[patch.crates-io]` в
//! `--config` сборки (ports/craft/builder/build-craft.sh) — исходники
//! приложений не трогаются.
//!
//! На других ОС крейт собирается (чтобы проверять типы на ПК), но
//! `run_native` там возвращает ошибку.

pub use egui;
pub use egui::{emath, epaint};

mod epi;
pub use epi::*;

pub mod icon_data;
mod storage;
pub use storage::{FileStorage, storage_dir};

#[cfg(target_os = "freeos")]
#[doc(hidden)]
pub mod freeos;
#[cfg(target_os = "freeos")]
mod run;

/// У eframe с возможностью `glow` — крейт glow. Здесь только тип, который
/// стоит в сигнатуре `App::on_exit`; OpenGL у программ FreeOS нет.
#[cfg(feature = "glow")]
pub mod glow {
    /// Контекста OpenGL никогда нет (`None`).
    pub struct Context {
        _private: (),
    }
}

/// Типы wgpu, которые приложения называют рядом с `wgpu_render_state`
/// (тип адаптера в журнале, «рисует процессор»). Устройства wgpu у нас нет —
/// значения никогда не создаются.
pub mod wgpu {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum DeviceType {
        Other,
        IntegratedGpu,
        DiscreteGpu,
        VirtualGpu,
        Cpu,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub enum Backend {
        Noop,
        Vulkan,
        Metal,
        Dx12,
        Gl,
        BrowserWebGpu,
    }

    #[derive(Debug, Clone)]
    pub struct AdapterInfo {
        pub name: String,
        pub vendor: u32,
        pub device: u32,
        pub device_type: DeviceType,
        pub driver: String,
        pub driver_info: String,
        pub backend: Backend,
    }

    #[derive(Debug, Clone, Default)]
    pub struct Limits {
        pub max_texture_dimension_2d: u32,
    }

    #[derive(Debug)]
    pub struct Adapter {
        _private: (),
    }

    impl Adapter {
        pub fn get_info(&self) -> AdapterInfo {
            AdapterInfo { name: String::new(), vendor: 0, device: 0, device_type: DeviceType::Cpu, driver: String::new(), driver_info: String::new(), backend: Backend::Noop }
        }
    }

    #[derive(Debug)]
    pub struct Device {
        _private: (),
    }

    impl Device {
        pub fn limits(&self) -> Limits {
            Limits::default()
        }
    }
}

/// `egui_wgpu::RenderState` — только тип поля `wgpu_render_state`.
pub mod egui_wgpu {
    /// Ошибка wgpu (вариант `eframe::Error::Wgpu`; у нас не возникает).
    #[derive(Debug)]
    pub struct WgpuError {
        _private: (),
    }

    impl core::fmt::Display for WgpuError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("wgpu is not available on FreeOS")
        }
    }

    #[derive(Debug)]
    pub struct RenderState {
        pub adapter: std::sync::Arc<crate::wgpu::Adapter>,
        pub device: std::sync::Arc<crate::wgpu::Device>,
    }
}

/// Запустить приложение в окне FreeOS.
///
/// # Errors
/// Окно не открылось или `app_creator` вернул ошибку.
#[allow(clippy::needless_pass_by_value)]
pub fn run_native(app_name: &str, native_options: NativeOptions, app_creator: AppCreator<'_>) -> Result {
    run_native_ext(app_name, native_options, None, app_creator)
}

/// Как [`run_native`], но со своим `egui::Context`.
///
/// # Errors
/// Окно не открылось или `app_creator` вернул ошибку.
#[allow(clippy::needless_pass_by_value)]
pub fn run_native_ext(app_name: &str, native_options: NativeOptions, egui_ctx: Option<egui::Context>, app_creator: AppCreator<'_>) -> Result {
    #[cfg(target_os = "freeos")]
    {
        run::run(app_name, native_options, egui_ctx, app_creator)
    }
    #[cfg(not(target_os = "freeos"))]
    {
        let _ = (app_name, native_options, egui_ctx, app_creator);
        Err(Error::Window(String::from("this eframe host only runs on FreeOS")))
    }
}

/// Проблемы запуска (как `eframe::Error`, без вариантов winit/glutin/wgpu).
#[derive(Debug)]
pub enum Error {
    /// Ошибка в коде приложения при его создании.
    AppCreation(Box<dyn core::error::Error + Send + Sync>),
    /// Окно стола не открылось.
    Window(String),
    /// Как у eframe с wgpu; здесь не возникает (приложения сопоставляют его,
    /// чтобы перезапуститься на OpenGL).
    Wgpu(egui_wgpu::WgpuError),
}

impl core::error::Error for Error {}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AppCreation(err) => write!(f, "app creation error: {err}"),
            Self::Window(err) => write!(f, "window error: {err}"),
            Self::Wgpu(err) => write!(f, "WGPU error: {err}"),
        }
    }
}

/// Сокращение для `Result<T, eframe::Error>`.
pub type Result<T = (), E = Error> = core::result::Result<T, E>;
