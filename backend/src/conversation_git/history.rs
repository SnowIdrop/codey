//! 从本地对话的已完成工具记录恢复文件范围，不将历史记录写成新的执行前基线。
use super::*;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::time::SystemTime;

const MAX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FAMILY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ACTORS: usize = 64;

#[derive(Clone)]
struct RecordedEdit {
    actor: String,
    start: Option<i64>,
    end: Option<i64>,
    ordinal: usize,
    edit: Edit,
    replacements: Option<usize>,
}

impl RecordedEdit {
    fn verified_apply(&self, value: Option<&Entry>) -> Result<Option<Entry>> {
        ensure!(
            self.start
                .zip(self.end)
                .is_some_and(|(start, end)| start <= end),
            "编辑时间记录不完整"
        );
        let next = apply_edit(value, &self.edit)?;
        let reverse = match &self.edit {
            Edit::Patch(hunks) => Edit::Patch(
                hunks
                    .iter()
                    .map(|(old, new)| (new.clone(), old.clone()))
                    .collect(),
            ),
            Edit::Replace(args) => {
                ensure!(
                    args["literal"] == true,
                    "历史正则替换缺少执行前内容，不能自动认领"
                );
                let pattern = args["pattern"].as_str().context("缺少替换模式")?;
                let replacement = args["replacement"].as_str().context("缺少替换内容")?;
                ensure!(
                    !pattern.is_empty()
                        && !replacement.is_empty()
                        && args["case_insensitive"] != true,
                    "历史替换无法唯一还原执行前内容"
                );
                let old_text = std::str::from_utf8(&value.context("缺少替换基线")?.bytes)?;
                let new_text = std::str::from_utf8(&next.as_ref().context("缺少替换结果")?.bytes)?;
                ensure!(
                    Some(old_text.matches(pattern).count()) == self.replacements
                        && Some(new_text.matches(replacement).count()) == self.replacements,
                    "替换回执与历史基线不一致"
                );
                let mut reversed = args.clone();
                reversed["pattern"] = json!(replacement);
                reversed["replacement"] = json!(pattern);
                Edit::Replace(reversed)
            }
            Edit::NativePatch(diff) => {
                let current = next.as_ref().context("缺少原生编辑结果")?;
                let original =
                    apply_native_patch(std::str::from_utf8(&current.bytes)?, diff, true)?;
                ensure!(
                    value.is_some_and(|before| before.bytes == original.as_bytes()),
                    "原生编辑无法还原基线"
                );
                return Ok(next);
            }
            Edit::Add(_) | Edit::Delete => bail!("历史新增或删除缺少执行前基线，不能自动认领"),
        };
        ensure!(
            apply_edit(next.as_ref(), &reverse)?.as_ref() == value,
            "历史编辑无法唯一还原基线"
        );
        Ok(next)
    }
}

#[derive(Clone)]
struct Spawn {
    binding: String,
    role: String,
}

#[derive(Clone, Default)]
struct Transcript {
    meta: Value,
    calls: Vec<(String, Value, Value, Option<i64>, Option<i64>, usize)>,
    spawns: Vec<Spawn>,
    native_changes: Vec<(Value, i64, i64, usize)>,
    incomplete: bool,
    bytes: u64,
}

