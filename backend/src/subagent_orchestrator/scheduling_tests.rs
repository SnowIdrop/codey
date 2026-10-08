use super::*;
use serde_json::json;
use std::sync::{Arc, Barrier};
use tempfile::tempdir;

fn input(task: &str) -> Value {
    json!({"task_name": task, "agent_type": "codey_worker", "message": "Update assigned files"})
}

#[test]
fn simultaneous_sessions_cannot_reserve_overlapping_workspaces() {
    for round in 0..12 {
        let temp = tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let handles = (0..2)
            .map(|index| {
                let root = temp.path().to_path_buf();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    pre_spawn_with_workspace_and_turn(
                        &root,
                        "runtime",
                        &format!("session-{index}"),
                        Some(&input(&format!("task_{index}"))),
                        RootHookContext::new(
                            Some(if index == 0 { "/repo" } else { "/repo/src" }),
                            0,
                            10,
                        ),
                    )
                    .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results.iter().filter(|result| result.is_none()).count(),
            1,
            "round {round}: {results:?}"
        );
        assert!(
            results
                .iter()
                .flatten()
                .all(|reason| reason.contains("CODEY_SUBAGENT_WORKSPACE_BUSY"))
        );
    }
}

#[test]
fn admission_timeout_leaves_no_reservation_and_does_not_block_status_reads() {
    let temp = tempdir().unwrap();
    let guard = AdmissionGuard::acquire(temp.path()).unwrap();
    assert_eq!(
        active_reservation_count(temp.path(), "runtime", "session", 1).unwrap(),
        None
    );
    let error = pre_spawn_with_workspace_and_turn(
        temp.path(),
        "runtime",
        "session",
        Some(&input("task_a")),
        RootHookContext::new(Some("/repo"), 0, 10),
    )
    .unwrap_err();
    assert!(error.to_string().contains("CODEY_SUBAGENT_ADMISSION_BUSY"));
    assert!(
        !temp
            .path()
            .join(hash_component("session"))
            .join(LEDGER_FILE)
            .exists()
    );
    drop(guard);
    assert_eq!(
        pre_spawn_with_workspace_and_turn(
            temp.path(),
            "runtime",
            "session",
            Some(&input("task_a")),
            RootHookContext::new(Some("/repo"), 0, 20),
        )
        .unwrap(),
        None
    );
}

#[test]
fn disjoint_workspaces_remain_available_across_sessions() {
    let temp = tempdir().unwrap();
    for (session, workspace) in [("a", "/repo-a"), ("b", "/repo-b")] {
        assert_eq!(
            pre_spawn_with_workspace_and_turn(
                temp.path(),
                "runtime",
                session,
                Some(&input("task_a")),
                RootHookContext::new(Some(workspace), 0, 10),
            )
            .unwrap(),
            None
        );
    }
}

#[test]
fn rejected_spawn_releases_admission_lock() {
    let temp = tempdir().unwrap();
    assert!(
        pre_spawn_with_workspace_and_turn(
            temp.path(),
            "runtime",
            "a",
            Some(&json!({})),
            RootHookContext::new(Some("/repo"), 0, 10),
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(
        pre_spawn_with_workspace_and_turn(
            temp.path(),
            "runtime",
            "b",
            Some(&input("valid_task")),
            RootHookContext::new(Some("/repo"), 0, 20),
        )
        .unwrap(),
        None
    );
}
