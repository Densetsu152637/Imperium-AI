//! Per-process GPU memory on Linux from the kernel's DRM usage statistics.
//!
//! Every open DRM file (`/dev/dri/*`) of a process has a `/proc/<pid>/fdinfo/<fd>` entry whose
//! `drm-*` keys the driver fills in (kernel documentation, "DRM client usage stats"). `amdgpu`
//! reports `drm-memory-vram`; `xe` reports `drm-total-vram<N>` and `drm-resident-vram<N>`; `i915`
//! reports `drm-total-local<N>` for a card's local memory. Only video memory counts here, never
//! the `gtt` or `system` regions. Duplicated descriptors of one client (`drm-client-id`) are
//! counted once.
//!
//! The text is parsed by [`parse_fdinfo`], separate from reading, so tests run on captured
//! output. Reading is bounded (descriptors per process, bytes per file) and anything unreadable
//! (another user's process, a kernel without the keys, not Linux) gives no figure, never zero.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

/// The most memory a GPU is believed to have; a larger figure is a parsing error.
const MAX_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024 * 1024;
/// Most descriptors looked at for one process.
const MAX_FDS: usize = 4096;
/// Most bytes read from one fdinfo file.
const MAX_FILE_BYTES: u64 = 16 * 1024;

/// What one DRM descriptor says about its client's video memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdInfo {
    pub driver: String,
    /// The PCI address of the device (`drm-pdev`), when the driver gives it.
    pub pdev: Option<String>,
    pub client_id: u64,
    pub vram_bytes: u64,
}

/// A size such as `8944 KiB`, `12 MiB`, `1 GiB` or a bare byte count.
fn size(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace();
    let value: u64 = parts.next()?.parse().ok()?;
    let unit: u64 = match parts.next() {
        None => 1,
        Some("KiB") => 1024,
        Some("MiB") => 1024 * 1024,
        Some("GiB") => 1024 * 1024 * 1024,
        Some(_) => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    value
        .checked_mul(unit)
        .filter(|bytes| *bytes <= MAX_MEMORY_BYTES)
}

/// Whether a region name (`vram`, `vram0`, `local0`) is video memory.
fn is_video_region(region: &str) -> bool {
    region.starts_with("vram") || region.starts_with("local")
}

/// One `fdinfo` file: `None` when it is not a DRM descriptor of a driver that reports video
/// memory per client.
pub fn parse_fdinfo(text: &str) -> Option<FdInfo> {
    let mut driver = None;
    let mut pdev = None;
    let mut client_id = None;
    // Per region the best figure wins: resident, else total, else the older `memory` key.
    let mut regions: Vec<(String, [Option<u64>; 3])> = Vec::new();
    for line in text.lines().take(512) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "drm-driver" => driver = Some(value.to_string()),
            "drm-pdev" => pdev = Some(value.to_string()),
            "drm-client-id" => client_id = value.parse::<u64>().ok(),
            _ => {
                let (rank, region) = if let Some(region) = key.strip_prefix("drm-resident-") {
                    (0, region)
                } else if let Some(region) = key.strip_prefix("drm-total-") {
                    (1, region)
                } else if let Some(region) = key.strip_prefix("drm-memory-") {
                    (2, region)
                } else {
                    continue;
                };
                if !is_video_region(region) {
                    continue;
                }
                let Some(bytes) = size(value) else { continue };
                match regions.iter_mut().find(|(name, _)| name == region) {
                    Some((_, slots)) => slots[rank] = Some(bytes),
                    None => {
                        let mut slots = [None; 3];
                        slots[rank] = Some(bytes);
                        regions.push((region.to_string(), slots));
                    }
                }
            }
        }
    }
    let driver = driver?;
    if regions.is_empty() {
        return None;
    }
    let vram_bytes = regions
        .iter()
        .filter_map(|(_, slots)| slots.iter().flatten().next().copied())
        .try_fold(0u64, u64::checked_add)
        .filter(|bytes| *bytes <= MAX_MEMORY_BYTES)?;
    Some(FdInfo {
        driver,
        pdev,
        client_id: client_id?,
        vram_bytes,
    })
}

