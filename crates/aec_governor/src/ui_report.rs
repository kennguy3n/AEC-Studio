//! Status-bar surface for the governor.

use serde::{Deserialize, Serialize};

use crate::policy::GovernorPolicy;
use crate::tier::HardwareTier;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalState {
    Nominal,
    Warm,
    Critical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernorState {
    pub tier: HardwareTier,
    pub active_render_jobs: u32,
    pub active_ai_jobs: u32,
    pub memory_used_mb: u64,
    pub memory_budget_mb: u64,
    pub thermal: ThermalState,
    pub user_paused: bool,
    pub overrides: Vec<String>,
}

impl GovernorState {
    pub fn new(policy: &GovernorPolicy, total_ram_mb: u64) -> Self {
        Self {
            tier: policy.tier,
            active_render_jobs: 0,
            active_ai_jobs: 0,
            memory_used_mb: 0,
            memory_budget_mb: total_ram_mb,
            thermal: ThermalState::Nominal,
            user_paused: false,
            overrides: Vec::new(),
        }
    }
}
