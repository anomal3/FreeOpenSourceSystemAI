//! Проба DirectX для Proton в FossAi (12.11.2026, шаг 6 плана Proton).
//!
//! `d3dprobe.exe monitors` (G5, 22.11) - `EnumDisplayMonitors` and the virtual
//! screen as Wine sees them (one line per monitor).
//!
//! `d3dprobe.exe hold [секунды]` (18.11) - скрытое окно держит `explorer` Wine живым
//! между замерами `proton-bench`.
//!
//! `d3dprobe.exe [11|9|12] [секунды] [ШxВ]` открывает окно (640x480 или ШxВ, 18.11 - замеры
//! размера кадра, P2), каждый кадр заливает его
//! меняющимся цветом через Direct3D 11 (по умолчанию), Direct3D 9 или Direct3D 12
//! (20.11: vkd3d-proton, как у Diablo II Resurrected) и показывает.
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
    RegisterClassW, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, MSG, PM_REMOVE, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WM_DESTROY, WM_QUIT, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

/// Размер окна и кадра по умолчанию.
const SIZE: (u32, u32) = (640, 480);

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn window(title: windows::core::PCWSTR, (width, height): (u32, u32)) -> Result<HWND> {
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
        let mut rect = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
        AdjustWindowRect(&mut rect, WS_OVERLAPPEDWINDOW, false)?;
        let hwnd = match CreateWindowExW(
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
        ) {
            Ok(hwnd) => hwnd,
            Err(err) => relaunch(err),
        };
        let _ = ShowWindow(hwnd, SW_SHOW);
        Ok(hwnd)
    }
}

/// Окна нет (18.11): окно стола Wine поднимает `explorer`, и Wine ждёт его
/// ограниченное время (10 с). В QEMU без ускорения он не успевает - «The
/// explorer process failed to start», `CreateWindowEx` даёт ошибку 1400, и
/// процесс до конца остаётся без драйвера окон (повтор в нём не помогает).
/// Сам `explorer` при этом поднимается дальше (в QEMU - больше минуты, держа
/// замок загрузчика): проба через 20 с запускает себя заново (до 8 раз,
/// `D3DPROBE_RELAUNCH`) - потомок наследует stderr, и замер
/// `fossai-proton --bench` читает его строки как свои. Родитель ждёт ещё 5 с,
/// чтобы Wine не погасил `explorer` между ними (он живёт, пока есть программы).
/// Надёжнее - `d3dprobe hold`, см. [`hold`].
fn relaunch(err: windows::core::Error) -> ! {
    let done: u32 = std::env::var("D3DPROBE_RELAUNCH").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    if done >= 8 {
        eprintln!("d3dprobe: error {err}");
        std::process::exit(1);
    }
    eprintln!("d3dprobe: no window ({err}), starting again in 20 s ({})", done + 1);
    std::thread::sleep(Duration::from_secs(20));
    let exe = std::env::current_exe().unwrap_or_else(|_| "d3dprobe.exe".into());
    let spawned = std::process::Command::new(exe)
        .args(std::env::args().skip(1))
        .env("D3DPROBE_RELAUNCH", (done + 1).to_string())
        .spawn();
    if let Err(spawn) = spawned {
        eprintln!("d3dprobe: error {err}, relaunch failed: {spawn}");
        std::process::exit(1);
    }
    std::thread::sleep(Duration::from_secs(5));
    std::process::exit(0);
}

/// `d3dprobe hold [секунды]` (18.11): скрытое окно, которое держит `explorer`
/// (окно стола Wine) живым, пока идут замеры `proton-bench`: каждый запуск
/// пробы иначе поднимал бы `explorer` заново и в QEMU не дожидался его. Когда
/// окно есть - строка `d3dprobe: holding the desktop`, её ждёт `proton-bench`.
fn hold(seconds: u64) -> Result<()> {
    let hwnd = window(w!("d3dprobe hold"), (64, 64))?;
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    eprintln!("d3dprobe: holding the desktop for {seconds} s");
    let start = Instant::now();
    while pump() && start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(200));
    }
    eprintln!("d3dprobe: hold done");
    Ok(())
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

