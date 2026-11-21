use serde::{Deserialize, Serialize};

/// Optional ceilings for shared text previews and their retained artifacts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtifactOutputConfig {
    pub spill_threshold_kib: Option<usize>,
    pub head_kib: Option<usize>,
    pub tail_kib: Option<usize>,
    pub tail_lines: Option<usize>,
    pub file_cap_mib: Option<usize>,
}
