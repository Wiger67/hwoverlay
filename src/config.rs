//! Settings in `hwoverlay.ini`, next to the exe (portable; edit it with Notepad).
//!
//! ```ini
//! [display]
//! horizontal=0     ; 1 = one compact line instead of a column
//! cpu=1            ; 0 hides the item AND stops collecting it
//! gpu=1
//! vram=1
//! ram=1
//! net=1
//! power=1
//! [window]
//! position=corner 0   ; corner 0..3 (TL, TR, BR, BL) or "pos X Y" in screen pixels
//! ```
//!
//! Read and written with the Win32 INI API (GetPrivateProfile* / WritePrivateProfileString) that
//! lives in kernel32: a few KB of code, where std::fs would add ~19 KB. Written only when something
//! changes, so there is no ongoing disk activity.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::WindowsProgramming::{
    GetPrivateProfileIntW, GetPrivateProfileStringW, WritePrivateProfileStringW,
};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Placement {
    Corner(u8),
    /// Top-left of the window in virtual-screen coordinates.
    Free { x: i32, y: i32 },
}

impl Placement {
    /// Cycle TL → TR → BR → BL; a dragged position restarts at TL.
    pub fn next_corner(self) -> Self {
        match self {
            Placement::Corner(c) => Placement::Corner((c + 1) % 4),
            Placement::Free { .. } => Placement::Corner(0),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        let mut it = s.split_whitespace();
        match it.next()? {
            "corner" => Some(Placement::Corner(it.next()?.parse::<u8>().ok()? % 4)),
            "pos" => Some(Placement::Free { x: it.next()?.parse().ok()?, y: it.next()?.parse().ok()? }),
            _ => None,
        }
    }

    fn serialize(self) -> String {
        match self {
            Placement::Corner(c) => format!("corner {c}"),
            Placement::Free { x, y } => format!("pos {x} {y}"),
        }
    }
}

/// Things that can be shown. Hidden items are not sampled either.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Item {
    Cpu,
    Gpu,
    Vram,
    Ram,
    Net,
    Power,
}

impl Item {
    pub const ALL: [Item; 6] = [Item::Cpu, Item::Gpu, Item::Vram, Item::Ram, Item::Net, Item::Power];

    fn key(self) -> PCWSTR {
        match self {
            Item::Cpu => w!("cpu"),
            Item::Gpu => w!("gpu"),
            Item::Vram => w!("vram"),
            Item::Ram => w!("ram"),
            Item::Net => w!("net"),
            Item::Power => w!("power"),
        }
    }

    /// Label in the move-mode right-click menu.
    pub fn menu_label(self) -> PCWSTR {
        match self {
            Item::Cpu => w!("CPU"),
            Item::Gpu => w!("GPU"),
            Item::Vram => w!("VRAM 顯示記憶體"),
            Item::Ram => w!("RAM 記憶體"),
            Item::Net => w!("網路 上傳 / 下載"),
            Item::Power => w!("功耗 PWR / BAT"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Settings {
    pub horizontal: bool,
    show: [bool; Item::ALL.len()],
    pub placement: Placement,
}

impl Default for Settings {
    fn default() -> Self {
        Self { horizontal: false, show: [true; Item::ALL.len()], placement: Placement::Corner(0) }
    }
}

impl Settings {
    pub fn shows(&self, item: Item) -> bool {
        self.show[item as usize]
    }

    pub fn toggle(&mut self, item: Item) {
        self.show[item as usize] ^= true;
    }

    /// Same items selected (i.e. the set of counters to collect is unchanged).
    pub fn same_items(&self, other: &Settings) -> bool {
        self.show == other.show
    }
}

/// `<exe dir>\hwoverlay.ini`. Rebuilt on each call: it's only used at startup and when saving.
fn ini_path() -> HSTRING {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    let dir = buf[..n].iter().rposition(|&c| c == b'\\' as u16).map_or(0, |i| i + 1);
    let name: Vec<u16> = buf[..dir].iter().copied().chain("hwoverlay.ini".encode_utf16()).collect();
    HSTRING::from_wide(&name).unwrap_or_default()
}

const DISPLAY: PCWSTR = w!("display");
const WINDOW: PCWSTR = w!("window");

fn read_bool(key: PCWSTR, default: bool) -> bool {
    unsafe { GetPrivateProfileIntW(DISPLAY, key, default as i32, &ini_path()) != 0 }
}

pub fn load() -> Settings {
    let d = Settings::default();
    let mut s = d;
    s.horizontal = read_bool(w!("horizontal"), d.horizontal);
    for item in Item::ALL {
        s.show[item as usize] = read_bool(item.key(), true);
    }

    let mut buf = [0u16; 64];
    let n = unsafe { GetPrivateProfileStringW(WINDOW, w!("position"), w!(""), Some(&mut buf), &ini_path()) };
    match Placement::parse(&String::from_utf16_lossy(&buf[..n as usize])) {
        Some(p) => s.placement = p,
        // No file (first run) or no position yet: write a complete file so there is something to edit.
        None => save(&s),
    }
    s
}

/// Best effort: if the exe's folder is read-only the settings just aren't remembered.
pub fn save(s: &Settings) {
    let write = |section: PCWSTR, key: PCWSTR, value: &str| unsafe {
        let _ = WritePrivateProfileStringW(section, key, &HSTRING::from(value), &ini_path());
    };
    let flag = |b: bool| if b { "1" } else { "0" };
    write(DISPLAY, w!("horizontal"), flag(s.horizontal));
    for item in Item::ALL {
        write(DISPLAY, item.key(), flag(s.shows(item)));
    }
    write(WINDOW, w!("position"), &s.placement.serialize());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_roundtrip() {
        for p in [Placement::Corner(2), Placement::Free { x: -1200, y: 34 }] {
            assert_eq!(Placement::parse(&p.serialize()), Some(p));
        }
        assert_eq!(Placement::parse("garbage"), None);
        assert_eq!(Placement::parse("pos 10"), None);
        assert_eq!(Placement::parse(""), None);
    }
}
