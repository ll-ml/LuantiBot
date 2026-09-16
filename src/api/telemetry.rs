use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TELEMETRY_SCHEMA_VERSION: u64 = 1;
const TELEMETRY_ONLINE_TTL: Duration = Duration::from_secs(8);

#[derive(Clone, Default)]
pub(super) struct AgentTelemetryStore {
    inner: Arc<Mutex<AgentTelemetryState>>,
}

#[derive(Default)]
struct AgentTelemetryState {
    revision: u64,
    updated_at: Option<Instant>,
    snapshot: Option<Value>,
}

pub(super) struct PublishResult {
    pub revision: u64,
    pub changed: bool,
}

impl AgentTelemetryStore {
    pub fn publish(&self, body: &str) -> Result<PublishResult, &'static str> {
        let snapshot: Value = serde_json::from_str(body).map_err(|_| "invalid_json")?;
        if !snapshot.is_object() {
            return Err("telemetry_must_be_an_object");
        }
        if snapshot.get("schema_version").and_then(Value::as_u64)
            != Some(TELEMETRY_SCHEMA_VERSION)
        {
            return Err("unsupported_telemetry_schema");
        }
        Ok(self.publish_at(snapshot, Instant::now()))
    }

    pub fn snapshot(&self) -> Value {
        self.snapshot_at(Instant::now())
    }

    fn publish_at(&self, snapshot: Value, now: Instant) -> PublishResult {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let changed = state.snapshot.as_ref() != Some(&snapshot);
        if changed {
            state.revision = state.revision.saturating_add(1);
            state.snapshot = Some(snapshot);
        }
        state.updated_at = Some(now);
        PublishResult {
            revision: state.revision,
            changed,
        }
    }

    fn snapshot_at(&self, now: Instant) -> Value {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let age = state
            .updated_at
            .map(|updated_at| now.saturating_duration_since(updated_at));
        json!({
            "online": age.is_some_and(|age| age <= TELEMETRY_ONLINE_TTL),
            "age_ms": age.map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX)),
            "revision": state.revision,
            "agent": state.snapshot,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn telemetry(tick: u64) -> Value {
        json!({"schema_version": 1, "tick": tick, "phase": "idle"})
    }

    #[test]
    fn empty_store_is_offline() {
        let store = AgentTelemetryStore::default();
        let snapshot = store.snapshot();
        assert_eq!(snapshot["online"], false);
        assert_eq!(snapshot["revision"], 0);
        assert!(snapshot["agent"].is_null());
        assert!(snapshot["age_ms"].is_null());
    }

    #[test]
    fn heartbeat_refreshes_age_without_changing_revision() {
        let store = AgentTelemetryStore::default();
        let started = Instant::now();
        let first = store.publish_at(telemetry(1), started);
        let heartbeat = store.publish_at(telemetry(1), started + Duration::from_secs(2));

        assert!(first.changed);
        assert_eq!(first.revision, 1);
        assert!(!heartbeat.changed);
        assert_eq!(heartbeat.revision, 1);

        let snapshot = store.snapshot_at(started + Duration::from_secs(3));
        assert_eq!(snapshot["online"], true);
        assert_eq!(snapshot["age_ms"], 1_000);
    }

    #[test]
    fn changed_snapshot_increments_revision_and_replaces_payload() {
        let store = AgentTelemetryStore::default();
        let started = Instant::now();
        store.publish_at(telemetry(1), started);
        let update = store.publish_at(telemetry(2), started + Duration::from_secs(1));

        assert!(update.changed);
        assert_eq!(update.revision, 2);
        let snapshot = store.snapshot_at(started + Duration::from_secs(1));
        assert_eq!(snapshot["agent"]["tick"], 2);
    }

    #[test]
    fn old_snapshot_is_reported_as_stale() {
        let store = AgentTelemetryStore::default();
        let started = Instant::now();
        store.publish_at(telemetry(1), started);

        let snapshot = store.snapshot_at(started + TELEMETRY_ONLINE_TTL + Duration::from_millis(1));
        assert_eq!(snapshot["online"], false);
        assert_eq!(snapshot["agent"]["tick"], 1);
    }

    #[test]
    fn publish_rejects_invalid_or_unknown_payloads() {
        let store = AgentTelemetryStore::default();
        assert_eq!(store.publish("not json").err(), Some("invalid_json"));
        assert_eq!(
            store.publish("[]").err(),
            Some("telemetry_must_be_an_object")
        );
        assert_eq!(
            store.publish(r#"{"schema_version":2}"#).err(),
            Some("unsupported_telemetry_schema")
        );
    }
}
