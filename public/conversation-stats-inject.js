// 会话状态条：区分历史会话累计与本次运行统计，未知数据保留空值。
// /session/stats 返回统计范围；会话 id 取自可见 composer，隐藏会话不参与。
(() => {
  const PROBE_KEY = "__codeyConversationStats";
  if (window[PROBE_KEY]?.installed) return;

  const barId = "codey-conversation-stats";
  const styleId = "codey-conversation-stats-styles";
  const conversationAttribute = "data-above-composer-conversation-id";
  // 与 codey-inject.js 的会话识别保持同一优先级：先看可见 composer，再看
  // 会话元素属性、活动侧边栏项，最后退回 URL。
  const sessionIdAttributes = [
    conversationAttribute,
    "data-session-id",
    "data-conversation-id",
    "data-thread-id",
    "data-request-user-input-auto-resolution-conversation-id",
    "data-response-annotation-conversation",
  ];
  const sessionAttributeSelector = sessionIdAttributes
    .map((attribute) => `[${attribute}]`)
    .join(",");
  const activeThreadSelector = '[data-app-action-sidebar-thread-active="true"]';
  const POLL_IDLE_MS = 3_000;
  const POLL_ACTIVE_MS = 1_100;
  const RESCAN_DEBOUNCE_MS = 120;

  let statsElement = null;
  let currentSessionId = "";
  let stats = null;
  let pollTimer = 0;
  let rescanTimer = 0;
  let fetchInFlight = false;
  let lastFetchOk = false;
  let lastResult = null;
  let sessionGeneration = 0;
  let fetchAttempted = false;

  const isRendered = (element) => (
    element instanceof HTMLElement
    && element.getClientRects().length > 0
  );

  const visibleSessionElement = (selector) => {
    const first = document.querySelector(selector);
    if (isRendered(first)) return first;
    return [...document.querySelectorAll(selector)].find(isRendered);
  };

  const composerInputSelector =
    "textarea, [contenteditable='true'], [role='textbox']";

  // 锚点是只带数据属性的标记元素，自身通常没有布局矩形，不能用
  // getClientRects 判断。当前对话的判定标准是锚点所在 composer 容器内
  // 存在视口内可见的输入框；Codex 保留的非活动会话不满足这一点。
  const anchorHostsVisibleComposer = (anchor) => {
    const host = anchor?.parentElement || anchor;
    if (!host) return false;
    const viewportHeight =
      window.innerHeight || document.documentElement.clientHeight || 0;
    for (const input of host.querySelectorAll?.(composerInputSelector) || []) {
      if (!(input instanceof HTMLElement)) continue;
      if (input.closest?.("[hidden], [aria-hidden='true']")) continue;
      const rect = input.getBoundingClientRect?.();
      if (!rect || rect.width <= 0 || rect.height <= 0) continue;
      if (viewportHeight > 0 && (rect.bottom <= 0 || rect.top >= viewportHeight)) {
        continue;
      }
      return true;
    }
    return false;
  };

  const findVisibleComposerAnchor = () => {
    for (const anchor of document.querySelectorAll(`[${conversationAttribute}]`)) {
      if (anchorHostsVisibleComposer(anchor)) return anchor;
    }
    return null;
  };

  const callBridge = (path, payload = {}, options = {}) => {
    if (typeof window.__codexSessionDeleteBridge === "function") {
      return window.__codexSessionDeleteBridge(path, payload, options);
    }
    return Promise.resolve({ status: "failed", message: "Codey bridge unavailable" });
  };

  const normalizeSessionId = (value) => (
    String(value || "").trim().replace(/^local:/, "").trim()
  );

  const resolveSessionId = () => {
    const anchor = findVisibleComposerAnchor();
    // 可见输入框的空标识表示会话尚未建立，不从旧 URL 或侧边栏取回旧会话。
    if (anchor) return normalizeSessionId(anchor.getAttribute(conversationAttribute));
    for (const attribute of sessionIdAttributes.slice(1)) {
      const value = visibleSessionElement(`[${attribute}]`)?.getAttribute(attribute);
      const normalized = normalizeSessionId(value);
      if (normalized) return normalized;
    }
    const activeThread = visibleSessionElement(activeThreadSelector)
      ?.getAttribute("data-app-action-sidebar-thread-id");
    if (activeThread) return normalizeSessionId(activeThread);
    const match = location.pathname.match(/(?:\/c\/|\/conversation\/|\/session\/)([A-Za-z0-9_-]+)/);
    if (match) return match[1];
    const params = new URLSearchParams(location.search);
    return normalizeSessionId(params.get("conversation_id") || params.get("session_id"));
  };

  const ensureStyle = () => {
    if (document.getElementById(styleId)) return;
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = `
      #${barId} { --cs-text: var(--text-primary, CanvasText); --cs-muted: color-mix(in srgb, var(--cs-text) 60%, transparent); --cs-line: color-mix(in srgb, var(--cs-text) 11%, transparent); --cs-surface: color-mix(in srgb, var(--bg-primary, Canvas) 97%, var(--cs-text)); -webkit-app-region: no-drag !important; pointer-events: auto !important; position: relative; box-sizing: border-box; width: 100%; min-width: 0; padding: 5px 4px 0; color: var(--cs-text); font: 12px/1.5 -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif; font-variant-numeric: tabular-nums; }
      #${barId} *, #${barId} *::before { box-sizing: border-box; }
      #${barId}[hidden], #${barId} [hidden] { display: none !important; }
      #${barId} .codey-cs-groups { display: flex; flex-wrap: wrap; justify-content: center; align-items: center; gap: 2px 10px; }
      #${barId} button { font: inherit; color: inherit; cursor: pointer; }
      #${barId} .codey-cs-summary { display: inline-flex; align-items: center; justify-content: center; flex-wrap: wrap; gap: 6px; min-width: 0; max-width: 100%; padding: 4px 9px; border: 0; border-radius: 999px; background: transparent; color: var(--cs-muted); text-align: center; }
      #${barId} .codey-cs-summary:hover, #${barId} .codey-cs-summary[aria-expanded="true"] { background: color-mix(in srgb, var(--cs-text) 5%, transparent); color: color-mix(in srgb, var(--cs-text) 72%, transparent); }
      #${barId} button:focus-visible { outline: 2px solid var(--cs-text); outline-offset: 2px; }
      #${barId} .codey-cs-icon { display: inline-block; width: 15px; height: 15px; flex: 0 0 15px; background: currentColor; mask: var(--cs-icon) center / contain no-repeat; }
      #${barId} .codey-cs-metric { white-space: nowrap; }
      #${barId} .codey-cs-details { position: fixed; z-index: 1000; width: min(360px, calc(100vw - 24px)); max-height: calc(100dvh - 24px); overflow: auto; padding: 17px 18px 15px; background: var(--cs-surface); border: 1px solid var(--cs-line); border-radius: 15px; box-shadow: 0 5px 20px #0000000d; user-select: text; }
      #${barId} .codey-cs-heading { display: flex; align-items: center; gap: 8px; padding-bottom: 11px; margin-bottom: 10px; border-bottom: 1px solid var(--cs-line); }
      #${barId} .codey-cs-heading strong { font-size: 13px; font-weight: 550; white-space: nowrap; }
      #${barId} .codey-cs-heading-total { margin-left: auto; font-size: 13px; text-align: right; }
      #${barId} dl { margin: 0; }
      #${barId} .codey-cs-detail-row { display: flex; justify-content: space-between; gap: 14px; padding: 4px 0; }
      #${barId} dt { color: var(--cs-muted); }
      #${barId} dd { margin: 0; text-align: right; color: color-mix(in srgb, var(--cs-text) 72%, transparent); overflow-wrap: anywhere; }
      #${barId} .codey-cs-note { margin: 10px 0 0; color: var(--cs-muted); font-size: 10px; line-height: 1.6; }
      @media (max-width: 480px) { #${barId} .codey-cs-groups { gap: 2px; } #${barId} .codey-cs-summary { padding-inline: 6px; font-size: 11px; } #${barId} .codey-cs-details { padding: 14px; } }
    `;
    document.documentElement.appendChild(style);
  };

  // 状态条挂在 composer 标记所在的同一列容器末尾，即输入卡片下方；
  // composer 容器会随会话切换重建，所以每次都要重新核对挂载父节点。
  const ensureBar = () => {
    const anchor = findVisibleComposerAnchor();
    const host = anchor?.parentElement;
    if (!host) return null;
    let bar = document.getElementById(barId);
    if (!bar?.__codeyCs) {
      bar = document.createElement("div");
      bar.id = barId;
      bar.className = "codey-conversation-stats";
      const make = (tag, className, text, parent) => {
        const element = document.createElement(tag);
        element.className = className;
        if (text) element.textContent = text;
        parent?.appendChild(element);
        return element;
      };
      const groups = make("div", "codey-cs-groups", "", bar);
      const refs = { buttons: {}, panels: {}, fields: {}, notes: {}, active: null, pinned: false, closeTimer: 0 };
      // Tabler Icons v3.45.0 (MIT), matching the existing application icon family.
      const paths = {
        activity: ["M3 12a9 9 0 1 0 18 0a9 9 0 1 0 -18 0", "M11 12a1 1 0 1 0 2 0a1 1 0 1 0 -2 0", "M13.41 10.59l2.59 -2.59", "M7 12a5 5 0 0 1 5 -5"],
        tokens: ["M4 6a8 3 0 1 0 16 0a8 3 0 1 0 -16 0", "M4 6v6a8 3 0 0 0 16 0v-6", "M4 12v6a8 3 0 0 0 16 0v-6"],
      };
      const icon = (kind, parent) => {
        const element = make("span", "codey-cs-icon", "", parent);
        element.setAttribute("aria-hidden", "true");
        const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">${paths[kind].map(d => `<path d="${d}"/>`).join("")}</svg>`;
        element.style.setProperty?.("--cs-icon", `url("data:image/svg+xml,${encodeURIComponent(svg)}")`);
      };
      for (const [kind, title, fields] of [
        ["activity", "会话统计", [["modelDurationMs", "模型用时"], ["toolDurationMs", "工具调用用时"], ["averageTtftMs", "首 token 平均（TTFT）"], ["speed", "输出速度（TPS）"]]],
        ["tokens", "Token 用量", [["cacheRate", "缓存命中"], ["uncachedInputTokens", "未缓存输入"], ["cachedInputTokens", "缓存读取"], ["outputTokens", "输出"]]],
      ]) {
        const button = make("button", `codey-cs-summary codey-cs-${kind}-summary`, "", groups);
        button.type = "button";
        button.setAttribute("aria-expanded", "false");
        button.setAttribute("aria-controls", `${barId}-${kind}`);
        icon(kind, button);
        if (kind === "activity") {
          refs.activity = make("span", "codey-cs-metric codey-cs-activity", "— 轮 — 步", button);
          make("span", "", "·", button).setAttribute("aria-hidden", "true");
          refs.speed = make("span", "codey-cs-metric codey-cs-speed", "—", button);
        } else {
          refs.total = make("span", "codey-cs-metric codey-cs-total", "—", button);
          make("span", "", "·", button).setAttribute("aria-hidden", "true");
          const cache = make("span", "codey-cs-metric", "缓存命中 ", button);
          refs["cache-rate"] = make("span", "codey-cs-cache-rate", "—", cache);
        }
        const panel = make("section", "codey-cs-details", "", bar);
        panel.id = `${barId}-${kind}`;
        panel.hidden = true;
        panel.setAttribute("aria-label", title);
        const heading = make("div", "codey-cs-heading", "", panel);
        icon(kind, heading);
        make("strong", "", title, heading);
        if (kind === "tokens") {
          refs.fields.totalTokens = make("span", "codey-cs-heading-total", "—", heading);
          refs.fields.totalTokens.setAttribute("data-field", "totalTokens");
        }
        const list = make("dl", "", "", panel);
        for (const [key, label] of fields) {
          const row = make("div", "codey-cs-detail-row", "", list);
          make("dt", "", label, row);
          refs.fields[key] = make("dd", "", "—", row);
          refs.fields[key].setAttribute("data-field", key);
        }
        refs.notes[kind] = make("p", `codey-cs-note codey-cs-${kind}-note`, "", panel);
        refs.buttons[kind] = button;
        refs.panels[kind] = panel;
        const preview = () => { clearTimeout(refs.closeTimer); if (!refs.pinned && !refs.restoringFocus) setExpanded(true, false, kind); };
        const leave = () => { clearTimeout(refs.closeTimer); refs.closeTimer = setTimeout(() => { if (!refs.pinned) setExpanded(false); }, 350); };
        button.addEventListener("pointerenter", preview);
        button.addEventListener("focus", preview);
        button.addEventListener("pointerleave", leave);
        button.addEventListener("blur", (event) => { if (!panel.contains(event.relatedTarget)) leave(); });
        panel.addEventListener("pointerenter", () => clearTimeout(refs.closeTimer));
        panel.addEventListener("pointerleave", leave);
        button.addEventListener("click", () => {
          const close = refs.active === kind && refs.pinned;
          setExpanded(!close, false, kind);
          refs.pinned = !close;
        });
      }
      bar.__codeyCs = refs;
    }
    if (bar.parentElement !== host) host.appendChild(bar);
    statsElement = bar;
    return bar;
  };

  const formatTokenCount = (value) => {
    if (value == null || !Number.isFinite(value)) return "—";
    if (value < 1_000) return `${Math.round(value)}`;
    if (value < 1_000_000) return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)}k`;
    return `${(value / 1_000_000).toFixed(1)}M`;
  };

  const positionPanel = () => {
    const refs = statsElement?.__codeyCs;
    if (!refs?.active) return;
    const panel = refs.panels[refs.active];
    const rect = refs.buttons[refs.active].getBoundingClientRect();
    const panelRect = panel.getBoundingClientRect();
    const height = window.innerHeight || document.documentElement.clientHeight;
    const width = window.innerWidth || document.documentElement.clientWidth;
    // 气泡贴近入口，鼠标可以直接移入；只覆盖编辑器，不推动页面布局。
    const available = Math.max(80, rect.top - 20);
    panel.style.left = `${Math.max(12, Math.min(rect.left + (rect.width - panelRect.width) / 2, width - panelRect.width - 12))}px`;
    panel.style.maxHeight = `${Math.min(height - 24, available)}px`;
    panel.style.top = `${Math.max(12, rect.top - Math.min(panelRect.height, available) - 8)}px`;
  };

  const setExpanded = (expanded, restoreFocus = false, kind = null) => {
    const refs = statsElement?.__codeyCs;
    if (!refs) return;
    const active = kind || refs.active || "activity";
    clearTimeout(refs.closeTimer);
    refs.active = expanded ? active : null;
    if (!expanded) refs.pinned = false;
    for (const key of Object.keys(refs.panels)) {
      refs.panels[key].hidden = !expanded || key !== active;
      refs.buttons[key].setAttribute("aria-expanded", String(expanded && key === active));
    }
    if (expanded) positionPanel();
    else if (restoreFocus) {
      refs.restoringFocus = true;
      refs.buttons[active].focus?.({ preventScroll: true });
      refs.restoringFocus = false;
    }
  };

  const resetSession = (sessionId) => {
    currentSessionId = sessionId;
    sessionGeneration += 1;
    stats = null;
    lastResult = null;
    lastFetchOk = false;
    fetchAttempted = false;
    setExpanded(false);
  };

  const formatSpeed = (value) => (
    value == null || !Number.isFinite(value) || value <= 0
      ? null
      : `${value >= 100 ? Math.round(value) : value.toFixed(1)} tok/s`
  );

  // 与请求日志同一口径：缓存读占输入总量的百分比；上游未回报缓存字段时
  // 返回 null，状态条显示未知值而不是显示 0%。
  const cacheRatePercent = (input, cached) => {
    if (input == null || cached == null || input <= 0 || cached < 0 || cached > input) return null;
    return `${Math.round((cached / input) * 100)}%`;
  };

  const render = () => {
    const bar = ensureBar();
    if (!bar) {
      // 输入框不可见（例如离开会话页）时，若旧宿主仍在隐藏会话里，显式
      // 隐藏状态条，避免恢复可见时短暂展示过期数据。
      if (statsElement?.isConnected) { statsElement.hidden = true; setExpanded(false); }
      return;
    }
    bar.hidden = false;
    const data = { ...stats };
    const sessionUsage = data.usageSource === "rollout" && data.usageScope === "session";
    const runtimeRequests = data.requestsScope === "runtime"
      || (data.usageSource == null && data.usageScope == null);
    const hasRuntimeRequest = runtimeRequests && (data.requests > 0 || data.inFlight > 0);
    const recordedSpeed = data.speedSource === "request_log"
      && data.speedScope === "latest_recorded_request" && !(data.inFlight > 0);
    const runtimeSpeed = hasRuntimeRequest && data.requestsScope === "runtime"
      && (data.speedSource == null || data.speedSource === "runtime")
      && (data.speedScope == null || data.speedScope === "latest_request");
    if (!hasRuntimeRequest) {
      for (const key of ["requests", "inFlight", "model", "lastStatus", "updatedAtUnixMs"]) data[key] = null;
    }
    // 历史速度独立于运行期请求信息；缺少明确归属时保留未知。
    if (!recordedSpeed && !runtimeSpeed) data.outputTokensPerSecond = null;
    for (const key of ["totalTokens", "inputTokens", "outputTokens", "cachedInputTokens", "cacheWriteInputTokens", "requests", "inFlight"]) {
      if (!Number.isFinite(data[key]) || data[key] < 0) data[key] = null;
    }
    const speedText = formatSpeed(data.outputTokensPerSecond) || "—";
    const speedKnown = speedText !== "—";
    const speedNote = speedKnown && recordedSpeed ? "最近有记录请求的输出速度，可能早于最近请求" : "本次运行最近请求的输出速度；未记录时显示未知";
    const rate = cacheRatePercent(data.inputTokens, data.cachedInputTokens) || "—";
    const refs = bar.__codeyCs;
    refs.speed.textContent = speedText;
    refs.speed.title = speedNote;
    refs["cache-rate"].textContent = rate;
    refs.total.textContent = data.totalTokens == null ? "—" : `${formatTokenCount(data.totalTokens)} tok`;
    const finite = value => Number.isFinite(value) && value >= 0;
    const count = value => finite(value) ? value.toLocaleString("zh-CN") : "—";
    const duration = (value, average = false) => {
      if (!finite(value)) return "—";
      const seconds = value / 1000;
      if (average || seconds < 60) return `${Number(seconds.toFixed(1))}秒`;
      return `${Math.floor(seconds / 60)}分 ${Math.floor(seconds % 60)}秒`;
    };
    refs.activity.textContent = `${count(data.turnCount)} 轮 ${count(data.stepCount)} 步`;
    const stale = fetchAttempted && !lastFetchOk;
    const hasData = stats && (hasRuntimeRequest || speedKnown || [data.totalTokens, data.inputTokens, data.outputTokens, data.turnCount, data.stepCount, data.modelDurationMs, data.toolDurationMs, data.averageTtftMs].some(finite));
    const unknownUsage = data.usageSource === "unknown" || data.usageScope === "unknown";
    const scopeLabel = !stats || unknownUsage ? "统计范围未知" : sessionUsage ? "会话累计" : "本次运行";
    const state = !currentSessionId ? "waiting" : stale ? "stale" : !fetchAttempted ? "loading" : !hasData ? "empty" : "ok";
    const status = {
      waiting: "等待会话", loading: "读取统计中", stale: "统计暂不可用", empty: "暂无统计",
      ok: fetchInFlight ? "更新统计中" : data.inFlight > 0 ? `${data.inFlight} 个进行中` : ({ failed: "最近请求失败", incomplete: "最近请求未完成", cancelled: "最近请求已取消", canceled: "最近请求已取消" }[data.lastStatus] || (sessionUsage ? "历史统计" : unknownUsage && recordedSpeed ? "历史速度记录" : "运行期统计")),
    }[state];
    bar.dataset.status = status;
    const tokenValue = value => finite(value) ? `${count(value)} tok` : "—";
    data.uncachedInputTokens = finite(data.inputTokens) && finite(data.cachedInputTokens) && data.cachedInputTokens <= data.inputTokens ? data.inputTokens - data.cachedInputTokens : null;
    for (const [key, element] of Object.entries(refs.fields)) {
      element.textContent = key === "cacheRate" ? rate : key === "speed" ? speedText : key.endsWith("Ms") ? duration(data[key], key === "averageTtftMs") : tokenValue(data[key]);
    }
    const pendingNote = stale ? (stats ? "更新失败，当前数据可能已过期。" : "暂时无法读取统计，将自动重试。") : !currentSessionId ? "建立会话后显示统计。" : !stats ? "缺失数据以 — 显示。" : "";
    const sourceNote = unknownUsage ? "Token 统计范围未知。" : sessionUsage ? "会话累计用量，缓存已包含在输入中。" : "仅统计本次运行的用量。";
    const timingNote = data.timingScope === "runtime" ? "模型计时仅覆盖本次运行。" : data.timingScope ? "模型计时仅覆盖有有效计时的请求。" : "缺少有效计时时显示 —。";
    const activityNote = data.stepScope === "runtime" ? "步数仅覆盖本次运行。" : data.stepScope === "recorded_requests" ? "步数仅覆盖有记录的生成请求。" : data.stepScope === "recorded_session" ? "步数为会话内模型生成次数。" : "缺失的轮数和步数显示 —。";
    const toolNote = data.toolTimingScope ? "工具用时为累计值，并行调用可能重叠。" : "";
    const speedDate = speedKnown && recordedSpeed && finite(data.speedRecordedAtUnixMs) ? `（${new Date(data.speedRecordedAtUnixMs).toLocaleString("zh-CN", { hour12: false })}）` : "";
    refs.notes.activity.textContent = `${pendingNote}${activityNote}${timingNote}${toolNote}${speedKnown ? `${speedNote}${speedDate}。` : ""}`;
    refs.notes.tokens.textContent = `${pendingNote}${sourceNote}`;
    refs.buttons.activity.setAttribute("aria-label", `会话统计，${refs.activity.textContent}，最近速度 ${speedText}，${status}。${speedNote}。查看统计详情`);
    refs.buttons.tokens.setAttribute("aria-label", `Token 用量，${scopeLabel} ${refs.total.textContent}，缓存命中 ${rate}，${status}。查看统计详情`);
    bar.dataset.running = data.inFlight > 0 ? "true" : "false";
    bar.dataset.state = state;
    positionPanel();
  };

  const schedulePoll = (delay) => {
    clearTimeout(pollTimer);
    pollTimer = setTimeout(poll, delay);
  };

  const poll = async () => {
    if (document.visibilityState === "hidden") {
      schedulePoll(POLL_IDLE_MS);
      return;
    }
    const sessionId = resolveSessionId();
    if (sessionId !== currentSessionId) {
      resetSession(sessionId);
      render();
    }
    if (!currentSessionId) {
      schedulePoll(POLL_IDLE_MS);
      return;
    }
    if (!fetchInFlight) {
      fetchInFlight = true;
      render();
      const requestSessionId = currentSessionId;
      const requestGeneration = sessionGeneration;
      try {
        const result = await callBridge("/session/stats", { sessionId: requestSessionId });
        if (requestGeneration !== sessionGeneration || resolveSessionId() !== requestSessionId) return;
        fetchAttempted = true;
        lastResult = result ?? null;
        if (result?.status === "ok") {
          lastFetchOk = true;
          // 轮询途中会话又切换时丢弃过期响应。
          if (resolveSessionId() === currentSessionId) {
            stats = result.stats ?? null;
          }
        } else {
          lastFetchOk = false;
        }
      } catch {
        if (requestGeneration === sessionGeneration && resolveSessionId() === requestSessionId) {
          fetchAttempted = true;
          lastFetchOk = false;
        }
      } finally {
        fetchInFlight = false;
        schedulePoll(requestGeneration !== sessionGeneration || resolveSessionId() !== requestSessionId ? 0 : stats?.inFlight > 0 ? POLL_ACTIVE_MS : POLL_IDLE_MS);
      }
      render();
    }
    schedulePoll(stats?.inFlight > 0 ? POLL_ACTIVE_MS : POLL_IDLE_MS);
  };

  const scheduleRescan = () => {
    if (rescanTimer) return;
    rescanTimer = setTimeout(() => {
      rescanTimer = 0;
      const sessionId = resolveSessionId();
      const changed = sessionId !== currentSessionId;
      if (changed) {
        resetSession(sessionId);
      }
      render();
      // 会话切换立即拉取，不等下一轮轮询。
      if (changed && currentSessionId) schedulePoll(0);
    }, RESCAN_DEBOUNCE_MS);
  };

  const mutationHandler = (mutations) => {
    for (const mutation of mutations || []) {
      if (mutation.type === "attributes") {
        const target = mutation.target;
        if (!(target instanceof HTMLElement)) continue;
        if (target.id === barId || target.closest?.(`#${barId}`)) continue;
        if (
          target.hasAttribute?.(conversationAttribute)
          || target.hasAttribute?.("hidden")
          || target.hasAttribute?.("aria-hidden")
        ) {
          scheduleRescan();
          return;
        }
        continue;
      }
      const nodes = [...(mutation.addedNodes || []), ...(mutation.removedNodes || [])];
      for (const node of nodes) {
        if (!(node instanceof HTMLElement)) continue;
        if (node.id === barId || node.closest?.(`#${barId}`) || node.querySelector?.(`#${barId}`)) {
          // 状态条自身被移除时也要尽快重挂。
          if (node.id === barId || node.querySelector?.(`#${barId}`)) scheduleRescan();
          continue;
        }
        if (
          node.matches?.(sessionAttributeSelector)
          || node.querySelector?.(sessionAttributeSelector)
          || node.querySelector?.(activeThreadSelector)
        ) {
          scheduleRescan();
          return;
        }
        // 会话容器子树变化一律重扫，代价只是一次 debounce 查询。
        if (node.querySelector?.("[contenteditable='true'], textarea")) {
          scheduleRescan();
          return;
        }
      }
    }
  };

  const bootstrap = () => {
    ensureStyle();
    render();
    const options = {
      attributes: true,
      attributeFilter: [
        conversationAttribute,
        "data-app-action-sidebar-thread-active",
        "hidden",
        "aria-hidden",
      ],
      childList: true,
      subtree: true,
    };
    const dispatcher = window.__codeyMutationDispatcher;
    if (typeof dispatcher?.subscribe === "function") {
      const unsubscribe = dispatcher.subscribe(mutationHandler, options);
      if (dispatcher.snapshot?.().observerInstalled) {
        window.__codeyConversationStatsObserver = { disconnect: unsubscribe };
      } else {
        unsubscribe?.();
      }
    }
    if (!window.__codeyConversationStatsObserver) {
      const observer = new MutationObserver(mutationHandler);
      observer.observe(document.documentElement, options);
      window.__codeyConversationStatsObserver = observer;
    }
    document.addEventListener?.("visibilitychange", () => {
      if (document.visibilityState !== "hidden") schedulePoll(0);
    });
    window.addEventListener?.("focus", () => schedulePoll(0));
    window.addEventListener?.("pageshow", () => schedulePoll(0));
    window.addEventListener?.("resize", positionPanel);
    document.addEventListener?.("scroll", positionPanel, true);
    document.addEventListener?.("pointerdown", (event) => {
      if (statsElement && !statsElement.contains(event.target)) setExpanded(false);
    });
    document.addEventListener?.("keydown", (event) => {
      if (event.key !== "Escape" || !statsElement?.__codeyCs?.active) return;
      const focusedInside = statsElement.contains(document.activeElement);
      setExpanded(false, focusedInside);
      if (focusedInside) { event.preventDefault(); event.stopPropagation(); }
    });
    schedulePoll(0);
    window[PROBE_KEY] = {
      installed: true,
      snapshot: () => ({
        sessionId: currentSessionId,
        anchorFound: Boolean(findVisibleComposerAnchor()),
        mounted: statsElement?.isConnected === true,
        hidden: statsElement?.hidden !== false,
        stats,
        lastFetchOk,
        lastResult,
      }),
      refresh: () => schedulePoll(0),
    };
  };

  try {
    bootstrap();
  } catch {
    // 同步引导失败不置 installed，CDP 会按 probe 重新注入。
  }
})();
