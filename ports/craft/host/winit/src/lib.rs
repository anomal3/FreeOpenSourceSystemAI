//! Заглушка winit 0.30 для сборки приложений Craft под FreeOS (см. Cargo.toml).
//!
//! Окон winit на FreeOS не бывает: `eframe::Frame::winit_window()` нашего
//! eframe всегда `None`. Здесь только типы, которые встречаются в коде
//! приложений рядом с этим вызовом (мониторы, размеры), чтобы он собирался
//! без правок; значения их методов никогда не спрашиваются.

pub mod dpi {
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct PhysicalPosition<P> {
        pub x: P,
        pub y: P,
    }

    impl<P> PhysicalPosition<P> {
        pub const fn new(x: P, y: P) -> Self {
            Self { x, y }
        }
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct PhysicalSize<P> {
        pub width: P,
        pub height: P,
    }

    impl<P> PhysicalSize<P> {
        pub const fn new(width: P, height: P) -> Self {
            Self { width, height }
        }
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct LogicalSize<P> {
        pub width: P,
        pub height: P,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct LogicalPosition<P> {
        pub x: P,
        pub y: P,
    }
}

pub mod error {
    #[derive(Debug, Clone, Default)]
    pub struct NotSupportedError {
        _private: (),
    }

    impl std::fmt::Display for NotSupportedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("the requested operation is not supported by Winit")
        }
    }

    impl std::error::Error for NotSupportedError {}
}

pub mod monitor {
    use crate::dpi::{PhysicalPosition, PhysicalSize};

    /// Монитор (на FreeOS никогда не создаётся).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MonitorHandle {
        _private: (),
    }

    impl MonitorHandle {
        pub fn name(&self) -> Option<String> {
            None
        }
        pub fn position(&self) -> PhysicalPosition<i32> {
            PhysicalPosition::default()
        }
        pub fn size(&self) -> PhysicalSize<u32> {
            PhysicalSize::default()
        }
        pub fn scale_factor(&self) -> f64 {
            1.0
        }
        pub fn refresh_rate_millihertz(&self) -> Option<u32> {
            None
        }
    }
}

pub mod window {
    use crate::dpi::{PhysicalPosition, PhysicalSize};
    use crate::error::NotSupportedError;
    use crate::monitor::MonitorHandle;

    /// Окно winit (на FreeOS никогда не создаётся: окна даёт eframe FossAi).
    #[derive(Debug)]
    pub struct Window {
        _private: (),
    }

    impl Window {
        pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
            Err(NotSupportedError::default())
        }
        pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, NotSupportedError> {
            Err(NotSupportedError::default())
        }
        pub fn outer_size(&self) -> PhysicalSize<u32> {
            PhysicalSize::default()
        }
        pub fn inner_size(&self) -> PhysicalSize<u32> {
            PhysicalSize::default()
        }
        pub fn scale_factor(&self) -> f64 {
            1.0
        }
        pub fn available_monitors(&self) -> impl Iterator<Item = MonitorHandle> {
            std::iter::empty()
        }
        pub fn primary_monitor(&self) -> Option<MonitorHandle> {
            None
        }
        pub fn current_monitor(&self) -> Option<MonitorHandle> {
            None
        }
        pub fn request_redraw(&self) {}
        pub fn set_visible(&self, _visible: bool) {}
        pub fn focus_window(&self) {}
    }

    /// Значок окна.
    #[derive(Debug, Clone)]
    pub struct Icon {
        _private: (),
    }

    #[derive(Debug, Clone)]
    pub struct BadIcon;

    impl Icon {
        pub fn from_rgba(_rgba: Vec<u8>, _width: u32, _height: u32) -> Result<Self, BadIcon> {
            Ok(Icon { _private: () })
        }
    }
}
