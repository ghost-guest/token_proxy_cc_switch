import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent } from "@tauri-apps/plugin-updater";

import { parseError } from "@/lib/error";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "available"
  | "uptodate"
  | "downloading"
  | "installing"
  | "installed"
  | "error";

export type UpdateInfo = {
  version: string;
  date?: string;
  body?: string;
};

export type DownloadState = {
  downloaded: number;
  total: number;
};

export type UpdateCheckSource = "auto" | "manual";

type UpdaterCheckResult = Awaited<ReturnType<typeof check>>;

async function closeUpdateHandle(updateHandle: UpdaterCheckResult, reason: string) {
  if (!updateHandle) {
    return;
  }

  try {
    await updateHandle.close();
  } catch (error) {
    console.warn("[updater] failed to close update handle", { error, reason });
  }
}

export function canStartUpdateCheck(status: UpdateStatus) {
  return (
    status !== "checking" &&
    status !== "downloading" &&
    status !== "installing" &&
    status !== "installed"
  );
}

type UpdateState = {
  status: UpdateStatus;
  statusMessage: string;
  lastCheckedAt: string;
  updateInfo: UpdateInfo | null;
  updateHandle: UpdaterCheckResult;
  downloadState: DownloadState;
  lastCheckSource: UpdateCheckSource | null;
  appProxyUrl: string;
  appProxyUrlReady: boolean;
};

type UpdateActions = {
  setAppProxyUrl: (value: string) => void;
  checkForUpdate: (args?: { source: UpdateCheckSource }) => Promise<void>;
  downloadAndInstall: () => Promise<void>;
  relaunchApp: () => Promise<void>;
};

type UpdaterContextValue = {
  state: UpdateState;
  actions: UpdateActions;
};

const UpdaterContext = createContext<UpdaterContextValue | null>(null);

