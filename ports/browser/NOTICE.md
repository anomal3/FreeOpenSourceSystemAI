# OBSOS (браузер FreeOS) — авторство и лицензии сторонних компонентов

Этот файл ставится вместе с пакетом `obsos` (`/opt/obsos/share/NOTICE.md`) и
показывается в «О программе» браузера и в «Параметры → О системе →
Сторонние компоненты». Код FreeOS — GPL-3.0-only; компоненты ниже остаются
под своими лицензиями. Файлы под MPL-2.0, взятые почти как есть, сохраняют
свой заголовок с лицензией (MPL действует пофайлово).

Исходники FreeOS вместе со всеми правками сторонних компонентов
(`ports/servo/mozjs/freeos.patch` и т. п.):
https://github.com/anomal3/FreeOpenSourceSystemAI

| Компонент | Авторы | Лицензия |
|---|---|---|
| Движок **Servo** | © The Servo Project Developers, https://servo.org | MPL-2.0 |
| Дизайн оболочки — **по мотивам Zervo** | © Goddv, https://github.com/Goddv/Zervo | MPL-2.0 |
| **SpiderMonkey** (через `mozjs`/`mozjs_sys`) | © Mozilla Foundation и участники | MPL-2.0 |
| **swgl / WebRender** | © Mozilla Foundation и участники | MPL-2.0 |
| **ICU** (Intl в SpiderMonkey) | © Unicode, Inc. и участники | Unicode License v3 (`Unicode-3.0`) |
| **ICU4X** (обёртки Intl в `mozjs_sys`) | © Unicode, Inc. и участники | Unicode-3.0 |
| **egui** | © Emil Ernerfeldt и участники | MIT или Apache-2.0 |
| **egui_software_backend** (с правкой FreeOS, `ports/servo/egui-software-backend`) | © DGriffin91 (github.com/DGriffin91) и участники | MIT или Apache-2.0 |
| **RustCrypto**: `argon2`, `chacha20poly1305`, `sha2` (хранилище паролей) | © RustCrypto Developers | MIT или Apache-2.0 |
| **zlib** (`libz-sys`) | © Jean-loup Gailly, Mark Adler | Zlib |
| **HarfBuzz** (`harfbuzz-sys`) | © HarfBuzz contributors | MIT («Old MIT») |
| **FreeType** (`freetype-sys`) | © The FreeType Project | FTL (BSD-подобная, с упоминанием) или GPL-2.0 — берём FTL |
| **libpng** (в FreeType) | © Contributing Authors and Group 42, Inc. | libpng License |
| **OTS** — OpenType Sanitizer (`fontsan`) | © The Chromium Authors | BSD-3-Clause |
| **LZ4** (в `fontsan`) | © Yann Collet | BSD-2-Clause |
| **AWS-LC** (`aws-lc-sys`, `aws-lc-rs`) | © Amazon.com, Inc.; © Google (BoringSSL); © OpenSSL Project | Apache-2.0 / ISC / OpenSSL |
| **rustls** | © rustls contributors | Apache-2.0 / ISC / MIT |
| **SQLite** (`libsqlite3-sys`) | авторы SQLite | общественное достояние |
| **Zstandard** (`zstd-sys`) | © Meta Platforms, Inc. | BSD-3-Clause |

Правки FreeOS в SpiderMonkey (ОС `freeos` в сборке, `getentropy()` для
случайности, видимость символов) лежат открыто в
`ports/servo/mozjs/freeos.patch` и распространяются на тех же условиях
MPL-2.0, что и исправленные файлы.

Текст MPL-2.0: https://mozilla.org/MPL/2.0/
Текст Unicode License v3: https://www.unicode.org/license.txt

Правка FreeOS в AWS-LC (случайность через `getentropy()`) делается при
сборке — `ports/servo/builder/patch-crates.sh`.

Список пополняется вместе с зависимостями движка: каждая новая библиотека
в пакете `servo` — строка здесь.
