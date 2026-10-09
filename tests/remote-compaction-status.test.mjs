import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("../src/remoteCompactionStatus.ts", import.meta.url), "utf8");
const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText;
const { remoteCompactionStatusText } = await import(`data:text/javascript;base64,${Buffer.from(code).toString("base64")}`);

test("saving remote compaction does not claim it is already active", () => {
  const local = remoteCompactionStatusText({ running: true, remoteCompaction: {
    configured: true, active: false, restartRequired: true, blockingRoutes: [],
  } });
  assert.equal(local.title, "当前使用本地压缩");
  assert.match(local.detail, /已保存配置支持远程压缩.*需重启 Codex/);
  const remote = remoteCompactionStatusText({ running: true, remoteCompaction: {
    configured: false, active: true, restartRequired: true, blockingRoutes: [],
  } });
  assert.equal(remote.title, "当前已启用远程压缩");
  assert.match(remote.detail, /未启用远程压缩.*需重启 Codex/);
});

test("missing status, stopped Codex and unmanaged compression remain distinct", () => {
  assert.equal(remoteCompactionStatusText({ running: true }).title, "压缩方式待确认");
  const remoteCompaction = { configured: true, active: null, restartRequired: false, blockingRoutes: [] };
  assert.equal(remoteCompactionStatusText({ running: false, remoteCompaction }).title, "Codex 未运行");
  assert.equal(remoteCompactionStatusText({ running: true, remoteCompaction }).title, "压缩由 Codex 自行管理");
});

test("mixed routes advertise independent compaction and capability changes require restart", () => {
  const status = { running: true, remoteCompaction: {
    configured: true, active: true, configuredMode: "mixed", activeMode: "mixed",
    restartRequired: true, blockingRoutes: [{ routeName: "Chat", reason: "上游协议不支持原生压缩" }],
  } };
  const text = remoteCompactionStatusText(status);
  assert.equal(text.title, "当前按线路选择压缩方式");
  assert.match(text.detail, /支持的线路独立使用远程压缩/);
  assert.match(text.detail, /需重启 Codex/);
});
