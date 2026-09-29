//! AGENT_CTX environment contract reader (v1 of the contract).
//!
//! Reads `AGENT_CTX_*` environment variables defensively. Missing variables
//! never break behavior; malformed values are ignored. See
//! `schemas/agent-context-env/agent-context-env.md` for the full contract.

use serde::{Deserialize, Serialize};

/// Effective AGENT_CTX context for the current process.
///
/// Every field is optional: tools must tolerate entirely missing context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentCtx {
    /// Contract version (e.g., "1")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Platform name (e.g., "oqto")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    /// Platform build/version (e.g., "0.17.3", "git:abc1234")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_version: Option<String>,

    /// Active harness/runtime (e.g., "pi")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,

    /// Runtime mode (e.g., "runner")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_mode: Option<String>,

    /// Platform session id (stable for joins)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_session_id: Option<String>,

    /// Harness-native session id (stable for joins)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness_session_id: Option<String>,

    /// Human-readable session label (display only — mutable, not durable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,

    /// Short friendly id for logs/UI
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readable_id: Option<String>,

    /// Stable workspace id/hash
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,

    /// Absolute workspace path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,

    /// Platform user id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,

    /// Active model id (e.g., "anthropic/claude-sonnet-4")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Per-action request id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    /// Cross-service correlation/trace id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    /// Agent identity within the multiplexer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Address of the agent process
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_address: Option<String>,
    /// Stable machine identity
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// Multiplexer name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub multiplexer: Option<String>,
    /// Hostname of the agent node
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_hostname: Option<String>,
    /// OS and architecture
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_arch: Option<String>,

    /// Active sandbox profile (observability hint)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<String>,
}

impl AgentCtx {
    /// Read AGENT_CTX_* from the process environment. Always returns a value;
    /// fields are populated only for variables that are present and non-empty.
    pub fn from_env() -> Self {
        Self {
            version: read("AGENT_CTX_VERSION"),
            platform: read("AGENT_CTX_PLATFORM_NAME"),
            platform_version: read("AGENT_CTX_PLATFORM_VERSION"),
            harness: read("AGENT_CTX_HARNESS"),
            run_mode: read("AGENT_CTX_RUN_MODE"),
            platform_session_id: read("AGENT_CTX_PLATFORM_SESSION_ID"),
            harness_session_id: read("AGENT_CTX_HARNESS_SESSION_ID"),
            session_name: read("AGENT_CTX_SESSION_NAME"),
            readable_id: read("AGENT_CTX_READABLE_ID"),
            workspace_id: read("AGENT_CTX_WORKSPACE_ID"),
            workspace_path: read("AGENT_CTX_WORKSPACE_PATH"),
            user_id: read("AGENT_CTX_USER_ID"),
            model: read("AGENT_CTX_MODEL"),
            request_id: read("AGENT_CTX_REQUEST_ID"),
            correlation_id: read("AGENT_CTX_CORRELATION_ID"),
            sandbox_profile: read("AGENT_CTX_SANDBOX_PROFILE"),
            agent_id: read("AGENT_CTX_AGENT_ID"),
            agent_address: read("AGENT_CTX_AGENT_ADDRESS"),
            machine_id: read("AGENT_CTX_MACHINE_ID"),
            multiplexer: read("AGENT_CTX_MULTIPLEXER"),
            node_hostname: read("AGENT_CTX_NODE_HOSTNAME"),
            os_arch: read("AGENT_CTX_OS_ARCH"),
        }
    }

    /// True when no AGENT_CTX_* variables were set.
    pub fn is_empty(&self) -> bool {
        self.version.is_none()
            && self.platform.is_none()
            && self.platform_version.is_none()
            && self.harness.is_none()
            && self.run_mode.is_none()
            && self.platform_session_id.is_none()
            && self.harness_session_id.is_none()
            && self.session_name.is_none()
            && self.readable_id.is_none()
            && self.workspace_id.is_none()
            && self.workspace_path.is_none()
            && self.user_id.is_none()
            && self.model.is_none()
            && self.request_id.is_none()
            && self.correlation_id.is_none()
            && self.sandbox_profile.is_none()
            && self.agent_id.is_none()
            && self.agent_address.is_none()
            && self.machine_id.is_none()
            && self.multiplexer.is_none()
            && self.node_hostname.is_none()
            && self.os_arch.is_none()
    }

    /// Stable session ids for indexing/dedup. Includes platform and harness ids
    /// when present, in that order.
    pub fn session_ids(&self) -> Vec<String> {
        let mut ids = Vec::with_capacity(2);
        if let Some(id) = &self.platform_session_id {
            ids.push(id.clone());
        }
        if let Some(id) = &self.harness_session_id
            && Some(id) != self.platform_session_id.as_ref()
        {
            ids.push(id.clone());
        }
        ids
    }
}