// 原生完成记录带精确行号；严格按坐标校验，避免把相同上下文中的其他位置认作本次编辑。
pub(super) fn apply_native_patch(input: &str, diff: &str, reverse: bool) -> Result<String> {
    ensure!(
        !input.contains('\r') && (input.is_empty() || input.ends_with('\n')),
        "原生补丁的换行格式无法精确确认"
    );
    let header = regex::Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(?: .*)?$")?;
    let source: Vec<_> = input.lines().collect();
    let lines: Vec<_> = diff.lines().collect();
    let mut output = Vec::new();
    let mut cursor = 0usize;
    let mut i = 0;
    ensure!(!lines.is_empty(), "原生补丁缺少内容");
    while i < lines.len() {
        let captures = header.captures(lines[i]).context("原生补丁头无效")?;
        let old_start: usize = captures[1].parse()?;
        let old_count: usize = captures.get(2).map_or("1", |v| v.as_str()).parse()?;
        let new_start: usize = captures[3].parse()?;
        let new_count: usize = captures.get(4).map_or("1", |v| v.as_str()).parse()?;
        let mut old = Vec::new();
        let mut new = Vec::new();
        i += 1;
        while i < lines.len() && !lines[i].starts_with("@@") {
            let (prefix, text) = lines[i].split_at_checked(1).context("原生补丁含无效空行")?;
            match prefix {
                " " => {
                    old.push(text);
                    new.push(text);
                }
                "-" => old.push(text),
                "+" => new.push(text),
                _ => bail!("原生补丁含不支持的内容或换行标记"),
            }
            i += 1;
        }
        ensure!(
            old.len() == old_count && new.len() == new_count,
            "原生补丁行数不一致"
        );
        let position = |start: usize, count: usize| -> Result<usize> {
            if count == 0 {
                Ok(start)
            } else {
                start.checked_sub(1).context("原生补丁行号无效")
            }
        };
        let (from, to, removed, added) = if reverse {
            (
                position(new_start, new_count)?,
                position(old_start, old_count)?,
                new,
                old,
            )
        } else {
            (
                position(old_start, old_count)?,
                position(new_start, new_count)?,
                old,
                new,
            )
        };
        ensure!(
            from >= cursor && from <= source.len(),
            "原生补丁位置超出基线或相互重叠"
        );
        let end = from
            .checked_add(removed.len())
            .context("原生补丁范围过大")?;
        ensure!(
            source.get(from..end) == Some(removed.as_slice()),
            "原生补丁内容与当前基线不一致"
        );
        output.extend_from_slice(&source[cursor..from]);
        ensure!(output.len() == to, "原生补丁前后行号不一致");
        output.extend(added);
        cursor = end;
    }
    output.extend_from_slice(&source[cursor..]);
    Ok(if output.is_empty() {
        String::new()
    } else {
        format!("{}\n", output.join("\n"))
    })
}

type Signature = Vec<(PathBuf, u64, SystemTime, String)>;
static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, (Signature, Transcript)>>> = OnceLock::new();

fn timestamp(record: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(record["timestamp"].as_str()?)
        .ok()
        .map(|time| time.timestamp_millis())
}

fn output(value: &Value) -> Value {
    // 新版 rollout 将工具输出存成文本块数组，旧版使用字符串。
    if value.is_array() {
        json!({"content": value})
    } else {
        value.clone()
    }
}

// 识别先执行一次补丁，再执行命令或收取命令结果的固定包装；不执行 JavaScript。
fn batch_patch(name: &str, args: &Value) -> Option<(Value, usize)> {
    if !matches!(name, "exec" | "functions.exec") {
        return None;
    }
    let code = args
        .as_str()
        .or_else(|| args["code"].as_str())
        .or_else(|| args["input"].as_str())?
        .trim();
    let code = if code.starts_with("// @exec:") {
        code.split_once('\n')?.1.trim()
    } else {
        code
    };
    let literal = code.strip_prefix("text(await tools.apply_patch(")?;
    let mut stream = serde_json::Deserializer::from_str(literal).into_iter::<String>();
    let patch = stream.next()?.ok()?;
    let mut rest = literal[stream.byte_offset()..].strip_prefix("));")?.trim();
    let string = r#""(?:[^"\\\r\n]|\\.)*""#;
    let value = format!(r"(?:{string}|-?\d+(?:\.\d+)?|true|false|null)");
    let field = format!(r"[a-zA-Z_][a-zA-Z0-9_]*\s*:\s*{value}");
    let call = regex::Regex::new(&format!(r"^text\(await tools\.(?:exec_command|write_stdin)\(\{{\s*(?:{field}(?:\s*,\s*{field})*\s*,?)?\s*\}}\)\);" )).ok()?;
    let mut count = 0;
    while !rest.is_empty() {
        let matched = call.find(rest)?;
        rest = rest[matched.end()..].trim();
        count += 1;
        if count > 8 {
            return None;
        }
    }
    (count > 0).then_some((json!(patch), count))
}

