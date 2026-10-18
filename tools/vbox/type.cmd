@echo off
rem Стенд VirtualBox: набрать строку на клавиатуре FreeOS-bench и нажать Enter.
rem   tools\vbox\type.cmd "beep 440 2000"
"C:\Program Files\Oracle\VirtualBox\VBoxManage.exe" controlvm FreeOS-bench keyboardputstring %1 || exit /b 1
"C:\Program Files\Oracle\VirtualBox\VBoxManage.exe" controlvm FreeOS-bench keyboardputscancode 1c 9c
