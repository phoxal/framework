//! Portable embedded interface declarations for independently released applications.
//!
//! Package versions are provenance, while these contract-scoped revisions
//! determine compatibility. Static metadata and inspected records share one
//! type; decoded strings borrow the input or are explicitly owned.

/// Link sections carrying the portable application record.
pub const APPLICATION_SECTION_NAMES: [&str; 2] = [".phoxal_app", "__phoxal_app"];
/// Portable V0 record identifier.
pub const APPLICATION_RECORD_MAGIC: &[u8; 8] = b"PHXAPP0\n";
/// Fixed record bound, including zero padding.
pub const APPLICATION_RECORD_BYTES: usize = 512;
/// The concrete Cargo target captured by the SDK build.
pub const HOST_EXECUTION_TARGET: &str = env!("PHOXAL_HOST_TARGET");

/// An independently revised application interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum InterfaceKind {
    /// Resolved bundle decoding.
    Bundle = 1,
    /// Supervisor invocation arguments and their semantics.
    SupervisorLaunch = 2,
    /// Participant execution protocol.
    Execution = 3,
    /// Controlled simulation protocol.
    Simulation = 4,
    /// Native simulator invocation and scene staging.
    SimulatorLaunch = 5,
    /// Native-free installer commands and machine-readable status.
    SimulatorInstall = 6,
}

/// Interface scope and its incompatible-change revision, independent of releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterfaceContract {
    /// The interface owner.
    pub kind: InterfaceKind,
    /// The supported revision within the V0 application record family.
    pub revision: u16,
}
impl InterfaceContract {
    /// Declares a different revision of this interface.
    pub const fn with_revision(self, revision: u16) -> Self {
        Self { revision, ..self }
    }
}
impl std::fmt::Display for InterfaceContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.kind {
            InterfaceKind::Bundle => "bundle",
            InterfaceKind::SupervisorLaunch => "supervisor-launch",
            InterfaceKind::Execution => "execution",
            InterfaceKind::Simulation => "simulation",
            InterfaceKind::SimulatorLaunch => "simulator-launch",
            InterfaceKind::SimulatorInstall => "simulator-install",
        };
        write!(f, "{name} revision {}", self.revision)
    }
}
/// Current resolved bundle interface.
pub const BUNDLE_CONTRACT: InterfaceContract = InterfaceContract {
    kind: InterfaceKind::Bundle,
    revision: 1,
};
/// Current supervisor invocation interface.
pub const SUPERVISOR_LAUNCH_CONTRACT: InterfaceContract = InterfaceContract {
    kind: InterfaceKind::SupervisorLaunch,
    revision: 1,
};
/// Current participant execution interface.
pub const EXECUTION_PROTOCOL_CONTRACT: InterfaceContract = InterfaceContract {
    kind: InterfaceKind::Execution,
    revision: 2,
};
/// Current controlled simulation interface.
pub const SIMULATION_PROTOCOL_CONTRACT: InterfaceContract = InterfaceContract {
    kind: InterfaceKind::Simulation,
    revision: 2,
};

/// Current simulator invocation and scene staging interface.
pub const SIMULATOR_LAUNCH_CONTRACT: InterfaceContract = InterfaceContract {
    kind: InterfaceKind::SimulatorLaunch,
    revision: 1,
};
/// Current native installation command/status interface.
/// Revision 1 supports independent native release selection through
/// `--simulator-version`; an older installer must not be selected for it.
pub const SIMULATOR_INSTALL_CONTRACT: InterfaceContract = InterfaceContract {
    kind: InterfaceKind::SimulatorInstall,
    revision: 1,
};

/// Application interfaces and target, statically constructed or decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplicationContract<T = &'static str> {
    /// Resolved bundle support.
    pub bundle: Option<InterfaceContract>,
    /// Application invocation support.
    pub launch: InterfaceContract,
    /// Participant execution support.
    pub execution: Option<InterfaceContract>,
    /// Controlled simulation support, if implemented.
    pub simulation: Option<InterfaceContract>,
    /// Concrete deployment target.
    pub target: T,
}
impl<T: AsRef<str>> ApplicationContract<T> {
    /// Copies an inspected target out of its input buffer.
    pub fn into_owned(self) -> ApplicationContract<String> {
        ApplicationContract {
            bundle: self.bundle,
            launch: self.launch,
            execution: self.execution,
            simulation: self.simulation,
            target: self.target.as_ref().to_owned(),
        }
    }
    /// Checks the interfaces needed by an operation and its target.
    /// An absent optional requirement imposes no condition on that interface.
    pub fn matches<U: AsRef<str>>(&self, required: &ApplicationContract<U>) -> bool {
        self.validate_for(required).is_ok()
    }

