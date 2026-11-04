//! Thin wrapper over Windows Performance Data Helper (PDH).
//! English counter paths are used so it works on localized (e.g. zh-TW) Windows.

use windows::core::PCWSTR;
use windows::Win32::System::Performance::*;

const PDH_MORE_DATA: u32 = 0x800007D2;

pub struct Query(isize);
pub struct Counter(isize);

impl Query {
    pub fn new() -> Option<Self> {
        let mut q = 0isize;
        let r = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut q) };
        (r == 0).then_some(Self(q))
    }

    pub fn add(&self, path: &str) -> Option<Counter> {
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut c = 0isize;
        let r = unsafe { PdhAddEnglishCounterW(self.0, PCWSTR(wide.as_ptr()), 0, &mut c) };
        (r == 0).then_some(Counter(c))
    }

    pub fn collect(&self) {
        unsafe {
            PdhCollectQueryData(self.0);
        }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.0);
        }
    }
}

impl Counter {
    pub fn value(&self) -> Option<f64> {
        let mut v = PDH_FMT_COUNTERVALUE::default();
        let r = unsafe { PdhGetFormattedCounterValue(self.0, PDH_FMT_DOUBLE, None, &mut v) };
        // CStatus 0 = valid, 1 = new data
        (r == 0 && v.CStatus <= 1).then(|| unsafe { v.Anonymous.doubleValue })
    }

    /// Values of a wildcard counter, e.g. `\Network Interface(*)\Bytes Received/sec`.
    pub fn values(&self) -> Vec<(String, f64)> {
        let mut size = 0u32;
        let mut count = 0u32;
        unsafe {
            let r = PdhGetFormattedCounterArrayW(self.0, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if r != PDH_MORE_DATA {
                return Vec::new();
            }
            // `size` is in bytes and includes the instance-name strings stored after the items.
            let n = size as usize / std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() + 1;
            let mut buf = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); n];
            let r = PdhGetFormattedCounterArrayW(
                self.0,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                Some(buf.as_mut_ptr()),
            );
            if r != 0 {
                return Vec::new();
            }
            buf[..count as usize]
                .iter()
                .filter(|it| it.FmtValue.CStatus <= 1)
                .map(|it| {
                    let name = it.szName.to_string().unwrap_or_default();
                    (name, it.FmtValue.Anonymous.doubleValue)
                })
                .collect()
        }
    }
}
