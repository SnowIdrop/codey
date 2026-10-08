import type { RuntimeStatus } from "./App.types";

export function remoteCompactionStatusText(status: RuntimeStatus) {
  const compaction = status.remoteCompaction;
  if (!compaction) {
    return { title: "压缩方式待确认", detail: "尚未读取到压缩运行状态。" };
  }
  const title = !status.running
    ? "Codex 未运行"
    : compaction.active === null
      ? "压缩由 Codex 自行管理"
      : compaction.active
        ? "当前已启用远程压缩"
        : "当前使用本地压缩";
  const saved = compaction.configured
    ? "已保存配置支持远程压缩"
    : "已保存配置未启用远程压缩";
  return {
    title,
    detail: compaction.restartRequired
      ? `${saved}；需重启 Codex 生效。`
      : `${saved}。`,
  };
}