fn d3d11(seconds: u64, size: (u32, u32)) -> Result<()> {
    let hwnd = window(w!("d3dprobe D3D11"), size)?;
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC { Width: size.0, Height: size.1, Format: DXGI_FORMAT_R8G8B8A8_UNORM, ..Default::default() },
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

fn d3d9(seconds: u64, size: (u32, u32)) -> Result<()> {
    let hwnd = window(w!("d3dprobe D3D9"), size)?;
    let d3d = unsafe { Direct3DCreate9(D3D_SDK_VERSION) }.ok_or_else(windows::core::Error::empty)?;
    unsafe {
        let mut id = D3DADAPTER_IDENTIFIER9::default();
        d3d.GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &mut id)?;
        let name: Vec<u8> = id.Description.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        eprintln!("d3dprobe: D3D9 adapter '{}'", String::from_utf8_lossy(&name));
    }
    let mut params = D3DPRESENT_PARAMETERS {
        BackBufferWidth: size.0,
        BackBufferHeight: size.1,
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

/// Direct3D 12 (20.11.2026): то, на чём стоит Diablo II Resurrected, - в
/// Proton это vkd3d-proton поверх Vulkan. Устройство, очередь, цепочка
/// показа FLIP_DISCARD на два буфера (DXGI DXVK), каждый кадр - список команд
/// с переходами PRESENT <-> RENDER_TARGET и очисткой, затем `Present` и забор
/// (`ID3D12Fence` - timeline-семафор Vulkan: ожидание через событие идёт в
/// vkd3d-proton через `vkWaitSemaphores`, у NVK - `SYNCOBJ_TIMELINE_WAIT`).
fn d3d12(seconds: u64, size: (u32, u32)) -> Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory2, IDXGIAdapter1, IDXGIFactory4, IDXGISwapChain3, DXGI_CREATE_FACTORY_FLAGS,
        DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
    const FRAMES: usize = 2;

    let hwnd = window(w!("d3dprobe D3D12"), size)?;
    let factory: IDXGIFactory4 = unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))? };
    let mut device: Option<ID3D12Device> = None;
    unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut device)? };
    let device = device.unwrap();
    unsafe {
        let luid = device.GetAdapterLuid();
        let name = factory
            .EnumAdapterByLuid::<IDXGIAdapter1>(luid)
            .and_then(|adapter| adapter.GetDesc1())
            .map(|desc| {
                let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
                String::from_utf16_lossy(&desc.Description[..len])
            })
            .unwrap_or_else(|err| format!("? ({err})"));
        let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS::default();
        let tier = device
            .CheckFeatureSupport(
                D3D12_FEATURE_D3D12_OPTIONS,
                (&mut options as *mut D3D12_FEATURE_DATA_D3D12_OPTIONS).cast(),
                std::mem::size_of::<D3D12_FEATURE_DATA_D3D12_OPTIONS>() as u32,
            )
            .map(|()| format!("{}", options.ResourceBindingTier.0))
            .unwrap_or_else(|err| format!("? ({err})"));
        eprintln!("d3dprobe: D3D12 device on adapter '{name}', resource binding tier {tier}");
    }
    let queue: ID3D12CommandQueue = unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })?
    };
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: size.0,
        Height: size.1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: FRAMES as u32,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        ..Default::default()
    };
    let swap: IDXGISwapChain3 = unsafe { factory.CreateSwapChainForHwnd(&queue, hwnd, &desc, None, None)?.cast()? };
    let heap: ID3D12DescriptorHeap = unsafe {
        device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            NumDescriptors: FRAMES as u32,
            ..Default::default()
        })?
    };
    let step = unsafe { device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV) } as usize;
    let start = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
    let rtv = |i: usize| D3D12_CPU_DESCRIPTOR_HANDLE { ptr: start.ptr + i * step };
    let mut buffers: Vec<ID3D12Resource> = Vec::new();
    let mut allocators: Vec<ID3D12CommandAllocator> = Vec::new();
    for i in 0..FRAMES {
        let buffer: ID3D12Resource = unsafe { swap.GetBuffer(i as u32)? };
        unsafe { device.CreateRenderTargetView(&buffer, None, rtv(i)) };
        buffers.push(buffer);
        allocators.push(unsafe { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)? });
    }
    let list: ID3D12GraphicsCommandList =
        unsafe { device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocators[0], None)? };
    unsafe { list.Close()? };
    let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE)? };
    let event = unsafe { CreateEventW(None, false, false, None)? };
    let mut value = 0u64;
    let mut waits = [0u64; FRAMES];
    let barrier = |resource: &ID3D12Resource, before, after| D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                // Без лишней ссылки (как в примерах Microsoft): барьер живёт
                // меньше ресурса.
                pResource: unsafe { std::mem::transmute_copy(resource) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    };
    let wait_for = |target: u64| -> Result<()> {
        unsafe {
            if fence.GetCompletedValue() < target {
                fence.SetEventOnCompletion(target, event)?;
                WaitForSingleObject(event, INFINITE);
            }
        }
        Ok(())
    };
    let result = run(seconds, |frame| unsafe {
        let i = swap.GetCurrentBackBufferIndex() as usize;
        wait_for(waits[i])?;
        allocators[i].Reset()?;
        list.Reset(&allocators[i], None)?;
        list.ResourceBarrier(&[barrier(&buffers[i], D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_RENDER_TARGET)]);
        list.ClearRenderTargetView(rtv(i), &color(frame), None);
        list.ResourceBarrier(&[barrier(&buffers[i], D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_PRESENT)]);
        list.Close()?;
        queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>()?)]);
        swap.Present(1, DXGI_PRESENT(0)).ok()?;
        value += 1;
        queue.Signal(&fence, value)?;
        waits[i] = value;
        Ok(())
    });
    // Дождаться GPU до освобождения ресурсов.
    let _ = wait_for(value);
    unsafe {
        let _ = CloseHandle(event);
    }
    result
}

