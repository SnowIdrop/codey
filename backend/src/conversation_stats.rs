//! 会话统计：原生 rollout 提供持久化累计，路由探针提供本次运行期间的请求信息。
//! 两种累计不可相加；找不到原生累计时，返回明确标注范围的运行期数据。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use serde::Serialize;

use crate::route_request_log::RouteRequestLogEntry;

#[path = "conversation_stats_activity.rs"]
mod activity;
#[path = "conversation_stats_history.rs"]
mod history;
pub(crate) use history::snapshot_with_history;

/// 单会话状态条口径只统计正式生成请求（Responses create）；压缩等其他请求
/// 仍会计入 Token 累计，但不刷新速度与请求计数。
pub(crate) const CONVERSATION_REQUEST_KIND: &str = "responses";

const MAX_TRACKED_SESSIONS: usize = 512;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationStatsSnapshot {
    /// 最近一次完成请求实际使用的模型；上游未回报时回退请求模型。
    pub model: Option<String>,
    /// 正式生成请求完成数（含失败）。
    pub requests: Option<u64>,
    /// 仍在途的请求数，用于页面区分生成中与静止状态。
    pub in_flight: Option<u32>,
    /// 最近一次正式请求的完成状态：succeeded/failed/incomplete/cancelled。
    pub last_status: Option<String>,
    /// 正式请求的输出速度（Token/s），来源、范围与记录时间由 speed 字段明确。
    pub output_tokens_per_second: Option<f64>,
    pub speed_source: Option<&'static str>,
    pub speed_scope: Option<&'static str>,
    pub speed_recorded_at_unix_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    /// 累计总 Token；仅统计上游实际回报的值，未回报不计入。
    pub total_tokens: Option<u64>,
    pub updated_at_unix_ms: u64,
    pub usage_source: &'static str,
    pub usage_scope: &'static str,
    pub requests_scope: Option<&'static str>,
    pub turn_count: Option<u64>,
    pub step_count: Option<u64>,
    pub model_duration_ms: Option<u64>,
    pub tool_duration_ms: Option<u64>,
    pub average_ttft_ms: Option<f64>,
    pub activity_source: Option<&'static str>,
    pub activity_scope: Option<&'static str>,
    pub step_source: Option<&'static str>,
    pub step_scope: Option<&'static str>,
    pub timing_source: Option<&'static str>,
    pub timing_scope: Option<&'static str>,
    pub tool_timing_source: Option<&'static str>,
    pub tool_timing_scope: Option<&'static str>,
    pub timing_request_count: Option<u64>,
    pub ttft_sample_count: Option<u64>,
}

#[derive(Debug, Default)]
struct SessionStats {
    model: Option<String>,
    requests: u64,
    in_flight: u32,
    last_status: Option<String>,
    output_tokens_per_second: Option<f64>,
    input_tokens: u64,
    input_known: bool,
    output_tokens: u64,
    output_known: bool,
    cached_input_tokens: u64,
    cached_known: bool,
    cache_write_input_tokens: u64,
    cache_write_known: bool,
    total_tokens: u64,
    total_known: bool,
    /// 单调淘汰序号；系统时钟粒度不足以区分连续写入时仍保证淘汰稳定。
    touched_seq: u64,
    updated_at_unix_ms: u64,
    model_duration_ms: u64,
    timing_request_count: u64,
    ttft_total_ms: u64,
    ttft_sample_count: u64,
}

#[derive(Debug, Default)]
pub(crate) struct ConversationStatsRegistry {
    sessions: Mutex<HashMap<String, SessionStats>>,
    sequence: AtomicU64,
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 页面 DOM 与请求头里的本地会话标识可能带 `local:` 前缀，两边统一剥掉，
/// 避免同一会话前后缀不一致时统计落空。
fn normalize_session_id(value: &str) -> &str {
    value
        .trim()
        .strip_prefix("local:")
        .unwrap_or(value.trim())
        .trim()
}

impl ConversationStatsRegistry {
    /// 请求进入路由时调用；会话 id 已在请求头解析阶段确认存在。
    pub(crate) fn begin_request(&self, session_id: &str) {
        let session_id = normalize_session_id(session_id);
        if session_id.is_empty() {
            return;
        }
        let touched_seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut sessions = lock_unpoisoned(&self.sessions);
        let stats = sessions.entry(session_id.to_string()).or_default();
        stats.in_flight = stats.in_flight.saturating_add(1);
        stats.output_tokens_per_second = None;
        stats.model = None;
        stats.last_status = None;
        stats.updated_at_unix_ms = 0;
        stats.touched_seq = touched_seq;
        evict_overflow(&mut sessions);
    }

