use std::time::Duration;

use derive_new::new;
use derive_setters::Setters;
use tracing::{info, instrument, warn};

use crate::bus::{Bus, Error, Result};

#[derive(Debug, Clone, new, Setters)]
#[setters(prefix = "with_", strip_option, into)]
pub struct Builder {
    #[new(into)]
    #[setters(skip)]
    router: String,

    #[new(default)]
    prefix: Option<String>,

    #[new(value = "\"client\".to_string()")]
    mode: String,

    #[new(value = "Duration::from_secs(60)")]
    connect_timeout: Duration,

    #[new(value = "5")]
    connect_retries: u32,
}

impl Builder {
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_deref().filter(|prefix| !prefix.is_empty())
    }

    pub fn router(&self) -> &str {
        &self.router
    }

    #[instrument(level = "info", skip(self))]
    pub async fn connect(self) -> Result<Bus> {
        let prefix = self.compose_prefix()?;
        let max_attempts = self.connect_retries.saturating_add(1);
        let connect_timeout_ms = duration_to_millis(self.connect_timeout);

        if self.router.is_empty() && self.mode == "client" {
            warn!("builder has no router configured; Zenoh connection is likely to fail");
        }

        info!(
            prefix = %prefix,
            mode = %self.mode,
            router = %self.router,
            connect_timeout_ms,
            max_attempts,
            "Opening Zenoh bus session"
        );

        let config = self.build_config()?;
        let mut last_error = None;
        for attempt in 1..=max_attempts {
            match zenoh::open(config.clone()).await {
                Ok(session) => {
                    info!(attempt, max_attempts, prefix = %prefix, "Zenoh session opened");
                    return Ok(Bus::new(session, prefix));
                }
                Err(error) => {
                    warn!(
                        attempt,
                        max_attempts,
                        connect_timeout_ms,
                        error = %error,
                        "Failed to open Zenoh session"
                    );
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.map(Error::from).unwrap_or_else(|| {
            Error::InvalidIdentifier("connect should attempt at least once".to_string())
        }))
    }

    fn compose_prefix(&self) -> Result<String> {
        if let Some(prefix) = self.prefix.as_deref().filter(|value| !value.is_empty()) {
            zenoh::key_expr::OwnedKeyExpr::new(prefix.to_string())
                .map_err(|error| Error::InvalidTopic(error.to_string()))?;
            return Ok(prefix.to_string());
        }
        Ok(String::new())
    }

    fn build_config(&self) -> Result<zenoh::Config> {
        let mut config = zenoh::Config::default();
        let connect_timeout_ms = duration_to_millis(self.connect_timeout);

        insert_json(&mut config, "mode", format!("\"{}\"", self.mode))?;
        insert_json(
            &mut config,
            "connect/timeout_ms",
            connect_timeout_ms.to_string(),
        )?;
        insert_json(
            &mut config,
            "connect/endpoints",
            serde_json::to_string(&[self.router.as_str()])
                .map_err(|error| Error::InvalidTopic(error.to_string()))?,
        )?;
        insert_json(
            &mut config,
            "open/return_conditions/connect_scouted",
            "true".into(),
        )?;
        insert_json(
            &mut config,
            "open/return_conditions/declares",
            "true".into(),
        )?;
        insert_json(
            &mut config,
            "scouting/timeout",
            connect_timeout_ms.to_string(),
        )?;
        insert_json(&mut config, "scouting/multicast/enabled", "false".into())?;

        Ok(config)
    }
}

fn insert_json(config: &mut zenoh::Config, path: &str, value: String) -> Result<()> {
    config.insert_json5(path, &value).map_err(|error| {
        Error::InvalidTopic(format!(
            "failed to apply Zenoh config override '{path}': {error}"
        ))
    })
}

fn duration_to_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}
