//! Minimal systemd readiness and watchdog notification for the participant runner.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

const READY: &[u8] = b"READY=1";
#[cfg(target_os = "linux")]
const WATCHDOG: &[u8] = b"WATCHDOG=1";

/// Default runner watchdog check cadence.
///
/// The thread checks once per second by default: stricter than the future
/// systemd `WatchdogSec` budget, cheap enough for every participant, and slow
/// enough to avoid noisy notify traffic. If systemd exports a shorter
/// `WATCHDOG_USEC`, the runner uses half of that budget so pings still arrive in
/// time.
const DEFAULT_WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);
const MIN_WATCHDOG_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn ready() {
    if let Err(error) = notify_ready() {
        tracing::warn!(
            target: "phoxal.runtime",
            error = %error,
            "sd_notify READY=1 failed"
        );
    }
}

fn notify_ready() -> io::Result<bool> {
    notify_from_env(READY)
}

fn notify_from_env(payload: &[u8]) -> io::Result<bool> {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(false);
    };

    notify_socket(socket.as_os_str(), payload)?;
    Ok(true)
}

pub(crate) struct Watchdog {
    state: Option<Arc<WatchdogState>>,
    shutdown: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

struct WatchdogState {
    generation: AtomicU64,
}

impl Watchdog {
    pub(crate) fn start() -> Self {
        Self::start_with_interval(watchdog_interval_from_env())
    }

