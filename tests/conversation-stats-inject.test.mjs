import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

import { flushTimers } from "./helpers/flush.mjs";
import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(
  new URL("../public/conversation-stats-inject.js", import.meta.url),
  "utf8",
);

class FakeElement extends FakeElementCore {
  constructor(tagName = "div", { visible = true } = {}) {
    super(tagName);
    this.visible = visible;
  }

  matches(selector) {
    const match = selector.match(/^(\w+)?\.([\w-]+)$/);
    if (match) return (!match[1] || this.tagName === match[1].toUpperCase())
      && String(this.className || "").split(/\s+/).includes(match[2]);
    return super.matches(selector);
  }

  focus() {
    if (this.ownerDocument) this.ownerDocument.activeElement = this;
  }

  getClientRects() {
    return this.visible ? [{ bottom: 10, left: 0, right: 10, top: 0 }] : [];
  }

  getBoundingClientRect() {
    return this.visible
      ? { bottom: 160, height: 60, left: 0, right: 600, top: 100, width: 600 }
      : { bottom: 0, height: 0, left: 0, right: 0, top: 0, width: 0 };
  }
}

const createEnvironment = (options = {}) => {
  const documentElement = new FakeElement("html");
  const body = new FakeElement("body");
  const scope = new FakeElement("div");
  const anchor = new FakeElement("div", { visible: options.anchorVisible !== false });
  anchor.setAttribute(
    "data-above-composer-conversation-id",
    options.conversationId ?? "conversation-1",
  );
  const textarea = new FakeElement("textarea", {
    visible: options.inputVisible !== false,
  });
  documentElement.appendChild(body);
  body.appendChild(scope);
  if (options.anchor !== false) {
    scope.appendChild(anchor);
    scope.appendChild(textarea);
  }

  const findById = (root, id) => {
    if (root.id === id || root.getAttribute?.("id") === id) return root;
    for (const child of root.children || []) {
      const found = findById(child, id);
      if (found) return found;
    }
    return null;
  };

  let latestObserver = null;
  class FakeMutationObserver {
    constructor(callback) {
      this.callback = callback;
      this.observed = false;
      latestObserver = this;
    }

    observe() {
      this.observed = true;
    }

    disconnect() {
      this.observed = false;
    }
  }

  const document = {
    body,
    documentElement,
    visibilityState: "visible",
    activeElement: body,
    createElement: (tagName) => Object.assign(new FakeElement(tagName), { ownerDocument: document }),
    getElementById: (id) => findById(documentElement, id),
    querySelector: (selector) => documentElement.querySelector(selector),
    querySelectorAll: (selector) => documentElement.querySelectorAll(selector),
    listeners: new Map(),
    addEventListener: FakeElementCore.prototype.addEventListener,
    dispatchEvent: FakeElementCore.prototype.dispatchEvent,
  };
  for (const element of [documentElement, body, scope, anchor, textarea]) element.ownerDocument = document;

  const location = {
    pathname: options.pathname ?? "/",
    search: options.search ?? "",
  };

  const statsBySession = new Map(Object.entries(options.statsBySession ?? {}));
  const calls = [];
  const bridge = async (path, payload) => {
    calls.push({ path, payload });
    if (path === "/session/stats") {
      if (options.request) return options.request(payload.sessionId);
      return {
        status: "ok",
        stats: statsBySession.get(payload.sessionId) ?? null,
      };
    }
    return { status: "ok" };
  };

  const window = {
    __codexSessionDeleteBridge: options.bridgeReady === false ? undefined : bridge,
    innerHeight: 800,
    innerWidth: 1000,
    location,
    addEventListener: () => {},
  };

  const testSetTimeout = (callback, delay, ...args) => {
    const timer = setTimeout(callback, delay, ...args);
    timer.unref?.();
    return timer;
  };
  const sandbox = {
    document,
    window,
    location,
    URLSearchParams,
    MutationObserver: FakeMutationObserver,
    HTMLElement: FakeElement,
    setTimeout: testSetTimeout,
    clearTimeout,
  };
  const context = vm.createContext(sandbox);
  vm.runInContext(source, context);
  const observer = latestObserver;

  return {
    anchor,
    textarea,
    document,
    scope,
    calls,
    statsBySession,
    bar: () => findById(documentElement, "codey-conversation-stats"),
    snapshot: () => context.window.__codeyConversationStats?.snapshot(),
    refresh: () => context.window.__codeyConversationStats.refresh(),
    setStats: (sessionId, value) => statsBySession.set(sessionId, value),
    setConversation: (id) => {
      anchor.setAttribute("data-above-composer-conversation-id", id);
    },
    emitMutation: (mutations = [{ type: "childList", target: documentElement }]) => {
      if (observer?.observed) observer.callback(mutations);
    },
    emitAnchorMutation: () => {
      observer?.callback?.([
        { type: "attributes", target: anchor, attributeName: "data-above-composer-conversation-id" },
      ]);
    },
    rerun: () => vm.runInContext(source, context),
  };
};

