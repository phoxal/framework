use super::*;

pub(super) fn required_log_limit() -> Result<u64> {
    let value = std::env::var(LOG_BYTE_LIMIT_ENV).with_context(|| {
        format!("required environment variable {LOG_BYTE_LIMIT_ENV} is missing")
    })?;
    let value = value
        .parse::<u64>()
        .with_context(|| format!("{LOG_BYTE_LIMIT_ENV} must contain decimal bytes"))?;
    ensure!(value >= 2, "{LOG_BYTE_LIMIT_ENV} must be at least 2 bytes");
    Ok(value)
}

#[derive(Clone)]
pub(super) struct BoundedStderr {
    state: Arc<Mutex<BoundedStderrState>>,
}

struct BoundedStderrState {
    limit: u64,
    written: u64,
    truncated: bool,
}

impl BoundedStderr {
    pub(super) fn new(limit: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(BoundedStderrState {
                limit,
                written: 0,
                truncated: false,
            })),
        }
    }

    pub(super) fn truncated(&self) -> bool {
        lock(&self.state).truncated
    }
}

impl std::io::Write for BoundedStderr {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = lock(&self.state);
        let remaining = state.limit.saturating_sub(state.written);
        let retained = usize::try_from(remaining.min(bytes.len() as u64)).unwrap_or(bytes.len());
        if retained > 0 {
            std::io::stderr().write_all(&bytes[..retained])?;
            state.written = state.written.saturating_add(retained as u64);
        }
        if retained < bytes.len() {
            state.truncated = true;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
