@echo off
rem Стенд VirtualBox: собрать свежий ISO, вставить в FreeOS-bench, загрузить
rem без окна и дождаться конца загрузки (строка drvd в журнале).
rem   tools\vbox\boot.cmd
rem Дальше: tools\vbox\type.cmd "команда", снимок - VBoxManage controlvm
rem FreeOS-bench screenshotpng <файл>, журнал - build\vbox\serial.log.
setlocal
set VB="C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
set VM=FreeOS-bench
set ROOT=%~dp0..\..
for %%i in ("%ROOT%") do set ROOT=%%~fi
set DIR=%ROOT%\build\vbox
%VB% controlvm %VM% poweroff >nul 2>&1
powershell -c "Start-Sleep 3"
pushd "%ROOT%"
cargo xtask iso --arch x86_64 > "%DIR%\iso.log" 2>&1 || (popd & exit /b 1)
popd
for /f "delims=" %%i in ('powershell -c "(Get-ChildItem '%ROOT%\build\ISO\FreeOS_*_x86_64_*.iso' | Sort-Object LastWriteTime | Select-Object -Last 1).FullName"') do set ISO=%%i
%VB% storageattach %VM% --storagectl SATA --port 1 --device 0 --type dvddrive --medium "%ISO%" || exit /b 1
del /q "%DIR%\serial.log" 2>nul
%VB% startvm %VM% --type headless || exit /b 1
powershell -c "$t=0; while ($t -lt 180) { if ((Test-Path '%DIR%\serial.log') -and (Select-String -Path '%DIR%\serial.log' -Pattern 'drvd: exited' -Quiet)) { break }; Start-Sleep 2; $t+=2 }"
echo BOOTED %ISO%