const STATS = {
  usageSource: "runtime",
  usageScope: "runtime",
  requestsScope: "runtime",
  model: "gpt-5",
  requests: 2,
  inFlight: 0,
  lastStatus: "succeeded",
  outputTokensPerSecond: 187.52,
  inputTokens: 128_400,
  outputTokens: 3_100,
  cachedInputTokens: 120_000,
  cacheWriteInputTokens: 2_000,
  totalTokens: 131_500,
  updatedAtUnixMs: 1_760_000_000_000,
};

const metric = (bar, name) => name === "status" ? bar.dataset.status : bar.querySelector(`.codey-cs-${name}`).textContent;
const field = (bar, name) => bar.querySelector(`[data-field="${name}"]`).textContent;
const summary = (env) => env.bar().querySelector("button.codey-cs-summary");
const panel = (env) => env.document.getElementById("codey-conversation-stats-activity");
const click = (element) => element.dispatchEvent({ type: "click" });
const assertExpanded = (env, expanded) => {
  assert.equal(summary(env).getAttribute("aria-expanded"), String(expanded));
  assert.equal(panel(env).hidden, !expanded);
};

test("mounts the stats bar under the composer and renders speed/cache/total", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);

  const bar = env.bar();
  assert.ok(bar, "stats bar should exist");
  assert.equal(bar.parentElement, env.scope);
  assert.equal(bar.hidden, false);
  assert.equal(metric(bar, "speed"), "188 tok/s");
  assert.equal(metric(bar, "cache-rate"), "93%");
  assert.equal(metric(bar, "total"), "132k tok");
  assert.equal(field(bar, "outputTokens"), "3,100 tok");
  assert.equal(summary(env).querySelectorAll(".codey-cs-metric").length, 2);
  for (const selector of [".codey-cs-meta", ".codey-cs-status", '[data-field="model"]']) {
    assert.equal(summary(env).querySelector(selector), null);
  }
  assert.equal(field(bar, "totalTokens"), "131,500 tok");
  assert.equal(field(bar, "cachedInputTokens"), "120,000 tok");
  assert.match(summary(env).getAttribute("aria-label"), /会话统计/);
  assert.match(summary(env).getAttribute("aria-label"), /查看统计详情/);
  assert.equal(
    env.calls.filter((call) => call.path === "/session/stats").at(-1).payload.sessionId,
    "conversation-1",
  );
});

test("keeps the bar visible while the current conversation has no stats", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  assert.equal(env.bar()?.hidden, false);
  assert.equal(metric(env.bar(), "status"), "暂无统计");
  assert.equal(metric(env.bar(), "total"), "—");
});

test("shows unknown values when upstream omits speed and cache", async () => {
  const env = createEnvironment({
    statsBySession: {
      "conversation-1": {
        ...STATS,
        outputTokensPerSecond: null,
        cachedInputTokens: null,
        inputTokens: null,
      },
    },
  });
  await flushTimers(30);
  const bar = env.bar();
  assert.equal(bar.hidden, false);
  assert.equal(metric(bar, "speed"), "—");
  assert.equal(metric(bar, "cache-rate"), "—");
  assert.equal(field(bar, "cachedInputTokens"), "—");
  assert.equal(metric(bar, "total"), "132k tok");
});