/// The video memory the clients of these descriptors hold together: each client of each device
/// once.
pub fn total_vram<'a>(infos: impl IntoIterator<Item = &'a FdInfo>) -> Option<u64> {
    let mut seen = HashSet::new();
    let mut total: Option<u64> = None;
    for info in infos {
        if seen.insert((info.driver.as_str(), info.pdev.as_deref(), info.client_id)) {
            total = Some(total.unwrap_or(0).saturating_add(info.vram_bytes));
        }
    }
    total.filter(|bytes| *bytes <= MAX_MEMORY_BYTES)
}

/// The video memory process `pid` holds, from `proc_root` (`/proc`); `None` when it has no DRM
/// descriptor that reports it or the process cannot be read.
pub fn process_vram(proc_root: &Path, pid: u32) -> Option<u64> {
    let entries = std::fs::read_dir(proc_root.join(pid.to_string()).join("fdinfo")).ok()?;
    let mut infos = Vec::new();
    for entry in entries.filter_map(Result::ok).take(MAX_FDS) {
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let mut text = String::new();
        if file.take(MAX_FILE_BYTES).read_to_string(&mut text).is_err() {
            continue;
        }
        // Most descriptors are not DRM files; skip them before parsing.
        if !text.contains("drm-driver") {
            continue;
        }
        if let Some(info) = parse_fdinfo(&text) {
            infos.push(info);
        }
    }
    total_vram(&infos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const KIB: u64 = 1024;
    const AMDGPU: &str = include_str!("../tests/fixtures/gpu/fdinfo-amdgpu.txt");
    const XE: &str = include_str!("../tests/fixtures/gpu/fdinfo-xe.txt");
    const I915: &str = include_str!("../tests/fixtures/gpu/fdinfo-i915.txt");

    #[test]
    fn amdgpu_reports_vram_not_gtt() {
        let info = parse_fdinfo(AMDGPU).expect("amdgpu");
        assert_eq!(info.driver, "amdgpu");
        assert_eq!(info.pdev.as_deref(), Some("0000:03:00.0"));
        assert_eq!(info.client_id, 7);
        assert_eq!(info.vram_bytes, 8944 * KIB);
    }

    #[test]
    fn xe_counts_resident_vram_and_not_system_memory() {
        let info = parse_fdinfo(XE).expect("xe");
        assert_eq!(info.vram_bytes, 12288 * KIB);
    }

    #[test]
    fn i915_counts_local_memory() {
        let info = parse_fdinfo(I915).expect("i915");
        assert_eq!(info.vram_bytes, 8192 * KIB);
    }

    #[test]
    fn other_text_gives_no_figure() {
        assert_eq!(parse_fdinfo(""), None);
        assert_eq!(parse_fdinfo("pos:\t0\nflags:\t02\nmnt_id:\t27\n"), None);
        // A driver with no video memory key, a missing client id, a bad unit.
        assert_eq!(parse_fdinfo("drm-driver: vc4\ndrm-client-id: 1\n"), None);
        assert_eq!(
            parse_fdinfo("drm-driver: amdgpu\ndrm-memory-vram: 5 KiB\n"),
            None
        );
        assert_eq!(
            parse_fdinfo("drm-driver: amdgpu\ndrm-client-id: 1\ndrm-memory-vram: 5 parsecs\n"),
            None
        );
        assert_eq!(
            parse_fdinfo(
                "drm-driver: amdgpu\ndrm-client-id: 1\ndrm-memory-vram: 99999999999999999999 KiB\n"
            ),
            None
        );
        assert_eq!(
            parse_fdinfo(
                "drm-driver: amdgpu\ndrm-client-id: 1\ndrm-memory-vram: 18446744073709551615 GiB\n"
            ),
            None
        );
    }

    #[test]
    fn a_client_seen_through_two_descriptors_counts_once() {
        let a = parse_fdinfo(AMDGPU).expect("a");
        let b = parse_fdinfo(&AMDGPU.replace("client-id:\t7", "client-id:\t8")).expect("b");
        let again = a.clone();
        assert_eq!(total_vram([&a, &again]), Some(8944 * KIB));
        assert_eq!(total_vram([&a, &b]), Some(2 * 8944 * KIB));
        assert_eq!(total_vram(Vec::<&FdInfo>::new()), None);
    }

    #[test]
    fn a_process_is_read_from_its_descriptors() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fdinfo = dir.path().join("4242").join("fdinfo");
        fs::create_dir_all(&fdinfo).expect("dirs");
        fs::write(fdinfo.join("0"), "pos:\t0\nflags:\t02\n").expect("write");
        fs::write(fdinfo.join("5"), AMDGPU).expect("write");
        fs::write(fdinfo.join("6"), AMDGPU).expect("write");
        fs::write(fdinfo.join("7"), XE).expect("write");
        assert_eq!(process_vram(dir.path(), 4242), Some((8944 + 12288) * KIB));
        // No such process, and one without a DRM descriptor.
        assert_eq!(process_vram(dir.path(), 1), None);
        let other = dir.path().join("77").join("fdinfo");
        fs::create_dir_all(&other).expect("dirs");
        fs::write(other.join("0"), "pos:\t0\n").expect("write");
        assert_eq!(process_vram(dir.path(), 77), None);
    }

    #[test]
    fn sizes_are_read_in_bytes_kib_mib_and_gib() {
        let vram = |value: &str| {
            parse_fdinfo(&format!(
                "drm-driver: amdgpu
drm-client-id: 1
drm-memory-vram: {value}
"
            ))
            .map(|info| info.vram_bytes)
        };
        assert_eq!(vram("4096"), Some(4096));
        assert_eq!(vram("4096 KiB"), Some(4 * 1024 * 1024));
        assert_eq!(vram("3 MiB"), Some(3 * 1024 * 1024));
        assert_eq!(vram("2 GiB"), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(vram("0"), Some(0));
        assert_eq!(vram("3 MB"), None);
        assert_eq!(vram("-3 KiB"), None);
        assert_eq!(vram("3 KiB extra"), None);
    }

    #[test]
    fn at_most_max_fds_descriptors_are_looked_at() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fdinfo = dir.path().join("5").join("fdinfo");
        fs::create_dir_all(&fdinfo).expect("dirs");
        for fd in 0..MAX_FDS + 20 {
            let text = format!(
                "drm-driver: amdgpu
drm-pdev: 0000:03:00.0
drm-client-id: {fd}
drm-memory-vram: 1 KiB
"
            );
            fs::write(fdinfo.join(fd.to_string()), text).expect("write");
        }
        assert_eq!(process_vram(dir.path(), 5), Some(MAX_FDS as u64 * KIB));
    }

    #[test]
    fn only_the_first_max_file_bytes_of_a_descriptor_are_read() {
        let dir = tempfile::tempdir().expect("temp dir");
        let fdinfo = dir.path().join("6").join("fdinfo");
        fs::create_dir_all(&fdinfo).expect("dirs");
        let head = "drm-driver: amdgpu
drm-client-id: 1
";
        let padding = "pad: 0
"
        .repeat(MAX_FILE_BYTES as usize / 7);
        // The memory key sits past the limit: it is not seen, so there is no figure.
        fs::write(
            fdinfo.join("1"),
            format!(
                "{head}{padding}drm-memory-vram: 5 KiB
"
            ),
        )
        .expect("write");
        assert_eq!(process_vram(dir.path(), 6), None);
        // The same key within the limit is.
        fs::write(
            fdinfo.join("1"),
            format!(
                "{head}drm-memory-vram: 5 KiB
{padding}"
            ),
        )
        .expect("write");
        assert_eq!(process_vram(dir.path(), 6), Some(5 * KIB));
    }
}
