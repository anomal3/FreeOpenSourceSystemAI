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
#   powershell -NoProfile -ExecutionPolicy Bypass -File ports\servo\snapshot-obsos.ps1 [-Src F:\ObsosBrowser] [-Rev <ветка|коммит>]
#
# Другая ветка (28.11): -Src на её рабочее дерево (git worktree), например
# `-Src F:\ObsosBrowser-wt\freeos-webrtc` (звонки obsos-media), — или
# `-Rev feat/freeos-webrtc`: тогда берётся ЗАКОММИЧЕННОЕ дерево этой ветки
# (git archive), без чьих-либо незакоммиченных правок, и рабочее дерево
# -Src не читается вовсе. Без -Rev — как прежде, рабочее дерево -Src.
param([string]$Src = 'F:\ObsosBrowser', [string]$Rev = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$dst = Join-Path $root 'build\obsos-src'
$from = $Src
if ($Rev) {
    $head = (& git -C $Src rev-parse --verify "$Rev^{commit}").Trim()
    if ($LASTEXITCODE -ne 0 -or -not $head) { throw "нет ревизии $Rev в $Src" }
    # git archive в файл (через конвейер PowerShell 5 двоичные данные портятся).
    $from = Join-Path $root 'build\obsos-src-export'
    $tar = "$from.tar"
    if (Test-Path $from) { Remove-Item -Recurse -Force $from }
    New-Item -ItemType Directory -Force $from | Out-Null
    & git -C $Src archive --format=tar -o $tar $head
    if ($LASTEXITCODE -ne 0) { throw "git archive: код $LASTEXITCODE" }
    & tar -xf $tar -C $from
    if ($LASTEXITCODE -ne 0) { throw "tar: код $LASTEXITCODE" }
    Remove-Item -Force $tar
}
if (-not (Test-Path (Join-Path $from 'obsos-fossai\Cargo.toml'))) { throw "нет OBSOS в $from" }
New-Item -ItemType Directory -Force $dst | Out-Null
# vendor/ снимка сохраняется между снимками (xtask обновляет его сам по штампам).
& robocopy $from $dst /MIR /NFL /NDL /NJH /NJS /NP /R:2 /W:2 `
    /XD (Join-Path $from '.git') (Join-Path $from 'target') (Join-Path $from 'target-dist') (Join-Path $from 'dist') `
        (Join-Path $from 'vendor') (Join-Path $from 'xtask\target') (Join-Path $dst 'vendor') (Join-Path $from 'docs\screenshots') `
    /XF '.obsos-source' | Out-Host
if ($LASTEXITCODE -ge 8) { throw "robocopy: код $LASTEXITCODE" }
if ($Rev) {
    Remove-Item -Recurse -Force $from
    $status = @()
    $diff = ''
    $source = "$Src @ $Rev (git anomal3/OBSOS, committed tree)"
} else {
    $head = (& git -C $Src rev-parse HEAD).Trim()
    $status = & git -C $Src status --porcelain
    $diff = (& git -C $Src diff HEAD --binary) -join "`n"
    $source = "$Src (git anomal3/OBSOS)"
}
$sha = [Security.Cryptography.SHA256]::Create()
$hash = -join ($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($diff)) | ForEach-Object { $_.ToString('x2') })
$info = @(
    "source: $source"
    "head: $head"
    "dirty: $(@($status).Count) files (working tree over head)"
    "diff-sha256: $hash"
    "snapshot: $((Get-Date).ToString('yyyy-MM-dd HH:mm'))"
    'changes:'
) + ($status | ForEach-Object { "  $_" })
[IO.File]::WriteAllText((Join-Path $dst '.obsos-source'), ($info -join "`n") + "`n", (New-Object Text.UTF8Encoding $false))
Write-Host "OBSOS: $source ($head + $(@($status).Count) изменённых) -> $dst"