test("switches stats context when the conversation changes", async () => {
  const env = createEnvironment({
    statsBySession: {
      "conversation-1": STATS,
      "conversation-2": { ...STATS, totalTokens: 900, model: "claude-sonnet" },
    },
  });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "132k tok");

  env.setConversation("conversation-2");
  env.emitAnchorMutation();
  await flushTimers(200);

  const bar = env.bar();
  assert.equal(metric(bar, "total"), "900 tok");
  assert.equal(env.calls.at(-1).payload.sessionId, "conversation-2");
});

test("clears stale numbers immediately on a session switch before refetch", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  assert.equal(env.bar().hidden, false);

  env.setConversation("conversation-3");
  env.emitAnchorMutation();
  await flushTimers(160);

  const bar = env.bar();
  assert.equal(bar.hidden, false);
  assert.equal(metric(bar, "total"), "—", "previous session totals must be cleared");
  assert.equal(metric(bar, "status"), "暂无统计");
  assert.match(summary(env).getAttribute("aria-label"), /会话统计/);
  assert.doesNotMatch(summary(env).getAttribute("aria-label"), /132k/);
});

test("remounts the bar after Codex rebuilds the composer DOM", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  const bar = env.bar();
  assert.ok(bar);
  bar.remove();
  env.emitMutation([
    { type: "childList", target: env.scope, removedNodes: [bar], addedNodes: [] },
  ]);
  await flushTimers(200);
  const remounted = env.bar();
  assert.ok(remounted, "bar should be re-created after removal");
  assert.equal(remounted.parentElement, env.scope);
  assert.equal(remounted.hidden, false);
});

test("marks in-flight requests as running", async () => {
  const env = createEnvironment({
    statsBySession: { "conversation-1": { ...STATS, inFlight: 1 } },
  });
  await flushTimers(30);
  const bar = env.bar();
  assert.equal(bar.hidden, false);
  assert.equal(bar.dataset.running, "true");
});

test("shows unsuccessful request states and prioritizes active requests", async () => {
  const env = createEnvironment();
  for (const [lastStatus, label, detail] of [
    ["failed", "最近请求失败", "失败"],
    ["incomplete", "最近请求未完成", "未完成"],
    ["cancelled", "最近请求已取消", "已取消"],
  ]) {
    env.setStats("conversation-1", { ...STATS, lastStatus });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "status"), label);
    assert.ok(summary(env).getAttribute("aria-label").includes(label));
  }
  env.setStats("conversation-1", { ...STATS, lastStatus: "failed", inFlight: 1 });
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "status"), "1 个进行中");
});

test("ignores hidden composer anchors and stays empty without a session", async () => {
  const env = createEnvironment({ inputVisible: false });
  await flushTimers(30);
  assert.equal(env.snapshot().sessionId, "");
});

test("does not double-install on repeated injection", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  // 同一上下文再次执行整个脚本：installed 标记应阻止重复挂载。
  env.rerun();
  await flushTimers(30);
  const count = env.scope.children.filter(
    (child) => child.id === "codey-conversation-stats",
  ).length;
  assert.equal(count, 1);
  assert.equal(env.snapshot().installed !== false, true);
});

test("keeps a waiting bar without a session and does not request stats", async () => {
  const env = createEnvironment({
    conversationId: "", pathname: "/c/previous-conversation",
    statsBySession: { "previous-conversation": STATS },
  });
  await flushTimers(30);
  assert.equal(env.bar().hidden, false);
  assert.equal(metric(env.bar(), "status"), "等待会话");
  assert.equal(metric(env.bar(), "total"), "—");
  assert.equal(env.calls.length, 0);
  click(summary(env));
  assertExpanded(env, true);
});

test("preserves reported zero tokens and zero cache hit rate", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": {
    requests: 1, totalTokens: 0, inputTokens: 100, outputTokens: 0,
    cachedInputTokens: 0, cacheWriteInputTokens: 0, inFlight: 0,
  } } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "0 tok");
  assert.equal(field(env.bar(), "outputTokens"), "0 tok");
  assert.equal(metric(env.bar(), "cache-rate"), "0%");
  assert.equal(metric(env.bar(), "status"), "运行期统计");
});

