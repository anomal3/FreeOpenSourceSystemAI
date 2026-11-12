//! Проба DirectX для Proton в FossAi (12.11.2026, шаг 6 плана Proton).
//!
//! `d3dprobe.exe [11|9] [секунды]` открывает окно 640x480, каждый кадр заливает его
//! меняющимся цветом через Direct3D 11 (по умолчанию) или Direct3D 9 и показывает.
//! В stderr: имя адаптера, затем раз в 2 с строка `d3dprobe: N frames ... FPS` -
//! сценарий ждёт её. Через заданное время (по умолчанию 0 = бесконечно) выходит сам.

use std::time::{Duration, Instant};

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Direct3D9::{
    Direct3DCreate9, IDirect3DDevice9, D3DADAPTER_IDENTIFIER9, D3DADAPTER_DEFAULT, D3DCLEAR_TARGET,
    D3DCREATE_SOFTWARE_VERTEXPROCESSING, D3DDEVTYPE_HAL, D3DFMT_UNKNOWN, D3DPRESENT_PARAMETERS, D3DSWAPEFFECT_DISCARD,
    D3D_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGISwapChain, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DispatchMessageW, PeekMessageW, PostQuitMessage,
    RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, MSG, PM_REMOVE, SW_SHOW,
    WINDOW_EX_STYLE, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn window(title: windows::core::PCWSTR) -> Result<HWND> {
    unsafe {
        let instance: HMODULE = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("d3dprobe"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let mut rect = RECT { left: 0, top: 0, right: WIDTH as i32, bottom: HEIGHT as i32 };
        AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false)?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("d3dprobe"),
            title,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = ShowWindow(hwnd, SW_SHOW);
        Ok(hwnd)
    }
}

/// Разбирает очередь сообщений; `false` - окно закрыто.
fn pump() -> bool {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

fn color(frame: u64) -> [f32; 4] {
    let t = frame as f32 / 60.0;
    [0.5 + 0.5 * t.sin(), 0.5 + 0.5 * (t * 0.7 + 2.0).sin(), 0.5 + 0.5 * (t * 1.3 + 4.0).sin(), 1.0]
}

/// Цикл кадров: `draw` рисует кадр, раз в 2 с - строка FPS.
fn run(seconds: u64, mut draw: impl FnMut(u64) -> Result<()>) -> Result<()> {
    let start = Instant::now();
    let mut mark = start;
    let (mut frame, mut since) = (0u64, 0u64);
    while pump() {
        draw(frame)?;
        frame += 1;
        since += 1;
        let now = Instant::now();
        if now - mark >= Duration::from_secs(2) {
            let fps = since as f64 / (now - mark).as_secs_f64();
            eprintln!("d3dprobe: {since} frames in {:.1} s = {fps:.1} FPS", (now - mark).as_secs_f64());
            mark = now;
            since = 0;
        }
        if seconds != 0 && now - start >= Duration::from_secs(seconds) {
            break;
        }
    }
    eprintln!("d3dprobe: done, {frame} frames");
    Ok(())
}

fn d3d11(seconds: u64) -> Result<()> {
    let hwnd = window(w!("d3dprobe D3D11"))?;
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC { Width: WIDTH, Height: HEIGHT, Format: DXGI_FORMAT_R8G8B8A8_UNORM, ..Default::default() },
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 1,
        OutputWindow: hwnd,
        Windowed: true.into(),
        SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
        ..Default::default()
    };
    let mut swap: Option<IDXGISwapChain> = None;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut level = D3D_FEATURE_LEVEL::default();
    unsafe {
        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&desc),
            Some(&mut swap),
            Some(&mut device),
            Some(&mut level),
            Some(&mut context),
        )?;
    }
    let (swap, device, context) = (swap.unwrap(), device.unwrap(), context.unwrap());
    unsafe {
        let adapter = device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?;
        let len = adapter.Description.iter().position(|&c| c == 0).unwrap_or(adapter.Description.len());
        eprintln!(
            "d3dprobe: D3D11 feature level {:#x}, adapter '{}'",
            level.0,
            String::from_utf16_lossy(&adapter.Description[..len])
        );
    }
    let back: ID3D11Texture2D = unsafe { swap.GetBuffer(0)? };
    let mut target: Option<ID3D11RenderTargetView> = None;
    unsafe { device.CreateRenderTargetView(&back, None, Some(&mut target))? };
    let target = target.unwrap();
    run(seconds, |frame| unsafe {
        context.ClearRenderTargetView(&target, &color(frame));
        swap.Present(1, DXGI_PRESENT(0)).ok()
    })
}

fn d3d9(seconds: u64) -> Result<()> {
    let hwnd = window(w!("d3dprobe D3D9"))?;
    let d3d = unsafe { Direct3DCreate9(D3D_SDK_VERSION) }.ok_or_else(windows::core::Error::empty)?;
    unsafe {
        let mut id = D3DADAPTER_IDENTIFIER9::default();
        d3d.GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &mut id)?;
        let name: Vec<u8> = id.Description.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        eprintln!("d3dprobe: D3D9 adapter '{}'", String::from_utf8_lossy(&name));
    }
    let mut params = D3DPRESENT_PARAMETERS {
        BackBufferWidth: WIDTH,
        BackBufferHeight: HEIGHT,
        BackBufferFormat: D3DFMT_UNKNOWN,
        BackBufferCount: 1,
        SwapEffect: D3DSWAPEFFECT_DISCARD,
        hDeviceWindow: hwnd,
        Windowed: true.into(),
        ..Default::default()
    };
    let mut device: Option<IDirect3DDevice9> = None;
    unsafe {
        d3d.CreateDevice(
            D3DADAPTER_DEFAULT,
            D3DDEVTYPE_HAL,
            hwnd,
            D3DCREATE_SOFTWARE_VERTEXPROCESSING as u32,
            &mut params,
            &mut device,
        )?;
    }
    let device = device.unwrap();
    run(seconds, |frame| unsafe {
        let [r, g, b, _] = color(frame);
        let argb = 0xff00_0000 | ((r * 255.0) as u32) << 16 | ((g * 255.0) as u32) << 8 | (b * 255.0) as u32;
        device.Clear(0, std::ptr::null(), D3DCLEAR_TARGET as u32, argb, 1.0, 0)?;
        device.BeginScene()?;
        device.EndScene()?;
        device.Present(std::ptr::null(), std::ptr::null(), HWND::default(), std::ptr::null())
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let api = args.get(1).map(String::as_str).unwrap_or("11");
    let seconds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    eprintln!("d3dprobe: start, Direct3D {api}");
    let result = if api == "9" { d3d9(seconds) } else { d3d11(seconds) };
    if let Err(err) = result {
        eprintln!("d3dprobe: error {err}");
        std::process::exit(1);
    }
}
