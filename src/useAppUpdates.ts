import {
  type Dispatch,
  type SetStateAction,
  useEffect,
  useState,
} from "react";

import { invoke } from "./api";
import type {
  Confirmation,
  InlineResult,
  Notice,
  UpdateCheck,
  UpdateDownload,
  UpdateInstallReport,
} from "./App.types";
import { errorText, withTimeout } from "./appUtils";
import { formatBytes } from "./formatters";

const UPDATE_AVAILABLE_EVENT = "codey-update-availability-changed";
const UPDATE_CHECK_TIMEOUT_MS = 12_000;

declare global {
  interface Window {
    __codeyUpdateAvailability?: UpdateCheck | null;
  }
}

function updateInstallReportText(report: UpdateInstallReport): string {
  return report.message
    ? `v${report.version} 更新未完成：${report.message}`
    : `v${report.version} 更新未完成，请重试`;
}

type UseAppUpdatesOptions = {
  configLoaded: boolean;
  isBusy: boolean;
  setBusy: Dispatch<SetStateAction<string | null>>;
  setNotice: Dispatch<SetStateAction<Notice>>;
  setConfirmation: Dispatch<SetStateAction<Confirmation | null>>;
  beforeInstall: () => Promise<void>;
};

const updateAvailable = (
  check: UpdateCheck | null | undefined,
): check is UpdateCheck => check?.updateAvailable === true;

function updateCheckText(result: UpdateCheck) {
  if (result.rollback) return `可从 v${result.currentVersion} 回退至 v${result.latestVersion}：${result.rollback.reason}`;
  const base = result.updateAvailable
    ? result.selectedAsset
      ? `发现 v${result.latestVersion} 更新（当前 v${result.currentVersion}）`
      : `发现 v${result.latestVersion} 更新，但当前系统暂无可安装包`
    : `当前已是最新版本 v${result.currentVersion}`;
  const notes = result.releaseNotes?.trim();
  return notes ? `${base}：${notes}` : base;
}

function updateResultTone(result: UpdateCheck): InlineResult["tone"] {
  return result.updateAvailable && !result.selectedAsset
    ? "error"
    : "success";
}

function publishUpdateAvailability(result: UpdateCheck | null) {
  window.__codeyUpdateAvailability = updateAvailable(result) ? result : null;
  window.dispatchEvent(
    new CustomEvent(UPDATE_AVAILABLE_EVENT, {
      detail: window.__codeyUpdateAvailability,
    }),
  );
}