for (const stats of [{}, { requests: 0 }]) {
  test(`treats ${JSON.stringify(stats)} as an empty session`, async () => {
    const env = createEnvironment({ statsBySession: { "conversation-1": stats } });
    await flushTimers(30);
    assert.equal(env.bar().hidden, false);
    assert.equal(metric(env.bar(), "status"), "暂无统计");
    assert.equal(metric(env.bar(), "total"), "—");
  });
}

test("pins each independent panel and switches between both groups", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  assert.equal(summary(env).getAttribute("aria-controls"), panel(env).id);
  click(summary(env));
  assertExpanded(env, true);
  const tokens = env.bar().querySelector(".codey-cs-tokens-summary");
  click(tokens);
  assertExpanded(env, false);
  assert.equal(tokens.getAttribute("aria-expanded"), "true");
  click(tokens);
  assert.equal(tokens.getAttribute("aria-expanded"), "false");
});

test("centers the details immediately above the trigger and clamps the viewport", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  summary(env).getBoundingClientRect = () => ({ left: 200, top: 700, width: 400, height: 28 });
  panel(env).getBoundingClientRect = () => ({ width: 420, height: 300 });
  env.textarea.getBoundingClientRect = () => ({ left: 100, top: 520, width: 600, height: 140 });
  click(summary(env));
  assert.equal(panel(env).style.left, "190px");
  assert.equal(panel(env).style.top, "392px");
  click(summary(env));
  summary(env).getBoundingClientRect = () => ({ left: 0, top: 700, width: 200, height: 28 });
  click(summary(env));
  assert.equal(panel(env).style.left, "12px");
});

test("Escape restores focus only when focus was inside the stats bar", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  for (const inside of [true, false]) {
    click(summary(env));
    const target = inside ? summary(env) : env.textarea;
    target.focus();
    let prevented = false;
    let stopped = false;
    env.document.dispatchEvent({
      type: "keydown", key: "Escape",
      preventDefault: () => { prevented = true; },
      stopPropagation: () => { stopped = true; },
    });
    assertExpanded(env, false);
    assert.equal(env.document.activeElement, inside ? summary(env) : env.textarea);
    assert.equal(prevented, inside);
    assert.equal(stopped, inside);
  }
});

test("pointerdown inside details keeps them open, outside closes them", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  click(summary(env));
  env.document.dispatchEvent({ type: "pointerdown", target: panel(env).querySelector("dd") });
  assertExpanded(env, true);
  env.textarea.focus();
  env.document.dispatchEvent({ type: "pointerdown", target: env.textarea });
  assertExpanded(env, false);
  assert.equal(env.document.activeElement, env.textarea);
});

test("hiding the composer closes its open details", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  click(summary(env));
  env.textarea.visible = false;
  env.textarea.setAttribute("hidden", "");
  env.emitMutation([{ type: "attributes", target: env.textarea, attributeName: "hidden" }]);
  await flushTimers(160);
  assert.equal(env.bar().hidden, true);
  assertExpanded(env, false);
});

test("keeps current totals on failed refresh and recovers after retry", async () => {
  let response = { status: "ok", stats: STATS };
  const env = createEnvironment({ request: () => {
    if (response instanceof Error) throw response;
    return response;
  } });
  await flushTimers(30);
  for (const failure of [{ status: "failed" }, new Error("offline")]) {
    response = failure;
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "total"), "132k tok");
    assert.equal(metric(env.bar(), "status"), "统计暂不可用");
    assert.equal(env.bar().hidden, false);
  }
  response = { status: "ok", stats: { ...STATS, totalTokens: 200 } };
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "200 tok");
  assert.equal(metric(env.bar(), "status"), "运行期统计");
});

