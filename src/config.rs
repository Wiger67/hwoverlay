//! Remembers where the overlay sits, in the registry: `HKCU\Software\hwoverlay`, value `Placement`.
//!
//! A REG_SZ string, either `corner N` (0 TL, 1 TR, 2 BR, 3 BL) or `pos X Y` (screen pixels).
//! Written only when the position changes, so there is no ongoing disk activity.
//! (Registry instead of a file: std::fs adds ~19 KB to the exe, this adds almost nothing.)

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ};

const KEY: PCWSTR = w!("Software\\hwoverlay");
const VALUE: PCWSTR = w!("Placement");

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

pub fn load() -> Placement {
    let mut buf = [0u16; 64];
    let mut len = std::mem::size_of_val(&buf) as u32;
    let err = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, KEY, VALUE, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len))
    };
    if err.is_err() {
        return Placement::Corner(0);
    }
    let s = String::from_utf16_lossy(&buf[..(len as usize / 2)]);
    Placement::parse(s.trim_end_matches('\0')).unwrap_or(Placement::Corner(0))
}

/// Best effort: if the write fails the position just isn't remembered.
pub fn save(p: Placement) {
    let wide: Vec<u16> = p.serialize().encode_utf16().chain(Some(0)).collect();
    unsafe {
        // Creates the key if it doesn't exist yet.
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            KEY,
            VALUE,
            REG_SZ.0,
            Some(wide.as_ptr().cast()),
            std::mem::size_of_val(wide.as_slice()) as u32,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for p in [Placement::Corner(2), Placement::Free { x: -1200, y: 34 }] {
            assert_eq!(Placement::parse(&p.serialize()), Some(p));
        }
        assert_eq!(Placement::parse("garbage"), None);
        assert_eq!(Placement::parse("pos 10"), None);
    }
}
