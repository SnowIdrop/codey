use base64::Engine as _;

use super::*;

// 压缩请求只在等待响应头时使用固定期限；响应体读取沿用上游响应的总期限
// 与空闲期限，避免把耗时较长的压缩在请求总期限处截断。
pub(crate) const COMPACTION_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(120);

// Codex 运行时的协作任务载荷使用 Fernet 令牌：版本字节、时间戳、初始向量、
// 按 16 字节分组且至少一组的密文、HMAC。Codey 不持有解密密钥，只能在发送
// 前做结构校验。
const FERNET_PREFIX_BYTES: usize = 1 + 8 + 16;
const FERNET_SUFFIX_BYTES: usize = 32;
const FERNET_BLOCK_BYTES: usize = 16;

enum EncryptedContentRewrite {
    Keep,
    Replace(Value),
    Drop,
}

/// Opt-in portable collaboration tasks: request plaintext from the producing
/// model rather than attempting to decrypt its result. Only the three native
/// collaboration message parameters are changed; unrelated secrets stay intact.
pub(crate) fn prepare_plaintext_agent_arguments(body: &mut Value) -> bool {
    let mut changed = request_plaintext_agent_arguments(body.get_mut("tools"), None);
    // Current native Codex publishes schemas as typed input items, not only
    // top-level tools. Do not recursively rewrite arbitrary message data.
    let items = match body.get_mut("input") {
        Some(Value::Array(items)) => items.as_mut_slice(),
        Some(item @ Value::Object(_)) => std::slice::from_mut(item),
        _ => return changed,
    };
    for item in items {
        if item["type"] == "additional_tools" {
            changed |= request_plaintext_agent_arguments(item.get_mut("tools"), None);
        }
    }
    changed
}

pub(crate) fn restrict_specialized_agent_roles(body: &mut Value, roles: &[String]) -> Result<bool> {
    let mut changed = restrict_spawn_tools(body.get_mut("tools"), None, roles)?;
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if item["type"] == "additional_tools" {
                changed |= restrict_spawn_tools(item.get_mut("tools"), None, roles)?;
            }
        }
    }
    Ok(changed)
}

fn restrict_spawn_tools(
    tools: Option<&mut Value>,
    namespace: Option<&str>,
    roles: &[String],
) -> Result<bool> {
    let Some(tools) = tools.and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let mut changed = false;
    let mut index = 0;
    while index < tools.len() {
        let tool = &mut tools[index];
        if tool["type"] == "namespace" {
            let name = tool["name"].as_str().unwrap_or_default().to_owned();
            if namespace.is_none() && matches!(name.as_str(), "agents" | "collaboration") {
                changed |= restrict_spawn_tools(tool.get_mut("tools"), Some(&name), roles)?;
            }
            index += 1;
            continue;
        }
        if tool["type"] != "function" {
            index += 1;
            continue;
        }
        let explicit_namespace = tool["namespace"].as_str().map(str::to_owned);
        let function = if tool.get("function").is_some() {
            &mut tool["function"]
        } else {
            tool
        };
        let Some(name) = function["name"].as_str() else {
            index += 1;
            continue;
        };
        let (ns, name) = name.split_once('.').unwrap_or((
            explicit_namespace
                .as_deref()
                .or(namespace)
                .unwrap_or_default(),
            name,
        ));
        if !matches!(ns, "agents" | "collaboration") || name != "spawn_agent" {
            index += 1;
            continue;
        }
        changed = true;
        if roles.is_empty() {
            tools.remove(index);
            continue;
        }
        let parameters = function
            .get_mut("parameters")
            .and_then(Value::as_object_mut)
            .context("原生 spawn_agent 缺少参数对象，无法约束可用角色")?;
        parameters
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .context("原生 spawn_agent 缺少参数属性对象，无法约束可用角色")?
            .insert("agent_type".into(), json!({"type":"string", "enum":roles}));
        let required = parameters
            .entry("required")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("原生 spawn_agent required 必须为数组")?;
        if !required.iter().any(|name| name == "agent_type") {
            required.push(json!("agent_type"));
        }
        index += 1;
    }
    Ok(changed)
}

pub(crate) fn request_plaintext_agent_arguments(
    tools: Option<&mut Value>,
    namespace: Option<&str>,
) -> bool {
    let Some(tools) = tools.and_then(Value::as_array_mut) else {
        return false;
    };
    let mut changed = false;
    for tool in tools {
        if tool["type"] == "namespace" {
            let name = tool["name"].as_str().unwrap_or_default().to_owned();
            if namespace.is_none() && matches!(name.as_str(), "agents" | "collaboration") {
                changed |= request_plaintext_agent_arguments(tool.get_mut("tools"), Some(&name));
            }
            continue;
        }
        if tool["type"] != "function" {
            continue;
        }
        let explicit_namespace = tool["namespace"].as_str().map(str::to_owned);
        let function = if tool.get("function").is_some() {
            &mut tool["function"]
        } else {
            tool
        };
        let Some(name) = function["name"].as_str() else {
            continue;
        };
        let (ns, name) = name.split_once('.').unwrap_or((
            explicit_namespace
                .as_deref()
                .or(namespace)
                .unwrap_or_default(),
            name,
        ));
        if !matches!(ns, "agents" | "collaboration")
            || !matches!(name, "spawn_agent" | "send_message" | "followup_task")
        {
            continue;
        }
        if let Some(message) = function
            .get_mut("parameters")
            .and_then(|p| p.get_mut("properties"))
            .and_then(|p| p.get_mut("message"))
            .and_then(Value::as_object_mut)
            && message.get("type").and_then(Value::as_str) == Some("string")
            && message.get("encrypted").and_then(Value::as_bool) == Some(true)
        {
            message.remove("encrypted");
            changed = true;
        }
    }
    changed
}

