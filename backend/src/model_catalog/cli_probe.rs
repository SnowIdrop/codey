use super::*;

/// The same install keeps different Codex builds in its staged and app
/// directories, so the stamp covers every candidate: a newer build anywhere is
/// enough to justify re-rendering the catalog once.
pub(super) fn codex_cli_stamp_for(candidates: &[PathBuf]) -> Option<std::time::SystemTime> {
    candidates
        .iter()
        .filter_map(|path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .max()
}

/// The snapshot is missing or predates the local Codex CLI. A missing snapshot
/// counts as stale because a Codex upgrade that arrives without a
/// `models_cache.json` has no other way to produce one, while an existing
/// instruction-bearing source would otherwise suppress the capture.
#[cfg(not(test))]
pub(super) fn runtime_snapshot_is_stale(home: &Path, codex_app_path: &str) -> bool {
    let snapshot = home.join(DEBUG_CATALOG_RELATIVE_PATH);
    let snapshot_is_account_backed = read_catalog_value(&snapshot)
        .and_then(|value| value.get("codey_account_snapshot").cloned())
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !snapshot_is_account_backed {
        return true;
    }
    let snapshot_mtime = fs::metadata(home.join(DEBUG_CATALOG_RELATIVE_PATH))
        .and_then(|metadata| metadata.modified())
        .ok();
    runtime_snapshot_is_stale_at(
        snapshot_mtime,
        codex_cli_stamp_for(&codex_cli_candidates_for(codex_app_path)),
    )
}

pub(super) fn runtime_snapshot_is_stale_at(
    snapshot_mtime: Option<std::time::SystemTime>,
    cli_stamp: Option<std::time::SystemTime>,
) -> bool {
    let Some(cli_stamp) = cli_stamp else {
        return false;
    };
    snapshot_mtime.is_none_or(|snapshot_mtime| cli_stamp > snapshot_mtime)
}

/// Unit tests never run the Codex CLI installed on the build machine, and the
/// snapshot capture itself is stubbed there. Staleness stays reachable through
/// `runtime_snapshot_is_stale_at` so the decision logic can still be tested.
#[cfg(test)]
pub(super) fn runtime_snapshot_is_stale(_home: &Path, _codex_app_path: &str) -> bool {
    false
}

#[cfg(not(test))]
pub(super) fn debug_model_entries(home: &Path, candidates: &[PathBuf]) -> Option<Vec<Value>> {
    for cli in candidates {
        for bundled in [false, true] {
            let Some(output) = run_codex_debug_models(cli, home, bundled) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&output) else {
                continue;
            };
            let models = official_models_from_value(&value);
            if snapshot_has_runtime_models(&models) {
                return Some(models);
            }
        }
    }
    None
}

/// Candidates are ordered the way the launch resolves its own runtime: the
/// configured app directory first, then the staged copy Codey keeps for
/// packages that cannot be executed in place, then any other installed copy.
#[cfg(not(test))]
pub(super) fn codex_cli_candidates_for(codex_app_path: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut push = |path: PathBuf| {
        if path.is_file() && !candidates.contains(&path) {
            candidates.push(path);
        }
    };
    let configured = codex_app_path.trim();
    if !configured.is_empty()
        && let Some(app_dir) =
            codey_runtime_core::app_paths::resolve_codex_app_dir(Some(Path::new(configured)))
        && let Some(executable) = codey_runtime_core::app_paths::codex_runtime_executable(&app_dir)
    {
        push(executable);
    }
    // The staged copy is verified runnable (the WindowsApps package location
    // needs special execution rights), so it comes before the app resources.
    for path in hashed_cli_dirs("Codey", "codex-runtime") {
        push(path.join("codex.exe"));
    }
    if let Some(app_dir) = codey_runtime_core::app_paths::resolve_codex_app_dir(None)
        && let Some(executable) = codey_runtime_core::app_paths::codex_runtime_executable(&app_dir)
    {
        push(executable);
    }
    for path in hashed_cli_dirs("OpenAI", "Codex") {
        push(path.join("codex.exe"));
    }
    if let Some(path) = cli_on_path() {
        push(path);
    }
    candidates
}

/// `%LOCALAPPDATA%\<vendor>\<dir>\<hash>` hosts the standalone Codex CLI and
/// Codey's staged copies; newest first so an updated install wins.
#[cfg(all(windows, not(test)))]
fn hashed_cli_dirs(vendor: &str, dir: &str) -> Vec<PathBuf> {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") else {
        return Vec::new();
    };
    let mut root = PathBuf::from(local_app_data).join(vendor).join(dir);
    // The standalone installer nests the hash directories under `bin`.
    if vendor == "OpenAI" {
        root = root.join("bin");
    }
    let mut entries = std::fs::read_dir(&root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    entries.sort_by_key(|path| {
        std::cmp::Reverse(
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok(),
        )
    });
    entries
}

#[cfg(all(not(windows), not(test)))]
fn hashed_cli_dirs(_vendor: &str, _dir: &str) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(not(test))]
fn cli_on_path() -> Option<PathBuf> {
    let name = if cfg!(windows) { "codex.exe" } else { "codex" };
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(not(test))]
const DEBUG_MODELS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(not(test))]
fn run_codex_debug_models(cli: &Path, home: &Path, bundled: bool) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut command = Command::new(cli);
    command.args(["debug", "models"]);
    if bundled {
        command.arg("--bundled");
    }
    command
        .env("CODEX_HOME", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(codey_runtime_core::windows_create_no_window());
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // The catalog is larger than the pipe buffer, so it must be drained while
    // the deadline is being polled.
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        stdout.read_to_end(&mut buffer).ok().map(|_| buffer)
    });
    let deadline = std::time::Instant::now() + DEBUG_MODELS_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = reader.join().ok().flatten();
                return status.success().then_some(()).and(output);
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}
