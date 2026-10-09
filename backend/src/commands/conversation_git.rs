use std::sync::Arc;

use serde_json::{Value, json};

use super::{AppState, prompt_optimization, string_argument};
use crate::{codex_config::codex_home, config::PromptOptimizationConfig, conversation_git};

pub(super) async fn invoke(
    state: &Arc<AppState>,
    command: &str,
    args: &Value,
) -> Result<Value, String> {
    let config = state.config.read().await.conversation_git.clone();
    if !config.enabled {
        return if command == "conversation_git_status" {
            Ok(json!({"visible": false, "reason": "对话 Git 提交增强已关闭"}))
        } else {
            Err("对话 Git 提交增强已关闭".into())
        };
    }
    config.validate()?;
    let session = string_argument(args, "sessionId")?;
    if command == "conversation_git_status" {
        let result =
            tokio::task::spawn_blocking(move || conversation_git::status(codex_home(), &session))
                .await
                .map_err(|error| error.to_string())?;
        return Ok(match result {
            Ok(value) => value,
            Err(error) => json!({"visible": false, "reason": format!("{error:#}")}),
        });
    }
    if command == "conversation_git_execute" {
        let token = string_argument(args, "token")?;
        let push = args.get("push").and_then(Value::as_bool).unwrap_or(true);
        return tokio::task::spawn_blocking(move || {
            conversation_git::execute(codex_home(), &session, &token, &config.model, push)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"));
    }
    let snapshot_session = session.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        conversation_git::snapshot(codex_home(), &snapshot_session)
    })
    .await
    .map_err(|error| error.to_string())?;
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => return Err(format!("{error:#}")),
    };
    let model_config = PromptOptimizationConfig {
        enabled: true,
        mode: crate::config::PROMPT_OPTIMIZATION_MODE_CODEY_ROUTE.into(),
        model: config.model.clone(),
        instruction: conversation_git::MESSAGE_INSTRUCTION.into(),
        ..PromptOptimizationConfig::default()
    };
    let request = prompt_optimization::resolve_request_config(state, &model_config)
        .await
        .map_err(|error| {
            format!(
                "提交分析模型不可用：{}",
                error
                    .replace("提示词优化", "Git 提交分析")
                    .replace("，或改用手动配置", "")
            )
        })?;
    let message = crate::prompt_optimization::optimize_prompt_resolved(
        prompt_optimization::optimizer_client(true)?,
        &request,
        &snapshot.diff,
    )
    .await
    .map_err(|error| format!("生成提交说明失败：{error}"))?;
    let message =
        conversation_git::validate_message(&message).map_err(|error| error.to_string())?;
    let current =
        tokio::task::spawn_blocking(move || conversation_git::snapshot(codex_home(), &session))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("{error:#}"))?;
    if current != snapshot {
        return Err("生成说明期间文件或 Git 状态发生变化，请重新预览".into());
    }
    if state.config.read().await.conversation_git != config {
        return Err("提交模型设置已变化，请重新预览".into());
    }
    conversation_git::save_preview(snapshot, message, config.model)
        .map_err(|error| error.to_string())
}