/// `d3dprobe.exe tone ГЦ СЕКУНДЫ`: синус через waveOut (winmm → mmdevapi →
/// winealsa в Wine), 48 кГц, 16 бит, стерео, четверть шкалы.
fn tone(hz: u32, seconds: u32) -> std::result::Result<(), String> {
    use windows::Win32::Media::Audio::{
        waveOutClose, waveOutOpen, waveOutPrepareHeader, waveOutUnprepareHeader, waveOutWrite, CALLBACK_NULL, HWAVEOUT,
        WAVEFORMATEX, WAVEHDR, WAVE_MAPPER, WHDR_DONE,
    };
    const RATE: u32 = 48_000;
    let frames = (RATE * seconds) as usize;
    let mut samples: Vec<i16> = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = ((i as f32 * hz as f32 / RATE as f32) * std::f32::consts::TAU).sin();
        let s = (v * 8192.0) as i16;
        samples.push(s);
        samples.push(s);
    }
    let format = WAVEFORMATEX {
        wFormatTag: 1, // WAVE_FORMAT_PCM
        nChannels: 2,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 4,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    let mut out = HWAVEOUT::default();
    let code = unsafe { waveOutOpen(Some(&mut out), WAVE_MAPPER, &format, Some(0), Some(0), CALLBACK_NULL) };
    if code != 0 {
        return Err(format!("waveOutOpen {code}"));
    }
    eprintln!("d3dprobe: tone {hz} Hz for {seconds} s, waveOut opened");
    let mut header = WAVEHDR {
        lpData: windows::core::PSTR(samples.as_mut_ptr().cast()),
        dwBufferLength: (samples.len() * 2) as u32,
        ..Default::default()
    };
    let size = std::mem::size_of::<WAVEHDR>() as u32;
    unsafe {
        let code = waveOutPrepareHeader(out, &mut header, size);
        if code != 0 {
            return Err(format!("waveOutPrepareHeader {code}"));
        }
        let code = waveOutWrite(out, &mut header, size);
        if code != 0 {
            return Err(format!("waveOutWrite {code}"));
        }
        let start = Instant::now();
        while header.dwFlags & WHDR_DONE == 0 {
            std::thread::sleep(Duration::from_millis(50));
            if start.elapsed() > Duration::from_secs(u64::from(seconds) + 20) {
                return Err(String::from("tone never finished"));
            }
        }
        eprintln!("d3dprobe: tone played in {:.1} s", start.elapsed().as_secs_f64());
        let _ = waveOutUnprepareHeader(out, &mut header, size);
        let _ = waveOutClose(out);
    }
    Ok(())
}

