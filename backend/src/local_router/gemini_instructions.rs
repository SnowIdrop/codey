use super::*;
use crate::model_catalog::{GEMINI_BASE_INSTRUCTIONS, is_gemini_upstream_model};

// Captured from CLI 0.153.3 and checked against both its model catalog and child request.
const LEGACY_BASE_INSTRUCTIONS: &str =
    include_str!("../../resources/codex-0.153.3-base-instructions.md");
const TEMPLATE_VERSION: &str = "antigravity-v1/codex-0.153.3";
pub(crate) const GEMINI_INSTRUCTIONS_ERROR: &str = "gemini_base_instructions_unrecognized";
pub(crate) const GEMINI_CHAT_TAIL_ERROR: &str = "gemini_chat_tail_unsupported";

/// 在共享发送入口补齐 Gemini 不接受的文本预填充尾轮；不修改历史或工具结果。
pub(crate) fn adapt_gemini_chat_tail(
    body: &mut Value,
    upstream_model: &str,
    official_account: bool,
    bridge: ProtocolBridge,
    request_kind: ResponsesRequestKind,
    compacting: bool,
) -> Result<Option<bool>> {
    if official_account
        || !is_gemini_upstream_model(upstream_model)
        || bridge != ProtocolBridge::ResponsesToChatCompletions
        || request_kind != ResponsesRequestKind::Create
        || compacting
    {
        return Ok(None);
    }
    // 日志只使用有限枚举；不得写入消息正文、调用 ID、函数名或参数。
    let tail_type = match body["messages"]
        .as_array()
        .and_then(|messages| messages.last())
    {
        Some(message) => match message["role"].as_str() {
            Some("assistant") => "assistant",
            Some("user") => "user",
            Some("tool") => "tool",
            _ => "other",
        },
        None => "missing",
    };
    let result = (|| -> Result<bool> {
        let messages = body
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow::anyhow!("Gemini Chat 请求缺少有效 messages；请求未发送上游"))?;
        if tail_type == "user" || tail_type == "tool" {
            return Ok(false);
        }
        anyhow::ensure!(
            tail_type == "assistant",
            "Gemini Chat 末轮不是可安全适配的消息；请求未发送上游"
        );

        // 只有新增继续轮次时才检查完整历史，防止先前尚未返回的并行调用被掩盖。
        let mut pending = std::collections::HashSet::new();
        for message in messages.iter() {
            anyhow::ensure!(
                message.get("function_call").is_none_or(Value::is_null),
                "Gemini Chat 历史含无法安全配对的旧式 function_call；请求未发送上游"
            );
            if message["role"] == "assistant" {
                if let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) {
                    let calls = calls.as_array().ok_or_else(|| {
                        anyhow::anyhow!("Gemini Chat tool_calls 不是有效数组；请求未发送上游")
                    })?;
                    for call in calls {
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .filter(|id| !id.trim().is_empty())
                            .ok_or_else(|| {
                                anyhow::anyhow!("Gemini Chat 工具调用缺少 ID；请求未发送上游")
                            })?;
                        anyhow::ensure!(
                            pending.insert(id),
                            "Gemini Chat 存在重复的待完成工具调用；请求未发送上游"
                        );
                    }
                }
            } else if message["role"] == "tool" {
                let id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Gemini Chat 工具结果缺少调用 ID；请求未发送上游")
                    })?;
                anyhow::ensure!(
                    pending.remove(id),
                    "Gemini Chat 工具结果没有对应的待完成调用；请求未发送上游"
                );
            }
        }
        anyhow::ensure!(
            pending.is_empty(),
            "Gemini Chat 存在尚未返回结果的工具调用；请求未发送上游"
        );
        let tail = messages.last().expect("assistant tail was checked");
        anyhow::ensure!(
            tail.get("content")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty()),
            "Gemini Chat 末轮不是非空文本 assistant，无法安全追加继续轮次；请求未发送上游"
        );
        messages.push(json!({"role":"user","content":"Continue the current task."}));
        Ok(true)
    })();
    let action = match &result {
        Ok(true) => "appended_continuation",
        Ok(false) => "unchanged",
        Err(_) => "rejected_unsafe_tail",
    };
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "router.gemini_chat_tail",
        json!({"requestId": ROUTER_REQUEST_ID.try_with(Clone::clone).ok(), "tailType": tail_type, "action": action}),
    );
    result.map(Some)
}

/// None means this route is outside the adaptation scope; the bool reports mutation.
pub(crate) fn adapt_gemini_base_instructions(
    body: &mut Value,
    upstream_model: &str,
    official_account: bool,
) -> Result<Option<bool>> {
    if official_account || !is_gemini_upstream_model(upstream_model) {
        return Ok(None);
    }
    // Only normalize line endings and surrounding whitespace. Never strip user content
    // or search input/developer messages for a prefix that resembles the base template.
    let instructions = body.get("instructions").and_then(Value::as_str);
    let normalized = instructions.map(|text| text.replace("\r\n", "\n"));
    let legacy = LEGACY_BASE_INSTRUCTIONS.replace("\r\n", "\n");
    let gemini = GEMINI_BASE_INSTRUCTIONS.replace("\r\n", "\n");
    let outcome = match normalized.as_deref().map(str::trim) {
        Some(text) if text == gemini.trim() => "already_adapted",
        Some(text) if text == legacy.trim() => {
            body["instructions"] = Value::String(gemini.trim().to_string());
            "replaced"
        }
        _ => "rejected_unrecognized",
    };
    let _ = codey_runtime_core::diagnostic_log::append_diagnostic_log(
        "router.gemini_base_instructions",
        json!({
            "requestId": ROUTER_REQUEST_ID.try_with(Clone::clone).ok(),
            "model": upstream_model,
            "outcome": outcome,
            "templateVersion": TEMPLATE_VERSION,
        }),
    );
    anyhow::ensure!(
        outcome != "rejected_unrecognized",
        "Gemini 基础指令未识别：仅支持顶层 instructions 中完整的已核实 GPT 或 Antigravity 模板；不支持缺失、空值、非字符串、自定义混合内容或 input 内基础指令。请求未发送上游；客户端模板更新后需更新 Codey 匹配基线"
    );
    Ok(Some(outcome == "replaced"))
}

#[cfg(test)]
#[path = "gemini_instructions_tests.rs"]
mod tests;
