@echo off
rem Стенд VirtualBox: нажать клавиши по именам, по одной, с паузой 0,7 с.
rem   tools\vbox\keys.cmd f1 down ret ret
setlocal
set VB="C:\Program Files\Oracle\VirtualBox\VBoxManage.exe"
:next
if "%~1"=="" exit /b 0
set CODE=
if /i "%~1"=="f1" set CODE=3b bb
if /i "%~1"=="ret" set CODE=1c 9c
if /i "%~1"=="esc" set CODE=01 81
if /i "%~1"=="down" set CODE=e0 50 e0 d0
if /i "%~1"=="up" set CODE=e0 48 e0 c8
if /i "%~1"=="tab" set CODE=0f 8f
if "%CODE%"=="" (echo unknown key %~1 & exit /b 1)
%VB% controlvm FreeOS-bench keyboardputscancode %CODE%
powershell -c "Start-Sleep -Milliseconds 700"
shift
goto next
