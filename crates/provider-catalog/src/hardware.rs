//! Best-effort, read-only hardware probe used to suggest local models that fit this machine.
//!
//! Everything here is local: nothing is sent to a model, a provider or telemetry. The probe never
//! needs elevated rights, never installs or downloads anything, and degrades to "unknown"
//! (`None`) instead of failing. The only process it may start is the vendor's own read-only
//! `nvidia-smi --query-gpu` listing, bounded by a short timeout.
//!
//! The collection logic is pure over a [`HardwareSource`], so tests inject sample outputs.

use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use serde::Serialize;

/// Longest the GPU tool may run before it is killed and the GPU is reported as unknown.
const GPU_TOOL_TIMEOUT: Duration = Duration::from_millis(1500);
/// Most bytes read from the GPU tool.
const GPU_TOOL_MAX_BYTES: u64 = 16 * 1024;
const MIB: u64 = 1024 * 1024;

/// The graphics adapter that would run a model, as far as it could be told.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuInfo {
    /// Marketing name as the vendor tool reports it (for example `NVIDIA GeForce RTX 4070`).
    pub name: String,
    /// Dedicated video memory in bytes; `None` when the tool did not say.
    pub vram_bytes: Option<u64>,
}

/// What was detected. Every field is `None`/`false` when it could not be determined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HardwareInfo {
    /// Physical RAM in bytes.
    pub ram_bytes: Option<u64>,
    /// Logical CPU cores.
    pub logical_cores: Option<u32>,
    /// Best discrete GPU (the one with the most video memory).
    pub gpu: Option<GpuInfo>,
    /// Apple silicon: the GPU shares system memory, so RAM is the model budget.
    pub unified_memory: bool,
    /// Free bytes on the volume holding the data directory.
    pub free_disk_bytes: Option<u64>,
    /// Operating system (`std::env::consts::OS`).
    pub os: String,
    /// CPU architecture (`std::env::consts::ARCH`).
    pub arch: String,
}

impl HardwareInfo {
    /// Nothing known.
    #[must_use]
    pub fn unknown(os: &str, arch: &str) -> Self {
        Self {
            ram_bytes: None,
            logical_cores: None,
            gpu: None,
            unified_memory: false,
            free_disk_bytes: None,
            os: os.to_owned(),
            arch: arch.to_owned(),
        }
    }
}

/// Where the raw numbers come from. The real implementation is [`SystemSource`].
pub trait HardwareSource {
    fn ram_bytes(&self) -> Option<u64>;
    fn logical_cores(&self) -> Option<u32>;
    /// Raw `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits` text, if the
    /// tool exists and answered in time.
    fn nvidia_smi_output(&self) -> Option<String>;
    fn free_disk_bytes(&self, dir: &Path) -> Option<u64>;
}

/// Builds the hardware summary from a source. Pure: no I/O of its own.
#[must_use]
pub fn collect(source: &dyn HardwareSource, data_dir: &Path, os: &str, arch: &str) -> HardwareInfo {
    let ram_bytes = source.ram_bytes().filter(|b| *b > 0);
    let gpu = source
        .nvidia_smi_output()
        .and_then(|text| parse_nvidia_smi(&text));
    HardwareInfo {
        ram_bytes,
        logical_cores: source.logical_cores().filter(|c| *c > 0),
        // Apple silicon has no separate VRAM; its unified memory is the RAM figure.
        unified_memory: os == "macos" && arch == "aarch64" && ram_bytes.is_some(),
        gpu,
        free_disk_bytes: source.free_disk_bytes(data_dir),
        os: os.to_owned(),
        arch: arch.to_owned(),
    }
}

/// Parses `name, memory.total` CSV lines (MiB, no units). Picks the GPU with the most memory;
/// a line without a readable memory figure still yields its name. Garbage yields `None`.
#[must_use]
pub fn parse_nvidia_smi(text: &str) -> Option<GpuInfo> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // The name never contains a comma in practice; split from the right regardless.
            let (name, mem) = match line.rsplit_once(',') {
                Some((n, m)) => (n.trim(), m.trim()),
                None => (line, ""),
            };
            let name: String = name.chars().filter(|c| !c.is_control()).take(80).collect();
            if name.is_empty() || name.to_ascii_lowercase().contains("nvidia-smi has failed") {
                return None;
            }
            let vram_bytes = mem
                .trim_end_matches("MiB")
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|m| *m > 0)
                .map(|m| m.saturating_mul(MIB));
            Some(GpuInfo { name, vram_bytes })
        })
        .max_by_key(|g| g.vram_bytes.unwrap_or(0))
}