/// One monitor of `EnumDisplayMonitors` (G5, 22.11).
unsafe extern "system" fn monitor_line(
    monitor: windows::Win32::Graphics::Gdi::HMONITOR,
    _dc: windows::Win32::Graphics::Gdi::HDC,
    _rect: *mut RECT,
    count: LPARAM,
) -> windows::core::BOOL {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO, MONITORINFOEXW};
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    let ok = unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool();
    let r = info.monitorInfo.rcMonitor;
    let w = info.monitorInfo.rcWork;
    let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
    let name = String::from_utf16_lossy(&info.szDevice[..len]);
    let index = unsafe { &mut *(count.0 as *mut u32) };
    eprintln!(
        "d3dprobe: monitor {index} '{name}' {},{} {}x{} work {},{} {}x{}{}{}",
        r.left,
        r.top,
        r.right - r.left,
        r.bottom - r.top,
        w.left,
        w.top,
        w.right - w.left,
        w.bottom - w.top,
        if info.monitorInfo.dwFlags & 1 != 0 { " primary" } else { "" },
        if ok { "" } else { " (GetMonitorInfo failed)" }
    );
    *index += 1;
    true.into()
}

/// `d3dprobe.exe monitors`: what Wine says about the monitors -
/// `EnumDisplayMonitors` with `GetMonitorInfo` and the virtual screen (G5,
/// 22.11: winewayland builds them from the compositor's wl_outputs).
fn monitors() {
    use windows::Win32::Graphics::Gdi::EnumDisplayMonitors;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CMONITORS, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    let mut count: u32 = 0;
    let ok = unsafe { EnumDisplayMonitors(None, None, Some(monitor_line), LPARAM(&mut count as *mut u32 as isize)) }.as_bool();
    let metric = |m| unsafe { GetSystemMetrics(m) };
    eprintln!(
        "d3dprobe: monitors {count}{} (SM_CMONITORS {}), primary {}x{}, virtual screen {},{} {}x{}",
        if ok { "" } else { " (EnumDisplayMonitors failed)" },
        metric(SM_CMONITORS),
        metric(SM_CXSCREEN),
        metric(SM_CYSCREEN),
        metric(SM_XVIRTUALSCREEN),
        metric(SM_YVIRTUALSCREEN),
        metric(SM_CXVIRTUALSCREEN),
        metric(SM_CYVIRTUALSCREEN)
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("monitors") {
        eprintln!("d3dprobe: start, monitors");
        monitors();
        eprintln!("d3dprobe: done, monitors");
        return;
    }
    if args.get(1).map(String::as_str) == Some("hold") {
        let seconds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3600);
        if let Err(err) = hold(seconds) {
            eprintln!("d3dprobe: error {err}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).map(String::as_str) == Some("tone") {
        let hz = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(440);
        let seconds = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3);
        eprintln!("d3dprobe: start, tone");
        if let Err(err) = tone(hz, seconds) {
            eprintln!("d3dprobe: error {err}");
            std::process::exit(1);
        }
        eprintln!("d3dprobe: done, tone");
        return;
    }
    let api = args.get(1).map(String::as_str).unwrap_or("11");
    let seconds = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let size = args
        .get(3)
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .filter(|&(w, h): &(u32, u32)| w > 0 && h > 0)
        .unwrap_or(SIZE);
    eprintln!("d3dprobe: start, Direct3D {api}, {}x{}", size.0, size.1);
    let result = match api {
        "9" => d3d9(seconds, size),
        "12" => d3d12(seconds, size),
        _ => d3d11(seconds, size),
    };
    if let Err(err) = result {
        eprintln!("d3dprobe: error {err}");
        std::process::exit(1);
    }
}
