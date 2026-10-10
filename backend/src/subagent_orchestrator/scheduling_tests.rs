use super::*;
use serde_json::json;
use tempfile::tempdir;

fn spawn(root: &Path, session: &str, task: &str, role: &str, workspace: &str) {
    let input =
        json!({"task_name": task, "agent_type": role, "message": "Complete the assigned task"});
    assert_eq!(
        pre_spawn_with_workspace_and_turn(
            root,
            "runtime",
            session,
            Some(&input),
            RootHookContext::new(Some(workspace), 0, 10),
        )
        .unwrap(),
        None
    );
    post_spawn(
        root,
        "runtime",
        session,
        Some(&input),
        Some(&json!({"agent_id": task})),
        11,
    )
    .unwrap();
}

fn tool(root: &Path, session: &str, task: &str, name: &str) -> Result<Option<String>> {
    authorize_child_tool_with_context(
        root,
        "runtime",
        session,
        ChildToolContext {
            agent_id: task,
            agent_type: None,
            transcript_path: None,
            tool_name: name,
            tool_input: None,
        },
        20,
    )
}

fn claimed(root: &Path, session: &str, task: &str) -> bool {
    LedgerStore::open(root, session)
        .unwrap()
        .load("runtime", session, 21)
        .unwrap()
        .unwrap()
        .reservations[task]
        .workspace_write_claimed
}

#[test]
fn independent_workspaces_can_write_concurrently() {
    let temp = tempdir().unwrap();
    for (session, task, workspace) in [("a", "task_a", "/repo-a"), ("b", "task_b", "/repo-b")] {
        spawn(temp.path(), session, task, "codey_worker", workspace);
        assert_eq!(
            tool(temp.path(), session, task, "apply_patch").unwrap(),
            None
        );
    }
}

#[test]
fn fenced_or_unbound_attempts_cannot_acquire_a_write_claim() {
    let temp = tempdir().unwrap();
    assert!(
        tool(temp.path(), "session", "unbound", "apply_patch")
            .unwrap()
            .is_some()
    );
    spawn(
        temp.path(),
        "session",
        "review",
        "codey_quick_scan",
        "/repo",
    );
    pre_interrupt_agent(
        temp.path(),
        "runtime",
        "session",
        Some(&json!({"target":"review"})),
        22,
    )
    .unwrap();
    assert!(
        tool(temp.path(), "session", "review", "apply_patch")
            .unwrap()
            .is_some()
    );
    assert!(!claimed(temp.path(), "session", "review"));
}