    /// Validates operation-scoped requirements without executable inspection or I/O.
    pub fn validate_for<U: AsRef<str>>(
        &self,
        required: &ApplicationContract<U>,
    ) -> Result<(), ApplicationCompatibilityError> {
        for (actual, expected) in [
            (self.bundle, required.bundle),
            (Some(self.launch), Some(required.launch)),
            (self.execution, required.execution),
            (self.simulation, required.simulation),
        ] {
            if let Some(expected) = expected
                && actual != Some(expected)
            {
                return Err(ApplicationCompatibilityError::Interface {
                    required: expected,
                    declared: actual,
                });
            }
        }
        if self.target.as_ref() != required.target.as_ref() {
            return Err(ApplicationCompatibilityError::Target {
                required: required.target.as_ref().into(),
                declared: self.target.as_ref().into(),
            });
        }
        Ok(())
    }
}

/// An interface or deployment-target refusal, independent of package versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplicationCompatibilityError {
    /// A required interface is missing or has an incompatible revision.
    Interface {
        /// Interface needed by the operation.
        required: InterfaceContract,
        /// Interface declared by the application, if present.
        declared: Option<InterfaceContract>,
    },
    /// The executable was built for a different deployment target.
    Target {
        /// Concrete target requested by the operation.
        required: String,
        /// Concrete target embedded by the application.
        declared: String,
    },
}
impl std::fmt::Display for ApplicationCompatibilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interface { required, declared } => write!(
                f,
                "interface contract requires {required}; application declares {declared:?}"
            ),
            Self::Target { required, declared } => write!(
                f,
                "application targets {declared}; this deployment targets {required}"
            ),
        }
    }
}
impl std::error::Error for ApplicationCompatibilityError {}

/// Constructs portable static bytes: magic, four scope/revision tuples,
/// a little-endian target byte length, UTF-8 target bytes, and zero padding.
/// No native enum layout is serialized.
pub const fn encode_application_contract(
    record: &ApplicationContract,
) -> [u8; APPLICATION_RECORD_BYTES] {
    let mut bytes = [0; APPLICATION_RECORD_BYTES];
    let mut i = 0;
    while i < APPLICATION_RECORD_MAGIC.len() {
        bytes[i] = APPLICATION_RECORD_MAGIC[i];
        i += 1;
    }
    let interfaces = [
        record.bundle,
        Some(record.launch),
        record.execution,
        record.simulation,
    ];
    let mut field = 0;
    while field < interfaces.len() {
        if let Some(interface) = interfaces[field] {
            bytes[i] = interface.kind as u8;
            bytes[i + 1] = (interface.revision & 255) as u8;
            bytes[i + 2] = (interface.revision >> 8) as u8;
        }
        i += 3;
        field += 1;
    }
    let target = record.target.as_bytes();
    assert!(!target.is_empty() && target.len() <= APPLICATION_RECORD_BYTES - 22);
    bytes[i] = (target.len() & 255) as u8;
    bytes[i + 1] = (target.len() >> 8) as u8;
    i += 2;
    let mut j = 0;
    while j < target.len() {
        bytes[i + j] = target[j];
        j += 1;
    }
    bytes
}

