@echo off
rem Стенд VirtualBox: загрузить УСТАНОВЛЕННУЮ систему (фаза 68i).
rem Берётся диск, который установщик разметил на стенде QEMU
rem (build\target-x86_64-debug.img, сценарий install), переводится в VDI и
rem ставится в FreeOS-bench вместо пустого диска; ISO вынимается.
rem   tools\vbox\installed.cmd [gui] [release]   (gui - с окном; release - диск release-прогона)
setlocal
set VB="C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
set VM=FreeOS-bench
set ROOT=%~dp0..\..
for %%i in ("%ROOT%") do set ROOT=%%~fi
set DIR=%ROOT%\build\vbox
set PROFILE=debug
if /i "%2"=="release" set PROFILE=release
set RAW=%ROOT%\build\target-x86_64-%PROFILE%.img
if not exist "%RAW%" (echo no %RAW% - run the install scenario first & exit /b 1)
%VB% controlvm %VM% poweroff >nul 2>&1
powershell -c "Start-Sleep 3"
%VB% storageattach %VM% --storagectl SATA --port 0 --device 0 --type hdd --medium none >nul 2>&1
%VB% storageattach %VM% --storagectl SATA --port 1 --device 0 --type dvddrive --medium emptydrive >nul 2>&1
%VB% closemedium disk "%DIR%\installed.vdi" --delete >nul 2>&1
%VB% convertfromraw "%RAW%" "%DIR%\installed.vdi" --format VDI || exit /b 1
%VB% storageattach %VM% --storagectl SATA --port 0 --device 0 --type hdd --medium "%DIR%\installed.vdi" || exit /b 1
del /q "%DIR%\serial.log" 2>nul
set TYPE=headless
if /i "%1"=="gui" set TYPE=gui
%VB% startvm %VM% --type %TYPE% || exit /b 1
powershell -c "$t=0; while ($t -lt 240) { if ((Test-Path '%DIR%\serial.log') -and (Select-String -Path '%DIR%\serial.log' -Pattern 'drvd: exited' -Quiet)) { break }; Start-Sleep 2; $t+=2 }"
echo BOOTED installed disk
