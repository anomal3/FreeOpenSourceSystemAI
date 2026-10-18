@echo off
rem Стенд VirtualBox: вставить в FreeOS-bench флешку USB (фаза 68n).
rem Берётся образ флешки стенда QEMU (build\test\usbstick-x86_64-debug-stick.img,
rem сценарий usbstick: MBR и FAT32), переводится в VDI и ставится на
rem контроллер хранения USB - VirtualBox изображает накопитель Bulk-Only сам.
rem Машина должна быть выключена; дальше - tools\vbox\boot.cmd.
rem   tools\vbox\stick.cmd
setlocal
set VB="C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
set VM=FreeOS-bench
set ROOT=%~dp0..\..
for %%i in ("%ROOT%") do set ROOT=%%~fi
set DIR=%ROOT%\build\vbox
set RAW=%ROOT%\build\test\usbstick-x86_64-debug-stick.img
if not exist "%RAW%" (echo no %RAW% - run the usbstick scenario first & exit /b 1)
%VB% controlvm %VM% poweroff >nul 2>&1
powershell -c "Start-Sleep 3"
%VB% storageattach %VM% --storagectl USB --port 0 --device 0 --medium none >nul 2>&1
%VB% closemedium disk "%DIR%\stick.vdi" --delete >nul 2>&1
%VB% convertfromraw "%RAW%" "%DIR%\stick.vdi" --format VDI || exit /b 1
%VB% storagectl %VM% --name USB --add usb --controller USB >nul 2>&1
%VB% storageattach %VM% --storagectl USB --port 0 --device 0 --type hdd --medium "%DIR%\stick.vdi" || exit /b 1
echo STICK attached
