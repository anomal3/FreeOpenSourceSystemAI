//! `d3dprobe.exe mouse [seconds]` (26.11, Uot: "no cursor, the turret does
//! not follow the mouse") - the mouse as a fullscreen game sees it under
//! Proton's winewayland:
//!
//! - a borderless window over the whole last non-primary monitor (the
//!   primary one if there is one monitor), painted with GDI;
//! - its own color cursor (a yellow ring with a red dot, 32x32, hotspot in
//!   the middle) - winewayland sends it as a `wl_pointer.set_cursor`
//!   surface;
//! - raw input (`RegisterRawInputDevices`, `WM_INPUT`) like Unity's mouse
//!   delta, `WM_MOUSEMOVE` and the buttons;
//! - `L` locks like a game in battle: the cursor hidden, `ClipCursor` to the
//!   window and `SetCursorPos` to its middle every tick (winewayland turns
//!   this into `zwp_locked_pointer_v1` + relative motion); `U` unlocks,
//!   `Esc` ends.
//!
//! Every second with news one line `d3dprobe: mouse <phase>: raw dx .. dy ..
//! (N WM_INPUT), N WM_MOUSEMOVE, cursor x,y`; the first raw motion of a
//! phase is also `d3dprobe: mouse <phase>: raw input moves`.

use std::time::{Duration, Instant};

use windows::core::{w, BOOL};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateBitmap, CreateDIBSection, CreateSolidBrush, DeleteObject, EndPaint, EnumDisplayMonitors, FillRect,
    GetMonitorInfoW, InvalidateRect, SetBkMode, SetTextColor, TextOutW, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HDC, HMONITOR, MONITORINFO, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RAWINPUTDEVICE_FLAGS, RID_INPUT,
    RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ClipCursor, CreateIconIndirect, CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
    GetWindowRect, PostQuitMessage, RegisterClassW, SetCursor, SetCursorPos, SetForegroundWindow, ShowWindow, HCURSOR,
    ICONINFO, SW_SHOW, WINDOW_EX_STYLE, WM_DESTROY, WM_INPUT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONDOWN, WM_SETCURSOR, WNDCLASSW, WS_POPUP, WS_VISIBLE,
};

/// What the window procedure saw (one window, one thread).
#[derive(Default)]
struct State {
    raw: (i64, i64),
    raw_count: u64,
    raw_moved: bool,
    moves: u64,
    last_move: (i32, i32),
    locked: bool,
    /// Change of phase asked by a key: `Some(true)` lock, `Some(false)` unlock.
    asked: Option<bool>,
    quit: bool,
    cursor: Option<HCURSOR>,
}

static mut STATE: Option<State> = None;

#[allow(static_mut_refs)]
fn state() -> &'static mut State {
    unsafe { STATE.get_or_insert_with(State::default) }
}

