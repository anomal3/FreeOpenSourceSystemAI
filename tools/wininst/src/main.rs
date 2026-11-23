//! Установщик-проба для Proton в FossAi (22.11.2026).
//!
//! hw-log15: Notepad++, поставленный своим установщиком (NSIS), не появился ни
//! в «Пуске», ни на столе, а сценарий стенда `winapps` этого не ловил: его
//! ярлыки писала оболочка ядра (`mklnk`). Эта программа делает то же, что
//! установщик, настоящими вызовами Windows в настоящем Wine:
//!
//! `wininst.exe install` — копирует себя в `C:\Program Files\FossDemo\fossdemo.exe`,
//! пишет ярлыки `IShellLinkW` + `IPersistFile::Save` в «Пуск» всех
//! (`%ProgramData%\Microsoft\Windows\Start Menu\Programs\FossDemo\FossDemo.lnk`)
//! и на общий стол (`%PUBLIC%\Desktop\FossDemo.lnk`) — Wine при этом зовёт
//! `winemenubuilder`, как у Notepad++, — и сопоставление `.txt` в реестре
//! (`reg add`, как `WriteRegStr` NSIS): ProgID `FossDemo.Document`,
//! `.txt\OpenWithProgids`, `Applications\fossdemo.exe\SupportedTypes`.
//!
//! `fossdemo.exe open ФАЙЛ` — «редактор»: окно с заголовком
//! `FossDemo - ФАЙЛ` (путь Windows, как его дал «Открыть с помощью»), в stderr —
//! `fossdemo: opened ФАЙЛ, N bytes`. Через 10 минут выходит сам.

use std::time::{Duration, Instant};

use windows::core::{Interface, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, PostQuitMessage, RegisterClassW, ShowWindow,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, MSG, PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE, WM_DESTROY,
    WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

const DIR: &str = "C:\\Program Files\\FossDemo";
const EXE: &str = "C:\\Program Files\\FossDemo\\fossdemo.exe";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("install") => install(),
        Some("open") => open(args.get(1).map_or("", String::as_str)),
        _ => {
            eprintln!("usage: wininst.exe install | fossdemo.exe open FILE");
            std::process::exit(2);
        }
    };
    if let Err(err) = result {
        eprintln!("wininst: error {err}");
        std::process::exit(1);
    }
}

fn install() -> Result<()> {
    std::fs::create_dir_all(DIR).map_err(io)?;
    let me = std::env::current_exe().map_err(io)?;
    std::fs::copy(&me, EXE).map_err(io)?;
    eprintln!("wininst: copied {} to {EXE}", me.display());
    // Сопоставление .txt — до ярлыков: стол перечитывает «Программы Windows»
    // по ярлыкам, а список «Открыть с помощью» читает реестр при открытии.
    let command = format!("\"{EXE}\" open \"%1\"");
    reg(&["add", "HKLM\\Software\\Classes\\FossDemo.Document", "/ve", "/d", "Документ FossDemo", "/f"]);
    reg(&["add", "HKLM\\Software\\Classes\\FossDemo.Document\\shell\\open\\command", "/ve", "/d", &command, "/f"]);
    reg(&["add", "HKLM\\Software\\Classes\\.txt\\OpenWithProgids", "/v", "FossDemo.Document", "/d", "", "/f"]);
    reg(&["add", "HKLM\\Software\\Classes\\Applications\\fossdemo.exe\\shell\\open\\command", "/ve", "/d", &command, "/f"]);
    reg(&["add", "HKLM\\Software\\Classes\\Applications\\fossdemo.exe\\SupportedTypes", "/v", ".txt", "/d", "", "/f"]);
    let data = std::env::var("ProgramData").unwrap_or_else(|_| String::from("C:\\ProgramData"));
    let public = std::env::var("PUBLIC").unwrap_or_else(|_| String::from("C:\\users\\Public"));
    let menu = format!("{data}\\Microsoft\\Windows\\Start Menu\\Programs\\FossDemo");
    std::fs::create_dir_all(&menu).map_err(io)?;
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    shortcut(&format!("{menu}\\FossDemo.lnk"))?;
    let desk = format!("{public}\\Desktop");
    let _ = std::fs::create_dir_all(&desk);
    shortcut(&format!("{desk}\\FossDemo.lnk"))?;
    eprintln!("wininst: installed");
    Ok(())
}

fn io(err: std::io::Error) -> windows::core::Error {
    eprintln!("wininst: {err}");
    windows::core::Error::empty()
}

/// `reg.exe` Wine — как `WriteRegStr` установщика.
fn reg(args: &[&str]) {
    match std::process::Command::new("reg.exe").args(args).status() {
        Ok(status) => eprintln!("wininst: reg {} -> {status}", args[1]),
        Err(err) => eprintln!("wininst: reg {}: {err}", args[1]),
    }
}

/// Ярлык на программу — `IShellLinkW` и `IPersistFile::Save`, как у NSIS
/// (`CreateShortCut`). Значок — Блокнота Wine (своего у пробы нет).
fn shortcut(path: &str) -> Result<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(EXE))?;
        link.SetWorkingDirectory(&HSTRING::from(DIR))?;
        link.SetDescription(&HSTRING::from("FossDemo"))?;
        link.SetIconLocation(&HSTRING::from("C:\\windows\\system32\\notepad.exe"), 0)?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(path), true)?;
    }
    eprintln!("wininst: shortcut {path}");
    Ok(())
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// «Редактор»: окно с путём файла в заголовке.
fn open(file: &str) -> Result<()> {
    let size = std::fs::metadata(file).map(|meta| meta.len());
    match &size {
        Ok(bytes) => eprintln!("fossdemo: opened {file}, {bytes} bytes"),
        Err(err) => eprintln!("fossdemo: cannot open {file}: {err}"),
    }
    let title = HSTRING::from(format!("FossDemo - {file}"));
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: windows::core::w!("fossdemo"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            windows::core::w!("fossdemo"),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            560,
            240,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
    let start = Instant::now();
    'outer: while start.elapsed() < Duration::from_secs(600) {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    break 'outer;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