/// Refuse agent payloads that would become an empty task at the destination.
/// A Responses-compatible endpoint is not evidence that it holds Codex's keys.
/// Reasoning/compaction ciphertext is deliberately outside this check.
pub(crate) fn validate_agent_payload_delivery(
    body: &Value,
    accepts_codex_ciphertext: bool,
) -> Result<()> {
    for item in input_items(body) {
        if item.get("type").and_then(Value::as_str) != Some("agent_message") {
            continue;
        }
        let parts = match item.get("content") {
            Some(Value::Array(parts)) => parts.as_slice(),
            Some(part @ Value::Object(_)) => std::slice::from_ref(part),
            _ => continue,
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("encrypted_content") {
                continue;
            }
            let payload = part
                .get("encrypted_content")
                .and_then(Value::as_str)
                .filter(|payload| !payload.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!(
                    "agent_task_body_unavailable: 协作任务正文为空或格式无效；已停止发送，不允许子代理根据历史猜测任务"
                ))?;
            if is_codex_encrypted_payload(payload) && !accepts_codex_ciphertext {
                anyhow::bail!(
                    "agent_task_body_unavailable: 当前线路无法保证读取 Codex 加密任务正文；Codey 不持有解密密钥，已停止发送。请使用兼容的官方 Responses 线路或通过受支持的明文任务通道重新派发；重复发送相同密文不会修复此问题"
                );
            }
        }
        // A previously adapted history may already have lost the encrypted
        // part. Do not mistake the remaining native envelope for a task body.
        if !parts.iter().any(|part| part["type"] == "encrypted_content") {
            let text = parts
                .iter()
                .filter(|part| {
                    matches!(
                        part["type"].as_str(),
                        Some("input_text" | "output_text" | "text")
                    )
                })
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if (text.starts_with("Message Type: NEW_TASK\n")
                || text.starts_with("Message Type: MESSAGE\n"))
                && text
                    .split_once("\nPayload:")
                    .is_some_and(|(_, body)| body.trim().is_empty())
            {
                anyhow::bail!(
                    "agent_task_body_unavailable: 协作任务只剩消息头，正文缺失；已停止发送"
                );
            }
        }
    }
    Ok(())
}

fn input_items(body: &Value) -> &[Value] {
    match body.get("input") {
        Some(Value::Array(items)) => items,
        Some(item @ Value::Object(_)) => std::slice::from_ref(item),
        _ => &[],
    }
}

pub(crate) fn is_compaction_request(body: &Value, kind: ResponsesRequestKind) -> bool {
    kind == ResponsesRequestKind::Compact
        || input_items(body)
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
}

pub(crate) fn validate_portable_context(body: &Value) -> Result<()> {
    let is_compaction = |item: &Value| {
        matches!(
            item.get("type").and_then(Value::as_str),
            Some("compaction" | "compaction_trigger")
        )
    };
    for item in input_items(body) {
        let content = item.get("content");
        if is_compaction(item)
            || content.is_some_and(|content| {
                is_compaction(content)
                    || content
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(is_compaction))
            })
        {
            anyhow::bail!(
                "context_not_portable: compaction 历史不能转换到当前线路；请回到原线路完成本地摘要后再切换"
            );
        }
    }
    Ok(())
}

pub(crate) fn validate_cross_route_context(body: &Value) -> Result<()> {
    validate_portable_context(body)?;
    fn contains_item_reference(value: &Value) -> bool {
        if value.get("type").and_then(Value::as_str) == Some("item_reference") {
            return true;
        }
        match value {
            Value::Array(items) => items.iter().any(contains_item_reference),
            Value::Object(object) => object.values().any(contains_item_reference),
            _ => false,
        }
    }
    if input_items(body).iter().any(contains_item_reference) {
        anyhow::bail!("context_not_portable: item_reference 属于上一条线路，不能发送到新的供应商");
    }
    Ok(())
}

/// 原生 Responses 线路在发送前统一处理协作任务载荷和跨线路 reasoning 状态。
pub(crate) fn normalize_native_responses_context(
    body: &mut Value,
    discard_opaque_reasoning: bool,
) -> bool {
    let mut changed = normalize_encrypted_agent_payloads(body);
    // 同一线路必须原样回传 reasoning，包括第三方 thinking 模式需要的明文内容。
    if discard_opaque_reasoning {
        changed |= discard_reasoning_history(body);
    }
    changed
}

/// 第三方 Responses 接口未必识别 Codex 的 agent_message 项；仅把正文改成
/// input_text 仍可能丢失整条任务。载荷校验、明文归一化后，将其作为收到的
/// user 消息发送，保留发送者、接收者和全部内容，不冒充当前代理的历史回答。
pub(crate) fn normalize_portable_agent_messages(body: &mut Value) -> bool {
    let items = match body.get_mut("input") {
        Some(Value::Array(items)) => items.as_mut_slice(),
        Some(item @ Value::Object(_)) => std::slice::from_mut(item),
        _ => return false,
    };
    let mut changed = false;
    for item in items {
        if item["type"] != "agent_message" {
            continue;
        }
        let Some(content) = item.get("content").or_else(|| item.get("message")).cloned() else {
            continue;
        };
        let mut parts = match content {
            Value::Array(parts) => parts,
            Value::String(text) => vec![json!({"type":"input_text","text":text})],
            part @ Value::Object(_) => vec![part],
            _ => continue,
        };
        let mut metadata = String::new();
        for field in ["author", "recipient"] {
            if let Some(value) = item.get(field).and_then(Value::as_str) {
                metadata.push_str(&format!("{field}: {value}\n"));
            }
        }
        if !metadata.is_empty() {
            parts.push(json!({"type":"input_text","text":metadata}));
        }
        // amsg ID 和内部路由字段不属于标准 message；归属信息已保留在正文中。
        *item = json!({"type":"message","role":"user","content":parts});
        changed = true;
    }
    changed
}

// 旧兼容路径使用的缺失明文标记；不能代替真实推理内容或恢复上游状态。
pub(crate) const MISSING_REASONING_TEXT_PLACEHOLDER: &str = "(thinking unavailable)";

/// 第三方中断响应的 opaque 状态可能失效；仅在已有真实明文时移除该字段，
/// 让上游按明文重放。调用方只在明确的 reasoning_text 错误后重试一次。
pub(crate) fn prefer_plaintext_reasoning(body: &mut Value) -> bool {
    let rewrite = |item: &mut Value| {
        let Some(object) = item.as_object_mut() else {
            return false;
        };
        if object.get("type").and_then(Value::as_str) != Some("reasoning") {
            return false;
        }
        let has_text = match object.get("content") {
            Some(Value::Array(parts)) => parts.iter().any(reasoning_part_has_text),
            Some(part @ Value::Object(_)) => reasoning_part_has_text(part),
            _ => false,
        };
        has_text && object.remove("encrypted_content").is_some()
    };
    match body.get_mut("input") {
        Some(Value::Array(items)) => {
            let mut changed = false;
            for item in items {
                changed |= rewrite(item);
            }
            changed
        }
        Some(item @ Value::Object(_)) => rewrite(item),
        _ => false,
    }
}

/// 部分第三方 thinking 模式（DeepSeek 等）要求把上一轮的 reasoning 明文原样
/// 回传，而 Codex 回放历史时会省略 reasoning 项的明文 content，只保留
/// encrypted_content，上游因此拒绝整条请求。这里给缺少明文的 reasoning 项补一段
/// 占位文本；已有明文的项保持原始字节不变。
pub(crate) fn fill_missing_reasoning_text(body: &mut Value) -> bool {
    match body.get_mut("input") {
        Some(Value::Array(items)) => {
            let mut changed = false;
            for item in items {
                changed |= fill_reasoning_item_text(item);
            }
            changed
        }
        Some(item @ Value::Object(_)) => fill_reasoning_item_text(item),
        _ => false,
    }
}

