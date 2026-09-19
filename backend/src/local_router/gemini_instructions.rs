use super::*;
use crate::model_catalog::{GEMINI_BASE_INSTRUCTIONS, is_gemini_upstream_model};

// Captured from CLI 0.153.3 and checked against both its model catalog and child request.
const LEGACY_BASE_INSTRUCTIONS: &str =
    include_str!("../../resources/codex-0.153.3-base-instructions.md");
const TEMPLATE_VERSION: &str = "antigravity-v1/codex-0.153.3";
pub(crate) const GEMINI_INSTRUCTIONS_ERROR: &str = "gemini_base_instructions_unrecognized";

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
