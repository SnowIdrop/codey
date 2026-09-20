# 在 GitHub 构建桌面安装包

打开 [Build desktop packages](https://github.com/SnowIdrop/codey/actions/workflows/build-desktop.yml)，点击 **Run workflow**，选择要构建的分支。当前集成版本在 `develop`。

- `platform` 默认 `windows`；也可选择 `macos`（arm64 与 x64）或 `all`。
- `run_checks` 默认关闭，仅执行类型检查、编译与打包；勾选后另行执行完整 JavaScript/Rust 测试、rustfmt 和 Clippy。
- 完成后在运行详情的 **Artifacts** 下载 `codey-windows-x64-installer` 或对应 macOS 压缩包。构建不会安装到本机。

只有 Git 推送权限时，可将待构建提交推到专用分支：

```sh
git push origin develop:refs/heads/build/windows
```

该分支的推送只构建 Windows，默认不运行完整检查。若目标分支已是同一提交，Git 不会创建新运行；可在 Actions 页面重新运行对应记录。普通 `develop` 推送不会触发桌面构建。无需强制推送。

手动构建及 `build/windows` 分支构建只上传 Actions 产物，不创建 Release，也不同步自动更新服务。原有 `v*` 标签推送继续执行全平台检查、打包及发布流程。

工作流配置同时维护在 `develop` 和仓库默认分支 `peropero/customizations`，以便 GitHub 展示手动输入选项；运行时仍需选择希望打包的分支。
