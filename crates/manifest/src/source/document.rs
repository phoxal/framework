//! The one implementation of "read, validate and write one authored document".
//!
//! The three authored grammars differ; the mechanics of getting one off disk
//! and back do not. Everything kind-specific is reached through [`Document`],
//! whose only parameter is the document type itself, so each grammar keeps its
//! own DTO, its own rules and its own typed rejections.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::strict_yaml::StrictYamlError;
use super::{component, robot, simulation};

/// Which authored document a value or a failure belongs to.
///
/// Every authored document kind owns its schema version independently:
/// `robot.yaml` v1 can be introduced without forcing `component.yaml` or
/// `simulation.yaml` to advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// A project-root `robot.yaml` document.
    Robot,
    /// A component-local `component.yaml` document.
    Component,
    /// A component-local `simulation.yaml` document.
    Simulation,
}

impl DocumentKind {
    /// Every authored document kind, in stable order.
    pub const ALL: [Self; 3] = [Self::Robot, Self::Component, Self::Simulation];

    /// The file name a document of this kind always has inside its directory.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Robot => "robot.yaml",
            Self::Component => "component.yaml",
            Self::Simulation => "simulation.yaml",
        }
    }
}

impl fmt::Display for DocumentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Robot => "robot",
            Self::Component => "component",
            Self::Simulation => "simulation",
        })
    }
}

/// The authored rules one document broke.
///
/// Each list stays in its own grammar's vocabulary so a caller can match the
/// exact rule rather than parse a rendered message.
#[derive(Debug)]
pub enum Violations {
    Robot(Vec<robot::v0::ValidationError>),
    Component(Vec<component::v0::ValidationError>),
    Simulation(Vec<simulation::v0::ValidationError>),
}

impl Violations {
    /// The document kind whose rules these are.
    #[must_use]
    pub const fn kind(&self) -> DocumentKind {
        match self {
            Self::Robot(_) => DocumentKind::Robot,
            Self::Component(_) => DocumentKind::Component,
            Self::Simulation(_) => DocumentKind::Simulation,
        }
    }

    /// The broken `robot.yaml` rules, when these are a robot document's.
    #[must_use]
    pub fn robot(&self) -> Option<&[robot::v0::ValidationError]> {
        match self {
            Self::Robot(errors) => Some(errors),
            _ => None,
        }
    }

    /// The broken `component.yaml` rules, when these are a component
    /// document's.
    #[must_use]
    pub fn component(&self) -> Option<&[component::v0::ValidationError]> {
        match self {
            Self::Component(errors) => Some(errors),
            _ => None,
        }
    }

    /// The broken `simulation.yaml` rules, when these are a simulation
    /// document's.
    #[must_use]
    pub fn simulation(&self) -> Option<&[simulation::v0::ValidationError]> {
        match self {
            Self::Simulation(errors) => Some(errors),
            _ => None,
        }
    }
}

impl fmt::Display for Violations {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn join<T: fmt::Display>(
            errors: &[T],
            formatter: &mut fmt::Formatter<'_>,
        ) -> std::fmt::Result {
            for (index, error) in errors.iter().enumerate() {
                if index > 0 {
                    formatter.write_str("\n")?;
                }
                write!(formatter, "{error}")?;
            }
            Ok(())
        }
        match self {
            Self::Robot(errors) => join(errors, formatter),
            Self::Component(errors) => join(errors, formatter),
            Self::Simulation(errors) => join(errors, formatter),
        }
    }
}

/// Where a document that failed came from.
///
/// A document parsed straight from text has no path to name, and saying so is
/// better than inventing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The document was read from this file.
    File(PathBuf),
    /// The document was handed to the parser as text.
    Text,
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(formatter, " {}", path.display()),
            Self::Text => Ok(()),
        }
    }
}

/// A failure reading, validating or writing one authored document.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The document could not be read from disk.
    #[error("failed to read {kind} document {}: {source}", path.display())]
    Read {
        kind: DocumentKind,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The document could not be written to disk.
    #[error("failed to write {kind} document {}: {source}", path.display())]
    Write {
        kind: DocumentKind,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The text is not YAML, or is not a document of this kind.
    #[error("failed to parse {kind} document{origin}: {source}")]
    Parse {
        kind: DocumentKind,
        origin: Origin,
        #[source]
        source: serde_yaml::Error,
    },

    /// The text is YAML but leaves the strict subset authored documents are
    /// restricted to.
    #[error("failed to parse {kind} document{origin}: {source}")]
    StrictYaml {
        kind: DocumentKind,
        origin: Origin,
        #[source]
        source: StrictYamlError,
    },

    /// The document parsed, and then broke its own grammar's rules.
    #[error("invalid {kind} document{origin}:\n{violations}", kind = violations.kind())]
    Invalid {
        origin: Origin,
        violations: Violations,
    },

    /// The document declares parents, which can only be resolved relative to
    /// the file that declared them.
    #[error(
        "{kind} document{origin} declares `extends`, which names paths that only exist relative \
         to a document on disk"
    )]
    UnresolvableExtends { kind: DocumentKind, origin: Origin },

    /// A declared parent could not be composed into the leaf document.
    #[error("failed to compose {kind} document {}: {source}", leaf.display())]
    Compose {
        kind: DocumentKind,
        leaf: PathBuf,
        #[source]
        source: ComposeError,
    },
}

