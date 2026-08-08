//! Product-owned safety and resource policy for delegated agents.
//!
//! Provider profiles choose credentials and model routes. This policy belongs
//! to Ovim's harness: it is always enabled, read-only, and network-isolated.

use std::path::PathBuf;

/// Compact routing guidance shared by root and delegated coordinators.
pub const DELEGATION_GUIDANCE: &str = "Luna@max: implementation and checklists. Terra: bounded implementation or measurement. Sol: subtle correctness and skeptical review; default to medium/high, and reserve xhigh for a named concurrency or architecture risk. Quote constraints; require evidence.";

#[derive(Debug, Clone, PartialEq)]
pub struct DelegatedAgentPolicy {
    pub max_concurrent: usize,
    pub max_queued: usize,
    pub max_children_per_parent: usize,
    pub max_total_per_run: usize,
    pub max_depth: usize,
    pub default_timeout_seconds: u64,
    pub budgets: DelegatedAgentBudget,
    pub workspaces: DelegatedAgentWorkspacePolicy,
}

impl Default for DelegatedAgentPolicy {
    fn default() -> Self {
        Self {
            max_concurrent: 3,
            max_queued: 8,
            max_children_per_parent: 4,
            max_total_per_run: 8,
            // Flat delegation is the safe default. Nested workflows remain an
            // explicit policy choice rather than an emergent model decision.
            max_depth: 1,
            default_timeout_seconds: 600,
            budgets: DelegatedAgentBudget::default(),
            workspaces: DelegatedAgentWorkspacePolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DelegatedAgentBudget {
    pub max_provider_events_per_agent: usize,
    pub max_tool_calls_per_agent: usize,
    pub max_total_provider_events: usize,
    pub max_total_tool_calls: usize,
}

impl Default for DelegatedAgentBudget {
    fn default() -> Self {
        Self {
            max_provider_events_per_agent: 256,
            max_tool_calls_per_agent: 48,
            max_total_provider_events: 1024,
            max_total_tool_calls: 160,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedAgentWorkspacePolicy {
    pub root: Option<PathBuf>,
    pub branch_prefix: String,
    pub completed_retention_hours: u64,
    pub minimum_free_space_mb: u64,
}

impl Default for DelegatedAgentWorkspacePolicy {
    fn default() -> Self {
        Self {
            root: None,
            branch_prefix: "ovim".into(),
            completed_retention_hours: 24,
            minimum_free_space_mb: 2_048,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_policy_is_safe_and_flat_by_default() {
        let policy = DelegatedAgentPolicy::default();
        assert_eq!(policy.max_depth, 1);
        assert_eq!(policy.max_concurrent, 3);
        assert_eq!(policy.max_total_per_run, 8);
        assert!(policy.max_concurrent <= policy.max_total_per_run);
        assert!(policy.default_timeout_seconds > 0);
    }

    #[test]
    fn routing_guidance_stays_compact_and_names_each_codex_tier() {
        assert!(DELEGATION_GUIDANCE.split_whitespace().count() <= 40);
        for name in ["Luna@max", "Terra", "Sol"] {
            assert!(DELEGATION_GUIDANCE.contains(name));
        }
    }
}
