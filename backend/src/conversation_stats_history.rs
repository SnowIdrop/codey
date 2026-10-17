//! 从当前 Codex 数据目录读取原生累计快照，不累加每轮或运行期统计。
use super::{ConversationStatsSnapshot, lock_unpoisoned, normalize_session_id};
use codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

#[cfg(test)]
const MAX_TAIL_BYTES: u64 = 8 * 1024 * 1024;
const LOOKUP_TTL: Duration = Duration::from_secs(60);
type CacheKey = (PathBuf, String);
type Cache = HashMap<CacheKey, Arc<Mutex<CachedHistory>>>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

#[derive(Default)]
struct CachedHistory {
    lookup_at: Option<Instant>,
    path: Option<PathBuf>,
    version: Option<FileVersion>,
    snapshot: Option<ConversationStatsSnapshot>,
    activity: super::activity::Activity,
}

#[derive(PartialEq, Eq)]
struct FileVersion {
    len: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileVersion {
    fn from_metadata(metadata: &std::fs::Metadata) -> Option<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            created: metadata.created().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        })
    }
}

pub(crate) async fn snapshot_with_history(
    home: PathBuf,
    session_id: String,
    runtime: Option<ConversationStatsSnapshot>,
) -> Option<ConversationStatsSnapshot> {
    let suppress_history_speed = runtime
        .as_ref()
        .is_some_and(|s| s.in_flight.unwrap_or(0) > 0 || s.output_tokens_per_second.is_some());
    let (history, speed, timing) = tokio::task::spawn_blocking(move || {
        let history = cached_history(&home, &session_id);
        let speed = if suppress_history_speed {
            None
        } else {
            cached_speed(&home, &session_id)
        };
        let timing = cached_timing(&home, &session_id);
        (history, speed, timing)
    })
    .await
    .ok()
    .unwrap_or_default();
    apply_recorded_speed(
        apply_recorded_timing(merge(history, runtime), timing),
        speed,
    )
}

type TimingCache = HashMap<
    (PathBuf, PathBuf, String),
    (
        Instant,
        Option<crate::route_request_log::SessionRecordedTiming>,
    ),
>;
static TIMING_CACHE: OnceLock<Mutex<TimingCache>> = OnceLock::new();
fn cached_timing(
    home: &Path,
    session_id: &str,
) -> Option<crate::route_request_log::SessionRecordedTiming> {
    let session_id = normalize_session_id(session_id);
    uuid::Uuid::parse_str(session_id).ok()?;
    let root = codey_runtime_core::paths::default_app_state_dir();
    let key = (
        home.canonicalize().ok()?,
        root.clone(),
        session_id.to_owned(),
    );
    let mut cache = lock_unpoisoned(TIMING_CACHE.get_or_init(Default::default));
    if let Some((at, timing)) = cache.get(&key) {
        if at.elapsed() < Duration::from_secs(30) {
            return *timing;
        }
    }
    let timing = crate::route_request_log::session_recorded_timing(&root, session_id);
    if cache.len() >= 128 && !cache.contains_key(&key) {
        if let Some(old) = cache.keys().next().cloned() {
            cache.remove(&old);
        }
    }
    cache.insert(key, (Instant::now(), timing));
    timing
}

fn apply_recorded_timing(
    snapshot: Option<ConversationStatsSnapshot>,
    timing: Option<crate::route_request_log::SessionRecordedTiming>,
) -> Option<ConversationStatsSnapshot> {
    let Some(timing) = timing else {
        return snapshot;
    };
    let mut snapshot = snapshot.unwrap_or_else(|| ConversationStatsSnapshot {
        usage_source: "unknown",
        usage_scope: "unknown",
        ..Default::default()
    });
    if snapshot.step_source != Some("rollout") && snapshot.step_count.unwrap_or(0) == 0 {
        snapshot.step_count = Some(timing.requests);
        snapshot.step_source = Some("request_log");
        snapshot.step_scope = Some("recorded_requests");
    }
    // 两者覆盖的时间范围不同，不能比较样本数量来判断新旧，也不能相加。
    if snapshot.model_duration_ms.is_none() && snapshot.average_ttft_ms.is_none() {
        snapshot.model_duration_ms = timing.model_duration_ms;
        snapshot.average_ttft_ms = timing.average_ttft_ms;
        snapshot.timing_request_count = Some(timing.timing_request_count);
        snapshot.ttft_sample_count = Some(timing.ttft_sample_count);
        snapshot.timing_source = Some("request_log");
        snapshot.timing_scope = Some("recorded_requests");
    }
    Some(snapshot)
}

