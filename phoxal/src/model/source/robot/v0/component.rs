use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Role, capability, driver::DriverConfig};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub component: String,
    pub mount_link: String,
    /// Hardware connection metadata retained as part of the exact v0 file
    /// contract. The canonical runtime model does not currently consume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<DriverConfig>,
    /// Authored role hints retained as part of the exact v0 file contract.
    /// Role-resolution policy has no current canonical runtime consumer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, Vec<Role>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, capability::Parameters>,
}