fn fill_reasoning_item_text(item: &mut Value) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return false;
    }
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    if object.get("content").is_none() {
        object.insert(
            "content".to_string(),
            Value::Array(vec![reasoning_text_placeholder()]),
        );
        return true;
    }
    let Some(content) = object.get_mut("content") else {
        return false;
    };
    match content {
        Value::Array(parts) => {
            if parts.iter().any(reasoning_part_has_text) {
                return false;
            }
            // 空白片段一并清理，只留下占位明文。
            parts.retain(|part| !is_reasoning_text_part(part));
            parts.push(reasoning_text_placeholder());
            true
        }
        Value::Object(_) => {
            if reasoning_part_has_text(content) {
                return false;
            }
            *content = reasoning_text_placeholder();
            true
        }
        _ => {
            *content = Value::Array(vec![reasoning_text_placeholder()]);
            true
        }
    }
}

fn reasoning_text_placeholder() -> Value {
    json!({"type":"reasoning_text","text":MISSING_REASONING_TEXT_PLACEHOLDER})
}

fn reasoning_part_has_text(part: &Value) -> bool {
    is_reasoning_text_part(part)
        && part
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
}

fn is_reasoning_text_part(part: &Value) -> bool {
    matches!(
        part.get("type").and_then(Value::as_str),
        Some("reasoning_text" | "text")
    )
}

/// 协作任务正文由 Codex 运行时刻写入 `agent_message` 的 `encrypted_content`
/// 字段，交给持有密钥的上游解密；线路本身不判断内容。第三方线路经常把该
/// 字段直接写成明文，接收方解密失败会拒绝整条请求。Chat Completions 和
/// Anthropic Messages 都表达不了这个字段，转换时只能丢弃，任务正文会随之
/// 消失，因此先按结构识别：令牌形态留给原生 Responses，适配线路显式拒绝；
/// 其余形态改写为可见文本。
pub(crate) fn normalize_encrypted_agent_payloads(body: &mut Value) -> bool {
    match body.get_mut("input") {
        Some(Value::Array(items)) => {
            let mut changed = false;
            for item in items {
                changed |= normalize_agent_message_item(item);
            }
            changed
        }
        Some(item @ Value::Object(_)) => normalize_agent_message_item(item),
        _ => false,
    }
}

/// Chat 和 Anthropic 无法解密协作任务，必须在转换丢弃不透明内容前拒绝。
/// 只检查 agent_message，其他消息的推理状态仍按原有规则处理。
pub(crate) fn validate_adapted_agent_payloads(body: &Value) -> Result<()> {
    let is_encrypted_payload = |part: &Value| {
        part.get("type").and_then(Value::as_str) == Some("encrypted_content")
            && part
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(is_codex_encrypted_payload)
    };
    for item in input_items(body) {
        if item.get("type").and_then(Value::as_str) != Some("agent_message") {
            continue;
        }
        if item.get("content").is_some_and(|content| {
            is_encrypted_payload(content)
                || content
                    .as_array()
                    .is_some_and(|parts| parts.iter().any(is_encrypted_payload))
        }) {
            anyhow::bail!(
                "context_not_portable: 当前线路无法解密子代理任务正文，请使用原生 Responses 线路或重新提供明文任务"
            );
        }
    }
    Ok(())
}

fn normalize_agent_message_item(item: &mut Value) -> bool {
    if item.get("type").and_then(Value::as_str) != Some("agent_message") {
        return false;
    }
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    let mut drop_content = false;
    match object.get_mut("content") {
        Some(Value::Array(parts)) => {
            parts.retain_mut(|part| match encrypted_content_rewrite(part) {
                EncryptedContentRewrite::Keep => true,
                EncryptedContentRewrite::Replace(replacement) => {
                    *part = replacement;
                    changed = true;
                    true
                }
                EncryptedContentRewrite::Drop => {
                    changed = true;
                    false
                }
            })
        }
        Some(part @ Value::Object(_)) => match encrypted_content_rewrite(part) {
            EncryptedContentRewrite::Keep => {}
            EncryptedContentRewrite::Replace(replacement) => {
                *part = replacement;
                changed = true;
            }
            EncryptedContentRewrite::Drop => {
                changed = true;
                drop_content = true;
            }
        },
        _ => {}
    }
    if drop_content {
        object.remove("content");
    }
    changed
}

fn encrypted_content_rewrite(part: &Value) -> EncryptedContentRewrite {
    if part.get("type").and_then(Value::as_str) != Some("encrypted_content") {
        return EncryptedContentRewrite::Keep;
    }
    let Some(payload) = part.get("encrypted_content").and_then(Value::as_str) else {
        return EncryptedContentRewrite::Drop;
    };
    if is_codex_encrypted_payload(payload) {
        return EncryptedContentRewrite::Keep;
    }
    if payload.trim().is_empty() {
        return EncryptedContentRewrite::Drop;
    }
    EncryptedContentRewrite::Replace(json!({"type":"input_text","text":payload}))
}

fn is_codex_encrypted_payload(value: &str) -> bool {
    let encoded = value.trim().trim_end_matches('=');
    let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    decoded.first() == Some(&0x80)
        && decoded.len() >= FERNET_PREFIX_BYTES + FERNET_SUFFIX_BYTES + FERNET_BLOCK_BYTES
        && (decoded.len() - FERNET_PREFIX_BYTES - FERNET_SUFFIX_BYTES)
            .is_multiple_of(FERNET_BLOCK_BYTES)
}

fn discard_reasoning_history(body: &mut Value) -> bool {
    let Some(input) = body.get_mut("input") else {
        return false;
    };
    match input {
        Value::Array(items) => {
            let previous_len = items.len();
            items.retain(|item| item.get("type").and_then(Value::as_str) != Some("reasoning"));
            items.len() != previous_len
        }
        Value::Object(_) if input.get("type").and_then(Value::as_str) == Some("reasoning") => {
            *input = Value::Array(Vec::new());
            true
        }
        _ => false,
    }
}

// Only in-flight work is owned here. Codex remains responsible for history
// versions, retries and installing a successful compaction result.
pub(crate) struct CompactionGuard {
    bindings: Arc<Mutex<RouteBindings>>,
    keys: Vec<String>,
}

impl CompactionGuard {
    pub(crate) fn acquire(bindings: &Arc<Mutex<RouteBindings>>, keys: Vec<String>) -> Result<Self> {
        let mut state = bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if keys.iter().any(|key| state.compacting.contains(key)) {
            anyhow::bail!("同一会话已有压缩请求正在执行，请等待完成后重试");
        }
        state.compacting.extend(keys.iter().cloned());
        Ok(Self {
            bindings: Arc::clone(bindings),
            keys,
        })
    }
}

