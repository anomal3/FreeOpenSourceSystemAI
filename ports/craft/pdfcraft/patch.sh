#!/bin/sh
# PdfCraft под FreeOS: правки скриптом (cwd — исходник выпуска).
#
# 1. Настройка wgpu (выбор адаптера, DX12, пределы текстур) — у FreeOS wgpu
#    нет, рисует процессор (eframe FossAi): функции и их вызов выключены.
# 2. Проверка «окно Wayland» через raw-window-handle — у FreeOS ни Wayland, ни
#    X11; ветка для прочих ОС отвечает «нет».
F=apps/pdfcraft/src/main.rs
for fn in configure_gpu dx12_presentation surface_texture_limits; do
    sed -i "s/^fn $fn(/#[cfg(not(target_os = \"freeos\"))]\nfn $fn(/" $F
done
sed -i 's/^    if renderer == eframe::Renderer::Wgpu {$/    #[cfg(not(target_os = "freeos"))]\n    if renderer == eframe::Renderer::Wgpu {/' $F
find apps/pdfcraft -name '*.rs' -o -name Cargo.toml | xargs sed -i \
    -e 's/all(unix, not(target_os = "macos"))/all(unix, not(any(target_os = "macos", target_os = "freeos")))/g'
echo "pdfcraft/patch.sh: $(grep -c 'target_os = "freeos"' $F) правок в main.rs"

# 3. thread-priority (уступить процессор потоку копирования) не знает FreeOS:
#    у нас приоритет потока не меняется — зависимость и вызов выключены.
O=crates/optimize
sed -i "s/^\[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies\]\$/[target.'cfg(not(any(target_arch = \"wasm32\", target_os = \"freeos\")))'.dependencies]/" $O/Cargo.toml
perl -0pi -e 's/    #\[cfg\(not\(target_arch = "wasm32"\)\)\]\n    let _ = thread_priority::set_current_thread_priority/    #[cfg(not(any(target_arch = "wasm32", target_os = "freeos")))]\n    let _ = thread_priority::set_current_thread_priority/' $O/src/pool.rs
grep -q 'target_os = "freeos"' $O/src/pool.rs || { echo "pdfcraft/patch.sh: pool.rs не поправился"; exit 1; }

# 4. LibreOffice при «Создать PDF» гасится всей группой процессов через rustix
#    (`process`, сигналы Linux) — у FreeOS ни LibreOffice, ни групп: kill().
C=crates/create
sed -i "s/^\[target.'cfg(unix)'.dependencies\]\$/[target.'cfg(all(unix, not(target_os = \"freeos\")))'.dependencies]/" $C/Cargo.toml
perl -0pi -e 's/        #\[cfg\(unix\)\]\n        \{\n            use rustix::process/        #[cfg(target_os = "freeos")]\n        let _ = child.kill();\n        #[cfg(all(unix, not(target_os = "freeos")))]\n        {\n            use rustix::process/' $C/src/office.rs
grep -q 'target_os = "freeos"' $C/src/office.rs || { echo "pdfcraft/patch.sh: office.rs не поправился"; exit 1; }