    pub(crate) fn feed(&self) {
        if let Some(state) = &self.state {
            state.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn start_with_interval(interval: Duration) -> Self {
        let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
            return Self::disabled();
        };

        Self::start_with_socket(socket, interval)
    }

    fn disabled() -> Self {
        Self {
            state: None,
            shutdown: None,
            thread: None,
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn start_with_socket(_socket: OsString, _interval: Duration) -> Self {
        tracing::warn!(
            target: "phoxal.runtime",
            "sd_notify WATCHDOG=1 requires Linux systemd notify sockets"
        );
        Self::disabled()
    }

    #[cfg(target_os = "linux")]
    fn start_with_socket(socket: OsString, interval: Duration) -> Self {
        Self::spawn(socket, interval)
    }

    #[cfg(target_os = "linux")]
    fn spawn(socket: OsString, interval: Duration) -> Self {
        let state = Arc::new(WatchdogState {
            generation: AtomicU64::new(0),
        });
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let thread_state = Arc::clone(&state);
        let thread = match std::thread::Builder::new()
            .name("phoxal-sd-watchdog".to_string())
            .spawn(move || watchdog_loop(socket, interval, thread_state, shutdown_rx))
        {
            Ok(thread) => Some(thread),
            Err(error) => {
                tracing::warn!(
                    target: "phoxal.runtime",
                    error = %error,
                    "failed to start sd_notify watchdog thread"
                );
                return Self::disabled();
            }
        };

        Self {
            state: Some(state),
            shutdown: Some(shutdown_tx),
            thread,
        }
    }

    pub(crate) fn shutdown(mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(target_os = "linux")]
fn watchdog_loop(
    socket: OsString,
    interval: Duration,
    state: Arc<WatchdogState>,
    shutdown: mpsc::Receiver<()>,
) {
    let mut last_ping_generation = state.generation.load(Ordering::Acquire);
    loop {
        match shutdown.recv_timeout(interval) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let generation = state.generation.load(Ordering::Acquire);
        if generation == last_ping_generation {
            continue;
        }

        if let Err(error) = notify_socket(socket.as_os_str(), WATCHDOG) {
            tracing::warn!(
                target: "phoxal.runtime",
                error = %error,
                "sd_notify WATCHDOG=1 failed"
            );
        }
        last_ping_generation = generation;
    }
}

fn watchdog_interval_from_env() -> Duration {
    let Some(usec) = std::env::var_os("WATCHDOG_USEC")
        .and_then(|value| value.into_string().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|usec| *usec > 0)
    else {
        return DEFAULT_WATCHDOG_INTERVAL;
    };

    let half_budget = Duration::from_micros(usec / 2);
    half_budget.clamp(MIN_WATCHDOG_INTERVAL, DEFAULT_WATCHDOG_INTERVAL)
}

#[cfg(target_os = "linux")]
fn notify_socket(socket: &OsStr, payload: &[u8]) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixDatagram;
    use std::path::Path;

    let datagram = UnixDatagram::unbound()?;
    datagram.set_nonblocking(true)?;

    let socket = socket.as_bytes();
    if socket.first() == Some(&b'@') {
        send_abstract(&datagram, &socket[1..], payload)
    } else {
        datagram.send_to(payload, Path::new(OsStr::from_bytes(socket)))?;
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
fn notify_socket(_socket: &OsStr, _payload: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sd_notify requires Linux systemd notify sockets",
    ))
}

#[cfg(target_os = "linux")]
fn send_abstract(
    datagram: &std::os::unix::net::UnixDatagram,
    name: &[u8],
    payload: &[u8],
) -> io::Result<()> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::SocketAddr;

    let addr = SocketAddr::from_abstract_name(name)?;
    datagram.send_to_addr(payload, &addr)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use serial_test::serial;
    use std::ffi::OsString;

    struct NotifySocketGuard(Option<OsString>);

    impl NotifySocketGuard {
        fn unset() -> Self {
            let previous = std::env::var_os("NOTIFY_SOCKET");
            // SAFETY: tests touching NOTIFY_SOCKET are serialized.
            unsafe { std::env::remove_var("NOTIFY_SOCKET") };
            Self(previous)
        }
    }

    impl Drop for NotifySocketGuard {
        fn drop(&mut self) {
            // SAFETY: tests touching NOTIFY_SOCKET are serialized.
            unsafe {
                match self.0.take() {
                    Some(previous) => std::env::set_var("NOTIFY_SOCKET", previous),
                    None => std::env::remove_var("NOTIFY_SOCKET"),
                }
            }
        }
    }

    #[test]
    #[serial]
    fn notify_ready_is_noop_without_notify_socket() {
        let _guard = NotifySocketGuard::unset();

        assert!(!notify_ready().expect("missing NOTIFY_SOCKET should be a no-op"));
    }

    #[test]
    #[serial]
    fn watchdog_is_noop_without_notify_socket() {
        let _guard = NotifySocketGuard::unset();

        let watchdog = Watchdog::start_with_interval(Duration::from_millis(10));
        watchdog.feed();
        watchdog.shutdown();
    }

    #[test]
    #[serial]
    fn watchdog_interval_uses_half_systemd_budget_with_bounds() {
        let _socket_guard = NotifySocketGuard::unset();
        let previous = std::env::var_os("WATCHDOG_USEC");
        struct Guard(Option<OsString>);
        impl Drop for Guard {
            fn drop(&mut self) {
                // SAFETY: tests touching WATCHDOG_USEC are serialized.
                unsafe {
                    match self.0.take() {
                        Some(previous) => std::env::set_var("WATCHDOG_USEC", previous),
                        None => std::env::remove_var("WATCHDOG_USEC"),
                    }
                }
            }
        }
        let _guard = Guard(previous);

        // SAFETY: tests touching WATCHDOG_USEC are serialized.
        unsafe { std::env::set_var("WATCHDOG_USEC", "400000") };
        assert_eq!(watchdog_interval_from_env(), Duration::from_millis(200));

        // SAFETY: tests touching WATCHDOG_USEC are serialized.
        unsafe { std::env::set_var("WATCHDOG_USEC", "10000") };
        assert_eq!(watchdog_interval_from_env(), MIN_WATCHDOG_INTERVAL);

        // SAFETY: tests touching WATCHDOG_USEC are serialized.
        unsafe { std::env::set_var("WATCHDOG_USEC", "10000000") };
        assert_eq!(watchdog_interval_from_env(), DEFAULT_WATCHDOG_INTERVAL);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn notify_socket_sends_ready_to_path_datagram() {
        use std::os::unix::net::UnixDatagram;
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir should be created");
        let socket_path = dir.path().join("notify.sock");
        let receiver =
            UnixDatagram::bind(&socket_path).expect("notify socket should bind to a path");
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("read timeout should be set");

        notify_socket(socket_path.as_os_str(), READY).expect("READY=1 should be sent");

        let mut buf = [0u8; 64];
        let len = receiver
            .recv(&mut buf)
            .expect("receiver should get the READY packet");
        assert_eq!(&buf[..len], READY);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn notify_socket_sends_ready_to_abstract_datagram() {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixDatagram};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let name = format!("phoxal-notify-{}-{unique}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).expect("abstract addr is valid");
        let receiver =
            UnixDatagram::bind_addr(&addr).expect("notify socket should bind abstract addr");
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .expect("read timeout should be set");
        let notify_socket_name = OsString::from(format!("@{name}"));

        notify_socket(notify_socket_name.as_os_str(), READY).expect("READY=1 should be sent");

        let mut buf = [0u8; 64];
        let len = receiver
            .recv(&mut buf)
            .expect("receiver should get the READY packet");
        assert_eq!(&buf[..len], READY);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn watchdog_pings_only_after_liveness_feed() {
        use std::io::ErrorKind;
        use std::os::unix::net::UnixDatagram;

        let dir = tempfile::tempdir().expect("tempdir should be created");
        let socket_path = dir.path().join("watchdog.sock");
        let receiver =
            UnixDatagram::bind(&socket_path).expect("notify socket should bind to a path");
        receiver
            .set_read_timeout(Some(Duration::from_millis(80)))
            .expect("read timeout should be set");

        let previous = std::env::var_os("NOTIFY_SOCKET");
        // SAFETY: tests touching NOTIFY_SOCKET are serialized.
        unsafe { std::env::set_var("NOTIFY_SOCKET", &socket_path) };
        let _guard = NotifySocketGuard(previous);

        let watchdog = Watchdog::start_with_interval(Duration::from_millis(20));
        assert_no_packet(&receiver);

        watchdog.feed();
        assert_packet(&receiver, WATCHDOG);

        assert_no_packet(&receiver);

        watchdog.feed();
        watchdog.feed();
        assert_packet(&receiver, WATCHDOG);
        watchdog.shutdown();

        fn assert_no_packet(receiver: &UnixDatagram) {
            let mut buf = [0u8; 64];
            let err = receiver
                .recv(&mut buf)
                .expect_err("watchdog must not ping without new liveness");
            assert!(
                matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
                "unexpected recv error: {err}"
            );
        }

        fn assert_packet(receiver: &UnixDatagram, expected: &[u8]) {
            let mut buf = [0u8; 64];
            let len = receiver
                .recv(&mut buf)
                .expect("receiver should get the WATCHDOG packet");
            assert_eq!(&buf[..len], expected);
        }
    }
}
