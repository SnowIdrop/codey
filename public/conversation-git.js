// 当前对话 ID 仅用于查询；工作区、文件范围和提交内容均由后台确定。
(() => {
  if (window.__codeyConversationGit) return;
  const id = "codey-conversation-git";
  let enabled = false;
  let ready = false;
  let sessionId = null;
  let generation = 0;
  let checking = false;
  let busy = false;
  let status = null;
  let panel = null;
  let timer = 0;
  const context = () => window.__codeyPromptOptimize?.composerContext?.();
  const call = async (name, payload) => {
    if (typeof window.__codexSessionDeleteBridge !== "function") throw new Error("Codey bridge 尚未就绪");
    const result = await window.__codexSessionDeleteBridge(name, payload);
    if (result?.status === "failed") throw new Error(result.message || "Git 操作失败");
    return result;
  };
  const button = document.createElement("button");
  button.id = id;
  button.type = "button";
  button.setAttribute("aria-label", "当前对话 Git 提交与推送");
  button.setAttribute("aria-haspopup", "dialog");
  button.setAttribute("aria-expanded", "false");
  button.style.display = "none";
  button.innerHTML = `
    <svg class="codey-git-icon" viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
      <circle cx="4" cy="4" r="2.2"></circle>
      <circle cx="4" cy="12" r="2.2"></circle>
      <circle cx="12" cy="6" r="2.2"></circle>
      <path d="M4 6.2v3.6"></path>
      <path d="M6 12h2a4 4 0 0 0 4-4V8.2"></path>
    </svg>
    <svg class="codey-git-spinner" viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true" focusable="false">
      <path d="M20 12a8 8 0 1 1-5.3-7.5"></path>
    </svg>
    <span>Git 推送</span>
  `;
  if (!button.textContent) button.textContent = "Git 推送";
  const style = document.createElement("style");
  style.textContent = `
    #${id} {
      -webkit-app-region: no-drag !important;
      pointer-events: auto !important;
      position: relative !important;
      z-index: 1 !important;
      display: none;
      flex: 0 0 auto;
      align-items: center;
      gap: 5px;
      box-sizing: border-box;
      min-height: 26px !important;
      height: 26px !important;
      margin: 0 0 0 6px;
      padding: 0 9px;
      border: 1px solid light-dark(rgba(0,0,0,.12), rgba(255,255,255,.16));
      border-radius: 999px;
      background: light-dark(#24292f, rgba(255,255,255,.12));
      color: light-dark(#ffffff, #f0f6fc);
      font: 500 12px/1 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      cursor: pointer;
      user-select: none;
      box-shadow: 0 1px 3px light-dark(rgba(0,0,0,.15), rgba(0,0,0,.4));
      opacity: .92;
      transition: opacity .15s ease, background-color .15s ease, transform .15s ease, box-shadow .15s ease;
    }
    #${id}:hover {
      opacity: 1;
      background: light-dark(#32383f, rgba(255,255,255,.2));
      border-color: light-dark(rgba(0,0,0,.2), rgba(255,255,255,.26));
      transform: translateY(-0.5px);
      box-shadow: 0 2px 6px light-dark(rgba(0,0,0,.2), rgba(0,0,0,.5));
    }
    #${id}:active {
      transform: translateY(0.5px);
      box-shadow: 0 1px 2px light-dark(rgba(0,0,0,.1), rgba(0,0,0,.3));
    }
    #${id}:disabled {
      opacity: .45;
      cursor: wait;
      box-shadow: none;
    }
    #${id} .codey-git-icon {
      flex: 0 0 auto;
      width: 13px;
      height: 13px;
    }
    #${id} .codey-git-spinner {
      display: none;
      flex: 0 0 auto;
      width: 13px;
      height: 13px;
      animation: codey-git-spin .75s linear infinite;
    }
    #${id}[data-busy="true"] .codey-git-icon {
      display: none;
    }
    #${id}[data-busy="true"] .codey-git-spinner {
      display: block;
    }
    @keyframes codey-git-spin {
      to { transform: rotate(360deg); }
    }
    #${id}-panel {
      -webkit-app-region: no-drag;
      position: fixed;
      z-index: 2147483646;
      box-sizing: border-box;
      max-width: calc(100vw - 32px);
      max-height: 78vh;
      overflow: auto;
      padding: 16px 18px;
      border: 1px solid light-dark(rgba(0,0,0,.15), rgba(255,255,255,.15));
      border-radius: 12px;
      background: light-dark(#ffffff, #1e1e1e);
      color: light-dark(#1f2328, #e6edf3);
      box-shadow: 0 16px 40px light-dark(rgba(0,0,0,.18), rgba(0,0,0,.55));
      color-scheme: light dark;
      font: 13px/1.55 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    #${id}-panel .codey-git-header {
      display: flex;
      align-items: baseline;
      flex-wrap: wrap;
      gap: 8px;
      margin-bottom: 12px;
      padding-bottom: 10px;
      border-bottom: 1px solid light-dark(rgba(0,0,0,.08), rgba(255,255,255,.1));
    }
    #${id}-panel .codey-git-header strong {
      font-size: 14px;
      font-weight: 600;
      color: light-dark(#1f2328, #f0f6fc);
    }
    #${id}-panel .codey-git-remote-info {
      font-size: 12px;
      color: light-dark(#656d76, #8b949e);
    }
    #${id}-panel .codey-git-section-title {
      font-weight: 600;
      font-size: 12px;
      margin: 10px 0 4px;
      color: light-dark(#57606a, #8b949e);
      text-transform: uppercase;
      letter-spacing: .3px;
    }
    #${id}-panel .codey-git-file-list {
      margin: 4px 0 10px;
      padding-left: 20px;
      overflow-wrap: anywhere;
      max-height: 120px;
      overflow: auto;
      color: light-dark(#24292f, #c9d1d9);
    }
    #${id}-panel .codey-git-file-list li {
      margin: 2px 0;
      font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
      font-size: 12px;
    }
    #${id}-panel .codey-git-partial-notice {
      margin: 6px 0 10px;
      padding: 6px 10px;
      border-radius: 6px;
      background: light-dark(rgba(234, 179, 8, .12), rgba(234, 179, 8, .16));
      border: 1px solid light-dark(rgba(202, 138, 4, .3), rgba(234, 179, 8, .3));
      color: light-dark(#854d0e, #fef08a);
      font-size: 12px;
    }
    #${id}-panel pre {
      white-space: pre-wrap;
      overflow-wrap: anywhere;
      max-height: 28vh;
      overflow: auto;
      padding: 10px 12px;
      border: 1px solid light-dark(rgba(0,0,0,.08), rgba(255,255,255,.08));
      border-radius: 8px;
      background: light-dark(#f6f8fa, #161b22);
      color: light-dark(#1f2328, #e6edf3);
      font: 12px/1.5 ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    }
    #${id}-panel .codey-git-commit-msg {
      border-left: 3px solid light-dark(#0969da, #388bfd);
    }
    #${id}-panel details {
      margin-top: 10px;
    }
    #${id}-panel details summary {
      cursor: pointer;
      font-size: 12px;
      font-weight: 500;
      color: light-dark(#0969da, #58a6ff);
      user-select: none;
      margin-bottom: 6px;
    }
    #${id}-panel .codey-git-actions {
      display: flex;
      gap: 8px;
      justify-content: flex-end;
      align-items: center;
      margin-top: 16px;
      padding-top: 12px;
      border-top: 1px solid light-dark(rgba(0,0,0,.08), rgba(255,255,255,.1));
    }
    #${id}-panel button {
      box-sizing: border-box;
      border: 1px solid light-dark(rgba(0,0,0,.15), rgba(255,255,255,.2));
      border-radius: 6px;
      padding: 6px 14px;
      background: light-dark(#f6f8fa, #21262d);
      color: light-dark(#24292f, #c9d1d9);
      font: 500 12px/1.4 system-ui, -apple-system, sans-serif;
      cursor: pointer;
      transition: all .15s ease;
    }
    #${id}-panel button:hover {
      background: light-dark(#eef0f3, #30363d);
      border-color: light-dark(rgba(0,0,0,.25), rgba(255,255,255,.3));
    }
    #${id}-panel button:disabled {
      opacity: .5;
      cursor: wait;
    }
    #${id}-panel .codey-git-btn-primary {
      background: light-dark(#1f883d, #238636);
      border-color: light-dark(#1a7f37, #2ea043);
      color: #ffffff;
      font-weight: 600;
    }
    #${id}-panel .codey-git-btn-primary:hover {
      background: light-dark(#1a7f37, #2ea043);
      border-color: light-dark(#166c2f, #3fb950);
    }
    #${id}-panel .codey-git-btn-secondary {
      background: light-dark(#24292f, #30363d);
      border-color: light-dark(#1b1f24, #3a424b);
      color: #ffffff;
      font-weight: 600;
    }
    #${id}-panel .codey-git-btn-secondary:hover {
      background: light-dark(#32383f, #3c444d);
    }
    #${id}-panel .codey-git-loading {
      display: flex;
      align-items: center;
      gap: 10px;
      padding: 16px 4px;
      color: light-dark(#57606a, #8b949e);
      font-size: 13px;
    }
    #${id}-panel .codey-git-loading-spinner {
      width: 14px;
      height: 14px;
      border: 2px solid light-dark(rgba(0,0,0,.15), rgba(255,255,255,.2));
      border-top-color: light-dark(#0969da, #58a6ff);
      border-radius: 50%;
      animation: codey-git-spin .75s linear infinite;
    }
    #${id}-panel [role=alert] {
      color: light-dark(#cf222e, #ff7b72);
      white-space: pre-wrap;
      background: light-dark(#ffebe9, rgba(248,81,73,.1));
      border: 1px solid light-dark(#ff818266, #f8514940);
      border-radius: 6px;
      padding: 8px 12px;
    }
  `;
  document.documentElement.appendChild(style);
  const close = () => { panel?.remove(); panel = null; button.setAttribute("aria-expanded", "false"); };
  const valid = (current, epoch) => enabled && generation === epoch && context()?.sessionId === current;
  const element = (tag, text) => {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const action = (text, handler) => {
    const node = element("button", text); node.type = "button";
    node.addEventListener("click", handler); return node;
  };
  const makePanel = (wide = true) => {
    close();
    panel = element("div"); panel.id = `${id}-panel`;
    panel.setAttribute("role", "dialog"); panel.setAttribute("aria-label", "当前对话 Git 提交与推送");
    const rect = button.getBoundingClientRect();
    panel.style.width = wide ? "680px" : "280px";
    panel.style.left = `${Math.max(16, Math.min(rect.left, window.innerWidth - (wide ? 696 : 296)))}px`;
    panel.style.bottom = `${Math.max(16, window.innerHeight - rect.top + 8)}px`;
    document.body.appendChild(panel);
    button.setAttribute("aria-expanded", "true");
    return panel;
  };
  const error = (container, message) => {
    container.replaceChildren();
    const text = element("p", message); text.setAttribute("role", "alert"); container.appendChild(text);
    const actions = element("div"); actions.className = "codey-git-actions";
    actions.appendChild(action("关闭", () => { close(); button.focus(); }));
    container.appendChild(actions);
    actions.querySelector("button")?.focus();
  };
  const preview = async () => {
    if (busy || !sessionId) return;
    const current = sessionId, epoch = generation;
    const container = makePanel(true);
    busy = true;
    button.disabled = true;
    button.dataset.busy = "true";

    const loading = element("div"); loading.className = "codey-git-loading";
    const spinner = element("span"); spinner.className = "codey-git-loading-spinner";
    loading.appendChild(spinner);
    loading.appendChild(element("span", "正在分析当前对话改动并生成提交说明…"));
    container.appendChild(loading);

    const loadingActions = element("div"); loadingActions.className = "codey-git-actions";
    const cancelInitial = action("取消", () => { close(); button.focus(); });
    cancelInitial.className = "codey-git-btn codey-git-btn-cancel";
    loadingActions.appendChild(cancelInitial);
    container.appendChild(loadingActions);
    cancelInitial.focus();

    try {
      const result = await call("/api/conversation_git_preview", { sessionId: current });
      if (!valid(current, epoch) || panel !== container) return;
      if (!result?.token || !result.message || !Array.isArray(result.files) || !result.diff) {
        throw new Error("提交预览不完整，请重试");
      }
      container.replaceChildren();

      const header = element("div"); header.className = "codey-git-header";
      header.appendChild(element("strong", `提交到 ${result.branch}`));
      if (result.remote && result.upstreamBranch) {
        const remoteInfo = element("span", ` · 推送到 ${result.remote}/${result.upstreamBranch}`);
        remoteInfo.className = "codey-git-remote-info";
        header.appendChild(remoteInfo);
      }
      container.appendChild(header);

      const filesHeader = element("div", `变更文件 (${result.files.length})`);
      filesHeader.className = "codey-git-section-title";
      container.appendChild(filesHeader);

      const files = element("ul"); files.className = "codey-git-file-list";
      result.files.forEach((file) => files.appendChild(element("li", file)));
      container.appendChild(files);

      if (Array.isArray(result.partialFiles) && result.partialFiles.length) {
        const partial = element("p", `以下文件仅提交本对话的改动块，其他改动继续保留在工作区：${result.partialFiles.join("、")}`);
        partial.className = "codey-git-partial-notice";
        container.appendChild(partial);
      }

      const logHeader = element("div", "提交说明");
      logHeader.className = "codey-git-section-title";
      container.appendChild(logHeader);

      const logPre = element("pre", result.message);
      logPre.className = "codey-git-commit-msg";
      container.appendChild(logPre);

      const details = element("details"); details.open = true;
      details.appendChild(element("summary", "本次完整改动"));
      details.appendChild(element("pre", result.diff));
      container.appendChild(details);

      const actions = element("div"); actions.className = "codey-git-actions";
      const cancel = action("取消", () => { close(); button.focus(); });
      cancel.className = "codey-git-btn codey-git-btn-cancel";

      const commitAction = async (push) => {
        if (busy) return;
        if (!valid(current, epoch)) { close(); return; }
        busy = true;
        cancel.disabled = true;
        commitOnly.disabled = true;
        commitAndPush.disabled = true;
        button.disabled = true;
        button.dataset.busy = "true";
        if (push) {
          commitAndPush.textContent = "正在提交并推送…";
        } else {
          commitOnly.textContent = "正在提交…";
        }
        try {
          const outcome = await call("/api/conversation_git_execute", {
            sessionId: current,
            token: result.token,
            push,
          });
          if (panel === container) {
            container.replaceChildren(element("p", outcome.message || "Git 操作已结束"));
            if (outcome.commit) container.appendChild(element("code", outcome.commit));
            const closeActions = element("div"); closeActions.className = "codey-git-actions";
            closeActions.appendChild(action("关闭", () => { close(); button.focus(); }));
            container.appendChild(closeActions);
            closeActions.querySelector("button")?.focus();
          }
        } catch (failure) {
          if (panel === container) error(container, String(failure?.message || failure));
        } finally {
          busy = false;
          button.disabled = false;
          delete button.dataset.busy;
          void refresh();
        }
      };

      const commitOnly = action("提交", () => void commitAction(false));
      commitOnly.className = "codey-git-btn codey-git-btn-secondary";

      const commitAndPush = action("提交并推送", () => void commitAction(true));
      commitAndPush.className = "codey-git-btn codey-git-btn-primary";

      actions.append(cancel, commitOnly, commitAndPush);
      container.appendChild(actions);
      commitAndPush.focus();
    } catch (failure) {
      if (valid(current, epoch) && panel === container) error(container, String(failure?.message || failure));
    } finally {
      busy = false;
      button.disabled = false;
      delete button.dataset.busy;
    }
  };
  button.addEventListener("click", (event) => {
    event.preventDefault(); event.stopPropagation();
    if (busy || !status?.visible) return;
    if (panel) { close(); return; }
    void preview();
  });
  const refresh = async () => {
    const current = context();
    if (current?.sessionId !== sessionId) {
      sessionId = current?.sessionId || null; generation += 1; status = null; close(); button.style.display = "none";
    }
    if (!enabled || !sessionId || !current?.target) { button.style.display = "none"; return; }
    if (checking || busy || document.hidden) return;
    const epoch = generation, selected = sessionId;
    checking = true;
    try {
      const result = await call("/api/conversation_git_status", { sessionId: selected });
      if (!valid(selected, epoch)) return;
      status = result;
      const target = context()?.target;
      if (!result?.visible || !target) { button.style.display = "none"; return; }
      const optimizer = document.getElementById("codey-prompt-optimize-button");
      const anchor = optimizer?.parentElement === target.host && optimizer.style.display !== "none" ? optimizer : target.anchor;
      if (anchor.nextElementSibling !== button) target.host.insertBefore(button, anchor.nextElementSibling);
      button.style.display = "inline-flex";
    } catch (failure) {
      if (valid(selected, epoch)) { status = { visible: false, reason: String(failure?.message || failure) }; button.style.display = "none"; }
    } finally { checking = false; }
  };
  const schedule = () => {
    // 导航时同步隐藏旧按钮，再延迟查询，防止旧对话的预览进入新对话。
    if (context()?.sessionId !== sessionId) { generation += 1; status = null; close(); button.style.display = "none"; }
    if (!enabled || timer) return;
    timer = setTimeout(() => { timer = 0; void refresh(); }, 300);
  };
  const load = async () => {
    try {
      const config = await call("/settings/get", {});
      enabled = config?.conversationGit?.enabled === true; ready = true;
      generation += 1; status = null; close(); button.style.display = "none";
      await refresh();
    } catch { ready = false; enabled = false; button.style.display = "none"; }
  };
  window.addEventListener("codey:config-changed", load);
  for (const event of ["popstate", "hashchange", "focus"]) window.addEventListener(event, schedule);
  document.addEventListener("visibilitychange", schedule);
  document.addEventListener("keydown", (event) => {
    if (!panel) return;
    if (event.key === "Escape" && !busy) { close(); button.focus(); }
    if (event.key === "Tab") {
      const controls = [...panel.querySelectorAll("button:not(:disabled), summary")];
      const first = controls[0], last = controls.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    }
  });
  document.addEventListener("pointerdown", (event) => {
    if (panel && !busy && !panel.contains(event.target) && !button.contains(event.target)) close();
  });
  const mutationHandler = (mutations) => {
    if (mutations.some((mutation) => !mutation.target?.closest?.(`#${id}, #${id}-panel`))) schedule();
  };
  const options = { childList: true, subtree: true, attributes: true, attributeFilter: ["data-above-composer-conversation-id"] };
  if (window.__codeyMutationDispatcher?.subscribe) window.__codeyMutationDispatcher.subscribe(mutationHandler, options);
  else new MutationObserver(mutationHandler).observe(document.documentElement, options);
  setInterval(() => { if (!ready) void load(); else if (enabled) void refresh(); }, 10_000);
  window.__codeyConversationGit = { snapshot: () => ({ ready, enabled, sessionId, visible: button.style.display !== "none", reason: status?.reason || "", busy }) };
  void load();
})();
