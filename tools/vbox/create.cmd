@echo off
rem Стенд VirtualBox (фаза 68g): создать машину FreeOS-bench заново.
rem   tools\vbox\create.cmd
rem UEFI, VMSVGA, SATA (AHCI), e1000, звук HDA с выводом null, USB OHCI с
rem планшетом, COM1 в файл build\vbox\serial.log - журнал ядра.
rem Чужие машины не трогаются: только FreeOS-bench.
setlocal
set VB="C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
set VM=FreeOS-bench
set DIR=%~dp0..\..\build\vbox
if not exist "%DIR%" mkdir "%DIR%"
for %%i in ("%DIR%") do set DIR=%%~fi
%VB% unregistervm %VM% --delete >nul 2>&1
%VB% createvm --name %VM% --ostype Other_64 --basefolder "%DIR%" --register || exit /b 1
%VB% modifyvm %VM% --memory 1024 --cpus 2 --firmware efi --graphicscontroller vmsvga --vram 64 ^
  --audio-driver null --audio-controller hda --audio-enabled on --audio-out on ^
  --nic1 nat --nictype1 82540EM --mouse usbtablet --usb-ohci on ^
  --uart1 0x3F8 4 --uart-mode1 file "%DIR%\serial.log" || exit /b 1
%VB% storagectl %VM% --name SATA --add sata --controller IntelAhci --portcount 2 || exit /b 1
%VB% createmedium disk --filename "%DIR%\%VM%\disk.vdi" --size 4096 || exit /b 1
%VB% storageattach %VM% --storagectl SATA --port 0 --device 0 --type hdd --medium "%DIR%\%VM%\disk.vdi" || exit /b 1
echo CREATED %VM% in %DIR%
