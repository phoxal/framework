use std::fs::OpenOptions;
use std::io::IsTerminal;
use std::io::{Result as IoResult, Write};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

pub fn parse_trimmed_non_empty(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err("value must not be empty".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

pub fn tracing_ansi_enabled() -> bool {
    std::io::stderr().is_terminal()
        && std::env::var("NO_COLOR").map_or(true, |value| value.is_empty())
}

pub fn init_tracing() -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let ansi = tracing_ansi_enabled();
    let subscriber = tracing_subscriber::fmt().with_env_filter(env_filter);

    match std::env::var("ROBOT_LOG_PATH") {
        Ok(path) if !path.trim().is_empty() => {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .context("failed to open ROBOT_LOG_PATH for append")?;
            let file = Arc::new(Mutex::new(file));
            let _ = subscriber
                .with_ansi(false)
                .with_writer(move || SharedLogWriter::new(file.clone()))
                .try_init();
        }
        _ => {
            let _ = subscriber.with_ansi(ansi).try_init();
        }
    }
    Ok(())
}

struct SharedLogWriter {
    file: Arc<Mutex<std::fs::File>>,
}

impl SharedLogWriter {
    fn new(file: Arc<Mutex<std::fs::File>>) -> Self {
        Self { file }
    }
}

impl Write for SharedLogWriter {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        self.file
            .lock()
            .map_err(|_| std::io::Error::other("log file lock poisoned"))?
            .write(buf)
    }

    fn flush(&mut self) -> IoResult<()> {
        self.file
            .lock()
            .map_err(|_| std::io::Error::other("log file lock poisoned"))?
            .flush()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_trimmed_non_empty;

    #[test]
    fn parser_trims_and_keeps_non_empty_values() {
        assert_eq!(
            parse_trimmed_non_empty("  robot-1  ").expect("parsed"),
            "robot-1"
        );
    }

    #[test]
    fn parser_rejects_blank_values() {
        assert!(parse_trimmed_non_empty("   ").is_err());
    }
}