fn phase(locked: bool) -> &'static str {
    if locked {
        "lock"
    } else {
        "cursor"
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let st = state();
    match msg {
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        WM_SETCURSOR if (lparam.0 & 0xffff) as u32 == 1 => {
            // HTCLIENT: ours, or none while locked (a game hides it).
            unsafe {
                SetCursor(if st.locked { None } else { st.cursor });
            }
            return LRESULT(1);
        }
        WM_INPUT => {
            let mut raw = RAWINPUT::default();
            let mut size = std::mem::size_of::<RAWINPUT>() as u32;
            let got = unsafe {
                GetRawInputData(
                    HRAWINPUT(lparam.0 as *mut core::ffi::c_void),
                    RID_INPUT,
                    Some(&mut raw as *mut RAWINPUT as *mut core::ffi::c_void),
                    &mut size,
                    std::mem::size_of::<RAWINPUTHEADER>() as u32,
                )
            };
            if got != u32::MAX && raw.header.dwType == RIM_TYPEMOUSE.0 {
                let mouse = unsafe { raw.data.mouse };
                // MOUSE_MOVE_ABSOLUTE = 1: not a delta.
                if mouse.usFlags.0 & 1 == 0 {
                    if !st.raw_moved && (mouse.lLastX != 0 || mouse.lLastY != 0) {
                        st.raw_moved = true;
                        eprintln!("d3dprobe: mouse {}: raw input moves", phase(st.locked));
                    }
                    st.raw.0 += i64::from(mouse.lLastX);
                    st.raw.1 += i64::from(mouse.lLastY);
                    st.raw_count += 1;
                }
            }
        }
        WM_MOUSEMOVE => {
            st.moves += 1;
            st.last_move = ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32);
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            let (x, y) = ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32);
            eprintln!(
                "d3dprobe: mouse {}: {} button down at {x},{y}",
                phase(st.locked),
                if msg == WM_LBUTTONDOWN { "left" } else { "right" }
            );
        }
        WM_KEYDOWN => match wparam.0 as u32 {
            0x4c => st.asked = Some(true),  // L
            0x55 => st.asked = Some(false), // U
            0x1b => st.quit = true,         // Esc
            _ => {}
        },
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let dc = BeginPaint(hwnd, &mut ps);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let back = if st.locked { CreateSolidBrush(COLORREF(0x0040_2010)) } else { CreateSolidBrush(COLORREF(0x0060_3018)) };
                FillRect(dc, &rect, back);
                let _ = DeleteObject(back.into());
                // A color band: screenshots of the monitor must not look empty.
                for i in 0..64 {
                    let band = CreateSolidBrush(COLORREF((i * 4) as u32 | (((255 - i * 4) as u32) << 8) | 0x0040_0000));
                    let r = RECT { left: rect.left + i * 20, top: rect.bottom - 60, right: rect.left + i * 20 + 20, bottom: rect.bottom };
                    FillRect(dc, &r, band);
                    let _ = DeleteObject(band.into());
                }
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00ff_ffff));
                let text: Vec<u16> = format!(
                    "d3dprobe mouse - {}: raw {},{} ({} WM_INPUT), {} WM_MOUSEMOVE at {},{}   [L lock, U unlock, Esc end]",
                    phase(st.locked),
                    st.raw.0,
                    st.raw.1,
                    st.raw_count,
                    st.moves,
                    st.last_move.0,
                    st.last_move.1
                )
                .encode_utf16()
                .collect();
                let _ = TextOutW(dc, 40, 40, &text);
                let _ = EndPaint(hwnd, &ps);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The 32x32 color cursor: a yellow ring with a black edge and a red dot,
/// hotspot 16,16.
fn ring_cursor() -> Option<HCURSOR> {
    const SIDE: i32 = 32;
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIDE,
                biHeight: -SIDE,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color: HBITMAP = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (SIDE * SIDE) as usize);
        for y in 0..SIDE {
            for x in 0..SIDE {
                let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
                let d = (dx * dx + dy * dy).sqrt();
                pixels[(y * SIDE + x) as usize] = if d < 3.0 {
                    0xffff_2020
                } else if (11.0..14.0).contains(&d) {
                    0xffff_e000
                } else if (10.0..15.5).contains(&d) {
                    0xff00_0000
                } else {
                    0
                };
            }
        }
        let mask_bits = vec![0u8; (SIDE * SIDE / 8) as usize];
        let mask = CreateBitmap(SIDE, SIDE, 1, 1, Some(mask_bits.as_ptr().cast()));
        let icon = ICONINFO { fIcon: BOOL(0), xHotspot: 16, yHotspot: 16, hbmMask: mask, hbmColor: color };
        let cursor = CreateIconIndirect(&icon).ok()?;
        Some(HCURSOR(cursor.0))
    }
}

unsafe extern "system" fn pick_monitor(monitor: HMONITOR, _dc: HDC, _rect: *mut RECT, out: LPARAM) -> BOOL {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let best = unsafe { &mut *(out.0 as *mut Option<(RECT, bool)>) };
        let primary = info.dwFlags & 1 != 0;
        // The last non-primary monitor wins; the primary only if alone.
        if best.is_none() || !primary {
            *best = Some((info.rcMonitor, primary));
        }
    }
    true.into()
}

