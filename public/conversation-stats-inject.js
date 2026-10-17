// 会话状态条：挂到当前可见输入框下方，展示本会话累计 Token、缓存命中率和
// 最近一次请求的输出速度。数据来自本地路由按会话聚合的实时统计（bridge
// /session/stats）；会话 id 取自可见 composer 的标记属性，隐藏会话不参与。
// 重新对话得到新会话 id 即自动隔离；同一会话内切模型时最近速度随下一次完
// 成的请求覆盖，不会继续展示旧模型的值。
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
    String(value || "").replace(/^local:/, "").trim()
  );

  const resolveSessionId = () => {
    const anchor = findVisibleComposerAnchor();
    const anchorId = normalizeSessionId(anchor?.getAttribute(conversationAttribute));
    if (anchorId) return anchorId;
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
    // 与额度条同一套数值：10px 等宽数字、低对比次要文本，挂在输入框区域
    // 底部，不抢占布局（等宽 tabular-nums 避免刷新时抖动）。
    style.textContent = `
      #${barId} { -webkit-app-region: no-drag !important; pointer-events: auto !important; display: flex; box-sizing: border-box; width: 100%; min-width: 0; align-items: baseline; justify-content: space-between; gap: 10px; padding: 5px 10px 0; color: color-mix(in srgb, CanvasText 52%, transparent); font: 10px/1.4 -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Helvetica Neue", sans-serif; font-variant-numeric: tabular-nums; letter-spacing: 0; user-select: none; }
      #${barId}[hidden] { display: none; }
      #${barId} .codey-cs-left, #${barId} .codey-cs-right { display: inline-flex; min-width: 0; align-items: baseline; gap: 8px; overflow: hidden; white-space: nowrap; }
      #${barId} .codey-cs-speed { color: color-mix(in srgb, CanvasText 62%, transparent); font-weight: 600; }
      #${barId}[data-running="true"] .codey-cs-speed { animation: codey-cs-pulse 1.4s ease-in-out infinite; }
      #${barId} .codey-cs-cache, #${barId} .codey-cs-total { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
      #${barId}[data-state="stale"] { opacity: .55; }
      @keyframes codey-cs-pulse { 0%, 100% { opacity: 1; } 50% { opacity: .45; } }
      @media (prefers-reduced-motion: reduce) { #${barId}[data-running="true"] .codey-cs-speed { animation: none; } }
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
      const left = document.createElement("span");
      left.className = "codey-cs-left";
      const speed = document.createElement("span");
      speed.className = "codey-cs-speed";
      const cache = document.createElement("span");
      cache.className = "codey-cs-cache";
      left.appendChild(speed);
      left.appendChild(cache);
      const right = document.createElement("span");
      right.className = "codey-cs-right";
      const total = document.createElement("span");
      total.className = "codey-cs-total";
      right.appendChild(total);
      bar.appendChild(left);
      bar.appendChild(right);
      bar.__codeyCs = { speed, cache, total };
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

  const formatSpeed = (value) => (
    value == null || !Number.isFinite(value) || value <= 0
      ? null
      : `${value >= 100 ? Math.round(value) : value.toFixed(1)} tok/s`
  );

  // 与请求日志同一口径：缓存读占输入总量的百分比；上游未回报缓存字段时
  // 返回 null，状态条直接隐藏这一段而不是显示 0%。
  const cacheRatePercent = (input, cached) => {
    if (input == null || cached == null || input <= 0 || cached < 0 || cached > input) return null;
    return `${Math.round((cached / input) * 100)}%`;
  };

  const render = () => {
    const bar = ensureBar();
    if (!bar) {
      // 输入框不可见（例如离开会话页）时，若旧宿主仍在隐藏会话里，显式
      // 隐藏状态条，避免恢复可见时短暂展示过期数据。
      if (statsElement?.isConnected) statsElement.hidden = true;
      return;
    }
    const hasData = stats
      && (stats.requests > 0 || stats.inFlight > 0 || stats.totalTokens > 0);
    if (!currentSessionId || !hasData) {
      bar.hidden = true;
      return;
    }
    bar.hidden = false;
    const speedText = formatSpeed(stats.outputTokensPerSecond);
    const { speed, cache, total } = bar.__codeyCs;
    speed.textContent = speedText ? `⚡ ${speedText}` : "";
    const rate = cacheRatePercent(stats.inputTokens, stats.cachedInputTokens);
    cache.textContent = rate ? `缓存 ${rate}` : "";
    total.textContent = `${formatTokenCount(stats.totalTokens)} tok`;
    const titleParts = [
      stats.model,
      `${stats.requests} 次请求`,
      `输入 ${formatTokenCount(stats.inputTokens)}`,
      `输出 ${formatTokenCount(stats.outputTokens)}`,
      stats.cachedInputTokens != null ? `缓存读 ${formatTokenCount(stats.cachedInputTokens)}` : null,
      stats.cacheWriteInputTokens != null ? `缓存写 ${formatTokenCount(stats.cacheWriteInputTokens)}` : null,
      stats.lastStatus && stats.lastStatus !== "succeeded" ? `最近请求 ${stats.lastStatus}` : null,
    ].filter(Boolean);
    bar.title = titleParts.join(" · ");
    bar.dataset.running = stats.inFlight > 0 ? "true" : "false";
    bar.dataset.state = lastFetchOk ? "ok" : "stale";
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
      currentSessionId = sessionId;
      // 切换会话立即清空旧数据，避免上一个会话的统计短暂留在页面上。
      stats = null;
      render();
    }
    if (!currentSessionId) {
      schedulePoll(POLL_IDLE_MS);
      return;
    }
    if (!fetchInFlight) {
      fetchInFlight = true;
      try {
        const result = await callBridge("/session/stats", { sessionId: currentSessionId });
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
        lastFetchOk = false;
      } finally {
        fetchInFlight = false;
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
        currentSessionId = sessionId;
        stats = null;
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
