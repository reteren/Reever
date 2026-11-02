// Замеры процесса для нативных окон: рабочий набор (RAM) и загрузка CPU за промежуток.
// Нужны демо и воротам этапов роудмапа (docs/research/native-ui-measurements.md).

use std::time::Instant;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

/// Память процесса: (рабочий набор, частные байты).
pub fn memory() -> (usize, usize) {
    let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    if ok.is_err() {
        return (0, 0);
    }
    (c.WorkingSetSize, c.PrivateUsage)
}

/// Процессорное время процесса (ядро + пользователь), в секундах.
pub fn cpu_seconds() -> f64 {
    let (mut create, mut exit, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut create, &mut exit, &mut kernel, &mut user) }.is_err() {
        return 0.0;
    }
    (filetime_100ns(kernel) + filetime_100ns(user)) as f64 / 1e7
}

fn filetime_100ns(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

pub fn logical_cpus() -> u32 {
    let mut info = SYSTEM_INFO::default();
    unsafe { GetSystemInfo(&mut info) };
    info.dwNumberOfProcessors.max(1)
}

/// Загрузка CPU за промежуток между `start()` и `finish()`.
pub struct CpuMeter {
    wall: Instant,
    cpu: f64,
}

/// Итог замера CPU.
#[derive(Debug, Clone, Copy)]
pub struct CpuUsage {
    /// Процент одного логического ядра (100 % = одно ядро занято целиком).
    pub of_one_core: f64,
    /// Процент всего процессора, как в диспетчере задач.
    pub of_total: f64,
}

impl CpuMeter {
    pub fn start() -> Self {
        Self { wall: Instant::now(), cpu: cpu_seconds() }
    }

    pub fn finish(&self) -> CpuUsage {
        let wall = self.wall.elapsed().as_secs_f64().max(1e-9);
        let one = (cpu_seconds() - self.cpu) / wall * 100.0;
        CpuUsage { of_one_core: one, of_total: one / logical_cpus() as f64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_and_cpu_are_readable() {
        let (ws, private) = memory();
        assert!(ws > 1024 * 1024 && private > 0);
        let meter = CpuMeter::start();
        let mut x = 0u64;
        for i in 0..2_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        std::hint::black_box(x);
        let u = meter.finish();
        assert!(u.of_one_core >= 0.0 && u.of_total <= u.of_one_core);
    }
}