// 缓存键包含当前 home 与实际应用数据目录，30 秒后包括缺失记录也重新查询。
type SpeedCache = HashMap<(PathBuf, PathBuf, String), (Instant, Option<(f64, u64)>)>;
static SPEED_CACHE: OnceLock<Mutex<SpeedCache>> = OnceLock::new();
fn cached_speed(home: &Path, session_id: &str) -> Option<(f64, u64)> {
    let session_id = normalize_session_id(session_id);
    uuid::Uuid::parse_str(session_id).ok()?;
    let root = codey_runtime_core::paths::default_app_state_dir();
    let key = (
        home.canonicalize().ok()?,
        root.clone(),
        session_id.to_owned(),
    );
    let mut cache = lock_unpoisoned(SPEED_CACHE.get_or_init(Default::default));
    if let Some((at, speed)) = cache.get(&key) {
        if at.elapsed() < Duration::from_secs(30) {
            return *speed;
        }
    }
    let speed = crate::route_request_log::latest_session_recorded_speed(&root, session_id);
    if cache.len() >= 128 && !cache.contains_key(&key) {
        if let Some(old) = cache.keys().next().cloned() {
            cache.remove(&old);
        }
    }
    cache.insert(key, (Instant::now(), speed));
    speed
}

fn apply_recorded_speed(
    snapshot: Option<ConversationStatsSnapshot>,
    speed: Option<(f64, u64)>,
) -> Option<ConversationStatsSnapshot> {
    let Some((speed, recorded_at)) = speed else {
        return snapshot;
    };
    let mut snapshot = snapshot.unwrap_or_else(|| ConversationStatsSnapshot {
        usage_source: "unknown",
        usage_scope: "unknown",
        ..Default::default()
    });
    if snapshot.in_flight.unwrap_or(0) == 0 && snapshot.output_tokens_per_second.is_none() {
        snapshot.output_tokens_per_second = Some(speed);
        snapshot.speed_source = Some("request_log");
        snapshot.speed_scope = Some("latest_recorded_request");
        snapshot.speed_recorded_at_unix_ms = Some(recorded_at);
    }
    Some(snapshot)
}

fn merge(
    history: Option<ConversationStatsSnapshot>,
    runtime: Option<ConversationStatsSnapshot>,
) -> Option<ConversationStatsSnapshot> {
    let Some(mut history) = history else {
        return runtime;
    };
    if let Some(runtime) = runtime {
        // 原生文件可能只有活动事件，尚无 Token 累计；此时保留运行期用量。
        if history.usage_source == "unknown" {
            history.input_tokens = runtime.input_tokens;
            history.output_tokens = runtime.output_tokens;
            history.cached_input_tokens = runtime.cached_input_tokens;
            history.cache_write_input_tokens = runtime.cache_write_input_tokens;
            history.total_tokens = runtime.total_tokens;
            history.usage_source = runtime.usage_source;
            history.usage_scope = runtime.usage_scope;
        }
        history.requests = runtime.requests;
        history.in_flight = runtime.in_flight;
        history.requests_scope = runtime.requests_scope;
        // 请求字段只描述本次运行。rollout 的写入时间通常晚于请求开始时间，
        // 不能用两者比较来判断模型和速度是否属于最近的运行期请求。
        history.model = runtime.model;
        history.last_status = runtime.last_status;
        history.output_tokens_per_second = runtime.output_tokens_per_second;
        history.speed_source = runtime.speed_source;
        history.speed_scope = runtime.speed_scope;
        history.speed_recorded_at_unix_ms = runtime.speed_recorded_at_unix_ms;
        history.updated_at_unix_ms = runtime.updated_at_unix_ms;
        history.model_duration_ms = runtime.model_duration_ms;
        history.average_ttft_ms = runtime.average_ttft_ms;
        history.timing_source = runtime.timing_source;
        history.timing_scope = runtime.timing_scope;
        history.timing_request_count = runtime.timing_request_count;
        history.ttft_sample_count = runtime.ttft_sample_count;
        if history.step_count.is_none() {
            history.step_count = runtime.step_count;
            history.step_source = runtime.step_source;
            history.step_scope = runtime.step_scope;
        }
    }
    Some(history)
}

fn cached_history(home: &Path, session_id: &str) -> Option<ConversationStatsSnapshot> {
    let session_id = normalize_session_id(session_id);
    // 原生会话使用 UUID，拒绝路径形态及非本地会话标识。
    uuid::Uuid::parse_str(session_id).ok()?;
    let home = home.canonicalize().ok()?;
    let key = (home.clone(), session_id.to_string());
    let entry = {
        let mut cache = lock_unpoisoned(CACHE.get_or_init(Default::default));
        if cache.len() >= 128 && !cache.contains_key(&key) {
            // 有界缓存只影响重新定位频率，不影响数据来源。
            if let Some(old) = cache.keys().next().cloned() {
                cache.remove(&old);
            }
        }
        Arc::clone(cache.entry(key).or_default())
    };
    let mut cached = lock_unpoisoned(&entry);
    if cached.lookup_at.is_none_or(|at| at.elapsed() >= LOOKUP_TTL) {
        let path = locate_rollout(&home, session_id);
        if cached.path != path {
            cached.version = None;
            cached.snapshot = None;
            cached.activity = Default::default();
        }
        cached.path = path;
        cached.lookup_at = Some(Instant::now());
    }
    cached.path.as_ref()?;
    // 文件可能刚被归档移动，不等待定位缓存到期；每次重新验证路径，
    // 也拒绝替换为外部符号链接的文件。
    let path = cached
        .path
        .as_ref()
        .and_then(|path| canonical_rollout(&home, path));
    let path = match path {
        Some(path) => path,
        None => {
            cached.path = locate_rollout(&home, session_id);
            cached.lookup_at = Some(Instant::now());
            cached.version = None;
            cached.snapshot = None;
            cached.activity = Default::default();
            canonical_rollout(&home, cached.path.as_ref()?)?
        }
    };
    let metadata = path.metadata().ok()?;
    let version = FileVersion::from_metadata(&metadata)?;
    if cached.version.as_ref() != Some(&version) {
        let appended = cached.version.as_ref().is_some_and(|old| {
            #[cfg(unix)]
            let same_file =
                old.identity.0 == version.identity.0 && old.identity.1 == version.identity.1;
            #[cfg(not(unix))]
            let same_file = old.created == version.created;
            same_file && version.len > old.len
        });
        if !appended {
            cached.activity = Default::default();
        }
        let activity_ok = cached.activity.read_append(&path, session_id).is_ok();
        let mut snapshot = activity_ok
            .then(|| cached.activity.usage_snapshot.clone())
            .flatten()
            .unwrap_or_else(|| ConversationStatsSnapshot {
                usage_source: "unknown",
                usage_scope: "unknown",
                ..Default::default()
            });
        if activity_ok {
            cached.activity.apply(&mut snapshot);
        }
        cached.snapshot = (snapshot.total_tokens.is_some()
            || snapshot.input_tokens.is_some()
            || snapshot.output_tokens.is_some()
            || snapshot.turn_count.is_some()
            || snapshot.step_count.is_some()
            || snapshot.tool_duration_ms.is_some())
        .then_some(snapshot);
        cached.version = Some(version);
    }
    cached.snapshot.clone()
}