fn batch_patch_receipt(response: &Value, trailing: usize) -> Option<Value> {
    let content = response["content"].as_array()?;
    if content.len() != trailing + 2 {
        return None;
    }
    for block in &content[2..] {
        let text = block["text"].as_str()?;
        if !serde_json::from_str::<Value>(text).ok()?.is_object() {
            return None;
        }
    }
    Some(json!({"content": &content[..2]}))
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn accepts_only_fixed_sequential_patch_wrapper_and_ordered_receipts() {
        let patch = "*** Begin Patch\n*** Update File: owned.txt\n@@\n-old\n+new\n*** End Patch";
        let code = format!(
            "text(await tools.apply_patch({}));\ntext(await tools.exec_command({{cmd:\"node --test test.mjs\",workdir:\"/tmp/repo\",max_output_tokens:2000,yield_time_ms:1000}}));\ntext(await tools.write_stdin({{session_id:71171,chars:\"\",max_output_tokens:2200,yield_time_ms:1000}}));",
            serde_json::to_string(patch).unwrap()
        );
        let found = batch_patch("exec", &json!(code.clone())).expect("明确包装可识别");
        assert_eq!(found, (json!(patch), 2));
        assert!(batch_patch("exec", &json!(format!("if (true) {{ {code} }}"))).is_none());
        assert!(batch_patch("exec", &json!(format!("{code}\ntext({{}});"))).is_none());
        let response = json!({"content":[{"type":"text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},{"type":"text","text":"{}"},{"type":"text","text":"{\"exit_code\":0}"},{"type":"text","text":"{\"exit_code\":0}"}]});
        assert!(batch_patch_receipt(&response, 2).is_some());
        assert!(batch_patch_receipt(&response, 1).is_none());
    }
}

fn segment_paths(home: &Path, anchor: &Path, id: &str) -> Result<Vec<PathBuf>> {
    // 仅枚举文件名，不读取其他对话的正文。文件名匹配后仍须校验内部身份。
    let pattern = regex::Regex::new(&format!(
        r"^rollout-(?:\d{{4}}-\d{{2}}-\d{{2}}T\d{{2}}-\d{{2}}-\d{{2}}-)?{}(?:_[0-9a-f-]{{36}})?\.jsonl$",
        regex::escape(id)
    ))?;
    let mut paths = BTreeSet::from([anchor.to_path_buf()]);
    let mut queue = vec![(home.join("sessions"), 0)];
    let mut entries = 0;
    while let Some((directory, depth)) = queue.pop() {
        if !directory.is_dir() {
            continue;
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries += 1;
            ensure!(entries <= 100_000, "本地对话目录过大，无法完整核对历史分段");
            let kind = entry.file_type()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if kind.is_dir() && depth < 3 && name.chars().all(|c| c.is_ascii_digit()) {
                queue.push((entry.path(), depth + 1));
            } else if kind.is_file() && pattern.is_match(&name) {
                paths.insert(crate::session_transfer::checked_rollout_path(
                    home,
                    &entry.path(),
                )?);
                ensure!(paths.len() <= 32, "同一对话的历史分段过多，无法完整校验");
            }
        }
    }
    Ok(paths.into_iter().collect())
}

fn read_transcript(home: &Path, raw: &Path) -> Result<Transcript> {
    let path = crate::session_transfer::checked_rollout_path(home, raw)?;
    ensure!(
        !path.components().any(|part| part.as_os_str() == "imported"),
        "导入对话的历史编辑缺少本机执行基线，无法自动认领"
    );
    let anchor_meta = tracking::metadata(home, &path)?;
    let id = anchor_meta["id"].as_str().context("对话分段缺少身份")?;
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "对话分段身份无效");
    let mut segments = Vec::new();
    let mut signature = Vec::new();
    let mut total_bytes = 0;
    let paths = segment_paths(home, &path, id)?;
    let segmented = paths.len() > 1;
    for segment in paths {
        let metadata = fs::metadata(&segment)?;
        ensure!(
            metadata.len() <= MAX_TRANSCRIPT_BYTES,
            "对话历史过大，无法完整校验编辑范围"
        );
        total_bytes += metadata.len();
        ensure!(
            total_bytes <= MAX_FAMILY_BYTES,
            "对话历史分段过大，无法完整校验"
        );
        let meta = tracking::metadata(home, &segment)?;
        ensure!(
            [
                "id",
                "cwd",
                "source",
                "forked_from_id",
                "agent_role",
                "agent_path",
                "subagent_history_start_ordinal",
                "creator_user_id",
                "creator_account_id"
            ]
            .iter()
            .all(|key| meta[*key] == anchor_meta[*key]),
            "同一对话历史分段的身份、工作区或继承边界不一致"
        );
        let created = match meta["timestamp"].as_str() {
            Some(value) => chrono::DateTime::parse_from_rfc3339(value)?.timestamp_millis(),
            None if !segmented => 0,
            None => bail!("对话分段缺少创建时间"),
        };
        signature.push((
            segment.clone(),
            metadata.len(),
            metadata.modified()?,
            digest(&read_bounded(&segment, MAX_TRANSCRIPT_BYTES)?),
        ));
        segments.push((created, segment));
    }
    segments.sort();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some((old, transcript)) = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("历史缓存不可用"))?
        .get(&path)
        && *old == signature
    {
        return Ok(transcript.clone());
    }
    let mut result = Transcript {
        bytes: total_bytes,
        ..Default::default()
    };
    let mut calls = HashMap::<String, (String, Value, Option<i64>, usize)>::new();
    let mut seen = BTreeSet::new();
    let mut native_seen = BTreeSet::new();
    let mut captured = 0usize;
    let mut records = Vec::new();
    let segmented = segments.len() > 1;
    let mut boundaries = Vec::new();
    for (_, segment) in &segments {
        let mut lines = BufReader::new(File::open(segment)?).lines();
        let meta: Value = serde_json::from_str(&lines.next().context("对话分段为空")??)?;
        if segmented {
            boundaries.push(
                meta["ordinal"]
                    .as_u64()
                    .context("对话历史分段缺少连续序号")?,
            );
        }
    }
    ensure!(
        boundaries.windows(2).all(|pair| pair[0] < pair[1]),
        "对话历史分段顺序不明确"
    );
    let mut previous = None;
    for (index, (_, segment)) in segments.iter().enumerate() {
        for (line_index, line) in BufReader::new(File::open(segment)?).lines().enumerate() {
            let line = line?;
            ensure!(line.len() <= 8 * 1024 * 1024, "单条对话记录过大");
            let record: Value =
                serde_json::from_str(&line).context("对话编辑记录尚未完整保存，请稍后重试")?;
            if line_index == 0 {
                ensure!(record["type"] == "session_meta", "对话分段缺少可信身份记录");
                if index == 0 {
                    records.push(record);
                }
                continue;
            }
            if segmented {
                let position = record["ordinal"]
                    .as_u64()
                    .context("对话历史分段记录缺少连续序号")?;
                // 恢复点之后属于新分段；旧进程的尾部记录不属于恢复后的编辑链。
                if boundaries
                    .get(index + 1)
                    .is_some_and(|end| position >= *end)
                {
                    continue;
                }
                ensure!(
                    position >= boundaries[index] && previous.is_none_or(|old| old < position),
                    "对话历史分段的记录顺序不明确"
                );
                previous = Some(position);
            }
            records.push(record);
        }
    }
    let mut ordinal = 0usize;
    for record in records {
        let payload = &record["payload"];
        if ordinal == 0 {
            ensure!(record["type"] == "session_meta", "对话缺少可信身份记录");
            result.meta = payload.clone();
            ensure!(
                result.meta.get("forked_from_id").is_none_or(Value::is_null)
                    || result.meta["subagent_history_start_ordinal"]
                        .as_u64()
                        .is_some(),
                "分叉对话缺少历史边界，无法将继承的编辑归入当前对话"
            );
        }
        ordinal += 1;
        let native_file_change = record["type"] == "event_msg"
            && payload["type"] == "item_completed"
            && payload["item"]["type"] == "FileChange";
        if (record["type"] == "response_item" || native_file_change)
            && let Some(start) = result.meta["subagent_history_start_ordinal"].as_u64()
        {
            let position = record["ordinal"]
                .as_u64()
                .context("子代理历史缺少继承边界序号")?;
            if position < start {
                continue;
            }
        }
        if native_file_change {
            let item = &payload["item"];
            if item["status"] != "completed" {
                continue;
            }
            ensure!(
                payload["thread_id"] == result.meta["id"],
                "原生编辑记录的对话身份不匹配"
            );
            let id = item["id"]
                .as_str()
                .filter(|id| !id.is_empty())
                .context("原生编辑记录缺少身份")?;
            ensure!(native_seen.insert(id.to_string()), "原生编辑记录身份重复");
            let start = payload["started_at_ms"]
                .as_i64()
                .context("原生编辑记录缺少开始时间")?;
            let end = payload["completed_at_ms"]
                .as_i64()
                .context("原生编辑记录缺少结束时间")?;
            ensure!(start <= end, "原生编辑时间无效");
            captured += item.to_string().len();
            ensure!(
                captured <= 16 * 1024 * 1024 && result.native_changes.len() < 4096,
                "对话编辑记录过多"
            );
            result
                .native_changes
                .push((item.clone(), start, end, ordinal));
            continue;
        }
        if record["type"] != "response_item" {
            continue;
        }
        let kind = payload["type"].as_str().unwrap_or_default();
        let Some(id) = payload["call_id"].as_str() else {
            continue;
        };
        if matches!(kind, "function_call" | "custom_tool_call") {
            let raw_name = payload["name"].as_str().unwrap_or_default();
            let name = match payload["namespace"].as_str() {
                Some(namespace) => format!("{namespace}.{raw_name}"),
                None => raw_name.into(),
            };
            let args = if kind == "custom_tool_call" {
                payload["input"].clone()
            } else if let Some(text) = payload["arguments"].as_str() {
                serde_json::from_str(text).context("历史工具参数无效")?
            } else {
                payload["arguments"].clone()
            };
            if name == "agents.spawn_agent"
                || tracking::normalized_input(&name, &args)
                    .ok()
                    .flatten()
                    .is_some()
                || batch_patch(&name, &args).is_some()
            {
                ensure!(
                    seen.insert(id.to_string()),
                    "对话中出现重复工具调用身份，无法确认编辑归属"
                );
                captured += args.to_string().len();
                ensure!(
                    captured <= 16 * 1024 * 1024 && calls.len() < 4096,
                    "对话编辑记录过多"
                );
                calls.insert(id.into(), (name, args, timestamp(&record), ordinal));
            }
        } else if matches!(kind, "function_call_output" | "custom_tool_call_output")
            && let Some((name, args, start, order)) = calls.remove(id)
        {
            let response = output(&payload["output"]);
            if name == "agents.spawn_agent" {
                let value = if let Some(text) = response.as_str() {
                    serde_json::from_str::<Value>(text).ok()
                } else if response.get("content").is_none() {
                    Some(response.clone())
                } else {
                    tracking::output_text(&response)
                        .and_then(|text| serde_json::from_str(&text).ok())
                };
                if let Some(value) = value
                    && value["isError"] != true
                    && value.get("error").is_none_or(Value::is_null)
                    && !matches!(value["status"].as_str(), Some("failed" | "error"))
                    && let Some(binding) = value["agent_id"]
                        .as_str()
                        .or_else(|| value["task_name"].as_str())
                {
                    result.spawns.push(Spawn {
                        binding: binding.into(),
                        role: args["agent_type"].as_str().unwrap_or("default").into(),
                    });
                }
            } else {
                result
                    .calls
                    .push((name, args, response, start, timestamp(&record), order));
            }
        }
    }
    result.incomplete = calls
        .values()
        .any(|(name, _, _, _)| name != "agents.spawn_agent");
    for (segment, length, modified, hash) in &signature {
        let current = fs::metadata(segment)?;
        ensure!(
            current.len() == *length
                && current.modified()? == *modified
                && digest(&read_bounded(segment, MAX_TRANSCRIPT_BYTES)?) == *hash,
            "读取期间对话记录发生变化，请重试"
        );
    }
    let mut cache = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("历史缓存不可用"))?;
    if cache.len() >= 32 {
        cache.clear();
    }
    cache.insert(path, (signature, result.clone()));
    Ok(result)
}

