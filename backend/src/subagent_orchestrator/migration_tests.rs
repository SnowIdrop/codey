use super::*;
use serde_json::json;
use tempfile::tempdir;

fn legacy_ledger(root: &Path, version: u32, writable: bool) -> PathBuf {
    let spawn = json!({"task_name": "task_a", "agent_type": "codey_worker", "message": "Work"});
    assert_eq!(
        pre_spawn(root, "runtime", "session", Some(&spawn), 0, 10).unwrap(),
        None
    );
    post_spawn(
        root,
        "runtime",
        "session",
        Some(&spawn),
        Some(&json!({"agent_id": "agent-a"})),
        11,
    )
    .unwrap();
    subagent_started_with_context(
        root,
        "runtime",
        "session",
        "agent-a",
        Some("codey_worker"),
        None,
        12,
    )
    .unwrap();
    let path = root.join(hash_component("session")).join(LEDGER_FILE);
    let mut ledger: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ledger["schema_version"] = json!(version);
    ledger["reservations"]["task_a"]["write_capable"] = json!(writable);
    // v14 fields removed by v15 must not prevent migration of live attempts.
    ledger["batch_number"] = json!(1);
    ledger["reservations"]["task_a"]["acceptance"] = json!([]);
    fs::write(&path, serde_json::to_vec(&ledger).unwrap()).unwrap();
    path
}

#[test]
fn supported_legacy_ledgers_preserve_active_identity_and_workspace_reservation() {
    for version in [14, 15] {
        let temp = tempdir().unwrap();
        let path = legacy_ledger(temp.path(), version, true);
        let before: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            active_reservation_count(temp.path(), "runtime", "session", 20).unwrap(),
            Some(1)
        );
        let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["schema_version"], json!(LEDGER_SCHEMA_VERSION));
        for field in [
            "attempt_id",
            "fencing_token",
            "origin_runtime_id_hash",
            "runtime_generation",
            "agent_id_hash",
            "state",
            "capabilities",
        ] {
            assert_eq!(
                after["reservations"]["task_a"][field], before["reservations"]["task_a"][field],
                "{field}"
            );
        }
        assert!(
            after["reservations"]["task_a"]
                .get("write_capable")
                .is_none()
        );
        let second = json!({"task_name":"task_b","agent_type":"codey_worker","message":"Work"});
        assert!(
            pre_spawn(temp.path(), "runtime", "other", Some(&second), 0, 21)
                .unwrap()
                .unwrap()
                .contains("CODEY_SUBAGENT_WORKSPACE_BUSY")
        );
        subagent_stopped(temp.path(), "runtime", "session", "agent-a", 22).unwrap();
        assert_eq!(
            pre_spawn(temp.path(), "runtime", "other", Some(&second), 0, 23).unwrap(),
            None
        );
    }
}

#[test]
fn migration_never_expands_legacy_permissions() {
    let temp = tempdir().unwrap();
    let path = legacy_ledger(temp.path(), 15, false);
    assert_eq!(
        active_reservation_count(temp.path(), "runtime", "session", 20).unwrap(),
        Some(1)
    );
    let ledger: SessionLedger = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(!reservation_declares_write(&ledger.reservations["task_a"]));
    let denied = authorize_child_tool_with_context(
        temp.path(),
        "runtime",
        "session",
        ChildToolContext {
            agent_id: "agent-a",
            agent_type: Some("codey_worker"),
            transcript_path: None,
            tool_name: "apply_patch",
            tool_input: None,
        },
        21,
    )
    .unwrap();
    assert!(denied.is_some());
}

#[test]
fn unsupported_or_incomplete_legacy_ledger_is_not_silently_rewritten() {
    for version in [13, LEDGER_SCHEMA_VERSION + 1] {
        let temp = tempdir().unwrap();
        let path = legacy_ledger(temp.path(), version, true);
        let before = fs::read(&path).unwrap();
        assert!(active_reservation_count(temp.path(), "runtime", "session", 20).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    let temp = tempdir().unwrap();
    let path = legacy_ledger(temp.path(), 14, true);
    let mut ledger: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    ledger["reservations"]["task_a"]
        .as_object_mut()
        .unwrap()
        .remove("origin_runtime_id_hash");
    let bytes = serde_json::to_vec(&ledger).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(active_reservation_count(temp.path(), "runtime", "session", 20).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn migration_preserves_fencing_and_does_not_quarantine_supported_ledgers() {
    let temp = tempdir().unwrap();
    let path = legacy_ledger(temp.path(), 14, true);
    pre_interrupt_agent(
        temp.path(),
        "runtime",
        "session",
        Some(&json!({"target":"agent-a"})),
        20,
    )
    .unwrap();
    assert_eq!(
        active_reservation_count(temp.path(), "runtime", "session", 21).unwrap(),
        Some(1)
    );
    recover_corrupt_gate_state(temp.path(), "runtime", "session", 22).unwrap();
    assert!(path.exists());
    let ledger: SessionLedger = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(ledger.reservations["task_a"].fenced_at_ms.is_some());
    assert_eq!(
        ledger.reservations["task_a"].outcome,
        ExecutionOutcome::Lost
    );
}