export function useAppUpdates({
  configLoaded,
  isBusy,
  setBusy,
  setNotice,
  setConfirmation,
  beforeInstall,
}: UseAppUpdatesOptions) {
  const [updateResult, setUpdateResult] = useState<InlineResult>({
    tone: "idle",
    text: "",
  });
  const [updateCheck, setUpdateCheck] = useState<UpdateCheck | null>(null);
  const [downloadedUpdate, setDownloadedUpdate] =
    useState<UpdateDownload | null>(null);

  // 上一次"安装并重启"的真实结果。助手把结论写在配置目录里，这里读一次并
  // 展示，避免用户只看到版本号没变却没有任何解释。
  useEffect(() => {
    if (!configLoaded) return;
    let cancelled = false;
    void (async () => {
      let report: UpdateInstallReport | null = null;
      try {
        report = await invoke<UpdateInstallReport | null>("update_install_report");
      } catch {
        return;
      }
      if (cancelled || !report || report.status === "installed") return;
      if (report.status === "started") {
        setNotice({
          tone: "info",
          text: `v${report.version} 更新未完成，请重新打开 Codey 或再次点击更新`,
        });
        return;
      }
      setNotice({ tone: "error", text: updateInstallReportText(report) });
    })();
    return () => {
      cancelled = true;
    };
  }, [configLoaded, setNotice]);

  useEffect(() => {
    const applyDetectedUpdate = (
      result: UpdateCheck | null | undefined,
    ) => {
      if (!updateAvailable(result)) return;
      setUpdateCheck(result);
      setDownloadedUpdate(null);
      setUpdateResult({
        tone: updateResultTone(result),
        text: updateCheckText(result),
      });
    };

    applyDetectedUpdate(window.__codeyUpdateAvailability);
    const handleUpdateAvailabilityChanged = (event: Event) => {
      applyDetectedUpdate((event as CustomEvent<UpdateCheck | null>).detail);
    };
    window.addEventListener(
      UPDATE_AVAILABLE_EVENT,
      handleUpdateAvailabilityChanged,
    );
    return () => {
      window.removeEventListener(
        UPDATE_AVAILABLE_EVENT,
        handleUpdateAvailabilityChanged,
      );
    };
  }, []);

  async function checkForUpdates() {
    if (!configLoaded || isBusy) return;
    setBusy("check-update");
    setUpdateResult({ tone: "pending", text: "正在检查更新…" });
    setUpdateCheck(null);
    setDownloadedUpdate(null);
    try {
      const result = await withTimeout(
        invoke<UpdateCheck>("check_for_updates", { forceRefresh: true }),
        UPDATE_CHECK_TIMEOUT_MS,
        "检查更新超时，请检查网络",
      );
      setUpdateCheck(result);
      publishUpdateAvailability(result);
      const text = updateCheckText(result);
      setUpdateResult({
        tone: updateResultTone(result),
        text,
      });
      setNotice({
        tone:
          result.updateAvailable && result.selectedAsset
            ? "info"
            : result.updateAvailable
              ? "error"
              : "success",
        text,
      });
      if (result.updateAvailable && result.selectedAsset) {
        if (
          downloadedUpdate?.latestVersion === result.latestVersion &&
          downloadedUpdate.publishId === result.publishId &&
          downloadedUpdate.policyId === result.policyId &&
          downloadedUpdate.fileName === result.selectedAsset.fileName &&
          downloadedUpdate.sha256 === result.selectedAsset.sha256 &&
          downloadedUpdate.size === result.selectedAsset.size
        ) {
          setDownloadedUpdate(downloadedUpdate);
          askInstallDownloadedUpdate(downloadedUpdate);
        } else {
          askDownloadUpdate(result);
        }
      }
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  function askDownloadUpdate(check?: UpdateCheck | null) {
    const target = check ?? updateCheck;
    if (!target?.updateAvailable || !target.selectedAsset || isBusy) return;
    setConfirmation({
      action: "download-update",
      title: target.rollback ? `回退 Codey 至 v${target.latestVersion}` : `发现 Codey 新版本 v${target.latestVersion}`,
      description: [
        target.rollback ? `管理员已授权从 v${target.currentVersion} 降级至 v${target.latestVersion}。回退原因：${target.rollback.reason}。请保存工作，确认后下载安装旧版本并重启。` : `当前版本为 v${target.currentVersion}，检测到新版本 v${target.latestVersion}。`,
        target.releaseNotes?.trim()
          ? `更新日志：\n${target.releaseNotes.trim()}`
          : null,
        "是否立即下载更新？",
      ]
        .filter(Boolean)
        .join("\n\n"),
      confirmLabel: target.rollback ? "下载回退版本" : "立即更新",
      run: () => void downloadUpdate(target),
    });
  }

  async function downloadUpdate(checkOverride?: UpdateCheck | null) {
    const target = checkOverride ?? updateCheck;
    if (
      !configLoaded ||
      isBusy ||
      !target?.updateAvailable ||
      !target.selectedAsset
    )
      return;
    setBusy("download-update");
    setDownloadedUpdate(null);
    setUpdateResult({ tone: "pending", text: "正在下载并校验更新…" });
    try {
      const result = await withTimeout(
        invoke<UpdateDownload>("download_update", { expectedVersion: target.latestVersion, expectedPolicyId: target.policyId ?? null }),
        300_000,
        "下载更新超时，请稍后重试",
      );
      setDownloadedUpdate(result);
      const text = `已下载 ${result.fileName}（${formatBytes(result.size)}），校验通过`;
      setUpdateResult({ tone: "success", text });
      setNotice({ tone: "success", text });
      askInstallDownloadedUpdate(result);
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
    } finally {
      setBusy(null);
    }
  }

  function askInstallDownloadedUpdate(downloadOverride?: UpdateDownload | null) {
    const target = downloadOverride ?? downloadedUpdate;
    if (!target || isBusy) return;
    setConfirmation({
      action: "install-update",
      title: target.rollback ? "确认回退并重启" : "安装更新",
      description: target.rollback ? `将安装旧版本 v${target.latestVersion} 并重启。原因：${target.rollback.reason}。安装前会再次验证回退授权，若已发布新版本则停止本次回退。` : `Codey 会先保存未保存的设置，再退出当前实例，安装 ${target.fileName}，然后尝试启动新版。`,
      confirmLabel: target.rollback ? "回退并重启" : "安装并重启",
      run: () => void installDownloadedUpdate(target),
    });
  }

  async function installDownloadedUpdate(downloadOverride?: UpdateDownload | null) {
    const target = downloadOverride ?? downloadedUpdate;
    if (!target || isBusy) return;
    setBusy("install-update");
    setUpdateResult({ tone: "pending", text: "正在启动安装器…" });
    try {
      await beforeInstall();
      await invoke("install_downloaded_update", {
        filePath: target.filePath,
      });
      const text = "正在退出 Codey 并启动安装器…";
      setUpdateResult({ tone: "pending", text });
      setNotice({ tone: "info", text });
    } catch (error) {
      const text = errorText(error);
      setUpdateResult({ tone: "error", text });
      setNotice({ tone: "error", text });
      setBusy(null);
    }
  }

  return {
    updateResult,
    updateCheck,
    downloadedUpdate,
    checkForUpdates,
    downloadUpdate,
    askDownloadUpdate,
    askInstallDownloadedUpdate,
  };
}
