// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! NVK на FossAi: чистая логика эмуляции DRM nouveau поверх GSP-RM
//! (18.11.2026, «Видеокарта для игр»). Цель — неизменённый Mesa NVK
//! (`libvulkan_nouveau.so`) на RTX 3060; спецификация — `nvk-spec.md`,
//! кратко — `docs/NVK.md`.
//!
//! Крейт не знает о ядре: только `core` + `alloc`, всё проверяется
//! `cargo test -p nvgpu` на хосте.
//!
//! * [`backend`] — трейт [`GpuBackend`] (договор WP-A/WP-B/WP-C) и его типы.

#![no_std]

extern crate alloc;

pub mod backend;

pub use backend::{BoHandle, ChanHandle, Errno, GpuBackend, GpuInfo, ObjHandle, PciSlot, Place, Push, VmHandle};