fn child_paths(home: &Path, parent: &str, workspace: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut result = BTreeMap::new();
    for database in codey_runtime_core::codex_sqlite::codex_session_db_paths_from_home(home) {
        if !database.exists() {
            continue;
        }
        let connection = rusqlite::Connection::open_with_flags(
            database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let columns = crate::sqlite_util::table_columns(&connection, "threads")?;
        // 发现列表也可能包含日志库等辅助数据库，与主会话查询保持一致。
        if columns.is_empty() {
            continue;
        }
        let has_source = columns.iter().any(|column| column == "source");
        if !has_source {
            let count: usize =
                connection.query_row("SELECT COUNT(*) FROM threads", [], |row| row.get(0))?;
            ensure!(
                count <= MAX_ACTORS,
                "早期会话库缺少子代理父关系且记录过多，无法完整确认历史范围"
            );
        }
        let query = if has_source {
            "SELECT id, rollout_path, cwd FROM threads WHERE json_valid(source) AND json_extract(source, '$.subagent.thread_spawn.parent_thread_id')=?1 LIMIT 65"
        } else {
            // 兼容早期会话库；只读取身份行，不能按文件名猜测子代理。
            "SELECT id, rollout_path, cwd FROM threads WHERE ?1 IS NOT NULL LIMIT 65"
        };
        let mut statement = connection.prepare(query)?;
        for row in statement.query_map([parent], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (id, path, cwd) = row?;
            if Path::new(&cwd).canonicalize().ok().as_deref() != Some(workspace) {
                continue;
            }
            let path = home.join(path);
            let meta = tracking::metadata(home, &path)?;
            if meta["parent_thread_id"] == parent {
                result.insert(id, path);
            }
        }
    }
    ensure!(
        result.len() < MAX_ACTORS,
        "历史子代理过多，无法完整校验范围"
    );
    Ok(result.into_iter().collect())
}

#[derive(Default)]
pub(super) struct History {
    files: BTreeMap<String, Vec<RecordedEdit>>,
    incomplete: bool,
}

impl History {
    pub(super) fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
    }

    pub(super) fn changes_in_head(&self, path: &str, head: Option<&Entry>) -> bool {
        let Some(head) = head else {
            return false;
        };
        let Some(records) = self.files.get(path) else {
            return false;
        };
        if records.is_empty()
            || !records.iter().all(|record| {
                matches!(
                    record.edit,
                    Edit::NativePatch(_) | Edit::Patch(_) | Edit::Replace(_)
                )
            })
        {
            return false;
        }
        let mut records = records.clone();
        records.sort_by_key(|record| (record.start, record.ordinal));
        let mut value = head.clone();
        for record in records.iter().rev() {
            let previous = (|| -> Result<Entry> {
                let edit = match &record.edit {
                    Edit::NativePatch(diff) => {
                        let mut previous = value.clone();
                        previous.bytes =
                            apply_native_patch(std::str::from_utf8(&value.bytes)?, diff, true)?
                                .into_bytes();
                        return Ok(previous);
                    }
                    Edit::Patch(hunks) => Edit::Patch(
                        hunks
                            .iter()
                            .map(|(old, new)| (new.clone(), old.clone()))
                            .collect(),
                    ),
                    Edit::Replace(args) => {
                        let mut reverse = args.clone();
                        reverse["pattern"] = args["replacement"].clone();
                        reverse["replacement"] = args["pattern"].clone();
                        Edit::Replace(reverse)
                    }
                    _ => bail!("编辑缺少可逆基线"),
                };
                apply_edit(Some(&value), &edit)?.context("编辑缺少可逆基线")
            })();
            let Ok(previous) = previous else {
                return false;
            };
            if record
                .verified_apply(Some(&previous))
                .ok()
                .flatten()
                .as_ref()
                != Some(&value)
            {
                return false;
            }
            value = previous;
        }
        true
    }

    pub(super) fn validate_complete(&self) -> Result<()> {
        ensure!(
            !self.incomplete,
            "当前对话历史仍有编辑未完成或缺少工具回执，已停止提交"
        );
        Ok(())
    }

    pub(super) fn replay(
        &self,
        path: &str,
        before: Option<&Entry>,
        after: Option<&Entry>,
    ) -> Result<()> {
        let records = self.files.get(path).context("文件没有可验证的历史编辑")?;
        let mut ordered = records.clone();
        ordered.sort_by_key(|record| (record.start, record.ordinal));
        for pair in ordered.windows(2) {
            if pair[0].actor != pair[1].actor {
                ensure!(
                    pair[0]
                        .end
                        .zip(pair[1].start)
                        .is_some_and(|(end, start)| end < start),
                    "{path} 的主代理与子代理编辑顺序不明确，已停止提交"
                );
            }
        }
        // 允许已提交的早期编辑留在历史中，只接受能唯一对应当前 HEAD 的后续编辑链。
        ensure!(ordered.len() <= 256, "{path} 的历史编辑过多，无法可靠重放");
        let mut matches = 0;
        for start in 0..ordered.len() {
            let mut value = before.cloned();
            let mut valid = true;
            for record in &ordered[start..] {
                let result = record.verified_apply(value.as_ref());
                match result {
                    Ok(next) => value = next,
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            }
            if valid && value.as_ref() == after {
                matches += 1;
            }
        }
        ensure!(
            matches == 1,
            "{path} 的历史编辑无法与当前 HEAD 和文件内容唯一对应，可能缺少执行前基线、含其他改动或记录不完整，已停止提交"
        );
        Ok(())
    }

    pub(super) fn isolate_changes(
        &self,
        root: &Path,
        path: &str,
        before: Option<&Entry>,
        disk: Option<&Entry>,
    ) -> Result<Entry> {
        let before = before.context("共享文件缺少已提交基线，无法自动分离")?;
        let disk = disk.context("共享文件已删除，无法自动分离")?;
        ensure!(
            before.mode == disk.mode,
            "{path} 的文件权限也已变化，无法自动分离"
        );
        let mut records = self
            .files
            .get(path)
            .context("共享文件缺少本对话的实际补丁记录")?
            .clone();
        records.sort_by_key(|record| (record.start, record.ordinal));
        ensure!(
            records.len() <= 256
                && records.iter().all(|record| matches!(
                    record.edit,
                    Edit::NativePatch(_) | Edit::Patch(_) | Edit::Replace(_)
                )),
            "{path} 缺少可验证的修改记录，无法自动分离"
        );
        for pair in records.windows(2) {
            ensure!(
                pair[0].actor == pair[1].actor
                    || pair[0]
                        .end
                        .zip(pair[1].start)
                        .is_some_and(|(end, start)| end < start),
                "{path} 的主代理与子代理编辑顺序不明确，无法自动分离"
            );
        }
        let mut candidates = Vec::new();
        for start in 0..records.len() {
            let candidate = (|| -> Result<Entry> {
                let mut value = before.clone();
                for record in &records[start..] {
                    value = record
                        .verified_apply(Some(&value))?
                        .context("共享文件修改不能删除文件")?;
                }
                ensure!(&value != before, "本对话补丁已提交或没有剩余改动");
                Ok(value)
            })();
            if let Ok(candidate) = candidate {
                candidates.push(candidate);
            }
        }
        ensure!(
            candidates.len() == 1,
            "{path} 的本对话补丁不能唯一对应当前 HEAD，无法自动分离"
        );
        let candidate = candidates.pop().unwrap();
        for entry in [before, &candidate, disk] {
            ensure!(
                !entry.bytes.contains(&0) && std::str::from_utf8(&entry.bytes).is_ok(),
                "共享文件仅支持 UTF-8 文本的自动分离"
            );
        }
        // 所有输入在临时目录中，-p 仅输出结果，不修改工作区、索引或用户文件。
        let directory = tempfile::tempdir()?;
        let own_path = directory.path().join("conversation");
        let base_path = directory.path().join("base");
        let disk_path = directory.path().join("disk");
        fs::write(&own_path, &candidate.bytes)?;
        fs::write(&base_path, &before.bytes)?;
        fs::write(&disk_path, &disk.bytes)?;
        let merged = git(
            root,
            &[
                "merge-file",
                "-p",
                "--diff3",
                "--",
                own_path.to_str().context("临时路径编码不受支持")?,
                base_path.to_str().context("临时路径编码不受支持")?,
                disk_path.to_str().context("临时路径编码不受支持")?,
            ],
            None,
            None,
        )
        .with_context(|| format!("{path} 的本对话改动与其他改动冲突，无法自动分离"))?;
        ensure!(
            merged == disk.bytes,
            "{path} 的本对话补丁已被修改或撤销，无法自动分离"
        );
        Ok(candidate)
    }
}

pub(super) fn recover(
    home: &Path,
    session: &str,
    root: &Path,
    workspace: &Path,
) -> Result<History> {
    let (thread, _) =
        crate::session_transfer::find_thread(home, session)?.context("未找到本地对话")?;
    let path = home.join(
        thread["rollout_path"]
            .as_str()
            .context("对话缺少编辑记录")?,
    );
    let mut queue = vec![(session.to_string(), path)];
    let mut visited = BTreeSet::new();
    let mut bytes = 0;
    let mut history = History::default();
    while let Some((actor, path)) = queue.pop() {
        ensure!(
            uuid::Uuid::parse_str(&actor).is_ok(),
            "历史对话或子代理身份格式无效"
        );
        ensure!(
            visited.insert(actor.clone()) && visited.len() <= MAX_ACTORS,
            "历史子代理身份重复或数量过多"
        );
        let transcript = read_transcript(home, &path)?;
        bytes += transcript.bytes;
        ensure!(
            bytes <= MAX_FAMILY_BYTES,
            "对话及子代理历史过大，无法完整校验"
        );
        history.incomplete |= transcript.incomplete;
        ensure!(
            transcript.meta["id"] == actor
                && transcript.meta["cwd"]
                    .as_str()
                    .and_then(|p| Path::new(p).canonicalize().ok())
                    .as_deref()
                    == Some(workspace),
            "历史对话身份或工作区不一致"
        );
        let mut native_scopes = Vec::new();
        for (item, start, end, ordinal) in transcript.native_changes {
            let reported = tracking::reported_files(&item["stdout"], true, root, workspace)?;
            let changes = item["changes"]
                .as_object()
                .context("原生编辑记录缺少文件范围")?;
            let mut edits = BTreeMap::new();
            for (raw, change) in changes {
                let file = relative_path(root, workspace, raw)?;
                ensure!(
                    change.get("move_path").is_none_or(Value::is_null),
                    "{file} 的历史移动缺少可验证基线，已停止提交"
                );
                let edit = match change["type"].as_str() {
                    Some("update") => Edit::NativePatch(
                        change["unified_diff"]
                            .as_str()
                            .context("原生编辑记录缺少实际补丁")?
                            .into(),
                    ),
                    Some("add") => Edit::Add(
                        change["content"]
                            .as_str()
                            .context("原生新增记录缺少内容")?
                            .into(),
                    ),
                    Some("delete") => Edit::Delete,
                    _ => bail!("原生编辑类型不受支持"),
                };
                ensure!(edits.insert(file, edit).is_none(), "原生编辑记录含重复路径");
            }
            ensure!(
                !reported.is_empty() && reported == edits.keys().cloned().collect(),
                "原生编辑记录与完成回执的文件范围不一致"
            );
            native_scopes.push((start, end, reported));
            for (file, edit) in edits {
                history.files.entry(file).or_default().push(RecordedEdit {
                    actor: actor.clone(),
                    start: Some(start),
                    end: Some(end),
                    ordinal,
                    edit,
                    replacements: None,
                });
            }
        }
        for (name, args, response, start, end, ordinal) in transcript.calls {
            let batch = batch_patch(&name, &args);
            let Some((patch, args)) = tracking::normalized_input(&name, &args)
                .ok()
                .flatten()
                .or_else(|| batch.as_ref().map(|(patch, _)| (true, patch.clone())))
            else {
                continue;
            };
            let response = if let Some((_, trailing)) = batch {
                let Some(receipt) = batch_patch_receipt(&response, trailing) else {
                    continue;
                };
                receipt
            } else {
                response
            };
            let response = if patch {
                let Some(text) = tracking::output_text(&response) else {
                    continue;
                };
                // rollout 文本块可能带执行耗时等元信息；只有一次已知补丁调用的成功回执可用。
                let marker = "Success. Updated the following files:\n";
                if text.matches(marker).count() == 1 {
                    json!(text[text.find(marker).expect("one marker")..].trim())
                } else {
                    response
                }
            } else {
                response
            };
            let reported_result = tracking::completed_files(
                &response,
                patch,
                root,
                workspace,
                matches!(name.as_str(), "functions.exec" | "exec"),
                &args,
            );
            let Ok(reported) = reported_result else {
                continue;
            };
            let edits = if patch {
                tracking::operations(root, workspace, true, &args)?
            } else {
                let raw = args["path"].as_str().context("历史替换缺少路径")?;
                let target = workspace.join(raw);
                // 目录替换只认领完成回执中的实际文件，不重新扫描当前目录。
                reported
                    .iter()
                    .map(|file| {
                        let absolute = root.join(file);
                        ensure!(
                            absolute == target || absolute.starts_with(&target),
                            "历史替换回执包含目标外文件"
                        );
                        Ok((file.clone(), Edit::Replace(args.clone())))
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            let expected: BTreeSet<_> = edits.iter().map(|(path, _)| path.clone()).collect();
            ensure!(
                expected == reported,
                "历史工具输入与完成回执的文件范围不一致"
            );
            // 同一次补丁可能同时留下工具回执和原生事件，只重放原生实际改动一次。
            if patch && let Some((start, end)) = start.zip(end) {
                let contained: Vec<_> = native_scopes
                    .iter()
                    .filter(|(from, to, _)| start <= *from && *to <= end)
                    .collect();
                if !contained.is_empty() {
                    ensure!(
                        contained.len() == 1 && contained[0].2 == reported,
                        "补丁调用与原生完成记录不能唯一对应"
                    );
                    continue;
                }
            }
            for (file, edit) in edits {
                let replacements = if patch {
                    None
                } else {
                    let text = tracking::output_text(&response).context("替换回执不完整")?;
                    let count = text.lines().find_map(|line| {
                        let (raw, count) = line.rsplit_once(": ")?;
                        (relative_path(root, workspace, raw).ok().as_deref() == Some(&file))
                            .then(|| count.split_whitespace().next()?.parse().ok())
                            .flatten()
                    });
                    count
                };
                history.files.entry(file).or_default().push(RecordedEdit {
                    actor: actor.clone(),
                    start,
                    end,
                    ordinal,
                    edit,
                    replacements,
                });
            }
        }
        if !transcript.spawns.is_empty() {
            for (id, child_path) in child_paths(home, &actor, workspace)? {
                let meta = tracking::metadata(home, &child_path)?;
                if meta["cwd"]
                    .as_str()
                    .and_then(|p| Path::new(p).canonicalize().ok())
                    .as_deref()
                    != Some(workspace)
                {
                    continue;
                }
                let nested = &meta["source"]["subagent"]["thread_spawn"];
                if nested
                    .get("parent_thread_id")
                    .is_some_and(|p| p != &json!(actor))
                {
                    continue;
                }
                let Some(role) = meta["agent_role"].as_str() else {
                    continue;
                };
                if nested.get("agent_role").is_some_and(|r| r != &json!(role)) {
                    continue;
                }
                let Some(agent_path) = meta["agent_path"].as_str() else {
                    continue;
                };
                if nested
                    .get("agent_path")
                    .is_some_and(|p| p != &json!(agent_path))
                {
                    continue;
                }
                let bindings = transcript
                    .spawns
                    .iter()
                    .filter(|spawn| {
                        (spawn.binding == id || spawn.binding == agent_path) && spawn.role == role
                    })
                    .count();
                if bindings == 1 {
                    queue.push((id, child_path));
                }
            }
        }
    }
    ensure!(
        history.files.len() <= MAX_FILES,
        "历史对话涉及文件过多，请人工拆分提交"
    );
    Ok(history)
}
