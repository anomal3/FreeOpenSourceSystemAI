#!/bin/sh
# MailCraft под FreeOS: ветки по ОС у него — явные списки (linux, freebsd…),
# FreeOS сама попадает в «прочие» (eframe::run_native, без трея и D-Bus).
# Окна winit и устройства wgpu наш eframe не даёт (`winit_window()` и
# `wgpu_render_state` — всегда None), код вокруг них собирается как есть.
:
