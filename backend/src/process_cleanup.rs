#[cfg(any(windows, test))]
use std::collections::HashMap;
#[cfg(any(unix, windows, test))]
use std::collections::HashSet;
#[cfg(unix)]
use std::path::Path;


#[cfg(unix)]
fn unix_codey_root_process_ids(
    processes: &[crate::process_tree::UnixProcessInfo],
    executable_path: &Path,
    current_pid: u32,
) -> HashSet<u32> {
    processes
        .iter()
        .filter(|process| {
            process.process_id != current_pid
                && crate::process_tree::command_uses_path(&process.command, executable_path)
        })
        .map(|process| process.process_id)
        .collect()
}


#[cfg(any(windows, test))]
fn process_ids_with_descendants_from_identities(
    processes: &[(u32, u32, u64)],
    roots: HashSet<u32>,
    excluded_process_id: u32,
) -> HashSet<u32> {
    let creation_times = processes
        .iter()
        .map(|(process_id, _, creation_time)| (*process_id, *creation_time))
        .collect::<HashMap<_, _>>();
    let mut process_ids = roots
        .into_iter()
        .filter(|process_id| {
            *process_id > 1
                && *process_id != excluded_process_id
                && creation_times.contains_key(process_id)
        })
        .collect::<HashSet<_>>();
    loop {
        let previous_len = process_ids.len();
        for (process_id, parent_process_id, creation_time) in processes {
            if *process_id > 1
                && *process_id != excluded_process_id
                && process_ids.contains(parent_process_id)
                && creation_times
                    .get(parent_process_id)
                    .is_some_and(|parent_creation_time| parent_creation_time <= creation_time)
            {
                process_ids.insert(*process_id);
            }
        }
        if process_ids.len() == previous_len {
            return process_ids;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::process_tree::{
        identities_for_process_ids, matching_process_ids, parse_unix_process_snapshot,
    };

    fn windows_filetime(time: std::time::SystemTime) -> Option<u64> {
        const UNIX_EPOCH_AS_FILETIME_SECONDS: u64 = 11_644_473_600;
        let since_unix = time.duration_since(std::time::UNIX_EPOCH).ok()?;
        since_unix
            .as_secs()
            .checked_add(UNIX_EPOCH_AS_FILETIME_SECONDS)?
            .checked_mul(10_000_000)?
            .checked_add(u64::from(since_unix.subsec_nanos() / 100))
    }

    fn started_before_shutdown(creation_time: u64, shutdown_started_at: Option<u64>) -> bool {
        shutdown_started_at.is_none_or(|shutdown_started_at| creation_time < shutdown_started_at)
    }

    #[test]
    fn unix_root_filter_requires_the_current_executable_path() {
        let processes = parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/Codey.app/Contents/MacOS/codey\n\
              200 1 200 Thu Jul 23 19:23:13 2026 /tmp/other/codey\n\
              300 1 300 Thu Jul 23 19:23:14 2026 /Applications/Codey.app/Contents/MacOS/codey --watch\n",
        );

        assert_eq!(
            unix_codey_root_process_ids(
                &processes,
                Path::new("/Applications/Codey.app/Contents/MacOS/codey"),
                100,
            ),
            HashSet::from([300])
        );
    }

    #[test]
    fn fixed_cleanup_identities_never_adopt_a_new_same_path_process() {
        let initial = parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/Codey.app/Contents/MacOS/codey\n",
        );
        let identities = identities_for_process_ids(&initial, &HashSet::from([100]));
        let later = parse_unix_process_snapshot(
            b"100 1 100 Thu Jul 23 19:23:12 2026 /Applications/Codey.app/Contents/MacOS/codey\n\
              200 1 200 Thu Jul 23 19:23:13 2026 /Applications/Codey.app/Contents/MacOS/codey\n",
        );

        assert_eq!(
            matching_process_ids(&later, &identities),
            HashSet::from([100])
        );
    }

    // 【自动化测试】退出清理 - 退出开始后才启动的 Codey（如等待交接的新实例）不在清理范围
    #[test]
    fn processes_started_after_shutdown_began_are_not_cleanup_roots() {
        let shutdown = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_500);
        let shutdown_filetime = windows_filetime(shutdown).unwrap();
        assert_eq!(shutdown_filetime, 116_444_736_015_000_000);

        assert!(started_before_shutdown(
            shutdown_filetime - 1,
            Some(shutdown_filetime)
        ));
        assert!(!started_before_shutdown(
            shutdown_filetime,
            Some(shutdown_filetime)
        ));
        assert!(started_before_shutdown(shutdown_filetime + 1, None));
        assert_eq!(
            windows_filetime(std::time::UNIX_EPOCH - std::time::Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn descendant_filter_freezes_the_initial_tree_and_rejects_stale_parent_ids() {
        let initial = [
            (100, 1, 200),
            (101, 100, 201),
            (102, 101, 202),
            (200, 100, 199),
            (300, 1, 203),
        ];

        assert_eq!(
            process_ids_with_descendants_from_identities(&initial, HashSet::from([100]), 999),
            HashSet::from([100, 101, 102])
        );
    }
}