/// Decodes a bounded V0 record without leaking or manufacturing static references.
pub fn decode_application_contract(bytes: &[u8]) -> Option<ApplicationContract<&str>> {
    if bytes.len() != APPLICATION_RECORD_BYTES || &bytes[..8] != APPLICATION_RECORD_MAGIC {
        return None;
    }
    fn interface(bytes: &[u8], offset: usize) -> Option<InterfaceContract> {
        let kind = match bytes[offset] {
            1 => InterfaceKind::Bundle,
            2 => InterfaceKind::SupervisorLaunch,
            3 => InterfaceKind::Execution,
            4 => InterfaceKind::Simulation,
            5 => InterfaceKind::SimulatorLaunch,
            6 => InterfaceKind::SimulatorInstall,
            _ => return None,
        };
        Some(InterfaceContract {
            kind,
            revision: u16::from_le_bytes([bytes[offset + 1], bytes[offset + 2]]),
        })
    }
    let bundle = if bytes[8..11] == [0, 0, 0] {
        None
    } else {
        Some(interface(bytes, 8)?)
    };
    let launch = interface(bytes, 11)?;
    let execution = if bytes[14..17] == [0, 0, 0] {
        None
    } else {
        Some(interface(bytes, 14)?)
    };
    let simulation = if bytes[17..20] == [0, 0, 0] {
        None
    } else {
        Some(interface(bytes, 17)?)
    };
    if bundle.is_some_and(|v| v.kind != InterfaceKind::Bundle)
        || !matches!(
            launch.kind,
            InterfaceKind::SupervisorLaunch
                | InterfaceKind::SimulatorLaunch
                | InterfaceKind::SimulatorInstall
        )
        || execution.is_some_and(|v| v.kind != InterfaceKind::Execution)
        || simulation.is_some_and(|v| v.kind != InterfaceKind::Simulation)
    {
        return None;
    }
    let length = usize::from(u16::from_le_bytes([bytes[20], bytes[21]]));
    if length == 0 || length > APPLICATION_RECORD_BYTES - 22 {
        return None;
    }
    let target = std::str::from_utf8(&bytes[22..22 + length]).ok()?;
    if target.contains('\0') {
        return None;
    }
    if bytes[22 + length..].iter().any(|b| *b != 0) {
        return None;
    }
    Some(ApplicationContract {
        bundle,
        launch,
        execution,
        simulation,
        target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const RECORD: ApplicationContract = ApplicationContract {
        bundle: Some(BUNDLE_CONTRACT),
        launch: SUPERVISOR_LAUNCH_CONTRACT,
        execution: Some(EXECUTION_PROTOCOL_CONTRACT),
        simulation: Some(SIMULATION_PROTOCOL_CONTRACT),
        target: HOST_EXECUTION_TARGET,
    };
    const ENCODED: [u8; APPLICATION_RECORD_BYTES] = encode_application_contract(&RECORD);
    #[test]
    fn static_and_inspected_records_share_the_portable_contract() {
        let decoded = decode_application_contract(&ENCODED).expect("decode");
        assert!(decoded.matches(&RECORD));
        assert!(decoded.into_owned().matches(&RECORD));
    }
    #[test]
    fn hardware_requires_no_simulation_interface_but_simulation_does() {
        let hardware = ApplicationContract {
            simulation: None,
            ..RECORD
        };
        assert!(hardware.validate_for(&hardware).is_ok());
        assert!(RECORD.validate_for(&hardware).is_ok());
        assert!(hardware.validate_for(&RECORD).is_err());
        let incompatible_simulation = ApplicationContract {
            simulation: Some(SIMULATION_PROTOCOL_CONTRACT.with_revision(3)),
            ..RECORD
        };
        assert!(incompatible_simulation.validate_for(&hardware).is_ok());
        assert!(incompatible_simulation.validate_for(&RECORD).is_err());
        let incompatible_execution = ApplicationContract {
            execution: None,
            ..hardware
        };
        assert!(incompatible_execution.validate_for(&hardware).is_err());
    }

    #[test]
    fn malformed_records_are_refused_and_revisions_remain_independent() {
        assert!(decode_application_contract(&ENCODED[..7]).is_none());
        for offset in [0, 8, 20, 21, 511] {
            let mut bytes = ENCODED;
            bytes[offset] = 255;
            assert!(decode_application_contract(&bytes).is_none());
        }
        let changed = ApplicationContract {
            launch: SUPERVISOR_LAUNCH_CONTRACT.with_revision(2),
            ..RECORD
        };
        let bytes = encode_application_contract(&changed);
        let decoded =
            decode_application_contract(&bytes).expect("well-formed incompatible revision");
        assert!(!decoded.matches(&RECORD));
    }
}