    /// 请求收尾（成功/失败/取消/不完整都会收尾）时调用。session 不存在时
    /// 静默忽略，保持与 begin_request 的对称语义。
    pub(crate) fn finish_request(&self, entry: &RouteRequestLogEntry) {
        let Some(session_id) = entry.codex_session_id.as_deref().map(normalize_session_id) else {
            return;
        };
        if session_id.is_empty() {
            return;
        }
        let touched_seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut sessions = lock_unpoisoned(&self.sessions);
        let stats = sessions.entry(session_id.to_string()).or_default();
        stats.in_flight = stats.in_flight.saturating_sub(1);
        stats.touched_seq = touched_seq;
        let own_generation =
            entry.request_kind == CONVERSATION_REQUEST_KIND && !entry.codex_session_is_parent;
        if own_generation {
            stats.updated_at_unix_ms = entry.timestamp_unix_ms;
            stats.model = entry
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .or_else(|| {
                    let requested = entry.requested_model.trim();
                    (!requested.is_empty()).then_some(requested)
                })
                .map(str::to_string);
        }
        let usage = &entry.token_usage;
        if let Some(value) = usage.input_tokens {
            stats.input_tokens = stats.input_tokens.saturating_add(value);
            stats.input_known = true;
        }
        if let Some(value) = usage.output_tokens {
            stats.output_tokens = stats.output_tokens.saturating_add(value);
            stats.output_known = true;
        }
        if let Some(value) = usage.cached_input_tokens {
            stats.cached_input_tokens = stats.cached_input_tokens.saturating_add(value);
            stats.cached_known = true;
        }
        if let Some(value) = usage.cache_creation_input_tokens {
            stats.cache_write_input_tokens = stats.cache_write_input_tokens.saturating_add(value);
            stats.cache_write_known = true;
        }
        let total =
            usage
                .total_tokens
                .or_else(|| match (usage.input_tokens, usage.output_tokens) {
                    (Some(input), Some(output)) => Some(input.saturating_add(output)),
                    _ => None,
                });
        if let Some(value) = total {
            stats.total_tokens = stats.total_tokens.saturating_add(value);
            stats.total_known = true;
        }
        if own_generation {
            stats.requests = stats.requests.saturating_add(1);
            if let Some(duration) = entry
                .router_pre_upstream_ms
                .and_then(|pre| entry.total_duration_ms.checked_sub(pre))
            {
                stats.model_duration_ms = stats.model_duration_ms.saturating_add(duration);
                stats.timing_request_count = stats.timing_request_count.saturating_add(1);
            }
            // first-byte 可能只是 SSE 心跳，只有实际首条内容计时才是 TTFT。
            if let Some(ttft) = entry
                .downstream_first_content_ms
                .filter(|v| *v <= entry.total_duration_ms)
            {
                stats.ttft_total_ms = stats.ttft_total_ms.saturating_add(ttft);
                stats.ttft_sample_count = stats.ttft_sample_count.saturating_add(1);
            }
            stats.last_status = Some(entry.status.as_str().to_string());
            stats.output_tokens_per_second = entry.output_tokens_per_second;
        }
        evict_overflow(&mut sessions);
    }

