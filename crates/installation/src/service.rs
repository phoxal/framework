//! Persistent deployment identity and generated systemd service configuration.
//!
//! The generated unit is the only persisted source of the deployment's routing
//! identity. Immutable releases remain reusable across deployment targets.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{ACTIVE_LINK, BUNDLE_DIR, SUPERVISOR_FILE};

/// Generated systemd unit stored outside immutable release directories.
pub const SERVICE_UNIT_FILE: &str = "phoxal-supervisor.service";

const SCOPE_PREFIX: &str = "Environment=PHOXAL_SCOPE=";
const SUPERVISOR_PREFIX: &str = "Environment=PHOXAL_SUPERVISOR_ID=";

/// Stable routing identity assigned when a deployment is installed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentIdentity {
    scope: String,
    supervisor_id: String,
}

impl DeploymentIdentity {
    /// Validate a complete deployment identity.
    ///
    /// Both values are lowercase ASCII key segments matching
    /// `[a-z0-9][a-z0-9_-]{0,63}`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceConfigError::InvalidIdentifier`] for either malformed
    /// value.
    pub fn new(
        scope: impl Into<String>,
        supervisor_id: impl Into<String>,
    ) -> Result<Self, ServiceConfigError> {
        let scope = scope.into();
        let supervisor_id = supervisor_id.into();
        validate_identifier("scope", &scope)?;
        validate_identifier("supervisor_id", &supervisor_id)?;
        Ok(Self {
            scope,
            supervisor_id,
        })
    }

    /// Deployment namespace governed by router policy.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Stable supervisor target within the deployment namespace.
    #[must_use]
    pub fn supervisor_id(&self) -> &str {
        &self.supervisor_id
    }
}

/// Requested identity behavior while regenerating the service configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityUpdate {
    /// Retain the complete identity already persisted in the generated unit.
    Retain,
    /// Persist a complete first or replacement identity.
    Set(DeploymentIdentity),
}

/// Generate and atomically persist the systemd service configuration.
///
/// [`IdentityUpdate::Retain`] never invents an identity. It succeeds only when
/// an existing generated unit contains both validated values. Selecting a new
/// identity always supplies both values together.
///
/// # Errors
///
/// Returns a typed identity, existing-unit, path, or filesystem error.
pub fn configure_systemd_service(
    installation_root: impl AsRef<Path>,
    update: IdentityUpdate,
) -> Result<DeploymentIdentity, ServiceConfigError> {
    let root = installation_root.as_ref();
    let unit = root.join(SERVICE_UNIT_FILE);
    let identity = match update {
        IdentityUpdate::Retain => read_systemd_identity(&unit)?
            .ok_or(ServiceConfigError::IdentityRequired { path: unit.clone() })?,
        IdentityUpdate::Set(identity) => identity,
    };
    fs::create_dir_all(root).map_err(|source| ServiceConfigError::CreateDirectory {
        path: root.to_owned(),
        source,
    })?;
    let contents = render_systemd_service(root, &identity)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".phoxal-supervisor.service-")
        .tempfile_in(root)
        .map_err(|source| ServiceConfigError::Write {
            path: root.to_owned(),
            source,
        })?;
    temporary
        .write_all(contents.as_bytes())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| ServiceConfigError::Write {
            path: temporary.path().to_owned(),
            source,
        })?;
    temporary
        .persist(&unit)
        .map_err(|error| ServiceConfigError::Write {
            path: unit,
            source: error.error,
        })?;
    Ok(identity)
}

/// Read the deployment identity from an existing generated unit.
///
/// Absence returns `Ok(None)`. Partial, duplicate, or otherwise changed unit
/// identity fields fail instead of guessing what should be retained.
///
/// # Errors
///
/// Returns a typed read, format, or identity validation error.
pub fn read_systemd_identity(
    unit: impl AsRef<Path>,
) -> Result<Option<DeploymentIdentity>, ServiceConfigError> {
    let unit = unit.as_ref();
    let contents = match fs::read_to_string(unit) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ServiceConfigError::Read {
                path: unit.to_owned(),
                source,
            });
        }
    };
    let scope = single_value(&contents, SCOPE_PREFIX, unit)?;
    let supervisor_id = single_value(&contents, SUPERVISOR_PREFIX, unit)?;
    match (scope, supervisor_id) {
        (Some(scope), Some(supervisor_id)) => {
            DeploymentIdentity::new(scope, supervisor_id).map(Some)
        }
        _ => Err(ServiceConfigError::MalformedUnit {
            path: unit.to_owned(),
        }),
    }
}

