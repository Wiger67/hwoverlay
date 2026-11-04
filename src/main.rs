//! hwoverlay — a tiny always-on-top, click-through hardware overlay for gaming.
//!
//! Hotkeys:
//!   Ctrl+Shift+F9   show / hide
//!   Ctrl+Shift+F10  move to next screen corner
//!   Ctrl+Shift+F11  move mode: drag with the mouse, right-click for the settings menu, F11 again to lock
//!   Ctrl+Shift+F12  quit
//!
//! Settings (items shown, horizontal layout, position) live in `hwoverlay.ini` next to the exe.

#![windows_subsystem = "windows"]

mod config;
mod pdh;
mod stats;

use std::cell::RefCell;

use config::{Item, Placement, Settings};
use stats::{Snapshot, Stats};
use windows::core::{w, PCWSTR};
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

// Move-mode right-click menu
const ID_ITEM: usize = 100; // + Item index
const ID_HORIZONTAL: usize = 200;
const ID_LOCK: usize = 201;

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
    settings: Settings,
    /// Last sample, kept so a settings change can re-layout without sampling early.
    snap: Snapshot,
    lines: Vec<Line>,
    font: HFONT,
    scale: f32,
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
        let settings = config::load();
        let stats = Stats::new(&settings);
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(REFRESH_MS as u64));
            for line in build_lines(&stats.sample(), &settings) {
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

        let settings = config::load();
        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                stats: Stats::new(&settings),
                settings,
                snap: Snapshot::default(),
                lines: Vec::new(),
                font,
                scale,
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
                    let s = with_app(|app| {
                        app.settings.placement = app.settings.placement.next_corner();
                        app.settings
                    });
                    config::save(&s);
                    layout(hwnd);
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
            let s = with_app(|app| app.drag.take().map(|_| app.settings));
            if let Some(s) = s {
                config::save(&s);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_menu(hwnd);
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

/// Sample the hardware, then re-layout.
unsafe fn tick(hwnd: HWND) {
    with_app(|app| app.snap = app.stats.sample());
    layout(hwnd);
}

/// Rebuild the text from the last sample, resize/reposition the window to fit, and repaint.
unsafe fn layout(hwnd: HWND) {
    let (w, h, placement, scale) = with_app(|app| {
        app.lines = build_lines(&app.snap, &app.settings);
        let (w, h) = measure(app);
        (w, h, app.settings.placement, app.scale)
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

/// Switch to new settings: only rebuild the counters if the set of items changed.
unsafe fn apply_settings(hwnd: HWND, new: Settings, save: bool) {
    with_app(|app| {
        if !new.same_items(&app.settings) {
            app.stats = Stats::new(&new);
        }
        app.settings = new;
    });
    if save {
        config::save(&new);
    }
    layout(hwnd);
}

/// Move-mode right-click menu: pick the items to show, layout, or lock.
unsafe fn show_menu(hwnd: HWND) {
    // Copy, not a borrow: the menu runs a modal loop that keeps delivering WM_TIMER ticks.
    let s = with_app(|app| app.settings);
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };
    let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING | MF_UNCHECKED };
    for (i, item) in Item::ALL.into_iter().enumerate() {
        let _ = AppendMenuW(menu, check(s.shows(item)), ID_ITEM + i, item.menu_label());
    }
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, check(s.horizontal), ID_HORIZONTAL, w!("橫排顯示"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_LOCK, w!("鎖定位置\tCtrl+Shift+F11"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // A popup menu only closes on an outside click if its owner is the foreground window.
    let _ = SetForegroundWindow(hwnd);
    let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON;
    let cmd = TrackPopupMenu(menu, flags, pt.x, pt.y, 0, hwnd, None).0 as usize;
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    let mut new = s;
    match cmd {
        ID_LOCK => return set_move_mode(hwnd, false),
        ID_HORIZONTAL => new.horizontal ^= true,
        c if (ID_ITEM..ID_ITEM + Item::ALL.len()).contains(&c) => new.toggle(Item::ALL[c - ID_ITEM]),
        _ => return, // dismissed
    }
    apply_settings(hwnd, new, true);
}

/// Toggle move mode by flipping WS_EX_TRANSPARENT (click-through) on and off.
/// Entering it also re-reads hwoverlay.ini, so edits made in Notepad take effect.
unsafe fn set_move_mode(hwnd: HWND, on: bool) {
    if on && !with_app(|app| app.moving) {
        let mut loaded = config::load();
        loaded.placement = with_app(|app| app.settings.placement);
        apply_settings(hwnd, loaded, false);
    }
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

    with_app(|app| app.settings.placement = Placement::Free { x, y });
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

fn build_lines(s: &Snapshot, cfg: &Settings) -> Vec<Line> {
    let mut lines = if cfg.horizontal { vec![compact_line(s, cfg)] } else { column(s, cfg) };
    lines.retain(|l| !l.is_empty());
    if lines.is_empty() {
        // Everything switched off: keep something visible to drag / right-click.
        lines.push(vec![("hwoverlay".into(), DIM)]);
    }
    lines
}

fn temp_seg(t: Option<f64>) -> Option<(String, COLORREF)> {
    t.map(|t| (format!(" {t:>3.0}°C"), temp_color(t.round() as u32)))
}

/// One item per line (the default layout).
fn column(s: &Snapshot, cfg: &Settings) -> Vec<Line> {
    let mut lines = Vec::new();

    if cfg.shows(Item::Cpu) {
        let mut cpu = vec![label("CPU"), pct_seg(s.cpu_pct), watts_seg(s.cpu_watts)];
        cpu.extend(temp_seg(s.cpu_temp));
        lines.push(cpu);
    }
    if cfg.shows(Item::Gpu) {
        let mut gpu = vec![label("GPU"), pct_seg(s.gpu_pct), watts_seg(s.gpu_watts)];
        gpu.extend(temp_seg(s.gpu_temp.map(f64::from)));
        lines.push(gpu);
    }
    if let (true, Some((used, total))) = (cfg.shows(Item::Vram), s.vram) {
        lines.push(vec![label("VRAM"), pct_seg(Some(used / total * 100.0)), (format!(" {used:.1}/{total:.1}G"), DIM)]);
    }
    if cfg.shows(Item::Ram) {
        lines.push(vec![
            label("RAM"),
            pct_seg(Some(s.ram_pct)),
            (format!(" {:.1}/{:.1}G", s.ram_used_gb, s.ram_total_gb), DIM),
        ]);
    }
    if cfg.shows(Item::Net) {
        lines.push(vec![label("DL"), (rate(s.net_down), TEXT)]);
        lines.push(vec![label("UL"), (rate(s.net_up), TEXT)]);
    }
    if cfg.shows(Item::Power) {
        if s.cpu_watts.is_some() || s.gpu_watts.is_some() {
            let total = s.cpu_watts.unwrap_or(0.0) + s.gpu_watts.unwrap_or(0.0);
            lines.push(vec![label("PWR"), (format!("{total:>5.1}W"), WARN), (" CPU+GPU".into(), DIM)]);
        }
        if let Some(w) = s.battery_watts {
            lines.push(vec![label("BAT"), (format!("{w:>5.1}W"), WARN), (" system".into(), DIM)]);
        }
    }
    lines
}

/// Network rate in at most 5 characters, e.g. ` 1.2M`, `  85K`, so the line keeps its width.
fn compact_rate(bytes_per_sec: f64) -> String {
    const K: f64 = 1024.0;
    let s = if bytes_per_sec >= K * K { format!("{:.1}M", bytes_per_sec / (K * K)) } else { format!("{:.0}K", bytes_per_sec / K) };
    format!("{s:>5}")
}

/// Everything on one short line: `CPU  23%  55°C │ GPU  41%  63°C │ RAM  48% │ ↓ 1.2M ↑  85K │ PWR  77W`.
/// Detail (GB used, per-chip watts) is left out to keep it narrow.
fn compact_line(s: &Snapshot, cfg: &Settings) -> Line {
    let tag = |t: &str| (format!("{t} "), LABEL);
    let mut groups: Vec<Line> = Vec::new();

    if cfg.shows(Item::Cpu) {
        let mut g = vec![tag("CPU"), pct_seg(s.cpu_pct)];
        g.extend(temp_seg(s.cpu_temp));
        groups.push(g);
    }
    if cfg.shows(Item::Gpu) {
        let mut g = vec![tag("GPU"), pct_seg(s.gpu_pct)];
        g.extend(temp_seg(s.gpu_temp.map(f64::from)));
        groups.push(g);
    }
    if let (true, Some((used, total))) = (cfg.shows(Item::Vram), s.vram) {
        groups.push(vec![tag("VRAM"), pct_seg(Some(used / total * 100.0))]);
    }
    if cfg.shows(Item::Ram) {
        groups.push(vec![tag("RAM"), pct_seg(Some(s.ram_pct))]);
    }
    if cfg.shows(Item::Net) {
        groups.push(vec![
            ("↓".into(), LABEL),
            (compact_rate(s.net_down), TEXT),
            (" ↑".into(), LABEL),
            (compact_rate(s.net_up), TEXT),
        ]);
    }
    if cfg.shows(Item::Power) {
        let mut g = Vec::new();
        if s.cpu_watts.is_some() || s.gpu_watts.is_some() {
            let total = s.cpu_watts.unwrap_or(0.0) + s.gpu_watts.unwrap_or(0.0);
            g.extend([tag("PWR"), (format!("{total:>3.0}W"), WARN)]);
        }
        if let Some(w) = s.battery_watts {
            g.extend([tag(if g.is_empty() { "BAT" } else { " BAT" }), (format!("{w:>3.0}W"), WARN)]);
        }
        groups.push(g);
    }

    groups.retain(|g| !g.is_empty());
    let mut line = Line::new();
    for (i, g) in groups.into_iter().enumerate() {
        if i > 0 {
            line.push((" │ ".into(), DIM));
        }
        line.extend(g);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(l: &Line) -> String {
        l.iter().map(|(s, _)| s.as_str()).collect()
    }

    fn sample() -> Snapshot {
        Snapshot {
            cpu_pct: Some(23.0),
            cpu_watts: Some(15.2),
            cpu_temp: Some(55.0),
            gpu_pct: Some(41.0),
            gpu_watts: Some(62.0),
            gpu_temp: Some(63),
            vram: Some((2.0, 8.0)),
            ram_pct: 48.0,
            ram_used_gb: 7.6,
            ram_total_gb: 16.0,
            net_down: 1.2 * 1024.0 * 1024.0,
            net_up: 85.0 * 1024.0,
            battery_watts: None,
        }
    }

    #[test]
    fn compact_layout() {
        let mut cfg = Settings::default();
        cfg.horizontal = true;
        let lines = build_lines(&sample(), &cfg);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            text(&lines[0]),
            "CPU  23%  55°C │ GPU  41%  63°C │ VRAM  25% │ RAM  48% │ ↓ 1.2M ↑  85K │ PWR  77W"
        );
    }

    #[test]
    fn hidden_items_are_dropped() {
        let mut cfg = Settings::default();
        for item in [Item::Gpu, Item::Vram, Item::Net, Item::Power] {
            cfg.toggle(item);
        }
        let column: Vec<String> = build_lines(&sample(), &cfg).iter().map(text).collect();
        assert_eq!(column, ["CPU   23%  15.2W  55°C", "RAM   48% 7.6/16.0G"]);

        cfg.horizontal = true;
        assert_eq!(text(&build_lines(&sample(), &cfg)[0]), "CPU  23%  55°C │ RAM  48%");

        cfg.toggle(Item::Cpu);
        cfg.toggle(Item::Ram);
        assert_eq!(text(&build_lines(&sample(), &cfg)[0]), "hwoverlay");
    }

    #[test]
    fn compact_rate_is_fixed_width() {
        for b in [0.0, 900.0, 85.0 * 1024.0, 999.0 * 1024.0, 1.2 * 1024.0 * 1024.0, 55.5 * 1024.0 * 1024.0] {
            assert_eq!(compact_rate(b).chars().count(), 5, "{b}");
        }
    }
}