    pub(crate) fn snapshot(&self, session_id: &str) -> Option<ConversationStatsSnapshot> {
        let session_id = normalize_session_id(session_id);
        if session_id.is_empty() {
            return None;
        }
        let touched_seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let mut sessions = lock_unpoisoned(&self.sessions);
        let stats = sessions.get_mut(session_id)?;
        stats.touched_seq = touched_seq;
        Some(ConversationStatsSnapshot {
            model: stats.model.clone(),
            requests: Some(stats.requests),
            in_flight: Some(stats.in_flight),
            last_status: stats.last_status.clone(),
            output_tokens_per_second: stats.output_tokens_per_second,
            speed_source: stats.output_tokens_per_second.map(|_| "runtime"),
            speed_scope: stats.output_tokens_per_second.map(|_| "latest_request"),
            speed_recorded_at_unix_ms: stats
                .output_tokens_per_second
                .map(|_| stats.updated_at_unix_ms),
            input_tokens: stats.input_known.then_some(stats.input_tokens),
            output_tokens: stats.output_known.then_some(stats.output_tokens),
            cached_input_tokens: stats.cached_known.then_some(stats.cached_input_tokens),
            cache_write_input_tokens: stats
                .cache_write_known
                .then_some(stats.cache_write_input_tokens),
            total_tokens: stats.total_known.then_some(stats.total_tokens),
            updated_at_unix_ms: stats.updated_at_unix_ms,
            usage_source: "runtime",
            usage_scope: "runtime",
            requests_scope: Some("runtime"),
            step_count: Some(stats.requests),
            activity_source: Some("runtime"),
            activity_scope: Some("runtime"),
            step_source: Some("runtime"),
            step_scope: Some("runtime"),
            model_duration_ms: (stats.timing_request_count > 0).then_some(stats.model_duration_ms),
            average_ttft_ms: (stats.ttft_sample_count > 0)
                .then(|| stats.ttft_total_ms as f64 / stats.ttft_sample_count as f64),
            timing_source: Some("runtime"),
            timing_scope: Some("runtime"),
            timing_request_count: Some(stats.timing_request_count),
            ttft_sample_count: Some(stats.ttft_sample_count),
            ..Default::default()
        })
    }
}

fn evict_overflow(sessions: &mut HashMap<String, SessionStats>) {
    while sessions.len() > MAX_TRACKED_SESSIONS {
        let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, stats)| stats.touched_seq)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        sessions.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route_request_log::{RequestProtocol, RequestStatus, RequestTokenUsage};

    fn sample_entry(session: Option<&str>, kind: &str) -> RouteRequestLogEntry {
        RouteRequestLogEntry {
            requested_service_tier: None,
            service_tier: None,
            schema_version: 1,
            request_id: "request".into(),
            trace_id: "trace".into(),
            timestamp_unix_ms: 1_000,
            provider: None,
            provider_name: None,
            official_account_id: None,
            requested_model: "codey/gpt-5".into(),
            model: Some("gpt-5".into()),
            upstream_response_model: None,
            reasoning_effort: None,
            thinking_budget_tokens: None,
            ttft_ms: None,
            router_pre_upstream_ms: None,
            upstream_first_byte_ms: None,
            downstream_first_content_ms: None,
            upstream_header_ms: None,
            total_duration_ms: 1,
            queue_delay_ms: 0,
            output_tokens_per_second: Some(87.5),
            token_usage: RequestTokenUsage {
                input_tokens: Some(1_000),
                output_tokens: Some(200),
                cached_input_tokens: Some(600),
                cache_creation_input_tokens: Some(50),
                reasoning_output_tokens: None,
                total_tokens: Some(1_200),
            },
            usage_reported: true,
            usage_unavailable_reason: None,
            request_protocol: RequestProtocol::Sse,
            upstream_transport: None,
            request_kind: kind.into(),
            status: RequestStatus::Succeeded,
            status_code: None,
            upstream_status_code: None,
            error_code: None,
            upstream_error_summary: None,
            completion_reason: None,
            fallback_count: 0,
            fallback_reason: None,
            upstream_authority: None,
            upstream_request_headers: None,
            upstream_response_headers: None,
            upstream_request_id: None,
            upstream_protocol: None,
            protocol_bridge: None,
            request_input_state: None,
            request_input_items: None,
            request_has_previous_response_id: None,
            request_bytes: None,
            upstream_input_state: None,
            upstream_input_items: None,
            upstream_has_previous_response_id: None,
            upstream_bytes: None,
            first_byte_source: None,
            client_fingerprint: None,
            subagent: false,
            codex_session_id: session.map(str::to_string),
            codex_session_is_parent: false,
        }
    }

    #[test]
    fn aggregates_usage_and_speed_per_session() {
        let registry = ConversationStatsRegistry::default();
        registry.begin_request("thread-a");
        registry.begin_request("thread-b");
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        registry.finish_request(&sample_entry(Some("thread-a"), "responses_compact"));

        let a = registry.snapshot("thread-a").unwrap();
        assert_eq!(a.in_flight, Some(0));
        assert_eq!(a.requests, Some(1));
        assert_eq!(a.model.as_deref(), Some("gpt-5"));
        assert_eq!(a.input_tokens, Some(2_000));
        assert_eq!(a.output_tokens, Some(400));
        assert_eq!(a.cached_input_tokens, Some(1_200));
        assert_eq!(a.cache_write_input_tokens, Some(100));
        assert_eq!(a.total_tokens, Some(2_400));
        assert_eq!(a.output_tokens_per_second, Some(87.5));

        let b = registry.snapshot("thread-b").unwrap();
        assert_eq!(b.in_flight, Some(1));
        assert_eq!(b.requests, Some(0));
        assert!(registry.snapshot("thread-c").is_none());
    }

