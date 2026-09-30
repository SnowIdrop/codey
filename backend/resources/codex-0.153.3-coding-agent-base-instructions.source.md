# codex-0.153.3 coding-agent 基础指令来源

本资源只保存会话元数据中的 session_meta.payload.base_instructions.text，不包含角色、任务、工具、权限或用户上下文。

- CLI 元数据版本：0.153.3。
- 主线程：01a0f2db-fdcd-71c0-9324-b8ccdbeedf51。
- 子代理：01a0f2e0-e98e-7322-b4a6-ce04c9ca1f62、01a0f2e1-3262-7d63-abb6-65328df367d9。
- 来源：本机 ~/.codex/sessions/2026/09/30/ 下三个线程对应的 rollout JSONL 首条 session_meta；分别为 rollout-2026-09-30T23-08-18-01a0f2db-fdcd-71c0-9324-b8ccdbeedf51.jsonl、rollout-2026-09-30T23-13-41-01a0f2e0-e98e-7322-b4a6-ce04c9ca1f62.jsonl、rollout-2026-09-30T23-14-00-01a0f2e1-3262-7d63-abb6-65328df367d9.jsonl。
- 归一化：仅 CRLF 转 LF，然后去除首尾空白。
- 三份归一化内容完全一致，长度为 20,750 个 UTF-16 code units。
- 归一化 SHA-256：ebfcbdce4a6c353e85d6cde37e508b89c77a902bd27771c91caba5fd494bdb83。

证据来自会话元数据，不等同于 CLI 二进制提取或生产请求抓包。路由只对完整归一化模板作精确匹配；缺失、未知、拼接自定义内容及仅嵌入 input 的指令仍在发送上游前拒绝。
