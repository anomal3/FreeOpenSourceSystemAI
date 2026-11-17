# Приложения Craft для FossAi — нативно

GridCraft (таблицы), WordCraft (документы), PdfCraft (PDF), MailCraft (почта) —
программы проекта Craft (https://github.com/storytold, https://github.com/UCS-Care-Group),
лицензия MIT OR Apache-2.0. Авторы разрешили сборку для FossAi под исходными
именами (soft fork): с указанием авторства и ссылками на исходники.

Собираются **родными программами FreeOS** (`<арх>-unknown-freeos`, наш `std`),
без слоя Linux. Исходники приложений не копируются и не правятся в
репозитории: берётся выпуск (тег) с GitHub, на него накладываются правки отсюда.

## Что здесь

| Путь | Что |
|---|---|
| `versions.toml` | приложения: репозиторий, закреплённый тег, подпись в «Пуске», значок, права |
| `host/eframe` | eframe 0.37 для FreeOS: окно `freeos_window_*`, egui рисует процессор, хранилище `~/.config/<app>/app.ron`, буфер обмена, указатель, ссылки → OBSOS |
| `host/egui-software-backend` | программный рендер egui (копия 0.0.3, перенесённая на egui 0.37) |
| `host/rfd` | окна выбора файла/сообщений с API rfd 0.17 — своё окно на egui |
| `host/arboard` | буфер обмена с API arboard 3.7 (`SYS_CLIPBOARD`; HTML/картинки — внутри программы) |
| `host/winit` | заглушка winit 0.30 (только типы) |
| `<app>/*.patch`, `<app>/patch.sh` | правки FreeOS к выпуску (`git apply`; условия компиляции — sed) |
| `builder/build-craft.sh` | сборка в Docker-контейнере `fossai-builder` (как OBSOS) |
| `builder/patch-craft-crates.sh` | крейты экосистемы с веткой FreeOS (getrandom 0.3, aws-lc-sys, filetime…) |
| `craftupd/` | обновление в системе: подписанный индекс → `pkg install` |

## Сборка

```
cargo xtask craft                      # все четыре, теги из versions.toml
cargo xtask craft --app gridcraft      # одно
cargo xtask craft --latest             # последний выпуск с GitHub (не собрался — закреплённый)
cargo xtask craft --no-build           # только пакеты и индекс из уже собранного
```

Перед первой сборкой на ПК должен отработать xtask для std (`cargo xtask test
-s rust-std`: `build/rust-std/src`, `patches.toml`) и быть образ
`fossai-builder` (`docker build -t fossai-builder ports/servo/builder`).

Итог: `build/linux/<app>-<арх>` (программа), `build/craft/<арх>/<app>-<версия>.fpk`
(подписан ключом выпуска), `craft-index.json` + `.sig`. Пакеты сами попадают на
носитель установки (`/media`) и живую флешку (`\FREEOS\PKG\GRIDCRAF.FPK` …).

## Обновления

Ночная сборка `.github/workflows/craft.yml` выкладывает пакеты и подписанный
индекс в выпуск `craft-packages`. В системе: `craftupd` (проверить и обновить
установленные), `craftupd --check`, `craftupd --all`, `craftupd --from <папка>`.
