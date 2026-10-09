import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { FakeElementCore } from "./helpers/fake-element.mjs";

const source = readFileSync(new URL("../public/conversation-git.js", import.meta.url), "utf8");
const flush = async () => { for (let i = 0; i < 15; i++) await Promise.resolve(); };
const deferred = () => { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; };

function harness({ enabled = true, visible = true, optimizer = true, preview, execute } = {}) {
  class Element extends FakeElementCore {
    replaceChildren(...children) { [...this.children].forEach((child) => child.remove()); this.append(...children); }
    getBoundingClientRect() { return { left: 100, top: 600 }; }
    focus() { document.activeElement = this; }
  }
  const document = new Element("document");
  document.createElement = (tag) => new Element(tag);
  document.documentElement = new Element("html");
  document.body = new Element("body");
  document.documentElement.appendChild(document.body);
  document.getElementById = (id) => document.documentElement.querySelector(`#${id}`);
  const host = new Element("div"), anchor = new Element("button");
  host.appendChild(anchor); document.body.appendChild(host);
  const optimize = new Element("button"); optimize.id = "codey-prompt-optimize-button";
  if (optimizer) host.appendChild(optimize);
  const state = { sessionId: "session-a", enabled, visible };
  const window = new Element("window");
  window.innerWidth = 1200; window.innerHeight = 800;
  window.__codeyPromptOptimize = { composerContext: () => ({ sessionId: state.sessionId, target: { host, anchor } }) };
  const calls = [];
  window.__codexSessionDeleteBridge = async (path, payload) => {
    calls.push({ path, payload });
    if (path === "/settings/get") return { conversationGit: { enabled: state.enabled } };
    if (path.endsWith("_status")) return { visible: state.visible, reason: "没有文件改动" };
    if (path.endsWith("_preview")) return preview ? preview(payload) : {
      token: "preview-token", files: ["owned.txt"], diff: "-old\n+new", message: "fix(conversation-git): 校验当前对话提交范围", branch: "main",
    };
    if (path.endsWith("_execute")) return execute ? execute(payload) : {
      status: payload?.push === false ? "committed" : "pushed",
      message: payload?.push === false ? "当前对话文件已提交到本地仓库" : "当前对话文件已提交并推送",
      commit: "abc123",
    };
    throw new Error(`unexpected path: ${path}`);
  };
  const timers = [], intervals = [];
  let mutation;
  window.__codeyMutationDispatcher = { subscribe(handler) { mutation = handler; } };
  vm.runInNewContext(source, { window, document, setTimeout: (fn) => { timers.push(fn); return timers.length; }, setInterval: (fn) => intervals.push(fn), console });
  return {
    window, document, calls, state, host, optimize,
    button: () => document.getElementById("codey-conversation-git"),
    panel: () => document.getElementById("codey-conversation-git-panel"),
    async tick() { intervals.forEach((fn) => fn()); await flush(); },
    async navigate(id) { state.sessionId = id; mutation([{ target: host }]); timers.splice(0).forEach((fn) => fn()); await flush(); },
    async click(node) { node.dispatchEvent({ type: "click", preventDefault() {}, stopPropagation() {} }); await flush(); },
    async load() { await flush(); },
  };
}

test("shows only enabled conversations with backend-confirmed Git changes beside optimize", async () => {
  for (const [enabled, visible] of [[false, true], [true, false], [true, true]]) {
    const env = harness({ enabled, visible }); await env.load();
    assert.equal(env.window.__codeyConversationGit.snapshot().visible, enabled && visible);
    if (enabled && visible) assert.equal(env.optimize.nextElementSibling, env.button());
    if (!enabled) assert.equal(env.calls.filter((call) => call.path.endsWith("_status")).length, 0);
  }
});