/// The real machine.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSource;

impl HardwareSource for SystemSource {
    fn ram_bytes(&self) -> Option<u64> {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        Some(system.total_memory())
    }

    fn logical_cores(&self) -> Option<u32> {
        std::thread::available_parallelism()
            .ok()
            .and_then(|n| u32::try_from(n.get()).ok())
    }

    fn nvidia_smi_output(&self) -> Option<String> {
        run_bounded(
            "nvidia-smi",
            &[
                "--query-gpu=name,memory.total",
                "--format=csv,noheader,nounits",
            ],
            GPU_TOOL_TIMEOUT,
        )
    }

    fn free_disk_bytes(&self, dir: &Path) -> Option<u64> {
        let mut probe = dir;
        while !probe.exists() {
            match probe.parent() {
                Some(p) if !p.as_os_str().is_empty() => probe = p,
                _ => break,
            }
        }
        fs4::available_space(probe).ok()
    }
}

/// Probes the real machine. Blocking (at most the GPU tool timeout); call from a blocking context.
#[must_use]
pub fn probe_hardware(data_dir: &Path) -> HardwareInfo {
    collect(
        &SystemSource,
        data_dir,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// Runs `program args` with a hard deadline and a capped read. `None` on a missing tool, a
/// non-zero exit, a timeout or unreadable output.
fn run_bounded(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(GPU_TOOL_MAX_BYTES).read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let bytes = reader.join().ok()?;
    status
        .success()
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        ram: Option<u64>,
        smi: Option<&'static str>,
    }

    impl HardwareSource for Fake {
        fn ram_bytes(&self) -> Option<u64> {
            self.ram
        }
        fn logical_cores(&self) -> Option<u32> {
            Some(8)
        }
        fn nvidia_smi_output(&self) -> Option<String> {
            self.smi.map(str::to_owned)
        }
        fn free_disk_bytes(&self, _: &Path) -> Option<u64> {
            Some(100 << 30)
        }
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn nvidia_sample_picks_the_biggest_card() {
        let text = "NVIDIA GeForce GTX 1050, 2048\nNVIDIA GeForce RTX 4090, 24564\n";
        let gpu = parse_nvidia_smi(text).expect("gpu");
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpu.vram_bytes, Some(24564 * MIB));
    }

    #[test]
    fn unreadable_gpu_output_degrades_to_unknown() {
        assert_eq!(parse_nvidia_smi(""), None);
        assert_eq!(
            parse_nvidia_smi("NVIDIA-SMI has failed because it couldn't communicate"),
            None
        );
        let no_mem = parse_nvidia_smi("Some GPU, [N/A]").expect("name only");
        assert_eq!(no_mem.vram_bytes, None);
    }

    #[test]
    fn apple_silicon_uses_unified_memory_and_intel_mac_does_not() {
        let src = Fake {
            ram: Some(16 * GIB),
            smi: None,
        };
        let apple = collect(&src, Path::new("."), "macos", "aarch64");
        assert!(apple.unified_memory && apple.gpu.is_none());
        let intel = collect(&src, Path::new("."), "macos", "x86_64");
        assert!(!intel.unified_memory);
    }

    #[test]
    fn nothing_known_stays_none_not_zero() {
        let src = Fake {
            ram: Some(0),
            smi: None,
        };
        let hw = collect(&src, Path::new("."), "linux", "x86_64");
        assert_eq!(hw.ram_bytes, None);
        assert_eq!(hw.gpu, None);
    }

    #[test]
    fn a_missing_tool_is_none_not_an_error() {
        assert_eq!(
            run_bounded(
                "praxi-definitely-not-a-tool",
                &[],
                Duration::from_millis(200)
            ),
            None
        );
    }
}
