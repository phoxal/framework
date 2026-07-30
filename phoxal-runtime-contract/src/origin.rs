use std::time::Duration;

use crate::TimelineId;

/// An identity for one host boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootId(u64);

impl BootId {
    /// This host's current boot identity.
    pub fn current() -> Self {
        Self(host_boot_identity())
    }

    /// Construct a boot identity for contract tests.
    #[doc(hidden)]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    /// The opaque launch-ABI representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The supervisor-minted origin of one real execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionOrigin {
    boot: BootId,
    boot_ns: u64,
    timeline: TimelineId,
}

impl ExecutionOrigin {
    /// Mint an origin now, returning `None` when the host boot clock is unreadable.
    pub fn try_mint() -> Option<Self> {
        Some(Self {
            boot: BootId::current(),
            boot_ns: read_boot_clock_ns()?,
            timeline: TimelineId::mint(),
        })
    }

    /// Mint an origin now, panicking on a host without a readable boot clock.
    pub fn mint() -> Self {
        Self::try_mint().expect("the host boot clock must be readable to start an execution")
    }

    /// Rebuild an origin from its typed fields.
    #[doc(hidden)]
    pub const fn new(boot: BootId, boot_ns: u64, timeline: TimelineId) -> Self {
        Self {
            boot,
            boot_ns,
            timeline,
        }
    }

    /// The boot this origin was minted during.
    pub const fn boot(self) -> BootId {
        self.boot
    }

    /// Nanoseconds on the boot clock at execution start.
    pub const fn boot_ns(self) -> u64 {
        self.boot_ns
    }

    /// The real execution's timeline.
    pub const fn timeline(self) -> TimelineId {
        self.timeline
    }

    /// Render as `<boot>:<boot-ns>:<timeline>`.
    pub fn encode(self) -> String {
        format!("{}:{}:{}", self.boot.0, self.boot_ns, self.timeline.get())
    }

    /// Parse the launch representation without compatibility fallbacks.
    pub fn decode(value: &str) -> Option<Self> {
        let mut parts = value.split(':');
        let boot = BootId(parts.next()?.parse().ok()?);
        let boot_ns = parts.next()?.parse().ok()?;
        let timeline = TimelineId::from_raw(parts.next()?.parse().ok()?)?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            boot,
            boot_ns,
            timeline,
        })
    }
}

fn read_boot_clock_ns() -> Option<u64> {
    // Keep this clock selection and nanosecond conversion identical to
    // `phoxal_bus::LocalInstant`. This crate cannot depend on phoxal-bus
    // without reversing the workspace ownership direction; the phoxal
    // facade's clock tests compare both implementations directly.
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

    let mut timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `clock_gettime` writes into the owned `timespec`.
    let outcome = unsafe { libc::clock_gettime(CLOCK, &raw mut timespec) };
    if outcome != 0 {
        return None;
    }
    Some(
        u64::try_from(timespec.tv_sec)
            .ok()?
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::try_from(timespec.tv_nsec).ok()?),
    )
}

fn host_boot_identity() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(boot_id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
            return fnv1a(boot_id.trim().as_bytes());
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut boottime = libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut size = std::mem::size_of::<libc::timeval>();
        // SAFETY: `sysctlbyname` writes at most `size` bytes into `boottime`.
        let outcome = unsafe {
            libc::sysctlbyname(
                c"kern.boottime".as_ptr(),
                (&raw mut boottime).cast(),
                &raw mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if outcome == 0 {
            return fnv1a(&boottime.tv_sec.to_le_bytes());
        }
    }
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let uptime = read_boot_clock_ns()
        .map(|ns| Duration::from_nanos(ns).as_secs())
        .unwrap_or(0);
    fnv1a(&wall.saturating_sub(uptime).to_le_bytes())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_round_trips_and_rejects_noncanonical_shapes() {
        let origin = ExecutionOrigin::mint();
        assert_eq!(ExecutionOrigin::decode(&origin.encode()), Some(origin));
        assert_eq!(ExecutionOrigin::decode("garbage"), None);
        assert_eq!(ExecutionOrigin::decode("1:2:0"), None);
        assert_eq!(ExecutionOrigin::decode("1:2:3:4"), None);
    }

    #[test]
    fn boot_identity_is_stable_within_one_boot() {
        assert_eq!(BootId::current(), BootId::current());
    }
}
