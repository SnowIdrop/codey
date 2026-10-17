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
    createElement: (tagName) => new FakeElement(tagName),
    getElementById: (id) => findById(documentElement, id),
    querySelector: (selector) => documentElement.querySelector(selector),
    querySelectorAll: (selector) => documentElement.querySelectorAll(selector),
    addEventListener: () => {},
  };

  const location = {
    pathname: options.pathname ?? "/",
    search: options.search ?? "",
  };

  const statsBySession = new Map(Object.entries(options.statsBySession ?? {}));
  const calls = [];
  const bridge = async (path, payload) => {
    calls.push({ path, payload });
    if (path === "/session/stats") {
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
    scope,
    calls,
    statsBySession,
    bar: () => findById(documentElement, "codey-conversation-stats"),
    snapshot: () => context.window.__codeyConversationStats?.snapshot(),
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

test("mounts the stats bar under the composer and renders speed/cache/total", async () => {
  const env = createEnvironment({ statsBySession: { "conversation-1": STATS } });
  await flushTimers(30);

  const bar = env.bar();
  assert.ok(bar, "stats bar should exist");
  assert.equal(bar.parentElement, env.scope);
  assert.equal(bar.hidden, false);
  assert.equal(bar.__codeyCs.speed.textContent, "⚡ 188 tok/s");
  assert.equal(bar.__codeyCs.cache.textContent, "缓存 93%");
  assert.equal(bar.__codeyCs.total.textContent, "132k tok");
  assert.match(bar.title, /gpt-5/);
  assert.equal(
    env.calls.filter((call) => call.path === "/session/stats").at(-1).payload.sessionId,
    "conversation-1",
  );
});

test("stays hidden while the current conversation has no stats", async () => {
  const env = createEnvironment();
  await flushTimers(30);
  assert.equal(env.bar()?.hidden, true);
});

test("hides speed and cache segments when upstream reports nothing", async () => {
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
  assert.equal(bar.__codeyCs.speed.textContent, "");
  assert.equal(bar.__codeyCs.cache.textContent, "");
  assert.equal(bar.__codeyCs.total.textContent, "132k tok");
});

test("switches stats context when the conversation changes", async () => {
  const env = createEnvironment({
    statsBySession: {
      "conversation-1": STATS,
      "conversation-2": { ...STATS, totalTokens: 900, model: "claude-sonnet" },
    },
  });
  await flushTimers(30);
  assert.equal(env.bar().__codeyCs.total.textContent, "132k tok");

  env.setConversation("conversation-2");
  env.emitAnchorMutation();
  await flushTimers(200);

  const bar = env.bar();
  assert.equal(bar.__codeyCs.total.textContent, "900 tok");
  assert.match(bar.title, /claude-sonnet/);
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
  assert.equal(bar.hidden, true, "new conversation with no stats should hide the bar");
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
