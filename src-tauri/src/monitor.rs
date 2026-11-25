use crate::{error, lock, Result, Shared};
use serde::Serialize;
use std::{
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::Child,
    time::{Duration, Instant},
};
use tauri::State;
use windows_sys::Win32::{
    Foundation::*,
    System::{ProcessStatus::*, SystemInformation::*, Threading::*},
};

pub struct Process {
    handle: OwnedHandle,
    last: Option<(Instant, u64)>,
    cached: Sample,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    cpu_percent: Option<f64>,
    working_set: Option<u64>,
    private_bytes: Option<u64>,
    sampled_at: u64,
    error: Option<String>,
}
fn ticks(v: FILETIME) -> u64 {
    (u64::from(v.dwHighDateTime) << 32) | u64::from(v.dwLowDateTime)
}
fn percentage(delta: u64, elapsed: Duration, cpus: u32) -> Option<f64> {
    if elapsed.as_secs_f64() <= 0.0 || cpus == 0 {
        return None;
    }
    Some(
        (delta as f64 / 10_000_000.0 / elapsed.as_secs_f64() / f64::from(cpus) * 100.0)
            .clamp(0.0, 100.0),
    )
}
impl Process {
    pub fn capture(child: &Child) -> Result<Self> {
        let mut copy = std::ptr::null_mut();
        unsafe {
            if DuplicateHandle(
                GetCurrentProcess(),
                child.as_raw_handle(),
                GetCurrentProcess(),
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) == 0
            {
                return Err(error(std::io::Error::last_os_error()));
            }
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(copy) };
        Ok(Self {
            handle,
            last: None,
            cached: Sample::default(),
        })
    }
    pub fn sample(&mut self) -> Sample {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|(at, _)| now.duration_since(at) < Duration::from_millis(900))
        {
            return self.cached.clone();
        }
        let mut sample = Sample {
            sampled_at: crate::backup_schedule::now(),
            ..Default::default()
        };
        unsafe {
            let mut created = std::mem::zeroed();
            let mut exited = std::mem::zeroed();
            let mut kernel = std::mem::zeroed();
            let mut user = std::mem::zeroed();
            if GetProcessTimes(
                self.handle.as_raw_handle(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            ) != 0
            {
                let total = ticks(kernel).saturating_add(ticks(user));
                if let Some((previous, old)) = self.last {
                    sample.cpu_percent = percentage(
                        total.saturating_sub(old),
                        now.duration_since(previous),
                        GetActiveProcessorCount(0xffff),
                    );
                }
                self.last = Some((now, total));
            } else {
                sample.error = Some("Windows CPU counters unavailable".into());
            }
            let mut memory: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            memory.cb = std::mem::size_of_val(&memory) as u32;
            if GetProcessMemoryInfo(
                self.handle.as_raw_handle(),
                (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                memory.cb,
            ) != 0
            {
                sample.working_set = Some(memory.WorkingSetSize as u64);
                sample.private_bytes = Some(memory.PrivateUsage as u64);
            } else {
                sample.error = Some("Windows process memory counters unavailable".into());
            }
        }
        self.cached = sample.clone();
        sample
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerMetrics {
    id: String,
    pid: u32,
    uptime_seconds: u64,
    stopping: bool,
    observed_online: usize,
    sample: Sample,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    processes: Vec<ServerMetrics>,
    total_working_set: u64,
    total_private_bytes: u64,
    total_cpu_percent: Option<f64>,
    complete: bool,
    physical_total: Option<u64>,
    physical_available: Option<u64>,
    heap_reserved_mib: u64,
}
pub fn snapshot(shared: &Shared) -> Result<Metrics> {
    let mut inner = lock(shared)?;
    let mut processes = Vec::new();
    for (id, running) in &mut inner.running {
        let sample = running
            .process
            .as_mut()
            .map(Process::sample)
            .unwrap_or_else(|| Sample {
                error: Some("Process measurement handle unavailable".into()),
                ..Default::default()
            });
        processes.push(ServerMetrics {
            id: id.clone(),
            pid: running.pid,
            uptime_seconds: running.started.elapsed().as_secs(),
            stopping: running.intent != crate::lifecycle::Intent::None,
            observed_online: running.connected.len(),
            sample,
        });
    }
    processes.sort_by(|a, b| a.id.cmp(&b.id));
    let complete = processes
        .iter()
        .all(|r| r.sample.working_set.is_some() && r.sample.private_bytes.is_some());
    let cpu = processes
        .iter()
        .map(|r| r.sample.cpu_percent)
        .collect::<Option<Vec<_>>>()
        .map(|v| v.iter().sum::<f64>().min(100.0));
    let mut memory: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    memory.dwLength = std::mem::size_of_val(&memory) as u32;
    let available = unsafe { GlobalMemoryStatusEx(&mut memory) } != 0;
    Ok(Metrics {
        total_working_set: processes.iter().filter_map(|r| r.sample.working_set).sum(),
        total_private_bytes: processes
            .iter()
            .filter_map(|r| r.sample.private_bytes)
            .sum(),
        total_cpu_percent: cpu,
        complete,
        physical_total: available.then_some(memory.ullTotalPhys),
        physical_available: available.then_some(memory.ullAvailPhys),
        heap_reserved_mib: inner.running.values().map(|r| u64::from(r.ram)).sum(),
        processes,
    })
}
#[tauri::command]
pub fn server_metrics(state: State<'_, Shared>) -> Result<Metrics> {
    snapshot(&state)
}
#[cfg(test)]
mod tests {
    use super::*;
    struct TestChild(Child);
    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    #[test]
    fn cpu_is_normalized_and_unknown_first_sample_is_preserved() {
        assert_eq!(
            percentage(20_000_000, Duration::from_secs(1), 4),
            Some(50.0)
        );
        assert_eq!(percentage(10, Duration::ZERO, 4), None);
        let child = TestChild(
            std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "ping -n 4 127.0.0.1 >nul"])
                .spawn()
                .unwrap(),
        );
        std::thread::sleep(Duration::from_millis(100));
        let mut process = Process::capture(&child.0).unwrap();
        let first = process.sample();
        assert!(first.cpu_percent.is_none());
        assert!(first.working_set.unwrap() > 0);
        assert!(first.private_bytes.is_some());
        std::thread::sleep(Duration::from_millis(1100));
        let second = process.sample();
        assert!(second.cpu_percent.is_some());
    }
}