impl SourceError {
    /// Which authored document this failure is about.
    #[must_use]
    pub const fn kind(&self) -> DocumentKind {
        match self {
            Self::Read { kind, .. }
            | Self::Write { kind, .. }
            | Self::Parse { kind, .. }
            | Self::StrictYaml { kind, .. }
            | Self::UnresolvableExtends { kind, .. }
            | Self::Compose { kind, .. } => *kind,
            Self::Invalid { violations, .. } => violations.kind(),
        }
    }

    /// The grammar rules the document broke, when it parsed and then failed
    /// them. Absent for every failure that happened before validation ran.
    #[must_use]
    pub const fn violations(&self) -> Option<&Violations> {
        match self {
            Self::Invalid { violations, .. } => Some(violations),
            _ => None,
        }
    }
}

/// Why composing a document with its declared parents failed.
#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    /// A document taking part in the composition could not be read.
    #[error("failed to read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A document taking part in the composition is not YAML, or leaves the
    /// strict subset authored documents are read in.
    #[error("failed to parse {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    /// A document taking part in the composition leaves the strict subset.
    #[error("{}: {source}", path.display())]
    StrictYaml {
        path: PathBuf,
        #[source]
        source: StrictYamlError,
    },

    /// The document is not a mapping, so it cannot declare or merge anything.
    #[error("document {} must be a mapping", path.display())]
    NotAMapping { path: PathBuf },

    /// The `extends` value is not a list of paths.
    #[error("invalid extends list in {}: {source}", path.display())]
    MalformedExtends {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },

    /// A parent path is absolute; parents are resolved against the leaf's own
    /// directory so that a project stays relocatable.
    #[error("extends path must be relative: {}", path.display())]
    AbsoluteParent { path: PathBuf },

    /// A parent could not be resolved on disk.
    #[error("failed to resolve parent {}: {source}", path.display())]
    UnresolvedParent {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A parent resolves outside the leaf document's own directory.
    #[error("parent {} escapes directory {}", path.display(), root.display())]
    EscapingParent { path: PathBuf, root: PathBuf },

    /// A document lists itself as a parent.
    #[error("document cannot extend itself: {}", path.display())]
    SelfParent { path: PathBuf },

    /// The same parent is listed twice, so its position in the merge order is
    /// ambiguous.
    #[error("duplicate parent: {}", path.display())]
    DuplicateParent { path: PathBuf },

    /// A parent declares parents of its own. The leaf is the single
    /// deterministic authority for parent order, so nesting is refused rather
    /// than flattened silently.
    #[error("parent {} declares nested extends; list every parent directly in the leaf", path.display())]
    NestedExtends { path: PathBuf },
}

/// One authored document kind: how it is identified, and what makes it valid.
///
/// Implementors get reading, parsing and writing for free; only the grammar's
/// own rules are theirs to supply.
pub(crate) trait Document: Sized + Serialize + DeserializeOwned {
    /// Which authored document this type is the exact shape of.
    const KIND: DocumentKind;

    /// Check the document against the rules its own grammar owns.
    ///
    /// # Errors
    ///
    /// Returns every rule the document broke, not just the first.
    fn check(&self) -> Result<(), Violations>;

    /// Reject text that is YAML but outside the subset this document kind
    /// accepts. Only `robot.yaml` narrows the subset today.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::StrictYaml`] when the text leaves the subset.
    fn precheck(_text: &str, _origin: &Origin) -> Result<(), SourceError> {
        Ok(())
    }

    /// Parse and validate one document from its exact text.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Parse`] when the text is not this document, and
    /// [`SourceError::Invalid`] when it is but breaks the grammar's rules.
    fn read_text(text: &str, origin: Origin) -> Result<Self, SourceError> {
        Self::precheck(text, &origin)?;
        let document: Self = serde_yaml::from_str(text).map_err(|source| SourceError::Parse {
            kind: Self::KIND,
            origin: origin.clone(),
            source,
        })?;
        document
            .check()
            .map_err(|violations| SourceError::Invalid { origin, violations })?;
        Ok(document)
    }

    /// Parse and validate the document at `path`, which is either the document
    /// file itself or the directory that holds it.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Read`] when the file cannot be read, and
    /// otherwise whatever [`Document::read_text`] rejects.
    fn read_path(path: &Path) -> Result<Self, SourceError> {
        let path = document_path(path, Self::KIND);
        let text = std::fs::read_to_string(&path).map_err(|source| SourceError::Read {
            kind: Self::KIND,
            path: path.clone(),
            source,
        })?;
        Self::read_text(&text, Origin::File(path))
    }

    /// Write the document into `directory`, creating the directory if needed.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Write`] when the directory or file cannot be
    /// written.
    fn write_dir(&self, directory: &Path) -> Result<(), SourceError> {
        std::fs::create_dir_all(directory).map_err(|source| SourceError::Write {
            kind: Self::KIND,
            path: directory.to_path_buf(),
            source,
        })?;
        let destination = directory.join(Self::KIND.file_name());
        // The DTO is a plain serde value with no non-string map keys and no
        // unrepresentable float, so serialization cannot fail; treating a
        // failure as a write failure keeps one error shape for one operation.
        let text = serde_yaml::to_string(self).map_err(|source| SourceError::Write {
            kind: Self::KIND,
            path: destination.clone(),
            source: std::io::Error::other(source),
        })?;
        std::fs::write(&destination, text).map_err(|source| SourceError::Write {
            kind: Self::KIND,
            path: destination,
            source,
        })
    }
}

/// The document file a caller meant, whether they named the file or the
/// directory that holds it.
pub(crate) fn document_path(path: &Path, kind: DocumentKind) -> PathBuf {
    if path.is_dir() {
        path.join(kind.file_name())
    } else {
        path.to_path_buf()
    }
}
