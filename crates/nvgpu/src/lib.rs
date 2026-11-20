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
//! * [`uapi`] — структуры DRM/nouveau/NVIF, номера ioctl, разбор NVIF.
//! * [`arena`] — распределители диапазонов: видеопамять (арены из
//!   нескольких кусков) и адреса.
//! * [`gmmu`] — PDE/PTE GP100+, построитель таблиц над [`gmmu::PtMem`],
//!   обход, блок экземпляра, регистры сброса TLB.
//! * [`gpfifo`] — записи GPFIFO, заголовки методов, забор, константы
//!   классов C56F/C7B5, сборщик пушбуферов.
//! * [`syncobj`] — двоичные и временные синхрообъекты (учёт без ожидания).
//! * [`vaspace`] — учёт VA файла DRM: отображения, разреженные области,
//!   область ядра, разбор VM_BIND.
//! * [`softgpu`] — программная видеокарта для QEMU и тестов.

#![no_std]

extern crate alloc;

pub mod arena;
pub mod backend;
pub mod gmmu;
pub mod gpfifo;
pub mod lock;
pub mod softgpu;
pub mod syncobj;
pub mod uapi;
pub mod vaspace;

pub use softgpu::{HeapHost, SoftGpu, SoftHost};
pub use backend::{BoHandle, ChanHandle, Errno, GpuBackend, GpuInfo, ObjHandle, PciSlot, Place, Push, VmHandle};
