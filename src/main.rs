//! hwoverlay — a tiny always-on-top, click-through hardware overlay for gaming.
//!
//! Hotkeys:
//!   Ctrl+Shift+F9   show / hide
//!   Ctrl+Shift+F10  move to next screen corner
//!   Ctrl+Shift+F12  quit

#![windows_subsystem = "windows"]

mod pdh;
mod stats;

use std::cell::RefCell;

use stats::{Snapshot, Stats};
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const REFRESH_MS: u32 = 1000;
const OPACITY: u8 = 200; // 0..=255, whole window
const FONT_PX: f32 = 15.0;
const MARGIN_PX: f32 = 10.0;
const PAD_PX: f32 = 6.0;

const HK_TOGGLE: i32 = 1;
const HK_CORNER: i32 = 2;
const HK_QUIT: i32 = 3;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}
const BG: COLORREF = rgb(16, 16, 20);
const LABEL: COLORREF = rgb(120, 180, 255);
const TEXT: COLORREF = rgb(235, 235, 235);
const DIM: COLORREF = rgb(160, 160, 160);
const OK: COLORREF = rgb(120, 230, 140);
const WARN: COLORREF = rgb(255, 210, 80);
const HOT: COLORREF = rgb(255, 95, 95);

type Line = Vec<(String, COLORREF)>;

