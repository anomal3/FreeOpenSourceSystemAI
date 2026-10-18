@echo off
rem Стенд VirtualBox: часы и звук (фаза 68g). Загрузка, 40 с на уточнение
rem частоты счётчика, beep 440 2000, uptime, 30 с по часам хоста, uptime.
rem Верно: beep около 2000 мс, между двумя uptime около 30 с.
setlocal
set DIR=%~dp0..\..\build\vbox
call "%~dp0boot.cmd" || exit /b 1
powershell -c "Start-Sleep 40"
call "%~dp0type.cmd" "beep 440 2000"
powershell -c "Start-Sleep 8"
call "%~dp0type.cmd" "uptime"
powershell -c "Start-Sleep 30"
call "%~dp0type.cmd" "uptime"
powershell -c "Start-Sleep 3"
findstr /C:"monotonic" /C:"timer       :" /C:"hda  " /C:"beep " /C:"timer ticks" "%DIR%\serial.log"
