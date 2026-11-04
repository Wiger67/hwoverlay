//! Hardware data collection.
//!
//! - CPU usage / CPU package power / CPU temperature (ACPI thermal zone) / system power / network / fallback GPU usage: PDH
//! - RAM: GlobalMemoryStatusEx
//! - NVIDIA GPU usage, power, temperature, VRAM: NVML (nvml.dll ships with the driver)

use std::collections::HashMap;

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

use crate::pdh::{Counter, Query};

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Default, Clone)]
pub struct Snapshot {
    pub cpu_pct: Option<f64>,
    pub cpu_watts: Option<f64>,
    pub cpu_temp: Option<f64>,
    pub gpu_pct: Option<f64>,
    pub gpu_watts: Option<f64>,
    pub gpu_temp: Option<u32>,
    /// (used GB, total GB)
    pub vram: Option<(f64, f64)>,
    pub ram_pct: f64,
    pub ram_used_gb: f64,
    pub ram_total_gb: f64,
    /// bytes per second
    pub net_down: f64,
    pub net_up: f64,
    /// Whole-system draw, only meaningful while running on battery.
    pub battery_watts: Option<f64>,
}

pub struct Stats {
    query: Query,
    cpu: Option<Counter>,
    cpu_power: Option<Counter>,
    /// (counter, divisor to Kelvin)
    cpu_temp: Option<(Counter, f64)>,
    sys_power: Option<Counter>,
    gpu_3d: Option<Counter>,
    net_rx: Option<Counter>,
    net_tx: Option<Counter>,
    nvml: Option<&'static Nvml>,
}

impl Stats {
    pub fn new() -> Self {
        let query = Query::new().expect("PdhOpenQuery failed");
        let cpu = query
            .add(r"\Processor Information(_Total)\% Processor Utility")
            .or_else(|| query.add(r"\Processor(_Total)\% Processor Time"));
        // RAPL package power in milliwatts (Intel, and AMD on recent Windows builds).
        let cpu_power = query.add(r"\Energy Meter(RAPL_Package0_PKG)\Power");
        // ACPI thermal zones reported by the embedded controller; on most laptops the hottest is the CPU.
        // "High Precision" is in tenths of Kelvin (Win10 1809+), the plain one in whole Kelvin.
        let cpu_temp = query
            .add(r"\Thermal Zone Information(*)\High Precision Temperature")
            .map(|c| (c, 10.0))
            .or_else(|| query.add(r"\Thermal Zone Information(*)\Temperature").map(|c| (c, 1.0)));
        // Battery-reported system draw in milliwatts (laptops).
        let sys_power = query.add(r"\Power Meter(_Total)\Power");
        let net_rx = query.add(r"\Network Interface(*)\Bytes Received/sec");
        let net_tx = query.add(r"\Network Interface(*)\Bytes Sent/sec");

        // Leaked once so Device<'static> borrows are trivial; lives for the whole process anyway.
        let nvml = Nvml::init().ok().map(|n| &*Box::leak(Box::new(n)));
        let gpu_3d = if nvml.is_none() {
            query.add(r"\GPU Engine(*engtype_3D)\Utilization Percentage")
        } else {
            None
        };

        let stats = Self { query, cpu, cpu_power, cpu_temp, sys_power, gpu_3d, net_rx, net_tx, nvml };
        // Rate counters need two samples; prime the first one now.
        stats.query.collect();
        stats
    }

    pub fn sample(&self) -> Snapshot {
        self.query.collect();
        let mut s = Snapshot {
            cpu_pct: self.cpu.as_ref().and_then(Counter::value).map(|v| v.min(100.0)),
            cpu_watts: self.cpu_power.as_ref().and_then(Counter::value).map(|mw| mw / 1000.0),
            cpu_temp: self.cpu_temp.as_ref().and_then(|(c, div)| {
                let kelvin = c.values().into_iter().map(|(_, v)| v / div).reduce(f64::max)?;
                // Ignore zones that report nothing useful (0 K or obviously bogus readings).
                Some(kelvin - 273.15).filter(|&c| (1.0..130.0).contains(&c))
            }),
            net_down: sum(&self.net_rx),
            net_up: sum(&self.net_tx),
            ..Default::default()
        };

        if on_battery() {
            s.battery_watts = self
                .sys_power
                .as_ref()
                .and_then(Counter::value)
                .filter(|&mw| mw > 0.0)
                .map(|mw| mw / 1000.0);
        }

        let mut mem = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
            s.ram_total_gb = mem.ullTotalPhys as f64 / GB;
            s.ram_used_gb = (mem.ullTotalPhys - mem.ullAvailPhys) as f64 / GB;
            s.ram_pct = s.ram_used_gb / s.ram_total_gb * 100.0;
        }

        if let Some(dev) = self.nvml.and_then(|n| n.device_by_index(0).ok()) {
            s.gpu_pct = dev.utilization_rates().ok().map(|u| u.gpu as f64);
            s.gpu_watts = dev.power_usage().ok().map(|mw| mw as f64 / 1000.0);
            s.gpu_temp = dev.temperature(TemperatureSensor::Gpu).ok();
            s.vram = dev.memory_info().ok().map(|m| (m.used as f64 / GB, m.total as f64 / GB));
        } else if let Some(c) = &self.gpu_3d {
            s.gpu_pct = busiest_adapter(c.values());
        }
        s
    }
}

fn sum(c: &Option<Counter>) -> f64 {
    c.as_ref().map(|c| c.values().iter().map(|(_, v)| v).sum()).unwrap_or(0.0)
}

/// Instance names look like `pid_1234_luid_0x0000_0x0001_phys_0_eng_0_engtype_3D`.
/// Sum per adapter (LUID) and report the busiest one, the same way Task Manager does.
fn busiest_adapter(values: Vec<(String, f64)>) -> Option<f64> {
    let mut per_luid: HashMap<String, f64> = HashMap::new();
    for (name, v) in values {
        let luid = name
            .split_once("luid_")
            .and_then(|(_, rest)| rest.split_once("_phys"))
            .map(|(l, _)| l.to_string())
            .unwrap_or_default();
        *per_luid.entry(luid).or_default() += v;
    }
    per_luid.into_values().reduce(f64::max).map(|v| v.min(100.0))
}

fn on_battery() -> bool {
    let mut ps = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut ps) }.is_ok() && ps.ACLineStatus == 0
}