pub fn mouse(seconds: u64) -> windows::core::Result<()> {
    let mut chosen: Option<(RECT, bool)> = None;
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(pick_monitor), LPARAM(&mut chosen as *mut _ as isize));
    }
    let (m, primary) = chosen.unwrap_or((RECT { left: 0, top: 0, right: 1280, bottom: 800 }, true));
    let (w, h) = (m.right - m.left, m.bottom - m.top);
    eprintln!(
        "d3dprobe: mouse window on the {} monitor {},{} {w}x{h}",
        if primary { "primary" } else { "second" },
        m.left,
        m.top
    );
    state().cursor = ring_cursor();
    eprintln!("d3dprobe: mouse own color cursor {}", if state().cursor.is_some() { "made" } else { "FAILED" });
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("d3dprobe-mouse"),
            hCursor: state().cursor.unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("d3dprobe-mouse"),
            w!("d3dprobe mouse"),
            WS_POPUP | WS_VISIBLE,
            m.left,
            m.top,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let device = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 2, dwFlags: RAWINPUTDEVICE_FLAGS(0), hwndTarget: hwnd };
        match RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32) {
            Ok(()) => eprintln!("d3dprobe: mouse raw input registered"),
            Err(err) => eprintln!("d3dprobe: mouse raw input FAILED: {err}"),
        }
        hwnd
    };
    eprintln!("d3dprobe: mouse phase cursor (move the mouse over the window; L locks)");
    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut told = (0u64, 0u64);
    let mut warp_told = false;
    while super::pump() && start.elapsed() < Duration::from_secs(seconds) {
        let st = state();
        if st.quit {
            break;
        }
        if let Some(lock) = st.asked.take() {
            if lock != st.locked {
                eprintln!(
                    "d3dprobe: mouse {} phase: raw {},{} ({} WM_INPUT), {} WM_MOUSEMOVE",
                    phase(st.locked),
                    st.raw.0,
                    st.raw.1,
                    st.raw_count,
                    st.moves
                );
                st.locked = lock;
                st.raw = (0, 0);
                st.raw_count = 0;
                st.raw_moved = false;
                st.moves = 0;
                told = (0, 0);
                unsafe {
                    if lock {
                        let mut rect = RECT::default();
                        let _ = GetWindowRect(hwnd, &mut rect);
                        SetCursor(None);
                        let clipped = ClipCursor(Some(&rect)).is_ok();
                        eprintln!(
                            "d3dprobe: mouse phase lock: cursor hidden, ClipCursor {},{} {}x{} {}",
                            rect.left,
                            rect.top,
                            rect.right - rect.left,
                            rect.bottom - rect.top,
                            if clipped { "ok" } else { "FAILED" }
                        );
                    } else {
                        let _ = ClipCursor(None);
                        SetCursor(st.cursor);
                        eprintln!("d3dprobe: mouse phase cursor: ClipCursor off, own cursor back");
                    }
                    let _ = InvalidateRect(Some(hwnd), None, true);
                }
            }
        }
        if st.locked {
            // Like Unity's CursorLockMode.Locked: back to the middle.
            let warped = unsafe { SetCursorPos(m.left + w / 2, m.top + h / 2) }.is_ok();
            if !warp_told {
                warp_told = true;
                eprintln!("d3dprobe: mouse SetCursorPos while locked {}", if warped { "ok" } else { "refused (relative mode)" });
            }
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            if (st.raw_count, st.moves) != told {
                told = (st.raw_count, st.moves);
                let mut at = POINT::default();
                let _ = unsafe { GetCursorPos(&mut at) };
                eprintln!(
                    "d3dprobe: mouse {}: raw dx {} dy {} ({} WM_INPUT), {} WM_MOUSEMOVE (last {},{}), cursor {},{}",
                    phase(st.locked),
                    st.raw.0,
                    st.raw.1,
                    st.raw_count,
                    st.moves,
                    st.last_move.0,
                    st.last_move.1,
                    at.x,
                    at.y
                );
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let st = state();
    eprintln!(
        "d3dprobe: mouse {} phase: raw {},{} ({} WM_INPUT), {} WM_MOUSEMOVE",
        phase(st.locked),
        st.raw.0,
        st.raw.1,
        st.raw_count,
        st.moves
    );
    unsafe {
        let _ = ClipCursor(None);
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}
