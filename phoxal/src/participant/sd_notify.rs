//! Minimal systemd readiness notification for the participant runner.

use std::ffi::OsStr;
use std::io;

const READY: &[u8] = b"READY=1";

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
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(false);
    };

    notify_socket(socket.as_os_str(), READY)?;
    Ok(true)
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
}
