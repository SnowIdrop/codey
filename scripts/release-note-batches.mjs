const DEFAULT_MAX_BYTES = 48 * 1024;

function serializedBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

function splitDiffLines(diff) {
  if (diff.length === 0) return [""];
  const lines = [];
  let start = 0;
  while (start < diff.length) {
    const end = diff.indexOf("\n", start);
    if (end < 0) {
      lines.push(diff.slice(start));
      break;
    }
    lines.push(diff.slice(start, end + 1));
    start = end + 1;
  }
  return lines;
}

function validateMaxBytes(maxBytes) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw new Error("单批大小必须是正整数");
}

export function chunkPatches(patches, maxBytes = DEFAULT_MAX_BYTES, measure = serializedBytes) {
  validateMaxBytes(maxBytes);
  if (!Array.isArray(patches)) throw new TypeError("差异补丁必须是数组");
  if (typeof measure !== "function") throw new TypeError("差异大小计算方式无效");

  const chunks = [];
  for (const patch of patches) {
    if (!patch || typeof patch.file !== "string" || typeof patch.diff !== "string") throw new TypeError("差异补丁必须包含文件路径和文本差异");
    let fragment = "";
    for (const line of splitDiffLines(patch.diff)) {
      const candidate = fragment + line;
      const entry = { file: patch.file, diff: candidate };
      if (measure([entry]) > maxBytes) {
        if (!fragment) throw new Error(`单行差异与文件路径序列化后超过单批大小限制：${patch.file}`);
        chunks.push({ file: patch.file, diff: fragment });
        fragment = line;
        if (measure([{ file: patch.file, diff: fragment }]) > maxBytes) throw new Error(`单行差异与文件路径序列化后超过单批大小限制：${patch.file}`);
      } else {
        fragment = candidate;
      }
    }
    chunks.push({ file: patch.file, diff: fragment });
  }

  const batches = [];
  let batch = [];
  for (const chunk of chunks) {
    const candidate = [...batch, chunk];
    if (measure(candidate) > maxBytes) {
      if (batch.length) batches.push(batch);
      batch = [chunk];
    } else {
      batch = candidate;
    }
  }
  if (batch.length) batches.push(batch);
  return batches;
}

function excerptForLine(lines, index) {
  let start = index;
  let end = index + 1;
  let excerpt = lines[index].replace(/\n$/, "");
  while (excerpt.length < 8 && (start > 0 || end < lines.length)) {
    if (start > 0) start -= 1;
    else end += 1;
    excerpt = lines.slice(start, end).join("");
  }
  return excerpt.length >= 8 ? excerpt : null;
}

export function createNoteBatchInput(patches, batchIndex = 0) {
  const references = new Map();
  const annotated = patches.map((patch, patchIndex) => {
    const lines = splitDiffLines(patch.diff);
    const diff = lines.map((line, lineIndex) => {
      if (!/^[+-](?![+-]{2}).+/.test(line)) return line;
      const excerpt = excerptForLine(lines, lineIndex);
      if (!excerpt) return line;
      const ref = `b${String(batchIndex + 1).padStart(10, "0")}p${patchIndex}l${lineIndex}`;
      references.set(ref, { file: patch.file, excerpt });
      return `[evidence:${ref}] ${line}`;
    }).join("");
    return { file: patch.file, diff };
  });
  return { patches: annotated, references };
}

export function chunkNotePatches(patches, maxBytes = DEFAULT_MAX_BYTES) {
  return chunkPatches(patches, maxBytes, batch => serializedBytes(createNoteBatchInput(batch).patches));
}

export function resolveNoteEntries(value, references) {
  if (!value || !Array.isArray(value.entries)) throw new Error("AI 输出必须包含 entries 数组");
  const evidence = value.entries.map((entry, index) => {
    if (!entry || typeof entry.note !== "string" || !entry.note.trim() || /[\r\n]/.test(entry.note)) throw new Error(`第 ${index + 1} 条日志文字无效`);
    const reference = typeof entry.ref === "string" ? references.get(entry.ref) : null;
    if (!reference) throw new Error(`第 ${index + 1} 条日志引用的变更编号不属于当前批次`);
    return { note: entry.note.trim(), ...reference };
  });
  return { notes: evidence.map(item => `- ${item.note}`).join("\n"), evidence };
}

export function collectNoteCandidates(results) {
  if (!Array.isArray(results)) throw new TypeError("日志结果必须是数组");
  const candidates = [];
  for (const result of results) {
    if (!result || typeof result.notes !== "string" || !Array.isArray(result.evidence)) throw new Error("分批日志结果无效");
    if (result.notes === "" && result.evidence.length === 0) continue;
    const lines = result.notes.trim().split(/\r?\n/).filter((line) => line.trim());
    if (lines.length !== result.evidence.length) throw new Error("分批日志与差异证据数量不一致");
    for (const [index, line] of lines.entries()) {
      if (!line.startsWith("- ") || typeof result.evidence[index]?.note !== "string" || result.evidence[index].note !== line.slice(2)) throw new Error("分批日志与差异证据不一致");
      candidates.push(result.evidence[index]);
    }
  }
  return candidates;
}

export function mergeNoteResults(results) {
  const candidates = collectNoteCandidates(results);
  const notes = [];
  const evidence = [];
  const seen = new Set();
  for (const candidate of candidates) {
    const note = candidate.note;
    if (seen.has(note)) continue;
    seen.add(note);
    notes.push(`- ${note}`);
    evidence.push(candidate);
  }
  return { notes: notes.join("\n"), evidence };
}