test("works when prompt optimization is disabled and requires preview confirmation before execution", async () => {
  const env = harness({ optimizer: false }); await env.load();
  await env.click(env.button());
  assert.deepEqual(env.panel().querySelectorAll("pre").map((node) => node.textContent), ["fix(conversation-git): 校验当前对话提交范围", "-old\n+new"]);
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
  const actionLabels = env.panel().querySelectorAll("button").map((node) => node.textContent);
  assert.ok(actionLabels.includes("取消"));
  assert.ok(actionLabels.includes("提交"));
  assert.ok(actionLabels.includes("提交并推送"));
  const confirm = env.panel().querySelectorAll("button").find((node) => node.textContent === "提交并推送");
  await env.click(confirm);
  const execution = env.calls.find((call) => call.path.endsWith("_execute"));
  assert.equal(JSON.stringify(execution.payload), JSON.stringify({ sessionId: "session-a", token: "preview-token", push: true }));
  assert.equal(env.panel().querySelector("p").textContent, "当前对话文件已提交并推送");
});

test("allows user to choose commit only without pushing", async () => {
  const env = harness(); await env.load();
  await env.click(env.button());
  const commitOnly = env.panel().querySelectorAll("button").find((node) => node.textContent === "提交");
  await env.click(commitOnly);
  const execution = env.calls.find((call) => call.path.endsWith("_execute"));
  assert.equal(JSON.stringify(execution.payload), JSON.stringify({ sessionId: "session-a", token: "preview-token", push: false }));
  assert.equal(env.panel().querySelector("p").textContent, "当前对话文件已提交到本地仓库");
});

test("opening an old conversation shows recovered changes without requiring a new edit", async () => {
  const env = harness({ visible: false }); await env.load();
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
  env.state.visible = true;
  await env.navigate("old-conversation");
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, true);
  assert.equal(env.optimize.nextElementSibling, env.button());
  assert.equal(env.calls.at(-1).payload.sessionId, "old-conversation");
  env.state.visible = false;
  await env.navigate("conversation-without-changes");
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
});

test("previews shared files as partial commits and requires confirmation", async () => {
  const env = harness({ preview: () => ({ token: "split", files: ["shared.rs"], partialFiles: ["shared.rs"],
    message: "fix(conversation-git): 拆分共享文件改动", diff: "-old\n+mine", branch: "main" }) });
  await env.load(); await env.click(env.button());
  assert.ok(env.panel().querySelectorAll("p").some((node) => node.textContent.includes("其他改动继续保留在工作区：shared.rs")));
  assert.deepEqual(env.panel().querySelectorAll("pre").map((node) => node.textContent), ["fix(conversation-git): 拆分共享文件改动", "-old\n+mine"]);
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
});

test("discards an old conversation's pending preview after navigation", async () => {
  const pending = deferred();
  const env = harness({ preview: () => pending.promise }); await env.load();
  await env.click(env.button());
  await env.navigate("session-b");
  pending.resolve({ token: "old", files: ["old.txt"], message: "旧对话", diff: "old diff" }); await flush();
  assert.equal(env.panel(), null);
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
});

test("renders model and stale-file failures as text without executing or changing the composer", async () => {
  const env = harness({ preview: () => ({ status: "failed", message: "文件已变化，请重新预览 <script>" }) }); await env.load();
  await env.click(env.button());
  assert.equal(env.panel().querySelector("[role=alert]").textContent, "文件已变化，请重新预览 <script>");
  assert.equal(env.calls.some((call) => call.path.endsWith("_execute")), false);
});

test("keeps the local commit outcome visible when remote push is rejected and prevents double submission", async () => {
  const pending = deferred();
  const env = harness({ execute: () => pending.promise }); await env.load();
  await env.click(env.button());
  const confirm = env.panel().querySelectorAll("button").find((node) => node.textContent === "提交并推送");
  await env.click(confirm); await env.click(confirm);
  assert.equal(env.calls.filter((call) => call.path.endsWith("_execute")).length, 1);
  pending.resolve({ status: "committed", commit: "abc", message: "本地提交已生成，但推送失败" }); await flush();
  assert.equal(env.panel().querySelector("p").textContent, "本地提交已生成，但推送失败");
});

test("turning off the enhancement closes previews and hides the button", async () => {
  const env = harness(); await env.load(); await env.click(env.button());
  env.state.enabled = false; env.window.dispatchEvent({ type: "codey:config-changed" }); await flush();
  assert.equal(env.panel(), null);
  assert.equal(env.window.__codeyConversationGit.snapshot().visible, false);
});