fn read(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_AGENT_CTX_ENV: &[&str] = &[
        "AGENT_CTX_VERSION",
        "AGENT_CTX_PLATFORM_NAME",
        "AGENT_CTX_PLATFORM_VERSION",
        "AGENT_CTX_HARNESS",
        "AGENT_CTX_RUN_MODE",
        "AGENT_CTX_PLATFORM_SESSION_ID",
        "AGENT_CTX_HARNESS_SESSION_ID",
        "AGENT_CTX_SESSION_NAME",
        "AGENT_CTX_READABLE_ID",
        "AGENT_CTX_WORKSPACE_ID",
        "AGENT_CTX_WORKSPACE_PATH",
        "AGENT_CTX_USER_ID",
        "AGENT_CTX_MODEL",
        "AGENT_CTX_REQUEST_ID",
        "AGENT_CTX_CORRELATION_ID",
        "AGENT_CTX_SANDBOX_PROFILE",
        "AGENT_CTX_AGENT_ID",
        "AGENT_CTX_AGENT_ADDRESS",
        "AGENT_CTX_MACHINE_ID",
        "AGENT_CTX_MULTIPLEXER",
        "AGENT_CTX_NODE_HOSTNAME",
        "AGENT_CTX_OS_ARCH",
    ];

    /// Helper that replaces all AGENT_CTX vars, runs a closure, and restores.
    /// Tests share a process so we serialize via a mutex. The harness running
    /// the tests may itself set AGENT_CTX_* values, so every known field must
    /// be cleared unless explicitly provided by the test.
    fn with_env<F: FnOnce()>(vars: &[(&str, &str)], f: F) {
        use std::panic::{AssertUnwindSafe, resume_unwind};
        use std::sync::Mutex;

        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let prior: Vec<(String, Option<String>)> = ALL_AGENT_CTX_ENV
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for k in ALL_AGENT_CTX_ENV {
            unsafe { std::env::remove_var(k) };
        }
        for (k, v) in vars {
            unsafe { std::env::set_var(k, v) };
        }

        let result = std::panic::catch_unwind(AssertUnwindSafe(f));

        for (k, prior) in prior {
            match prior {
                Some(v) => unsafe { std::env::set_var(&k, v) },
                None => unsafe { std::env::remove_var(&k) },
            }
        }

        if let Err(payload) = result {
            resume_unwind(payload);
        }
    }

    #[test]
    fn from_env_empty_when_unset() {
        with_env(
            &[
                ("AGENT_CTX_VERSION", ""),
                ("AGENT_CTX_PLATFORM_NAME", ""),
                ("AGENT_CTX_USER_ID", ""),
            ],
            || {
                let ctx = AgentCtx::from_env();
                assert!(ctx.is_empty());
            },
        );
    }

    #[test]
    fn from_env_populates_present_fields() {
        with_env(
            &[
                ("AGENT_CTX_VERSION", "1"),
                ("AGENT_CTX_PLATFORM_NAME", "oqto"),
                ("AGENT_CTX_USER_ID", "u_123"),
                ("AGENT_CTX_PLATFORM_SESSION_ID", "sess_8f"),
                ("AGENT_CTX_AGENT_ID", "agent-1"),
                ("AGENT_CTX_MACHINE_ID", "machine-1"),
                ("AGENT_CTX_MULTIPLEXER", "herdr"),
                ("AGENT_CTX_OS_ARCH", "linux/amd64"),
            ],
            || {
                let ctx = AgentCtx::from_env();
                assert_eq!(ctx.version.as_deref(), Some("1"));
                assert_eq!(ctx.platform.as_deref(), Some("oqto"));
                assert_eq!(ctx.user_id.as_deref(), Some("u_123"));
                assert_eq!(ctx.platform_session_id.as_deref(), Some("sess_8f"));
                assert_eq!(ctx.agent_id.as_deref(), Some("agent-1"));
                assert_eq!(ctx.machine_id.as_deref(), Some("machine-1"));
                assert_eq!(ctx.multiplexer.as_deref(), Some("herdr"));
                assert_eq!(ctx.os_arch.as_deref(), Some("linux/amd64"));
                assert!(!ctx.is_empty());
            },
        );
    }

    #[test]
    fn whitespace_only_values_are_ignored() {
        with_env(&[("AGENT_CTX_VERSION", "   ")], || {
            assert!(AgentCtx::from_env().version.is_none());
        });
    }

    #[test]
    fn session_ids_dedup_when_platform_and_harness_match() {
        let ctx = AgentCtx {
            platform_session_id: Some("s1".into()),
            harness_session_id: Some("s1".into()),
            ..Default::default()
        };
        assert_eq!(ctx.session_ids(), vec!["s1".to_string()]);
    }

    #[test]
    fn session_ids_returns_both_when_distinct() {
        let ctx = AgentCtx {
            platform_session_id: Some("p".into()),
            harness_session_id: Some("h".into()),
            ..Default::default()
        };
        assert_eq!(ctx.session_ids(), vec!["p".to_string(), "h".to_string()]);
    }
}
