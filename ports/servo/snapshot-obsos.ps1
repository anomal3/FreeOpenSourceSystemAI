# Снимок исходников OBSOS для сборки FossAi: build\obsos-src (внутри
# контейнера fossai-builder — /work/build/obsos-src, отдельный том не нужен).
#
# Источник с 26.11.2026 — проект F:\ObsosBrowser (git anomal3/OBSOS), причём
# РАБОЧЕЕ ДЕРЕВО со всеми незакоммиченными правками. Его репозиторий не
# трогается: копируются только исходники (без .git, target, dist, vendor —
# vendor/ пересоздаёт `cargo xtask vendor` в контейнере). Рядом пишется
# .obsos-source: коммит, список изменённых файлов и хеш diff — по нему
# build-servoprobe.sh печатает, из чего собран браузер.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File ports\servo\snapshot-obsos.ps1 [-Src F:\ObsosBrowser]
param([string]$Src = 'F:\ObsosBrowser')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$dst = Join-Path $root 'build\obsos-src'
if (-not (Test-Path (Join-Path $Src 'obsos-fossai\Cargo.toml'))) { throw "нет OBSOS в $Src" }
New-Item -ItemType Directory -Force $dst | Out-Null
# vendor/ снимка сохраняется между снимками (xtask обновляет его сам по штампам).
& robocopy $Src $dst /MIR /NFL /NDL /NJH /NJS /NP /R:2 /W:2 `
    /XD (Join-Path $Src '.git') (Join-Path $Src 'target') (Join-Path $Src 'target-dist') (Join-Path $Src 'dist') `
        (Join-Path $Src 'vendor') (Join-Path $Src 'xtask\target') (Join-Path $dst 'vendor') (Join-Path $Src 'docs\screenshots') `
    /XF '.obsos-source' | Out-Host
if ($LASTEXITCODE -ge 8) { throw "robocopy: код $LASTEXITCODE" }
$head = (& git -C $Src rev-parse HEAD).Trim()
$status = & git -C $Src status --porcelain
$diff = (& git -C $Src diff HEAD --binary) -join "`n"
$sha = [Security.Cryptography.SHA256]::Create()
$hash = -join ($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($diff)) | ForEach-Object { $_.ToString('x2') })
$info = @(
    "source: $Src (git anomal3/OBSOS)"
    "head: $head"
    "dirty: $(@($status).Count) files (working tree over head)"
    "diff-sha256: $hash"
    "snapshot: $((Get-Date).ToString('yyyy-MM-dd HH:mm'))"
    'changes:'
) + ($status | ForEach-Object { "  $_" })
[IO.File]::WriteAllText((Join-Path $dst '.obsos-source'), ($info -join "`n") + "`n", (New-Object Text.UTF8Encoding $false))
Write-Host "OBSOS: $Src ($head + $(@($status).Count) изменённых) -> $dst"