export function formatBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unitIndex]}`;
}

function toUpdateInfo(update: NonNullable<UpdaterCheckResult>): UpdateInfo {
  return {
    version: update.version,
    date: update.date,
    body: update.body,
  };
}

async function readAppProxyUrl() {
  try {
    const response = await invoke<{ config?: { app_proxy_url?: string | null } }>(
      "read_proxy_config"
    );
    return response?.config?.app_proxy_url ?? "";
  } catch (error) {
    console.warn("[updater] failed to read app proxy url", { error });
    return "";
  }
}

type UpdaterProviderProps = {
  children: ReactNode;
};

export function UpdaterProvider({ children }: UpdaterProviderProps) {
  const checkInFlightRef = useRef(false);
  const statusRef = useRef<UpdateStatus>("idle");
  const updateHandleRef = useRef<UpdaterCheckResult>(null);
  const appProxyUrlRequestIdRef = useRef(0);
  const [state, setState] = useState<UpdateState>({
    status: "idle",
    statusMessage: "",
    lastCheckedAt: "",
    updateInfo: null,
    updateHandle: null,
    downloadState: { downloaded: 0, total: 0 },
    lastCheckSource: null,
    appProxyUrl: "",
    appProxyUrlReady: false,
  });

  useEffect(() => {
    statusRef.current = state.status;
  }, [state.status]);

  useEffect(() => {
    updateHandleRef.current = state.updateHandle;
  }, [state.updateHandle]);

  useEffect(() => {
    const requestId = appProxyUrlRequestIdRef.current + 1;
    appProxyUrlRequestIdRef.current = requestId;
    void readAppProxyUrl().then((appProxyUrl) => {
      if (appProxyUrlRequestIdRef.current !== requestId) {
        return;
      }
      setState((prev) => {
        if (prev.appProxyUrlReady && prev.appProxyUrl === appProxyUrl) {
          return prev;
        }
        return {
          ...prev,
          appProxyUrl,
          appProxyUrlReady: true,
        };
      });
    });
  }, []);

  const setAppProxyUrl = useCallback((value: string) => {
    appProxyUrlRequestIdRef.current += 1;
    setState((prev) => {
      if (prev.appProxyUrlReady && prev.appProxyUrl === value) {
        return prev;
      }
      return {
        ...prev,
        appProxyUrl: value,
        appProxyUrlReady: true,
      };
    });
  }, []);

  const checkForUpdate = useCallback(
    async (args?: { source: UpdateCheckSource }) => {
      const source = args?.source ?? "manual";
      const status = statusRef.current;
      if (checkInFlightRef.current) {
        console.info("[updater] skip update check while one is already running", { source });
        return;
      }
      if (!canStartUpdateCheck(status)) {
        console.info("[updater] skip update check while update workflow is busy", {
          source,
          status,
        });
        return;
      }
      checkInFlightRef.current = true;
      statusRef.current = "checking";

      const staleUpdateHandle = updateHandleRef.current;
      updateHandleRef.current = null;

      setState((prev) => ({
        ...prev,
        status: "checking",
        statusMessage: "",
        lastCheckSource: source,
        updateInfo: null,
        updateHandle: null,
        downloadState: { downloaded: 0, total: 0 },
      }));

      try {
        await closeUpdateHandle(staleUpdateHandle, "before-check");
        const proxy = state.appProxyUrl.trim();
        const result = await check(proxy ? { proxy } : undefined);
        updateHandleRef.current = result;
        statusRef.current = result ? "available" : "uptodate";
        setState((prev) => ({
          ...prev,
          status: result ? "available" : "uptodate",
          updateInfo: result ? toUpdateInfo(result) : null,
          updateHandle: result,
          lastCheckedAt: new Date().toLocaleString(),
        }));
      } catch (error) {
        statusRef.current = "error";
        setState((prev) => ({
          ...prev,
          status: "error",
          statusMessage: parseError(error),
          updateHandle: null,
        }));
      } finally {
        checkInFlightRef.current = false;
      }
    },
    [state.appProxyUrl]
  );

  const downloadAndInstall = useCallback(async () => {
    const updateHandle = updateHandleRef.current;
    if (!updateHandle) {
      return;
    }

    statusRef.current = "downloading";
    setState((prev) => ({
      ...prev,
      status: "downloading",
      statusMessage: "",
      downloadState: { downloaded: 0, total: 0 },
    }));

    const onProgress = (progress: DownloadEvent) => {
      if (progress.event === "Started") {
        setState((prev) => ({
          ...prev,
          downloadState: {
            downloaded: 0,
            total: progress.data?.contentLength ?? 0,
          },
        }));
        return;
      }
      if (progress.event === "Progress") {
        setState((prev) => ({
          ...prev,
          downloadState: {
            downloaded: prev.downloadState.downloaded + (progress.data?.chunkLength ?? 0),
            total: prev.downloadState.total,
          },
        }));
        return;
      }
      if (progress.event === "Finished") {
        statusRef.current = "installing";
        setState((prev) => ({ ...prev, status: "installing" }));
      }
    };

    try {
      await updateHandle.downloadAndInstall(onProgress);
      statusRef.current = "installed";
      updateHandleRef.current = null;
      setState((prev) => ({ ...prev, status: "installed", updateHandle: null }));
    } catch (error) {
      statusRef.current = "error";
      updateHandleRef.current = null;
      setState((prev) => ({
        ...prev,
        status: "error",
        statusMessage: parseError(error),
        updateHandle: null,
      }));
    } finally {
      await closeUpdateHandle(updateHandle, "after-download-install");
    }
  }, []);

  const relaunchApp = useCallback(async () => {
    setState((prev) => ({ ...prev, statusMessage: "" }));
    try {
      // Best-effort graceful shutdown before relaunching.
      try {
        await invoke<void>("prepare_relaunch");
      } catch (error) {
        setState((prev) => ({ ...prev, statusMessage: parseError(error) }));
      }
      await relaunch();
    } catch (error) {
      // 安装成功但重启失败时，不应把更新状态标记为失败；仅展示错误提示。
      setState((prev) => ({ ...prev, statusMessage: parseError(error) }));
    }
  }, []);

  const value = useMemo<UpdaterContextValue>(
    () => ({
      state,
      actions: { setAppProxyUrl, checkForUpdate, downloadAndInstall, relaunchApp },
    }),
    [checkForUpdate, downloadAndInstall, relaunchApp, setAppProxyUrl, state]
  );

  return <UpdaterContext.Provider value={value}>{children}</UpdaterContext.Provider>;
}

export function useUpdater() {
  const ctx = useContext(UpdaterContext);
  if (!ctx) {
    throw new Error("useUpdater must be used within an UpdaterProvider.");
  }
  return ctx;
}
