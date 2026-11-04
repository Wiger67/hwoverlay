//! hwoverlay — a tiny always-on-top, click-through hardware overlay for gaming.
//!
//! Hotkeys:
//!   Ctrl+Shift+F9   show / hide
//!   Ctrl+Shift+F10  move to next screen corner
//!   Ctrl+Shift+F11  move mode: drag with the mouse, right-click or F11 again to lock
//!   Ctrl+Shift+F12  quit

#![windows_subsystem = "windows"]

mod config;
mod pdh;
mod stats;

use std::cell::RefCell;

use config::Placement;
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
const SNAP_PX: f32 = 16.0; // while dragging, stick to the screen margin when this close
const BORDER_PX: f32 = 2.0; // move-mode outline

const HK_TOGGLE: i32 = 1;
const HK_CORNER: i32 = 2;
const HK_QUIT: i32 = 3;
const HK_MOVE: i32 = 4;

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
    placement: Placement,
    visible: bool,
    /// Move mode: window accepts the mouse (not click-through) and can be dragged.
    moving: bool,
    /// While dragging: cursor offset from the window's top-left corner.
    drag: Option<POINT>,
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
                placement: config::load(),
                visible: true,
                moving: false,
                drag: None,
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
        let _ = RegisterHotKey(hwnd, HK_MOVE, mods, VK_F11.0 as u32);
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
                HK_TOGGLE => {
                    let visible = with_app(|app| {
                        app.visible = !app.visible;
                        app.visible
                    });
                    if !visible {
                        set_move_mode(hwnd, false); // don't leave a hidden window catching clicks
                    }
                    let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
                }
                HK_CORNER => {
                    let p = with_app(|app| {
                        app.placement = app.placement.next_corner();
                        app.placement
                    });
                    config::save(p);
                    tick(hwnd);
                }
                HK_MOVE => set_move_mode(hwnd, !with_app(|app| app.moving)),
                HK_QUIT => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        // ----- move mode only; while click-through the window never sees the mouse -----
        WM_SETCURSOR => {
            let _ = SetCursor(LoadCursorW(None, IDC_SIZEALL).unwrap_or_default());
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            let mut cur = POINT::default();
            let mut rc = RECT::default();
            let _ = GetCursorPos(&mut cur);
            let _ = GetWindowRect(hwnd, &mut rc);
            with_app(|app| app.drag = Some(POINT { x: cur.x - rc.left, y: cur.y - rc.top }));
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            drag_to_cursor(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // ReleaseCapture sends WM_CAPTURECHANGED synchronously, so don't hold the APP borrow across it.
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            let p = with_app(|app| app.drag.take().map(|_| app.placement));
            if let Some(p) = p {
                config::save(p);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            set_move_mode(hwnd, false);
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
    let (w, h, placement, scale) = with_app(|app| {
        let snap = app.stats.sample();
        app.lines = build_lines(&snap);
        let (w, h) = measure(app);
        (w, h, app.placement, app.scale)
    });

    let m = (MARGIN_PX * scale) as i32;
    let (x, y) = match placement {
        Placement::Corner(c) => {
            let sw = GetSystemMetrics(SM_CXSCREEN);
            let sh = GetSystemMetrics(SM_CYSCREEN);
            match c {
                0 => (m, m),
                1 => (sw - w - m, m),
                2 => (sw - w - m, sh - h - m),
                _ => (m, sh - h - m),
            }
        }
        // Clamp so a saved spot on a since-unplugged monitor (or a wider layout) stays on screen.
        Placement::Free { x, y } => {
            let mon = monitor_rect(x, y);
            (x.clamp(mon.left, (mon.right - w).max(mon.left)), y.clamp(mon.top, (mon.bottom - h).max(mon.top)))
        }
    };
    // Re-assert topmost every tick: some games push themselves above other topmost windows.
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
    let _ = InvalidateRect(hwnd, None, false);
}

/// Full bounds of the monitor nearest to (x, y).
unsafe fn monitor_rect(x: i32, y: i32) -> RECT {
    let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let _ = GetMonitorInfoW(mon, &mut mi);
    mi.rcMonitor
}

/// Toggle move mode by flipping WS_EX_TRANSPARENT (click-through) on and off.
unsafe fn set_move_mode(hwnd: HWND, on: bool) {
    let changed = with_app(|app| {
        let changed = app.moving != on;
        app.moving = on;
        if on && !app.visible {
            app.visible = true;
        }
        changed
    });
    if !changed {
        return;
    }
    let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    let ex = if on { ex & !(WS_EX_TRANSPARENT.0 as isize) } else { ex | WS_EX_TRANSPARENT.0 as isize };
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
    if on {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    } else {
        let _ = ReleaseCapture(); // saves position via WM_CAPTURECHANGED if a drag was in progress
    }
    let _ = InvalidateRect(hwnd, None, false);
}

/// Follow the cursor while dragging, snapping to the screen margin near edges.
unsafe fn drag_to_cursor(hwnd: HWND) {
    let Some((off, scale)) = with_app(|app| app.drag.map(|d| (d, app.scale))) else {
        return;
    };
    let mut cur = POINT::default();
    let mut rc = RECT::default();
    let _ = GetCursorPos(&mut cur);
    let _ = GetWindowRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let (mut x, mut y) = (cur.x - off.x, cur.y - off.y);

    let mon = monitor_rect(cur.x, cur.y);
    let m = (MARGIN_PX * scale) as i32;
    let snap = (SNAP_PX * scale) as i32;
    let stick = |v: i32, target: i32| if (v - target).abs() <= snap { target } else { v };
    x = stick(stick(x, mon.left + m), mon.right - w - m);
    y = stick(stick(y, mon.top + m), mon.bottom - h - m);

    with_app(|app| app.placement = Placement::Free { x, y });
    let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
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

        if app.moving {
            let b = ((BORDER_PX * app.scale) as i32).max(1);
            let brush = CreateSolidBrush(WARN);
            for edge in [
                RECT { right: b, ..rc },
                RECT { left: rc.right - b, ..rc },
                RECT { bottom: b, ..rc },
                RECT { top: rc.bottom - b, ..rc },
            ] {
                FillRect(mem, &edge, brush);
            }
            let _ = DeleteObject(brush);
        }
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
