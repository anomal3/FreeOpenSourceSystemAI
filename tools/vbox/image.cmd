@echo off
rem Стенд VirtualBox: загрузить образ флешки (cargo xtask image) как диск
rem SATA и пробросить SSH гостя на 127.0.0.1:2222 (фаза 68o).
rem   tools\vbox\image.cmd build\FreeOS_..._x86_64_release.img
rem Дальше: ssh -p 2222 -i <ключ> live@127.0.0.1 "kernel usb"
setlocal
set VB="C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
set VM=FreeOS-bench
set ROOT=%~dp0..\..
for %%i in ("%ROOT%") do set ROOT=%%~fi
set DIR=%ROOT%\build\vbox
if "%~1"=="" (echo usage: image.cmd ^<image.img^> & exit /b 1)
%VB% controlvm %VM% poweroff >nul 2>&1
powershell -c "Start-Sleep 3"
%VB% storageattach %VM% --storagectl SATA --port 0 --device 0 --type hdd --medium none >nul 2>&1
%VB% storageattach %VM% --storagectl SATA --port 1 --device 0 --type dvddrive --medium emptydrive >nul 2>&1
%VB% closemedium disk "%DIR%\image.vdi" --delete >nul 2>&1
%VB% convertfromraw "%~f1" "%DIR%\image.vdi" --format VDI || exit /b 1
%VB% storageattach %VM% --storagectl SATA --port 0 --device 0 --type hdd --medium "%DIR%\image.vdi" || exit /b 1
%VB% modifyvm %VM% --natpf1 delete ssh >nul 2>&1
%VB% modifyvm %VM% --natpf1 "ssh,tcp,127.0.0.1,2222,,22" || exit /b 1
del /q "%DIR%\serial.log" 2>nul
%VB% startvm %VM% --type headless || exit /b 1
powershell -c "$t=0; while ($t -lt 240) { if ((Test-Path '%DIR%\serial.log') -and (Select-String -Path '%DIR%\serial.log' -Pattern 'sshd: listening' -Quiet)) { break }; Start-Sleep 2; $t+=2 }"
echo BOOTED %~1