struct App {
    stats: Stats,
    lines: Vec<Line>,
    font: HFONT,
    scale: f32,
    corner: u8, // 0 TL, 1 TR, 2 BR, 3 BL
    visible: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn main() -> windows::core::Result<()> {
    // `hwoverlay --dump | more` prints a few samples as text instead of showing the overlay.
    if std::env::args().any(|a| a == "--dump") {
        let stats = Stats::new();
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(REFRESH_MS as u64));
            for line in build_lines(&stats.sample()) {
                println!("{}", line.iter().map(|(s, _)| s.as_str()).collect::<String>());
            }
            println!();
        }
        return Ok(());
    }

    unsafe {
        // Single instance: a second copy would fail to register the hotkeys anyway.
        let _mutex = CreateMutexW(None, true, w!("Local\\hwoverlay_single_instance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let scale = GetDpiForSystem() as f32 / 96.0;

        let instance = GetModuleHandleW(None)?;
        let class = w!("hwoverlay");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let font = CreateFontW(
            -(FONT_PX * scale).round() as i32,
            0,
            0,
            0,
            FW_BOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (FF_MODERN.0 | FIXED_PITCH.0) as u32,
            w!("Consolas"),
        );

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                stats: Stats::new(),
                lines: Vec::new(),
                font,
                scale,
                corner: 0,
                visible: true,
            })
        });

        // Layered + transparent = click-through; toolwindow + noactivate = never steals focus / no taskbar button.
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("hwoverlay"),
            WS_POPUP,
            0,
            0,
            200,
            100,
            None,
            None,
            instance,
            None,
        )?;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), OPACITY, LWA_ALPHA)?;

        let mods = MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT;
        let _ = RegisterHotKey(hwnd, HK_TOGGLE, mods, VK_F9.0 as u32);
        let _ = RegisterHotKey(hwnd, HK_CORNER, mods, VK_F10.0 as u32);
        let _ = RegisterHotKey(hwnd, HK_QUIT, mods, VK_F12.0 as u32);

        SetTimer(hwnd, 1, REFRESH_MS, None);
        // Show placeholder layout right away; real values arrive on the first tick.
        tick(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            tick(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_HOTKEY => {
            match wparam.0 as i32 {
                HK_TOGGLE => with_app(|app| {
                    app.visible = !app.visible;
                    let _ = ShowWindow(hwnd, if app.visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
                }),
                HK_CORNER => {
                    with_app(|app| app.corner = (app.corner + 1) % 4);
                    tick(hwnd);
                }
                HK_QUIT => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.with(|a| f(a.borrow_mut().as_mut().expect("app not initialised")))
}

/// Refresh stats, resize/reposition the window to fit, and repaint.
unsafe fn tick(hwnd: HWND) {
    let (w, h, corner, scale) = with_app(|app| {
        let snap = app.stats.sample();
        app.lines = build_lines(&snap);
        let (w, h) = measure(app);
        (w, h, app.corner, app.scale)
    });

    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let m = (MARGIN_PX * scale) as i32;
    let (x, y) = match corner {
        0 => (m, m),
        1 => (sw - w - m, m),
        2 => (sw - w - m, sh - h - m),
        _ => (m, sh - h - m),
    };
    // Re-assert topmost every tick: some games push themselves above other topmost windows.
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    let _ = InvalidateRect(hwnd, None, false);
}

unsafe fn measure(app: &App) -> (i32, i32) {
    let hdc = GetDC(None);
    let old = SelectObject(hdc, app.font);
    let mut tm = TEXTMETRICW::default();
    let _ = GetTextMetricsW(hdc, &mut tm);
    let width = app
        .lines
        .iter()
        .map(|line| line.iter().map(|(s, _)| text_width(hdc, s)).sum::<i32>())
        .max()
        .unwrap_or(0);
    SelectObject(hdc, old);
    ReleaseDC(None, hdc);
    let pad = (PAD_PX * app.scale) as i32;
    (width + pad * 2, tm.tmHeight * app.lines.len() as i32 + pad * 2)
}

unsafe fn text_width(hdc: HDC, s: &str) -> i32 {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &wide, &mut size);
    size.cx
}

unsafe fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);

    // Double-buffer to avoid flicker.
    let mem = CreateCompatibleDC(hdc);
    let bmp = CreateCompatibleBitmap(hdc, rc.right, rc.bottom);
    let old_bmp = SelectObject(mem, bmp);
    let bg = CreateSolidBrush(BG);
    FillRect(mem, &rc, bg);
    let _ = DeleteObject(bg);

    with_app(|app| {
        let old_font = SelectObject(mem, app.font);
        SetBkMode(mem, TRANSPARENT);
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(mem, &mut tm);
        let pad = (PAD_PX * app.scale) as i32;
        for (i, line) in app.lines.iter().enumerate() {
            let mut x = pad;
            let y = pad + i as i32 * tm.tmHeight;
            for (text, color) in line {
                let wide: Vec<u16> = text.encode_utf16().collect();
                SetTextColor(mem, *color);
                let _ = TextOutW(mem, x, y, &wide);
                x += text_width(mem, text);
            }
        }
        SelectObject(mem, old_font);
    });

    let _ = BitBlt(hdc, 0, 0, rc.right, rc.bottom, mem, 0, 0, SRCCOPY);
    SelectObject(mem, old_bmp);
    let _ = DeleteObject(bmp);
    let _ = DeleteDC(mem);
    let _ = EndPaint(hwnd, &ps);
}

// ---------- text layout ----------

fn load_color(pct: f64) -> COLORREF {
    match pct {
        p if p >= 85.0 => HOT,
        p if p >= 60.0 => WARN,
        _ => OK,
    }
}

fn temp_color(c: u32) -> COLORREF {
    match c {
        c if c >= 90 => HOT,
        c if c >= 80 => WARN,
        _ => TEXT,
    }
}

fn pct_seg(v: Option<f64>) -> (String, COLORREF) {
    match v {
        Some(p) => (format!("{:>3.0}%", p), load_color(p)),
        None => ("  --".into(), DIM),
    }
}

fn watts_seg(v: Option<f64>) -> (String, COLORREF) {
    match v {
        Some(w) => (format!(" {:>5.1}W", w), TEXT),
        None => ("     --".into(), DIM),
    }
}

fn rate(bytes_per_sec: f64) -> String {
    const K: f64 = 1024.0;
    if bytes_per_sec >= K * K {
        format!("{:>6.2} MB/s", bytes_per_sec / (K * K))
    } else {
        format!("{:>6.1} KB/s", bytes_per_sec / K)
    }
}

fn label(s: &str) -> (String, COLORREF) {
    (format!("{s:<5}"), LABEL)
}

fn build_lines(s: &Snapshot) -> Vec<Line> {
    let mut lines = Vec::new();

    let mut cpu = vec![label("CPU"), pct_seg(s.cpu_pct), watts_seg(s.cpu_watts)];
    if let Some(t) = s.cpu_temp {
        cpu.push((format!(" {:>3.0}°C", t), temp_color(t.round() as u32)));
    }
    lines.push(cpu);

    let mut gpu = vec![label("GPU"), pct_seg(s.gpu_pct), watts_seg(s.gpu_watts)];
    if let Some(t) = s.gpu_temp {
        gpu.push((format!(" {t:>3}°C"), temp_color(t)));
    }
    lines.push(gpu);

    if let Some((used, total)) = s.vram {
        lines.push(vec![label("VRAM"), pct_seg(Some(used / total * 100.0)), (format!(" {used:.1}/{total:.1}G"), DIM)]);
    }

    lines.push(vec![
        label("RAM"),
        pct_seg(Some(s.ram_pct)),
        (format!(" {:.1}/{:.1}G", s.ram_used_gb, s.ram_total_gb), DIM),
    ]);

    lines.push(vec![label("DL"), (rate(s.net_down), TEXT)]);
    lines.push(vec![label("UL"), (rate(s.net_up), TEXT)]);

    if s.cpu_watts.is_some() || s.gpu_watts.is_some() {
        let total = s.cpu_watts.unwrap_or(0.0) + s.gpu_watts.unwrap_or(0.0);
        lines.push(vec![label("PWR"), (format!("{total:>5.1}W"), WARN), (" CPU+GPU".into(), DIM)]);
    }
    if let Some(w) = s.battery_watts {
        lines.push(vec![label("BAT"), (format!("{w:>5.1}W"), WARN), (" system".into(), DIM)]);
    }

    lines
}
