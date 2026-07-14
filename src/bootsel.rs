use anyhow::Result;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::Disks;

/// Find a BOOTSEL USB storage drive by looking for the INFO_UF2.TXT sentinel file.
pub fn find_bootsel_drive() -> Option<PathBuf> {
    let disks = Disks::new_with_refreshed_list();
    for disk in &disks {
        let mount = disk.mount_point();
        if mount.join("INFO_UF2.TXT").is_file() {
            log::info!("Found BOOTSEL drive at {}", mount.display());
            return Some(mount.to_owned());
        }
    }
    None
}

/// Poll for a BOOTSEL drive until one appears or `timeout` elapses.
pub fn wait_for_bootsel_drive(timeout: Duration) -> Result<PathBuf> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(path) = find_bootsel_drive() {
            return Ok(path);
        }
        thread::sleep(Duration::from_millis(250));
    }
    anyhow::bail!("Timed out waiting for BOOTSEL drive to appear")
}
