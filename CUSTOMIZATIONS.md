# 个人适配维护

本 fork 为 SnowIdrop/codey，上游为 SuperGness/codey。`develop` 集成 fork 的 `master`、`peropero/customizations` 个人适配和 `codex/workflow-engine-preview` 工作流预览；原三个分支与默认分支设置保持不变。当前上游基线为 `3b4774e64d5d3598776283eb19ec91d3b383b954`。

## 已保留的适配

- 跨线路明文任务、设置持久化与原生模型目录派发修复。第三方 Responses 将协作任务及回报转换为标准 user 消息，保留正文与发送者／接收者，避免上游忽略 Codex 专用消息项；官方账号保留原生格式。
- 子代理思考深度优先读取按线路保存的自定义配置，包括 max。
- 第三方 Responses 的 reasoning_text 400 优先使用已存在的真实明文重放一次，避免失效 opaque 状态遮蔽有效正文；官方线路及压缩请求不采用该回退。
- 由主代理选择同步／异步：`sync_` 或未标注的任务等待整批结束；`async_` 任务身份确认后允许主代理继续独立读写，最终交付前仍须汇合。同一活动批次不得混合模式。
- 显式 `fork_turns="none"` 隔离主会话，任务正文包含问题、已知事实、允许／排除范围和结束证据。快扫首轮三次有界调查调用内返回结果或阶段证据，不自行扩查。不再需要的异步旁路先收已有结果，最多等待一次十秒、核对后必要时中断；这是提示规则，不新增运行时预算或路径沙箱。
- 子代理通常实施代码脚本和结构简单的文本；Prefab 复杂 YAML 等层级、引用、序列化关系复杂的内容由主代理修改。子代理可调查并建议，不得通过脚本或编辑器工具间接写入复杂内容。
- 工作流预览默认关闭，与原生子代理调度增强互斥；复用现有启动注入和控制台，保留任务权限、持久化恢复及可见原生回退。

最后一项及本机调度规则保存在 `customizations/codex-constraints/`，是可编辑的 Codey 约束源文件。代码不会自动把仓库里的这些文件安装到用户配置目录；部署时需同时更新。这里不包含 API Key、个人线路配置、会话数据库、用户级 AGENTS、已移除的 skill 或程序备份。

异步文件范围由任务所有权约定约束，不是按路径强制隔离的操作系统沙箱。同工作区单写入型子代理互斥仍保留。

## 按需合并上游

首次克隆：

```powershell
git clone https://github.com/SnowIdrop/codey.git
Set-Location codey
git remote add upstream https://github.com/SuperGness/codey.git
git config remote.pushDefault origin
git switch develop
```

每次更新先确认工作区干净，再获取并审查上游：

```powershell
git status --short
git fetch upstream
git log --oneline HEAD..upstream/master
git diff HEAD...upstream/master -- backend/src/codex_config_guidance.rs backend/src/subagent_gate.rs backend/src/subagent_orchestrator.rs src/subagentModels.ts
git merge upstream/master
```

冲突应按最终行为合并，不要整文件选择 ours/theirs。尤其检查明文任务、模型目录、设置保存、角色能力、同步／异步门禁，以及仓库约束文件与源码提示是否仍一致。无法完成时可用 `git merge --abort` 取消本次合并，避免覆盖原有本地改动。

完成源码核对、必要编译和已获授权的验证后，再提交冲突解决结果并执行：

```powershell
git push origin develop
```

不要把 GitHub 的 Sync fork 操作当作个人适配分支的无条件更新方式，也不要向上游直接推送。

## 构建和本机部署

Windows 需要 Node/npm、pnpm 11.5.2、Rust MSVC、Visual Studio C++ Build Tools 和 Windows SDK。本机构建环境使用 Rust 1.98.1。

```powershell
npx --yes pnpm@11.5.2 install --frozen-lockfile
npm run check
cargo build --release -p codey --bins --locked
```

每一步成功后再继续。正常退出 Codey 及其管理的 Codex 后，备份原程序和约束文件，再将 `target/release/codey.exe` 与 `target/release/codey-fastctx.exe` 安装到实际 Codey 安装目录，保留原目录配套文件。将 `customizations/codex-constraints/` 中全部八个文件按相对路径复制到 `%APPDATA%/Codey/Codey/config/codex-constraints/`；覆盖前核对本机后续编辑。不要手动编辑其 `runtime/` 生成目录。重新启动 Codey 并新建会话加载规则。

模型、线路和思考深度仍由 Codey 设置页管理，不从其他机器复制路由 ID 或凭据。

## 验证边界

默认仅执行基本编译、源码回读、diff 检查及不修改源码、不连带运行测试的静态检查。测试、运行时和界面验收须单独获准；源码合并与编译通过不代表工作流预览、全部客户端或跨平台行为已经完成验收。

不得为同步分支启用 GitHub Actions 或创建发布标签。当前质量检查仅由 `master` 推送或 PR 触发，桌面打包由版本标签或手动触发；推送 `develop` 前仍须核对实际工作流，避免触发未经批准的测试或发布。

上游 AGPL-3.0 许可证继续适用；分发修改程序时应同时提供相应源码和许可信息。
