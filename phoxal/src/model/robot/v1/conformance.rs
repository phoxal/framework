use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceStatus {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceReport {
    pub status: ConformanceStatus,
    pub evidence: Vec<ConformanceEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<ConformanceFailure>,
}

impl ConformanceReport {
    #[must_use]
    pub fn pass(evidence: Vec<ConformanceEvidence>) -> Self {
        Self {
            status: ConformanceStatus::Pass,
            evidence,
            failures: Vec::new(),
        }
    }

    #[must_use]
    pub fn fail(evidence: Vec<ConformanceEvidence>, failures: Vec<ConformanceFailure>) -> Self {
        Self {
            status: ConformanceStatus::Fail,
            evidence,
            failures,
        }
    }

    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self.status, ConformanceStatus::Pass)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceEvidence {
    pub check: String,
    pub detail: String,
}

impl ConformanceEvidence {
    #[must_use]
    pub fn new(check: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceFailure {
    pub check: String,
    pub reason: String,
}

impl ConformanceFailure {
    #[must_use]
    pub fn new(check: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            reason: reason.into(),
        }
    }
}
