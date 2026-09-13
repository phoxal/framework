//! Owned process group for supervisor executable acceptance.

use std::{path::Path, time::Duration};

pub struct SupervisorProcess {
    child: tokio::process::Child,
    group: i32,
}

impl SupervisorProcess {
    pub fn launch(bundle: &Path, id: &str) -> Self {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_phoxal-supervisor"));
        command
            .arg(bundle)
            .args(["--scope", "local", "--supervisor-id", id]);
        command.process_group(0).kill_on_drop(true);
        let child = command.spawn().expect("launch supervisor executable");
        let group = child.id().expect("live supervisor PID") as i32;
        Self { child, group }
    }

    pub fn is_finished(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("query supervisor exit")
            .is_some()
    }

    pub async fn shutdown(&mut self) {
        // SAFETY: this positive PID belongs to the live child retained by this guard.
        assert_eq!(unsafe { libc::kill(self.group, libc::SIGTERM) }, 0);
        let status = tokio::time::timeout(Duration::from_secs(15), self.child.wait())
            .await
            .expect("bounded supervisor shutdown")
            .expect("reap supervisor");
        assert!(status.success(), "supervisor shutdown failed: {status}");
    }
}

impl Drop for SupervisorProcess {
    fn drop(&mut self) {
        // SAFETY: the child was started in its own process group. Kill only that group,
        // including runtime children, when an assertion unwinds before normal shutdown.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
    }
}