impl Drop for CompactionGuard {
    fn drop(&mut self) {
        let mut state = self
            .bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in &self.keys {
            state.compacting.remove(key);
        }
    }
}

impl RouterServer {
    pub(crate) async fn proxy_with_compaction_budget<D: ResponsesDownstream + ?Sized>(
        &self,
        request: HttpRequest,
        body: Value,
        encoded_body: Option<Vec<u8>>,
        kind: ResponsesRequestKind,
        downstream: &mut D,
    ) -> Result<()> {
        if !is_compaction_request(&body, kind) {
            return self
                .proxy_parsed_responses_inner(request, body, encoded_body, kind, downstream)
                .await;
        }
        let keys = request_binding_keys(&request);
        // ponytail: without a session identifier only identical requests can be
        // deduplicated; a host revision is required for stronger idempotency.
        let keys = if keys.is_empty() {
            let bytes = serde_json::to_vec(&body)?;
            vec![format!("compact-input:{:x}", Sha256::digest(bytes))]
        } else {
            keys
        };
        let _guard = match CompactionGuard::acquire(&self.bindings, keys) {
            Ok(guard) => guard,
            Err(error) => {
                return downstream
                    .write_error(409, "compaction_in_progress", error.to_string(), None)
                    .await;
            }
        };
        // 压缩结果必须完整校验后才能写回下游：等待上游期间不能先开始 SSE 或
        // HTTP 响应，否则失败时只能在已经开始的响应里追加 JSON 错误。
        self.proxy_parsed_responses_inner(request, body, encoded_body, kind, downstream)
            .await
    }
}

