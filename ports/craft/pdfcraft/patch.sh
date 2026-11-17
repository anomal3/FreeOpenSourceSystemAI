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
