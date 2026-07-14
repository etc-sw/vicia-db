use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MemoryBreakdown {
    pub(crate) anonymous_rss_bytes: u64,
    pub(crate) file_backed_rss_bytes: u64,
    pub(crate) heap_mapping_rss_bytes: u64,
    pub(crate) database_mapped_rss_bytes: u64,
}

impl MemoryBreakdown {
    pub(crate) fn saturating_sub(self, baseline: Self) -> Self {
        Self {
            anonymous_rss_bytes: self
                .anonymous_rss_bytes
                .saturating_sub(baseline.anonymous_rss_bytes),
            file_backed_rss_bytes: self
                .file_backed_rss_bytes
                .saturating_sub(baseline.file_backed_rss_bytes),
            heap_mapping_rss_bytes: self
                .heap_mapping_rss_bytes
                .saturating_sub(baseline.heap_mapping_rss_bytes),
            database_mapped_rss_bytes: self
                .database_mapped_rss_bytes
                .saturating_sub(baseline.database_mapped_rss_bytes),
        }
    }
}

pub(crate) fn current_rss_bytes() -> Option<u64> {
    proc_status_kib("VmRSS:")?.checked_mul(1024)
}

pub(crate) fn peak_rss_bytes() -> Option<u64> {
    proc_status_kib("VmHWM:")?.checked_mul(1024)
}

pub(crate) fn memory_breakdown(database_path: &Path) -> Result<MemoryBreakdown> {
    let smaps = fs::read_to_string("/proc/self/smaps").context("read /proc/self/smaps")?;
    let database_path = database_path.canonicalize()?;
    let database_path = database_path.to_string_lossy();
    let mut current_heap = false;
    let mut current_database = false;
    let mut total_rss = 0_u64;
    let mut anonymous_rss = 0_u64;
    let mut heap_rss = 0_u64;
    let mut database_rss = 0_u64;

    for line in smaps.lines() {
        if is_smaps_header(line) {
            let path = line.split_whitespace().nth(5).unwrap_or("");
            current_heap = path == "[heap]";
            current_database = path == database_path.as_ref();
            continue;
        }
        if let Some(bytes) = smaps_kib_value(line, "Rss:") {
            total_rss = total_rss.saturating_add(bytes);
            if current_heap {
                heap_rss = heap_rss.saturating_add(bytes);
            }
            if current_database {
                database_rss = database_rss.saturating_add(bytes);
            }
        } else if let Some(bytes) = smaps_kib_value(line, "Anonymous:") {
            anonymous_rss = anonymous_rss.saturating_add(bytes);
        }
    }
    Ok(MemoryBreakdown {
        anonymous_rss_bytes: anonymous_rss,
        file_backed_rss_bytes: total_rss.saturating_sub(anonymous_rss),
        heap_mapping_rss_bytes: heap_rss,
        database_mapped_rss_bytes: database_rss,
    })
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn trim_allocator() -> (bool, bool) {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }

    // SAFETY: this benchmark child is single-threaded, owns no foreign
    // allocator state, and passes the value accepted by glibc for full trim.
    (true, unsafe { malloc_trim(0) != 0 })
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) fn trim_allocator() -> (bool, bool) {
    (false, false)
}

fn proc_status_kib(field: &str) -> Option<u64> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|line| line.starts_with(field))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn is_smaps_header(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|range| {
        range.contains('-')
            && range
                .bytes()
                .all(|byte| byte == b'-' || byte.is_ascii_hexdigit())
    })
}

fn smaps_kib_value(line: &str, field: &str) -> Option<u64> {
    line.strip_prefix(field)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_breakdown_subtraction_saturates_each_owner() {
        let current = MemoryBreakdown {
            anonymous_rss_bytes: 10,
            file_backed_rss_bytes: 20,
            heap_mapping_rss_bytes: 5,
            database_mapped_rss_bytes: 1,
        };
        let baseline = MemoryBreakdown {
            anonymous_rss_bytes: 4,
            file_backed_rss_bytes: 30,
            heap_mapping_rss_bytes: 2,
            database_mapped_rss_bytes: 3,
        };
        let delta = current.saturating_sub(baseline);
        assert_eq!(delta.anonymous_rss_bytes, 6);
        assert_eq!(delta.file_backed_rss_bytes, 0);
        assert_eq!(delta.heap_mapping_rss_bytes, 3);
        assert_eq!(delta.database_mapped_rss_bytes, 0);
    }

    #[test]
    fn smaps_values_require_exact_field_prefix() {
        assert_eq!(smaps_kib_value("Rss: 12 kB", "Rss:"), Some(12 * 1024));
        assert_eq!(smaps_kib_value("AnonHugePages: 12 kB", "Rss:"), None);
    }
}