test("discards the original A response after switching A to B to A", async () => {
  const pending = [];
  const env = createEnvironment({ request: (sessionId) => new Promise((resolve) => {
    pending.push({ sessionId, resolve });
  }) });
  await flushTimers(30);
  assert.equal(pending[0].sessionId, "conversation-1");
  click(summary(env));
  env.setConversation("conversation-2");
  env.emitAnchorMutation();
  await flushTimers(160);
  assertExpanded(env, false);
  env.setConversation("conversation-1");
  env.emitAnchorMutation();
  await flushTimers(160);
  pending[0].resolve({ status: "ok", stats: STATS });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "—", "old A request must not repopulate A after a round trip");
  assert.equal(pending.length, 2, "current A must fetch again immediately after the stale request settles");
  assert.equal(pending[1].sessionId, "conversation-1");
  pending[1].resolve({ status: "ok", stats: { ...STATS, totalTokens: 700 } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "700 tok");
});

test("restores historical totals without inventing historical request information", async () => {
  const history = { ...STATS, usageSource: "rollout", usageScope: "session", requestsScope: null, requests: null, inFlight: null, cacheWriteInputTokens: null };
  const env = createEnvironment({ statsBySession: { "conversation-1": history } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "132k tok");
  assert.equal(metric(env.bar(), "status"), "历史统计");
  assert.match(env.bar().querySelector(".codey-cs-tokens-summary").getAttribute("aria-label"), /会话累计 132k/);
  assert.equal(env.bar().dataset.running, "false");
});

test("historical tokens and current runtime request details keep separate scopes", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS, usageSource: "rollout", usageScope: "session" } } });
  await flushTimers(30);
  assert.equal(field(env.bar(), "totalTokens"), "131,500 tok");
  assert.equal(metric(env.bar(), "speed"), "188 tok/s");
  env.setStats("conversation-1", { ...STATS, usageSource: "rollout", usageScope: "session", outputTokensPerSecond: null });
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "—");
});

test("shows recorded historical speed independently of runtime request fields", async () => {
  const history = {
    usageSource: "rollout", usageScope: "session", requestsScope: null,
    totalTokens: 131500, requests: null, model: null, lastStatus: null,
    outputTokensPerSecond: 62.35, speedSource: "request_log",
    speedScope: "latest_recorded_request", speedRecordedAtUnixMs: 1_760_000_000_000,
  };
  const env = createEnvironment({ statsBySession: { "conversation-1": history } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "62.4 tok/s");
  assert.match(summary(env).getAttribute("aria-label"), /可能早于最近请求/);
  env.setStats("conversation-1", { ...history, totalTokens: null, usageSource: "unknown", usageScope: "unknown" });
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "62.4 tok/s");
  assert.equal(env.bar().dataset.state, "ok");
  assert.match(env.bar().querySelector(".codey-cs-tokens-summary").getAttribute("aria-label"), /统计范围未知/);
  assert.equal(metric(env.bar(), "status"), "历史速度记录");
});

test("recorded speed has its own scope and is hidden while a new request runs", async () => {
  const data = { ...STATS, speedSource: "request_log", speedScope: "latest_recorded_request" };
  const env = createEnvironment({ statsBySession: { "conversation-1": data } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "188 tok/s");
  for (const changes of [{ inFlight: 1 }, { outputTokensPerSecond: null }, { speedScope: null }]) {
    env.setStats("conversation-1", { ...data, ...changes });
    env.refresh();
    await flushTimers(30);
    assert.equal(metric(env.bar(), "speed"), "—");
  }
  env.setStats("conversation-1", { ...STATS, speedSource: "runtime", speedScope: "latest_request" });
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "speed"), "188 tok/s");
});

test("legacy backend is explicitly runtime only and cannot claim a latest speed", async () => {
  const legacy = { ...STATS };
  delete legacy.usageSource;
  delete legacy.usageScope;
  delete legacy.requestsScope;
  const env = createEnvironment({ statsBySession: { "conversation-1": legacy } });
  await flushTimers(30);
  assert.match(env.bar().querySelector(".codey-cs-tokens-summary").getAttribute("aria-label"), /本次运行/);
  assert.doesNotMatch(summary(env).getAttribute("aria-label"), /会话累计/);
  assert.equal(metric(env.bar(), "speed"), "—");
});