pub(crate) fn validate_compaction_result(value: &Value, v2: bool) -> Result<()> {
    check_context_length_error(value)?;
    // A valid candidate must also fit in the next request. Ciphertext byte
    // length cannot establish token count or semantic quality; Codex rechecks
    // the target model budget before installing/sending its history.
    bounded_json_bytes(value, MAX_REQUEST_BYTES)?;
    if v2 && value.get("status").and_then(Value::as_str) != Some("completed") {
        anyhow::bail!("远程压缩未成功完成");
    }
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("远程压缩缺少 output 数组"))?;
    let mut count = 0;
    for item in output {
        if item.get("type").and_then(Value::as_str).is_none() {
            anyhow::bail!("远程压缩包含无效的输出项");
        }
        if item["type"] == "compaction" {
            count += 1;
            if item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_none_or(|s| s.trim().is_empty())
            {
                anyhow::bail!("远程压缩缺少有效的 encrypted_content");
            }
        }
    }
    if count != 1 {
        anyhow::bail!("远程压缩必须返回且仅返回一个 compaction 项，实际收到 {count} 个");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specialized_roles_constrain_native_spawn_schemas_only() {
        let spawn = json!({"type":"function","name":"spawn_agent","parameters":{
            "type":"object","properties":{"agent_type":{"type":["string","null"]},"message":{"type":"string"}},
            "required":["message"]
        }});
        let mut qualified = spawn.clone();
        qualified["name"] = json!("agents.spawn_agent");
        let mut body = json!({
            "tools":[{"type":"namespace","name":"agents","tools":[spawn.clone()]}],
            "input":[{"type":"additional_tools","tools":[qualified,
                {"type":"namespace","name":"other","tools":[spawn.clone()]}]}]
        });
        let roles = vec!["codey_quick_scan".to_string(), "codey_comments".to_string()];
        assert!(restrict_specialized_agent_roles(&mut body, &roles).unwrap());
        for function in [&body["tools"][0]["tools"][0], &body["input"][0]["tools"][0]] {
            assert_eq!(
                function["parameters"]["properties"]["agent_type"],
                json!({"type":"string","enum":roles})
            );
            assert_eq!(
                function["parameters"]["required"],
                json!(["message", "agent_type"])
            );
        }
        assert_eq!(body["input"][0]["tools"][1]["tools"][0], spawn);
        assert!(restrict_specialized_agent_roles(&mut body, &[]).unwrap());
        assert_eq!(body["tools"][0]["tools"], json!([]));
        assert_eq!(body["input"][0]["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn specialized_roles_reject_malformed_native_schema() {
        for parameters in [
            Value::Null,
            json!([]),
            json!({"properties":null}),
            json!({"properties":{},"required":null}),
        ] {
            let mut body = json!({"tools":[{"type":"function","name":"agents.spawn_agent","parameters":parameters}]});
            assert!(
                restrict_specialized_agent_roles(&mut body, &["codey_comments".into()]).is_err()
            );
        }
    }

    #[test]
    fn specialized_roles_snapshot_respects_enhancement_and_enabled_state() {
        let mut config = CodeyConfig {
            subagent_optimization: false,
            ..CodeyConfig::default()
        };
        assert!(
            RouterSnapshot::from_config(&config)
                .specialized_agent_roles
                .is_none()
        );
        config.subagent_optimization = true;
        config.subagent_roles = crate::config::default_subagent_roles();
        config
            .subagent_roles
            .get_mut("codey_worker")
            .unwrap()
            .enabled = false;
        let snapshot = RouterSnapshot::from_config(&config);
        let roles = snapshot.specialized_agent_roles.unwrap();
        assert!(roles.contains(&"codey_quick_scan".to_string()));
        assert!(!roles.contains(&"default".to_string()));
        assert!(!roles.contains(&"codey_worker".to_string()));
    }

    fn fernet_token(ciphertext: &[u8]) -> String {
        let mut token = vec![0x80];
        token.extend_from_slice(&1_700_000_000u64.to_be_bytes());
        token.extend_from_slice(&[0x11; 16]);
        token.extend_from_slice(ciphertext);
        token.extend_from_slice(&[0x22; 32]);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
    }

    fn agent_message(content: Value) -> Value {
        json!({
            "type":"agent_message",
            "id":"amsg_test",
            "author":"/root",
            "recipient":"/root/child",
            "content":content
        })
    }

    #[test]
    fn portable_agent_schema_changes_only_reviewed_message_arguments() {
        let schema = || {
            json!({"type":"object","properties":{
                "message":{"type":"string","encrypted":true},
                "secret":{"type":"string","encrypted":true}
            }})
        };
        for namespace in ["agents", "collaboration"] {
            let mut tools = json!([{"type":"namespace","name":namespace,"tools":[
                {"type":"function","name":"spawn_agent","parameters":schema()},
                {"type":"function","name":"send_message","parameters":schema()},
                {"type":"function","name":"followup_task","parameters":schema()},
                {"type":"function","name":"other","parameters":schema()}
            ]}]);
            assert!(request_plaintext_agent_arguments(Some(&mut tools), None));
            for i in 0..3 {
                assert_eq!(
                    tools[0]["tools"][i]["parameters"]["properties"]["message"]["encrypted"],
                    Value::Null
                );
                assert_eq!(
                    tools[0]["tools"][i]["parameters"]["properties"]["secret"]["encrypted"],
                    true
                );
            }
            assert_eq!(
                tools[0]["tools"][3]["parameters"]["properties"]["message"]["encrypted"],
                true
            );
            assert!(!request_plaintext_agent_arguments(Some(&mut tools), None));
        }
    }

    #[test]
    fn portable_agent_schema_supports_flat_names_but_not_unrelated_namespaces() {
        let schema =
            json!({"type":"object","properties":{"message":{"type":"string","encrypted":true}}});
        let mut tools = json!([
            {"type":"function","name":"agents.spawn_agent","parameters":schema},
            {"type":"function","function":{"name":"collaboration.send_message","parameters":schema}},
            {"type":"namespace","name":"other","tools":[{"type":"function","name":"spawn_agent","parameters":schema}]},
            {"type":"function","name":"spawn_agent","parameters":schema}
        ]);
        let original = tools.clone();
        assert!(request_plaintext_agent_arguments(Some(&mut tools), None));
        assert_eq!(
            tools[0]["parameters"]["properties"]["message"]["encrypted"],
            Value::Null
        );
        assert_eq!(
            tools[1]["function"]["parameters"]["properties"]["message"]["encrypted"],
            Value::Null
        );
        assert_eq!(tools[2], original[2]);
        assert_eq!(tools[3], original[3]);
    }

    #[test]
    fn portable_agent_schema_handles_native_additional_tools_only() {
        let tools = json!([{"type":"function","name":"agents.send_message","parameters":{
            "type":"object","properties":{"message":{"type":"string","encrypted":true}}
        }}]);
        let item =
            json!({"type":"additional_tools","id":"tools_native","role":"system","tools":tools});
        for input in [item.clone(), json!([item.clone(), item.clone()])] {
            let mut body = json!({"input":input});
            assert!(prepare_plaintext_agent_arguments(&mut body));
            assert!(!prepare_plaintext_agent_arguments(&mut body));
            assert!(!body.to_string().contains("\"encrypted\":true"));
        }
        let mut body = json!({"input":[{"type":"message","role":"user","tools":tools}]});
        let original = body.clone();
        assert!(!prepare_plaintext_agent_arguments(&mut body));
        assert_eq!(body, original);
    }

    #[test]
    fn agent_delivery_rejects_ciphertext_without_destination_support() {
        for kind in ["NEW_TASK", "MESSAGE"] {
            let body = json!({"input":[agent_message(json!([
                {"type":"input_text","text":format!("Message Type: {kind}\nPayload:\n")},
                {"type":"encrypted_content","encrypted_content":fernet_token(&[0x33; 16])}
            ]))]});
            let original = body.clone();
            let error = validate_agent_payload_delivery(&body, false).unwrap_err();
            assert!(error.to_string().contains("agent_task_body_unavailable"));
            assert!(validate_agent_payload_delivery(&body, true).is_ok());
            assert_eq!(body, original);
        }
    }

    #[test]
    fn agent_delivery_accepts_plaintext_and_rejects_missing_payloads() {
        for payload in [json!(""), json!("  "), json!(null), json!(17)] {
            let body = json!({"input":agent_message(json!({
                "type":"encrypted_content", "encrypted_content":payload
            }))});
            for supported in [true, false] {
                assert!(validate_agent_payload_delivery(&body, supported).is_err());
            }
        }
        let mut body = json!({"input":agent_message(json!({
            "type":"encrypted_content", "encrypted_content":"nonce: delivery-test"
        }))});
        assert!(validate_agent_payload_delivery(&body, false).is_ok());
        assert!(normalize_encrypted_agent_payloads(&mut body));
        assert_eq!(body["input"]["content"]["text"], "nonce: delivery-test");
        assert!(validate_agent_payload_delivery(&body, false).is_ok());
    }

    #[test]
    fn agent_delivery_does_not_reinterpret_unrelated_ciphertext() {
        let body = json!({"input":[
            {"type":"reasoning","encrypted_content":fernet_token(&[0x33; 16])},
            {"type":"message","role":"user","content":[
                {"type":"input_text","text":"Payload:"}
            ]}
        ]});
        assert!(validate_agent_payload_delivery(&body, false).is_ok());
    }

    #[test]
    fn agent_delivery_rejects_header_only_after_previous_conversion() {
        let mut body = json!({"input":[agent_message(json!([
            {"type":"input_text","text":"Message Type: NEW_TASK\nTask name: /root/child\nPayload:\n"}
        ]))]});
        assert!(validate_agent_payload_delivery(&body, false).is_err());
        body["input"][0]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"input_text","text":"nonce: visible"}));
        assert!(validate_agent_payload_delivery(&body, false).is_ok());
    }

    #[test]
    fn native_reasoning_normalization_preserves_same_route_history() {
        for encrypted in [None, Some("opaque-state")] {
            let mut reasoning = json!({
                "type":"reasoning", "id":"rs_provider", "summary":[],
                "content":[{"type":"reasoning_text","text":"检查工具结果。Next step 🙂"}]
            });
            if let Some(encrypted) = encrypted {
                reasoning["encrypted_content"] = json!(encrypted);
            }
            let user = json!({"role":"user","content":"continue"});
            for input in [reasoning.clone(), json!([reasoning, user.clone()])] {
                let original = json!({"input":input});
                let mut body = original.clone();
                assert!(!normalize_native_responses_context(&mut body, false));
                assert_eq!(body, original);

                assert!(normalize_native_responses_context(&mut body, true));
                assert_eq!(
                    body["input"],
                    if input.is_array() {
                        json!([user])
                    } else {
                        json!([])
                    }
                );
                assert!(!normalize_native_responses_context(&mut body, true));
            }
        }
    }

    #[test]
    fn missing_reasoning_text_gets_placeholder() {
        let mut body = json!({
            "input":[
                {"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque"},
                {"type":"reasoning","id":"rs_2","summary":[],"encrypted_content":"opaque",
                 "content":[{"type":"reasoning_text","text":"真实明文"}]},
                {"type":"reasoning","id":"rs_3","summary":[],"encrypted_content":"opaque","content":[]},
                {"role":"user","content":[{"type":"input_text","text":"继续"}]}
            ]
        });
        assert!(fill_missing_reasoning_text(&mut body));
        assert_eq!(
            body["input"][0]["content"],
            json!([{"type":"reasoning_text","text":MISSING_REASONING_TEXT_PLACEHOLDER}])
        );
        assert_eq!(
            body["input"][1]["content"],
            json!([{"type":"reasoning_text","text":"真实明文"}])
        );
        assert_eq!(
            body["input"][2]["content"],
            json!([{"type":"reasoning_text","text":MISSING_REASONING_TEXT_PLACEHOLDER}])
        );
        assert_eq!(
            body["input"][3],
            json!({"role":"user","content":[{"type":"input_text","text":"继续"}]})
        );
        // 补齐后的请求再次经过时保持字节不变。
        assert!(!fill_missing_reasoning_text(&mut body));
    }

    #[test]
    fn blank_reasoning_text_is_replaced_and_other_inputs_are_kept() {
        let mut single = json!({
            "input":{"type":"reasoning","id":"rs_1","summary":[],
                     "content":[{"type":"reasoning_text","text":"   "}]}
        });
        assert!(fill_missing_reasoning_text(&mut single));
        assert_eq!(
            single["input"]["content"],
            json!([{"type":"reasoning_text","text":MISSING_REASONING_TEXT_PLACEHOLDER}])
        );

        let mut text_plaintext = json!({
            "input":[{"type":"reasoning","id":"rs_2","summary":[],
                      "content":[{"type":"text","text":"第三方明文"}]}]
        });
        assert!(!fill_missing_reasoning_text(&mut text_plaintext));

        let mut plain = json!({"input":"没有 reasoning 项"});
        let original = plain.clone();
        assert!(!fill_missing_reasoning_text(&mut plain));
        assert_eq!(plain, original);
    }

    #[test]
    fn plaintext_agent_payloads_become_visible_text_without_route_change() {
        let task = "只读冒烟任务（第 1 轮）。禁止派生任何子代理。";
        let mut body = json!({
            "input":[
                {"role":"user","content":"continue"},
                agent_message(json!([
                    {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
                    {"type":"encrypted_content","encrypted_content":task}
                ]))
            ]
        });
        let expected_content = json!([
            {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
            {"type":"input_text","text":task}
        ]);

        assert!(normalize_native_responses_context(&mut body, false));
        assert_eq!(body["input"][1]["content"], expected_content);
        assert_eq!(
            body["input"][0],
            json!({"role":"user","content":"continue"})
        );
        assert!(!normalize_native_responses_context(&mut body, false));

        // 线路切换会额外丢弃 reasoning，但已经改写的任务正文保持不变。
        assert!(!normalize_native_responses_context(&mut body, true));
        assert_eq!(body["input"][1]["content"], expected_content);
    }

    #[test]
    fn encrypted_agent_payloads_are_preserved_byte_for_byte() {
        let mut body = json!({
            "input":[agent_message(json!([
                {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
                {"type":"encrypted_content","encrypted_content":fernet_token(&[0x33; 16])}
            ]))]
        });
        let original = body.clone();

        assert!(!normalize_native_responses_context(&mut body, false));
        assert_eq!(body, original);
        assert!(
            ProtocolBridge::NativeResponses
                .convert_responses_body(&body)
                .unwrap()
                .is_none()
        );
        assert!(validate_cross_route_context(&body).is_ok());
    }

    #[test]
    fn adapted_agent_payloads_reject_encrypted_tasks_without_exposing_them() {
        let token = fernet_token(&[0x33; 16]);
        let part = json!({"type":"encrypted_content","encrypted_content":token});
        for content in [
            part.clone(),
            json!([
                {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
                part
            ]),
        ] {
            let item = agent_message(content);
            for input in [item.clone(), json!([item])] {
                let mut body = json!({"model":"model","input":input});
                let original = body.clone();
                assert!(!normalize_encrypted_agent_payloads(&mut body));
                for bridge in [
                    ProtocolBridge::ResponsesToChatCompletions,
                    ProtocolBridge::ResponsesToAnthropicMessages,
                ] {
                    let error = bridge
                        .convert_responses_body(&body)
                        .err()
                        .unwrap()
                        .to_string();
                    assert!(error.contains("context_not_portable"));
                    assert!(!error.contains(&token));
                }
                assert_eq!(body, original);
            }
        }
    }

    #[test]
    fn adapted_agent_payloads_preserve_recovered_plaintext() {
        let task = "检查测试结果并报告问题";
        let mut body = json!({"model":"model","input":[agent_message(json!([
            {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
            {"type":"encrypted_content","encrypted_content":task}
        ]))]});
        assert!(normalize_encrypted_agent_payloads(&mut body));
        for bridge in [
            ProtocolBridge::ResponsesToChatCompletions,
            ProtocolBridge::ResponsesToAnthropicMessages,
        ] {
            let converted = bridge.convert_responses_body(&body).unwrap().unwrap();
            assert!(converted.body["messages"].to_string().contains(task));
        }
    }

    #[test]
    fn adapted_agent_payloads_allow_unrelated_opaque_state() {
        let token = fernet_token(&[0x44; 16]);
        let body = json!({"model":"model","input":[
            {"type":"reasoning","encrypted_content":token,
             "content":[{"type":"encrypted_content","encrypted_content":token}]},
            {"role":"user","content":[
                {"type":"input_text","text":"continue"},
                {"type":"encrypted_content","encrypted_content":token}
            ]}
        ]});
        for bridge in [
            ProtocolBridge::ResponsesToChatCompletions,
            ProtocolBridge::ResponsesToAnthropicMessages,
        ] {
            assert!(bridge.convert_responses_body(&body).is_ok());
        }
    }

    #[test]
    fn empty_or_unreadable_agent_payloads_are_dropped() {
        for payload in [json!(""), json!("   "), json!(17), json!(null)] {
            let mut body = json!({"input":[agent_message(json!([
                {"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"},
                {"type":"encrypted_content","encrypted_content":payload}
            ]))]});
            assert!(normalize_native_responses_context(&mut body, false));
            assert_eq!(
                body["input"][0]["content"],
                json!([{"type":"input_text","text":"Message Type: NEW_TASK\nPayload:\n"}])
            );
        }
    }

    #[test]
    fn single_object_agent_content_keeps_the_rewritten_payload() {
        let mut plaintext = json!({
            "input":[agent_message(json!({
                "type":"encrypted_content","encrypted_content":"独立验证任务：只读。"
            }))]
        });
        assert!(normalize_native_responses_context(&mut plaintext, false));
        // 单个对象形态只替换内容本身，不额外改变内容结构。
        assert_eq!(
            plaintext["input"][0]["content"],
            json!({"type":"input_text","text":"独立验证任务：只读。"})
        );

        let mut empty = json!({
            "input":[agent_message(json!({"type":"encrypted_content","encrypted_content":""}))]
        });
        assert!(normalize_native_responses_context(&mut empty, false));
        assert!(empty["input"][0].get("content").is_none());
    }

    #[test]
    fn non_agent_items_keep_their_encrypted_state() {
        let mut body = json!({
            "input":[
                {"type":"reasoning","id":"rs_1","encrypted_content":"第三方线路的推理状态"},
                {"role":"user","content":[{"type":"encrypted_content","encrypted_content":"未识别的普通内容"}]}
            ]
        });
        let original = body.clone();

        assert!(!normalize_native_responses_context(&mut body, false));
        assert_eq!(body, original);
    }

    #[test]
    fn cross_route_context_rejects_nested_item_reference() {
        let body = json!({
            "input": [{
                "role": "assistant",
                "content": [{"type": "output_text", "annotations": [{"type": "item_reference"}]}]
            }]
        });
        assert!(validate_cross_route_context(&body).is_err());
    }

    #[test]
    fn compaction_guard_releases_on_failure_and_serializes_overlapping_sessions() {
        let bindings = Arc::new(Mutex::new(RouteBindings::default()));
        let guard =
            CompactionGuard::acquire(&bindings, vec!["thread:a".into(), "session:a".into()])
                .unwrap();
        assert!(CompactionGuard::acquire(&bindings, vec!["session:a".into()]).is_err());
        let other = CompactionGuard::acquire(&bindings, vec!["thread:b".into()]).unwrap();
        drop(guard);
        assert!(CompactionGuard::acquire(&bindings, vec!["thread:a".into()]).is_ok());
        drop(other);
        assert!(bindings.lock().unwrap().compacting.is_empty());
    }

    #[test]
    fn compaction_candidate_requires_complete_single_nonempty_snapshot() {
        let valid = json!({"status":"completed","output":[{"type":"compaction","encrypted_content":"opaque"}]});
        assert!(validate_compaction_result(&valid, true).is_ok());
        for invalid in [
            json!({"output":[]}),
            json!({"status":"incomplete","output":valid["output"]}),
            json!({"status":"completed","output":[{"type":"compaction","encrypted_content":""}]}),
            json!({"status":"completed","output":[valid["output"][0],valid["output"][0]]}),
            json!({"status":"completed","output":[{"type":"message","content":"summary"}]}),
        ] {
            assert!(validate_compaction_result(&invalid, true).is_err());
        }
        let mut accumulator = CompactionAccumulator::default();
        assert!(parse_sse_frames(b"data: [DONE]\n\n", &mut accumulator).is_err());
        assert!(!accumulator.finished());
        let failure = json!({"type":"response.failed","response":{"error":{"code":"context_length_exceeded"}}});
        assert!(
            accumulator
                .ingest_frame(&failure.to_string(), false)
                .unwrap_err()
                .is::<ContextLengthExceeded>()
        );
        let oversized = json!({"status":"completed","output":[{"type":"compaction","encrypted_content":"x".repeat(MAX_REQUEST_BYTES)}]});
        assert!(validate_compaction_result(&oversized, true).is_err());
    }

    #[test]
    fn compaction_sse_restores_done_items_only_after_successful_completion() {
        let item = json!({"type":"compaction","encrypted_content":"opaque"});
        let message = json!({"type":"message","role":"user","content":[]});
        for response in [json!({"id":"resp"}), json!({"id":"resp","output":[]})] {
            let mut accumulator = CompactionAccumulator::default();
            // Preserve output order even when done events arrive out of order.
            for (index, value) in [(1, &item), (0, &message)] {
                accumulator.ingest_frame(&json!({"type":"response.output_item.done","output_index":index,"item":value}).to_string(), false).unwrap();
            }
            assert!(!accumulator.finished());
            accumulator
                .ingest_frame(
                    &json!({"type":"response.completed","response":response}).to_string(),
                    false,
                )
                .unwrap();
            let result = accumulator.response.unwrap();
            assert_eq!(result["status"], "completed");
            assert_eq!(result["output"], json!([message, item]));
        }
        for (event_type, items, terminal) in [
            (
                "response.output_item.added",
                vec![item.clone()],
                json!({"type":"response.completed","response":{"output":[]}}),
            ),
            (
                "response.output_item.done",
                vec![item.clone(), item.clone()],
                json!({"type":"response.completed","response":{"output":[]}}),
            ),
            (
                "response.output_item.done",
                vec![json!({"type":"compaction","encrypted_content":""})],
                json!({"type":"response.completed","response":{"output":[]}}),
            ),
            (
                "response.output_item.done",
                vec![item.clone()],
                json!({"type":"response.incomplete","response":{}}),
            ),
            (
                "response.output_item.done",
                vec![item.clone()],
                json!({"type":"response.completed","response":null}),
            ),
            (
                "response.output_item.done",
                vec![item.clone()],
                json!({"type":"response.completed","response":{"output":[message]}}),
            ),
        ] {
            let mut accumulator = CompactionAccumulator::default();
            for (index, value) in items.iter().enumerate() {
                accumulator
                    .ingest_frame(
                        &json!({"type":event_type,"output_index":index,"item":value}).to_string(),
                        false,
                    )
                    .unwrap();
            }
            assert!(
                accumulator
                    .ingest_frame(&terminal.to_string(), false)
                    .is_err()
            );
            assert!(!accumulator.finished());
        }
        let mut accumulator = CompactionAccumulator::default();
        accumulator
            .ingest_frame(
                &json!({"type":"response.completed","response":{"output":[item]}}).to_string(),
                false,
            )
            .unwrap();
        assert!(accumulator.finished());
    }

    #[test]
    fn compaction_sse_rejects_conflicting_or_missing_output_items() {
        let item = json!({"type":"compaction","encrypted_content":"opaque"});
        let done = |index, item: Value| {
            json!({"type":"response.output_item.done","output_index":index,"item":item}).to_string()
        };
        let complete = json!({"type":"response.completed","response":{"output":[]}}).to_string();
        let mut duplicate = CompactionAccumulator::default();
        duplicate
            .ingest_frame(&done(0, item.clone()), false)
            .unwrap();
        duplicate
            .ingest_frame(&done(0, item.clone()), false)
            .unwrap();
        assert!(
            duplicate
                .ingest_frame(&done(0, json!({"type":"message"})), false)
                .is_err()
        );
        let mut missing = CompactionAccumulator::default();
        missing.ingest_frame(&done(1, item.clone()), false).unwrap();
        assert!(missing.ingest_frame(&complete, false).is_err());
        let mut unfinished = CompactionAccumulator::default();
        unfinished.ingest_frame(&done(0, item), false).unwrap();
        unfinished.ingest_frame(&json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message"}}).to_string(), false).unwrap();
        assert!(unfinished.ingest_frame(&complete, false).is_err());
    }

    #[test]
    fn compaction_tool_parts_do_not_restore_filtered_ciphertext() {
        for mixed in [
            json!([{"type":"encrypted_content","encrypted_content":"secret"},{"type":"unknown","value":1}]),
            json!([17,{"type":"encrypted_content","encrypted_content":"secret"}]),
            json!([{"type":"text","text":"visible"},{"custom":"json"}]),
            json!([{"type":"compaction","encrypted_content":"secret"}]),
        ] {
            assert!(responses_tool_output_content(&mixed).is_err());
        }
        assert!(
            responses_tool_output_content(&json!({"type":"custom_json","value":17}))
                .unwrap()
                .is_none()
        );
        assert_eq!(responses_tool_output_content(&json!([{"type":"text","text":"visible"},{"type":"reasoning","encrypted_content":"secret"}])).unwrap().unwrap().0, "visible");
    }

    #[test]
    fn compaction_context_errors_survive_json_and_stream_adapters() {
        for error in [
            json!({"error":{"code":"context_length_exceeded","message":"limit"}}),
            json!({"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 100 > 50"}}),
        ] {
            assert!(
                check_context_length_error(&error)
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                ChatSseAccumulator::new("model")
                    .ingest(&error)
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                AnthropicSseAccumulator::new("model")
                    .ingest(&error)
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                chat_completion_to_responses_body(error.clone(), "model")
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
            assert!(
                anthropic_message_to_responses_body(&error, "model")
                    .unwrap_err()
                    .is::<ContextLengthExceeded>()
            );
        }
        assert!(!is_context_length_error(
            &json!({"error":{"message":"image too large","code":"invalid_request_error"}})
        ));
    }
}

#[derive(Default)]
struct CompactionAccumulator {
    response: Option<Value>,
    output: BTreeMap<u64, Value>,
    output_count: u64,
}
impl SseFrameAccumulator for CompactionAccumulator {
    const PROTOCOL_LABEL: &'static str = "Responses compaction";
    const READ_OPERATION: &'static str = "读取远程压缩响应失败";
    // A compaction item carries the complete encrypted snapshot in one frame.
    // collect_sse_frames still enforces the cumulative response/memory budgets.
    const MAX_BUFFER_BYTES: usize = MAX_UPSTREAM_RESPONSE_BYTES;
    fn ingest_frame(&mut self, data: &str, trailing: bool) -> Result<()> {
        let event = sse_json_frame(data, Self::PROTOCOL_LABEL, trailing)?;
        check_context_length_error(&event)?;
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.added" | "response.output_item.done") => {
                let index = event["output_index"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("远程压缩输出项缺少有效的 output_index"))?;
                self.output_count = self.output_count.max(
                    index
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("远程压缩 output_index 超过上限"))?,
                );
                if event["type"] == "response.output_item.added" {
                    return Ok(());
                }
                if let Some(previous) = self.output.get(&index)
                    && previous != &event["item"]
                {
                    anyhow::bail!("远程压缩包含冲突的 output_index");
                }
                self.output.insert(index, event["item"].clone());
            }
            Some("response.completed") => {
                let mut response = event["response"].clone();
                if !response.is_object() {
                    anyhow::bail!("远程压缩完成事件缺少有效的 response");
                }
                // Completed events may omit the output already sent in item.done.
                // Never restore item.added: its encrypted content may be incomplete.
                if response.get("output").is_none()
                    || response["output"].as_array().is_some_and(Vec::is_empty)
                {
                    if self.output.keys().copied().ne(0..self.output_count) {
                        anyhow::bail!("远程压缩输出项不连续，无法恢复完整结果");
                    }
                    response["output"] =
                        Value::Array(std::mem::take(&mut self.output).into_values().collect());
                }
                // The event itself establishes completion, even if a provider
                // omits the redundant response.status field.
                if response.get("status").is_none() {
                    response["status"] = json!("completed");
                }
                validate_compaction_result(&response, true)?;
                self.response = Some(response);
            }
            Some("response.failed" | "response.incomplete" | "error") => {
                anyhow::bail!("远程压缩返回失败或未完成终态")
            }
            _ => {}
        }
        Ok(())
    }
    fn finished(&self) -> bool {
        self.response.is_some()
    }
}

pub(crate) async fn write_validated_compaction<D: ResponsesDownstream + ?Sized>(
    downstream: &mut D,
    response: reqwest::Response,
    v2: bool,
    stream: bool,
    route: &RouteTarget,
) -> Result<()> {
    let probe = downstream.request_log_probe().cloned();
    let result = await_upstream(downstream, async {
        let mut prepared =
            prepare_upstream_response(response, "读取远程压缩响应失败", probe.as_ref()).await?;
        if prepared.is_sse {
            if !v2 {
                anyhow::bail!("旧版压缩端点必须返回 JSON");
            }
            let mut accumulator = CompactionAccumulator::default();
            collect_sse_frames(&mut prepared, &mut accumulator, probe.as_ref()).await?;
            accumulator
                .response
                .ok_or_else(|| anyhow::anyhow!("远程压缩未返回完成事件"))
        } else {
            let bytes = read_bounded_prepared_upstream_body(
                &mut prepared,
                MAX_REQUEST_BYTES,
                "读取远程压缩响应失败",
                probe.as_ref(),
            )
            .await?;
            let value: Value = serde_json::from_slice(&bytes).context("远程压缩返回无效 JSON")?;
            validate_compaction_result(&value, v2)?;
            Ok(value)
        }
    })
    .await?;
    match result {
        Ok(value) if stream => write_responses_response_as_events(downstream, &value).await,
        Ok(value) => downstream.write_json(200, &value).await,
        Err(error) => {
            let timeout = is_upstream_timeout_error(&error);
            let (status, code) = if timeout {
                (504, "compaction_timeout")
            } else if error.is::<ContextLengthExceeded>() {
                (400, CONTEXT_LENGTH_EXCEEDED)
            } else {
                (502, "invalid_compaction_response")
            };
            // 上游断流、超时与本地校验失败此前都只打印最外层上下文，压缩失败
            // 因此难以定位；这里保留脱敏后的完整原因链。
            let detail = sanitize_upstream_error_text(&format!("{error:#}"), route, 512)
                .unwrap_or_else(|| "上游未返回可用的压缩结果".to_string());
            let message = if timeout {
                format!(
                    "远程压缩读取上游响应超时（{detail}），原始会话历史未被 Codey 修改，请稍后重试"
                )
            } else if status == 400 {
                error.to_string()
            } else {
                format!("远程压缩上游响应未能完成：{detail}")
            };
            downstream
                .write_error(status, code, message, Some(route))
                .await
        }
    }
}
