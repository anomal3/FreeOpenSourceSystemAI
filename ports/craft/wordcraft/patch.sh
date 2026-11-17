#!/bin/sh
# WordCraft под FreeOS: правки условий компиляции скриптом (переживают смену
# выпуска лучше, чем .patch). Подключается из build-craft.sh, cwd — исходник.
#
# FreeOS — unix, но не «настольный Linux»: X11/Wayland (winit, wl-clipboard-rs,
# x11rb), D-Bus (zbus, портал настроек), xdg-mime, zenity у нас нет. Ветки
# `all(unix, not(target_os = "macos"))` и `all(unix, not(any(macos, android,
# emscripten)))` — это они; FreeOS из них исключается и попадает в общие
# запасные ветки (их WordCraft держит для прочих ОС).
D=apps/wordcraft
find $D -name '*.rs' -o -name Cargo.toml | xargs sed -i \
    -e 's/all(unix, not(target_os = "macos"))/all(unix, not(any(target_os = "macos", target_os = "freeos")))/g' \
    -e 's/target_os = "android", target_os = "emscripten")/target_os = "android", target_os = "emscripten", target_os = "freeos")/g' \
    -e 's/not(any(target_os = "windows", unix))/not(any(target_os = "windows", all(unix, not(target_os = "freeos"))))/g'
echo "wordcraft/patch.sh: $(grep -rl 'target_os = "freeos"' $D | wc -l) файлов с веткой freeos"