test("partial historical usage keeps absent values unknown and preserves explicit zero", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": {
    usageSource: "rollout", usageScope: "session", requestsScope: null,
    totalTokens: null, inputTokens: 0, outputTokens: null, cachedInputTokens: null,
  } } });
  await flushTimers(30);
  assert.equal(field(env.bar(), "outputTokens"), "—");
  for (const name of ["total", "cache-rate", "speed"]) assert.equal(metric(env.bar(), name), "—");
  assert.equal(metric(env.bar(), "status"), "历史统计");
});

test("normalizes local conversation IDs before reading historical stats", async () => {
  const env = createEnvironment({ conversationId: " local:conversation-1 ", statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);
  assert.equal(env.calls[0].payload.sessionId, "conversation-1");
  assert.equal(metric(env.bar(), "total"), "132k tok");
});

test("preserves the same session values while refreshing and marks failed values stale", async () => {
  let resolveRefresh;
  let first = true;
  const env = createEnvironment({ request: () => {
    if (first) { first = false; return { status: "ok", stats: STATS }; }
    return new Promise((resolve) => { resolveRefresh = resolve; });
  } });
  await flushTimers(30);
  env.refresh();
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "132k tok");
  assert.equal(metric(env.bar(), "status"), "更新统计中");
  resolveRefresh({ status: "failed" });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "total"), "132k tok");
  assert.match(env.bar().querySelector(".codey-cs-note").textContent, /可能已过期/);
});

test("shows reference activity rows and computes uncached input without double counting", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS,
    turnCount: 4, stepCount: 209, modelDurationMs: 1286000, toolDurationMs: 573000,
    averageTtftMs: 2800, activityScope: "recorded_session", timingScope: "recorded_requests",
  } } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "activity"), "4 轮 209 步");
  assert.equal(field(env.bar(), "modelDurationMs"), "21分 26秒");
  assert.equal(field(env.bar(), "toolDurationMs"), "9分 33秒");
  assert.equal(field(env.bar(), "averageTtftMs"), "2.8秒");
  assert.equal(field(env.bar(), "uncachedInputTokens"), "8,400 tok");
  assert.equal(env.bar().querySelectorAll(".codey-cs-summary").length, 2);
});

test("preview remains open during pointer travel and click pins it", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  summary(env).dispatchEvent({ type: "pointerenter" });
  assertExpanded(env, true);
  summary(env).dispatchEvent({ type: "pointerleave" });
  panel(env).dispatchEvent({ type: "pointerenter" });
  await flushTimers(380);
  assertExpanded(env, true);
  panel(env).dispatchEvent({ type: "pointerleave" });
  await flushTimers(380);
  assertExpanded(env, false);
  summary(env).dispatchEvent({ type: "focus" });
  click(summary(env));
  summary(env).dispatchEvent({ type: "pointerleave" });
  await flushTimers(380);
  assertExpanded(env, true);
});

test("does not fabricate missing activity or subtract inconsistent cache", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS,
    turnCount: 0, stepCount: null, toolDurationMs: 0, modelDurationMs: -1,
    averageTtftMs: null, cachedInputTokens: 999999,
  } } });
  await flushTimers(30);
  assert.equal(metric(env.bar(), "activity"), "0 轮 — 步");
  assert.equal(field(env.bar(), "toolDurationMs"), "0秒");
  for (const key of ["modelDurationMs", "averageTtftMs", "uncachedInputTokens", "cacheRate"]) assert.equal(field(env.bar(), key), "—");
});

test("explains independently sourced steps and accumulated tool timing", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": { ...STATS,
    turnCount: 4, stepCount: 2, activityScope: "recorded_session", stepScope: "runtime",
    timingScope: "recorded_requests", toolTimingScope: "recorded_tools",
  } } });
  await flushTimers(30);
  const note = env.bar().querySelector(".codey-cs-activity-note").textContent;
  assert.match(note, /步数仅覆盖本次运行/);
  assert.match(note, /有有效计时的请求/);
  assert.match(note, /工具用时为累计值，并行调用可能重叠/);
});