/// Render a generated systemd unit for an installed artifact store.
///
/// # Errors
///
/// Returns [`ServiceConfigError::RelativeInstallationRoot`] when the supplied
/// path is not absolute.
pub fn render_systemd_service(
    installation_root: impl AsRef<Path>,
    identity: &DeploymentIdentity,
) -> Result<String, ServiceConfigError> {
    let root = installation_root.as_ref();
    if !root.is_absolute() {
        return Err(ServiceConfigError::RelativeInstallationRoot {
            path: root.to_owned(),
        });
    }
    let active = root.join(ACTIVE_LINK);
    let supervisor = systemd_word(&active.join(SUPERVISOR_FILE));
    let bundle = systemd_word(&active.join(BUNDLE_DIR));
    Ok(format!(
        "[Unit]\nDescription=Phoxal supervisor\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=notify\nEnvironment=PHOXAL_SCOPE={}\nEnvironment=PHOXAL_SUPERVISOR_ID={}\nExecStart={supervisor} {bundle} --scope ${{PHOXAL_SCOPE}} --supervisor-id ${{PHOXAL_SUPERVISOR_ID}}\nRestart=on-failure\nKillMode=mixed\nTimeoutStopSec=30s\n\n[Install]\nWantedBy=multi-user.target\n",
        identity.scope, identity.supervisor_id
    ))
}

fn single_value(
    contents: &str,
    prefix: &str,
    path: &Path,
) -> Result<Option<String>, ServiceConfigError> {
    let values = contents
        .lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .collect::<Vec<_>>();
    match values.as_slice() {
        [] => Ok(None),
        [value] => Ok(Some((*value).to_owned())),
        _ => Err(ServiceConfigError::MalformedUnit {
            path: path.to_owned(),
        }),
    }
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ServiceConfigError> {
    let valid = (1..=64).contains(&value.len())
        && value.is_ascii()
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().skip(1).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(ServiceConfigError::InvalidIdentifier {
            field,
            value: value.to_owned(),
        })
    }
}

fn systemd_word(path: &Path) -> String {
    let value = path.to_string_lossy().replace('%', "%%");
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Generated service configuration failures.
#[derive(Debug, thiserror::Error)]
pub enum ServiceConfigError {
    /// A deployment identity segment is invalid.
    #[error("{field} `{value}` must match [a-z0-9][a-z0-9_-]{{0,63}}")]
    InvalidIdentifier { field: &'static str, value: String },
    /// First installation cannot retain an identity that does not exist.
    #[error("deployment identity is required before generating {}", path.display())]
    IdentityRequired { path: PathBuf },
    /// The existing unit cannot safely be interpreted as generated state.
    #[error("generated service unit has missing or duplicate identity fields: {}", path.display())]
    MalformedUnit { path: PathBuf },
    /// Installation roots must be absolute in host service configuration.
    #[error("installation root must be absolute: {}", path.display())]
    RelativeInstallationRoot { path: PathBuf },
    /// The installation root could not be created.
    #[error("failed to create installation directory {}: {source}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// An existing unit could not be read.
    #[error("failed to read generated service unit {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A generated unit could not be atomically persisted.
    #[error("failed to write generated service unit {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_match_the_public_routing_contract() {
        let identity = DeploymentIdentity::new("workshop", "rover-01").expect("identity");
        assert_eq!(identity.scope(), "workshop");
        assert_eq!(identity.supervisor_id(), "rover-01");
        for invalid in ["", "Workshop", "-rover", "rover.1", &"r".repeat(65)] {
            assert!(DeploymentIdentity::new(invalid, "rover-01").is_err());
        }
    }

    #[test]
    fn first_install_requires_both_values_and_reinstall_retains_them() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("install");
        assert!(matches!(
            configure_systemd_service(&root, IdentityUpdate::Retain),
            Err(ServiceConfigError::IdentityRequired { .. })
        ));
        let expected = DeploymentIdentity::new("workshop", "rover-01").expect("identity");
        configure_systemd_service(&root, IdentityUpdate::Set(expected.clone())).expect("set");
        assert_eq!(
            configure_systemd_service(&root, IdentityUpdate::Retain).expect("retain"),
            expected
        );
        let unit = fs::read_to_string(root.join(SERVICE_UNIT_FILE)).expect("unit");
        assert!(unit.contains("--scope ${PHOXAL_SCOPE} --supervisor-id ${PHOXAL_SUPERVISOR_ID}"));
        assert!(unit.contains(&format!(
            "ExecStart=\"{}/active/phoxal-supervisor\"",
            root.display()
        )));
    }

    #[test]
    fn deliberate_change_replaces_both_values_together() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("install");
        configure_systemd_service(
            &root,
            IdentityUpdate::Set(DeploymentIdentity::new("workshop", "rover-01").expect("identity")),
        )
        .expect("first");
        let changed = DeploymentIdentity::new("field", "rover-02").expect("identity");
        configure_systemd_service(&root, IdentityUpdate::Set(changed.clone())).expect("change");
        assert_eq!(
            read_systemd_identity(root.join(SERVICE_UNIT_FILE)).expect("read"),
            Some(changed)
        );
    }

    #[test]
    fn installation_path_is_quoted_and_systemd_specifiers_are_escaped() {
        let identity = DeploymentIdentity::new("local", "local").expect("identity");
        let unit =
            render_systemd_service(Path::new("/opt/phoxal % demo"), &identity).expect("absolute");
        assert!(unit.contains("\"/opt/phoxal %% demo/active/phoxal-supervisor\""));
    }
}
