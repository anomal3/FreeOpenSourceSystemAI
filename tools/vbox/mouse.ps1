# Стенд VirtualBox: подвигать мышью FreeOS-bench (фаза 68k). VBoxManage мыши
# не умеет, поэтому — через COM API: IMouse.putMouseEventAbsolute, координаты
# в пикселях экрана гостя, с единицы.
#   powershell -File tools\vbox\mouse.ps1 [-Click] x1 y1 [x2 y2 ...]
param([switch]$Click, [Parameter(ValueFromRemainingArguments)][int[]]$Points)
$vb = New-Object -ComObject VirtualBox.VirtualBox
$session = New-Object -ComObject VirtualBox.Session
$machine = $vb.FindMachine('FreeOS-bench')
$machine.LockMachine($session, 1)  # LockType_Shared
try {
    $mouse = $session.Console.Mouse
    for ($i = 0; $i + 1 -lt $Points.Count; $i += 2) {
        $mouse.PutMouseEventAbsolute($Points[$i], $Points[$i + 1], 0, 0, 0)
        Start-Sleep -Milliseconds 100
        if ($Click) {
            $mouse.PutMouseEventAbsolute($Points[$i], $Points[$i + 1], 0, 0, 1)
            Start-Sleep -Milliseconds 100
            $mouse.PutMouseEventAbsolute($Points[$i], $Points[$i + 1], 0, 0, 0)
            Start-Sleep -Milliseconds 100
        }
    }
} finally {
    $session.UnlockMachine()
}
Write-Output "MOVED $($Points.Count / 2) point(s)"