    #[test]
    fn ignores_entries_without_session() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(None, "responses"));
        assert!(registry.snapshot("thread-a").is_none());
    }

    #[test]
    fn keeps_unreported_fields_as_none() {
        let registry = ConversationStatsRegistry::default();
        let mut entry = sample_entry(Some("thread-a"), "responses");
        entry.token_usage = RequestTokenUsage::default();
        entry.output_tokens_per_second = None;
        registry.finish_request(&entry);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.requests, Some(1));
        assert_eq!(snapshot.input_tokens, None);
        assert_eq!(snapshot.cached_input_tokens, None);
        assert_eq!(snapshot.total_tokens, None);
        assert_eq!(snapshot.output_tokens_per_second, None);
    }

    #[test]
    fn model_switch_updates_display_model_and_speed() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        let mut switched = sample_entry(Some("thread-a"), "responses");
        switched.model = Some("claude-sonnet".into());
        switched.output_tokens_per_second = Some(42.0);
        registry.finish_request(&switched);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.model.as_deref(), Some("claude-sonnet"));
        assert_eq!(snapshot.output_tokens_per_second, Some(42.0));
    }

    #[test]
    fn inherited_subagent_request_cannot_replace_parent_request_speed() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        let mut child = sample_entry(Some("thread-a"), "responses");
        child.codex_session_is_parent = true;
        child.model = Some("child-model".into());
        child.output_tokens_per_second = Some(500.0);
        registry.finish_request(&child);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.requests, Some(1));
        assert_eq!(snapshot.model.as_deref(), Some("gpt-5"));
        assert_eq!(snapshot.output_tokens_per_second, Some(87.5));
    }

    #[test]
    fn new_request_and_unreported_speed_clear_previous_speed() {
        let registry = ConversationStatsRegistry::default();
        registry.finish_request(&sample_entry(Some("thread-a"), "responses"));
        registry.begin_request("thread-a");
        let active = registry.snapshot("thread-a").unwrap();
        assert_eq!(active.output_tokens_per_second, None);
        assert_eq!(active.model, None);
        assert_eq!(active.last_status, None);
        assert_eq!(active.updated_at_unix_ms, 0);
        let mut entry = sample_entry(Some("thread-a"), "responses");
        entry.output_tokens_per_second = None;
        entry.model = None;
        entry.requested_model.clear();
        registry.finish_request(&entry);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.output_tokens_per_second, None);
        assert_eq!(snapshot.model, None);
    }

    #[test]
    fn timing_uses_own_requests_and_actual_first_content_only() {
        let registry = ConversationStatsRegistry::default();
        let mut entry = sample_entry(Some("thread-a"), "responses");
        entry.total_duration_ms = 1200;
        entry.router_pre_upstream_ms = Some(200);
        entry.downstream_first_content_ms = Some(300);
        registry.finish_request(&entry);
        entry.downstream_first_content_ms = None;
        entry.ttft_ms = Some(1);
        registry.finish_request(&entry);
        entry.codex_session_is_parent = true;
        registry.finish_request(&entry);
        let snapshot = registry.snapshot("thread-a").unwrap();
        assert_eq!(snapshot.step_count, Some(2));
        assert_eq!(snapshot.turn_count, None);
        assert_eq!(snapshot.model_duration_ms, Some(2000));
        assert_eq!(snapshot.average_ttft_ms, Some(300.0));
        assert_eq!(snapshot.timing_request_count, Some(2));
        assert_eq!(snapshot.ttft_sample_count, Some(1));
        assert_eq!(snapshot.tool_duration_ms, None);
    }

    #[test]
    fn evicts_oldest_sessions_beyond_cap() {
        let registry = ConversationStatsRegistry::default();
        for index in 0..MAX_TRACKED_SESSIONS + 4 {
            let key = format!("thread-{index}");
            registry.begin_request(&key);
            registry.finish_request(&sample_entry(Some(&key), "responses"));
        }
        assert!(registry.snapshot("thread-0").is_none());
        assert!(registry.snapshot("thread-4").is_some());
    }
}