fn canonical_rollout(home: &Path, path: &Path) -> Option<PathBuf> {
    path.canonicalize()
        .ok()
        .filter(|path| path.starts_with(home) && path.is_file())
}

fn locate_rollout(home: &Path, session_id: &str) -> Option<PathBuf> {
    for db in codex_session_db_paths_from_home(home) {
        let Ok(connection) = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        else {
            continue;
        };
        let _ = connection.busy_timeout(Duration::from_millis(100));
        let path = connection
            .query_row(
                "SELECT rollout_path FROM threads WHERE id=?1 LIMIT 1",
                [session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(path) = path {
            let path = home.join(path);
            if let Some(path) = canonical_rollout(home, &path) {
                return Some(path);
            }
        }
    }
    // 没有目录映射时，同一会话可能保留多个恢复副本；选择最近写入的副本。
    crate::message_delete::find_rollout_files_by_session_id(home, session_id)
        .into_iter()
        .filter_map(|path| {
            let path = canonical_rollout(home, &path)?;
            let modified = path.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max()
        .map(|(_, path)| path)
}

#[cfg(test)]
fn read_rollout(
    path: &Path,
    session_id: &str,
) -> std::io::Result<Option<ConversationStatsSnapshot>> {
    let mut activity = super::activity::Activity::default();
    activity.read_append(path, session_id)?;
    Ok(activity.usage_snapshot)
}

#[cfg(test)]
fn parse_tail(bytes: &[u8], session_id: &str) -> Option<ConversationStatsSnapshot> {
    let mut activity = super::activity::Activity::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        if let Ok(record) = serde_json::from_slice::<Value>(line) {
            activity.record(&record, session_id);
        }
    }
    activity.usage_snapshot
}

pub(super) fn usage_from_record(
    record: &Value,
    session_id: &str,
) -> Option<ConversationStatsSnapshot> {
    let payload = &record["payload"];
    let usage = match record["type"].as_str() {
        Some("event_msg") if payload["type"] == "token_count" => {
            &payload["info"]["total_token_usage"]
        }
        Some("token_usage_record")
            if payload["session_id"]
                .as_str()
                .is_some_and(|id| normalize_session_id(id).eq_ignore_ascii_case(session_id)) =>
        {
            &payload["thread_token_usage"]
        }
        _ => return None,
    };
    let input = usage["input_tokens"].as_u64();
    let output = usage["output_tokens"].as_u64();
    let total = usage["total_tokens"]
        .as_u64()
        .or_else(|| Some(input?.saturating_add(output?)));
    if input.is_none() && output.is_none() && total.is_none() {
        return None;
    }
    Some(ConversationStatsSnapshot {
        input_tokens: input,
        output_tokens: output,
        total_tokens: total,
        cached_input_tokens: usage["cached_input_tokens"].as_u64(),
        cache_write_input_tokens: usage["cache_write_input_tokens"].as_u64(),
        usage_source: "rollout",
        usage_scope: "session",
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const ID: &str = "01a1471c-962b-7d00-a9c4-0658dd26dd46";

    fn event(total: u64) -> String {
        json!({"type":"event_msg","timestamp":"2026-10-17T00:00:00Z","payload":{
        "type":"token_count","info":{"total_token_usage":{
            "input_tokens":total-20,"output_tokens":20,"total_tokens":total,
            "cached_input_tokens":10
        }}}})
        .to_string()
    }

    #[test]
    fn activity_only_history_keeps_runtime_usage_and_logs_do_not_double_count_timing() {
        let history = ConversationStatsSnapshot {
            turn_count: Some(2),
            step_count: Some(3),
            step_source: Some("rollout"),
            usage_source: "unknown",
            usage_scope: "unknown",
            ..Default::default()
        };
        let runtime = ConversationStatsSnapshot {
            total_tokens: Some(10),
            usage_source: "runtime",
            usage_scope: "runtime",
            model_duration_ms: Some(1000),
            average_ttft_ms: Some(200.0),
            timing_request_count: Some(1),
            ttft_sample_count: Some(1),
            timing_source: Some("runtime"),
            timing_scope: Some("runtime"),
            ..Default::default()
        };
        let merged = merge(Some(history), Some(runtime));
        let timing = crate::route_request_log::SessionRecordedTiming {
            requests: 4,
            model_duration_ms: Some(5000),
            average_ttft_ms: Some(300.0),
            timing_request_count: 4,
            ttft_sample_count: 4,
        };
        let snapshot = apply_recorded_timing(merged, Some(timing)).unwrap();
        assert_eq!(snapshot.total_tokens, Some(10));
        assert_eq!(snapshot.turn_count, Some(2));
        assert_eq!(snapshot.step_count, Some(3));
        assert_eq!(snapshot.model_duration_ms, Some(1000));
        assert_eq!(snapshot.average_ttft_ms, Some(200.0));
        assert_eq!(snapshot.timing_scope, Some("runtime"));
    }

    #[test]
    fn recorded_timing_fills_missing_data_without_overwriting_current_runtime_samples() {
        let timing = crate::route_request_log::SessionRecordedTiming {
            requests: 100,
            model_duration_ms: Some(500_000),
            average_ttft_ms: Some(3000.0),
            timing_request_count: 100,
            ttft_sample_count: 100,
        };
        let recovered = apply_recorded_timing(None, Some(timing)).unwrap();
        assert_eq!(recovered.step_count, Some(100));
        assert_eq!(recovered.model_duration_ms, Some(500_000));
        assert_eq!(recovered.timing_scope, Some("recorded_requests"));
        assert_eq!(recovered.total_tokens, None);

        let runtime = ConversationStatsSnapshot {
            step_count: Some(1),
            step_source: Some("runtime"),
            step_scope: Some("runtime"),
            model_duration_ms: Some(0),
            average_ttft_ms: Some(0.0),
            timing_request_count: Some(1),
            ttft_sample_count: Some(1),
            timing_source: Some("runtime"),
            timing_scope: Some("runtime"),
            ..Default::default()
        };
        let current = apply_recorded_timing(Some(runtime), Some(timing)).unwrap();
        assert_eq!(current.step_count, Some(1));
        assert_eq!(current.step_scope, Some("runtime"));
        assert_eq!(current.model_duration_ms, Some(0));
        assert_eq!(current.average_ttft_ms, Some(0.0));
        assert_eq!(current.timing_scope, Some("runtime"));
        assert_eq!(current.timing_request_count, Some(1));
    }

    #[test]
    fn recorded_speed_is_independent_of_usage_and_runtime_request_state() {
        let speed = Some((25.0, 123));
        let recovered = apply_recorded_speed(None, speed).unwrap();
        assert_eq!(recovered.output_tokens_per_second, Some(25.0));
        assert_eq!(recovered.speed_source, Some("request_log"));
        assert_eq!(recovered.speed_scope, Some("latest_recorded_request"));
        assert_eq!(recovered.speed_recorded_at_unix_ms, Some(123));
        assert_eq!(recovered.model, None);
        assert_eq!(recovered.total_tokens, None);
        let history = parse_tail(event(100).as_bytes(), ID);
        let result = apply_recorded_speed(merge(history, None), speed).unwrap();
        assert_eq!(result.total_tokens, Some(100));
        assert_eq!(result.output_tokens_per_second, Some(25.0));
        let running = ConversationStatsSnapshot {
            in_flight: Some(1),
            ..Default::default()
        };
        assert_eq!(
            apply_recorded_speed(Some(running), speed)
                .unwrap()
                .output_tokens_per_second,
            None
        );
        let completed_without_speed = ConversationStatsSnapshot {
            in_flight: Some(0),
            ..Default::default()
        };
        assert_eq!(
            apply_recorded_speed(Some(completed_without_speed), speed)
                .unwrap()
                .speed_scope,
            Some("latest_recorded_request")
        );
        let live = ConversationStatsSnapshot {
            output_tokens_per_second: Some(50.0),
            speed_source: Some("runtime"),
            ..Default::default()
        };
        let result = apply_recorded_speed(Some(live), speed).unwrap();
        assert_eq!(result.output_tokens_per_second, Some(50.0));
        assert_eq!(result.speed_source, Some("runtime"));
        assert!(apply_recorded_speed(None, None).is_none());
    }

    #[test]
    fn latest_cumulative_record_replaces_duplicates_and_decreases() {
        let data = format!("{}\n{}\n{}\n{{\"type\":", event(100), event(100), event(60));
        let snapshot = parse_tail(data.as_bytes(), ID).unwrap();
        assert_eq!(snapshot.total_tokens, Some(60));
        assert_eq!(snapshot.input_tokens, Some(40));
        assert_eq!(snapshot.cached_input_tokens, Some(10));
        assert_eq!(snapshot.cache_write_input_tokens, None);
        assert_eq!(snapshot.requests, None);
        assert_eq!(snapshot.in_flight, None);
        assert_eq!(snapshot.model, None);
    }

    #[test]
    fn thread_usage_records_require_own_session_and_ignore_per_turn_values() {
        let own = json!({"type":"token_usage_record","payload":{
            "session_id":ID,"usage":{"total_tokens":800},
            "turn_token_usage":{"total_tokens":900},"thread_token_usage":{"total_tokens":42}}});
        let foreign = json!({"type":"token_usage_record","payload":{
            "session_id":"other","thread_token_usage":{"total_tokens":9999}}});
        let data = format!("{}\n{own}\n{foreign}\n", event(100));
        let snapshot = parse_tail(data.as_bytes(), ID).unwrap();
        assert_eq!(snapshot.total_tokens, Some(42));
        assert_eq!(snapshot.input_tokens, None);
        assert_eq!(snapshot.cached_input_tokens, None);
    }

    #[test]
    fn native_usage_ignores_foreign_segments_and_explicit_foreign_ids() {
        let foreign_meta = json!({"type":"session_meta","payload":{"id":"other"}});
        let own_meta = json!({"type":"session_meta","payload":{"id":ID}});
        let foreign_count = json!({"type":"event_msg","payload":{
            "type":"token_count","thread_id":"other",
            "info":{"total_token_usage":{"total_tokens":9999}}
        }});
        let data = format!(
            "{foreign_meta}\n{}\n{own_meta}\n{}\n{foreign_count}\n{foreign_meta}\n{}\n",
            event(900),
            event(100),
            event(800)
        );
        assert_eq!(
            parse_tail(data.as_bytes(), ID).unwrap().total_tokens,
            Some(100)
        );
        assert!(parse_tail(format!("{foreign_meta}\n{}\n", event(900)).as_bytes(), ID).is_none());
    }

    #[test]
    fn database_mapping_to_another_sessions_file_does_not_return_its_usage() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("other-session.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":"other"}}),
                event(900)
            ),
        )
        .unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO threads VALUES (?1, ?2)",
            [ID, path.to_str().unwrap()],
        )
        .unwrap();
        assert!(cached_history(home.path(), ID).is_none());
    }

    #[test]
    fn merge_uses_history_without_double_counting_and_exposes_runtime_scope() {
        let mut history = parse_tail(event(100).as_bytes(), ID).unwrap();
        // 模拟历史落盘晚于请求开始，不能因此丢掉运行期字段或串用历史时间。
        history.updated_at_unix_ms = 2_000;
        let runtime = ConversationStatsSnapshot {
            total_tokens: Some(20),
            requests: Some(1),
            in_flight: Some(0),
            requests_scope: Some("runtime"),
            model: Some("runtime-model".into()),
            output_tokens_per_second: Some(12.0),
            updated_at_unix_ms: 1_000,
            ..Default::default()
        };
        let merged = merge(Some(history), Some(runtime)).unwrap();
        assert_eq!(merged.total_tokens, Some(100));
        assert_eq!(merged.requests, Some(1));
        assert_eq!(merged.requests_scope, Some("runtime"));
        assert_eq!(merged.model.as_deref(), Some("runtime-model"));
        assert_eq!(merged.output_tokens_per_second, Some(12.0));
        assert_eq!(merged.updated_at_unix_ms, 1_000);
        assert_eq!(merged.usage_scope, "session");
    }

    #[test]
    fn history_survives_empty_runtime_and_cache_invalidates_on_file_change() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("sessions/2026/10/17");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-17T00-00-00-{ID}.jsonl"));
        std::fs::write(&path, event(100)).unwrap();
        assert_eq!(
            cached_history(home.path(), ID).unwrap().total_tokens,
            Some(100)
        );
        std::fs::write(&path, format!("{}\n{}\n", event(100), event(200))).unwrap();
        let snapshot = cached_history(home.path(), ID).unwrap();
        assert_eq!(merge(Some(snapshot), None).unwrap().total_tokens, Some(200));
        assert!(cached_history(home.path(), "../secrets").is_none());
    }

    #[test]
    fn history_before_large_tool_output_is_still_available() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{}", event(300)).unwrap();
        // 超大但无统计的记录，以及末尾未写完的内容，均不覆盖已有累计。
        file.write_all(&vec![b'x'; MAX_TAIL_BYTES as usize + 100])
            .unwrap();
        file.write_all(b"\n{\"type\":").unwrap();
        assert_eq!(
            read_rollout(file.path(), ID).unwrap().unwrap().total_tokens,
            Some(300)
        );
    }

    #[test]
    fn fallback_selects_newest_rollout_and_detects_same_size_replacement() {
        use std::fs::{FileTimes, OpenOptions};
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join(format!("rollout-2026-01-01-{ID}.jsonl"));
        let current = dir.join(format!("rollout-2026-01-02-{ID}.jsonl"));
        std::fs::write(&old, event(100)).unwrap();
        std::fs::write(&current, event(200)).unwrap();
        let old_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let current_time = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000);
        OpenOptions::new()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old_time))
            .unwrap();
        OpenOptions::new()
            .write(true)
            .open(&current)
            .unwrap()
            .set_times(FileTimes::new().set_modified(current_time))
            .unwrap();
        assert_eq!(
            cached_history(home.path(), ID).unwrap().total_tokens,
            Some(200)
        );
        let replacement = dir.join("replacement.jsonl");
        std::fs::write(&replacement, event(300)).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_times(FileTimes::new().set_modified(current_time))
            .unwrap();
        // Windows rename 不覆盖目标；先移走旧文件，同路径替换仍保留相同大小和 mtime。
        std::fs::rename(&current, dir.join("previous.jsonl")).unwrap();
        std::fs::rename(&replacement, &current).unwrap();
        assert_eq!(
            cached_history(home.path(), ID).unwrap().total_tokens,
            Some(300)
        );
        std::fs::remove_file(&current).unwrap();
        std::fs::remove_file(&old).unwrap();
        assert!(cached_history(home.path(), ID).is_none());
    }

    #[test]
    fn database_path_cannot_read_outside_selected_home() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), event(500)).unwrap();
        let db = Connection::open(home.path().join("state_5.sqlite")).unwrap();
        db.execute(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO threads VALUES (?1, ?2)",
            [ID, outside.path().to_str().unwrap()],
        )
        .unwrap();
        assert!(cached_history(home.path(), ID).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn fallback_does_not_follow_symlinks_outside_home() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), event(500)).unwrap();
        let dir = home.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join(format!("rollout-test-{ID}.jsonl")))
            .unwrap();
        assert!(cached_history(home.path(), ID).is_none());
    }
}
