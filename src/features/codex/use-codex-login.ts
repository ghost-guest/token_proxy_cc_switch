import { useCallback, useEffect, useRef, useState } from "react";

import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import { pollCodexLogin, startCodexLogin } from "@/features/codex/api";
import type { CodexLoginStartResponse } from "@/features/codex/types";
import { parseError } from "@/lib/error";
import { m } from "@/paraglide/messages.js";

export type CodexLoginState = {
  status: "idle" | "waiting" | "polling" | "success" | "error";
  start?: CodexLoginStartResponse;
  error?: string;
};

type LoginPollingHandlers = {
  onSuccess: (accountId?: string) => Promise<void>;
  onError: (message: string) => void;
  onPending: () => boolean;
  onException: (error: unknown) => void;
};

type UseCodexLoginOptions = {
  onRefresh: (accountId?: string) => Promise<void> | void;
  onSelect?: (accountId: string) => void;
};

function startLoginPolling(
  state: string,
  intervalSeconds: number,
  handlers: LoginPollingHandlers,
) {
  let timerId: number | null = null;
  let stopped = false;
  const delayMs = intervalSeconds * 1000;

  const scheduleNext = () => {
    if (stopped) {
      return;
    }
    timerId = window.setTimeout(runPoll, delayMs);
  };

  const runPoll = async () => {
    timerId = null;
    if (stopped) {
      return;
    }
    try {
      const result = await pollCodexLogin(state);
      if (stopped) {
        return;
      }
      if (result.status === "success") {
        await handlers.onSuccess(result.account?.account_id ?? undefined);
        return;
      }
      if (result.status === "error") {
        handlers.onError(result.error ?? m.codex_login_failed());
        return;
      }
      if (handlers.onPending()) {
        scheduleNext();
      }
    } catch (error) {
      handlers.onException(error);
    }
  };

  scheduleNext();

  return () => {
    stopped = true;
    if (timerId !== null) {
      window.clearTimeout(timerId);
      timerId = null;
    }
  };
}

function normalizeIntervalSeconds(intervalSeconds: number | null | undefined, fallback: number) {
  if (typeof intervalSeconds !== "number" || !Number.isFinite(intervalSeconds)) {
    return fallback;
  }
  return Math.max(1, intervalSeconds);
}

export function useCodexLogin({ onRefresh, onSelect }: UseCodexLoginOptions) {
  const [login, setLogin] = useState<CodexLoginState>({ status: "idle" });
  const stopPolling = useRef<(() => void) | null>(null);
  const loginRunSeq = useRef(0);

  const clearPoller = useCallback(() => {
    if (stopPolling.current !== null) {
      stopPolling.current();
      stopPolling.current = null;
    }
  }, []);

  const cancelLoginRun = useCallback(() => {
    loginRunSeq.current += 1;
    clearPoller();
  }, [clearPoller]);

  const isCurrentLoginRun = useCallback((loginRun: number) => loginRunSeq.current === loginRun, []);

  const resetLogin = useCallback(() => {
    // 关闭添加账户弹窗时丢弃旧授权轮次，防止晚到回调恢复“正在授权”状态。
    cancelLoginRun();
    console.debug("[codex-login] reset authorization state");
    setLogin({ status: "idle" });
  }, [cancelLoginRun]);

  const startPolling = useCallback(
    (state: string, intervalSeconds: number, loginRun: number) => {
      clearPoller();
      stopPolling.current = startLoginPolling(state, intervalSeconds, {
        onSuccess: async (accountId) => {
          if (!isCurrentLoginRun(loginRun)) {
            return;
          }
          clearPoller();
          setLogin({ status: "success" });
          await Promise.resolve(onRefresh(accountId));
          if (!isCurrentLoginRun(loginRun)) {
            return;
          }
          toast.success(m.codex_login_success());
          if (accountId && onSelect) {
            onSelect(accountId);
          }
        },
        onError: (message) => {
          if (!isCurrentLoginRun(loginRun)) {
            return;
          }
          clearPoller();
          setLogin({ status: "error", error: message });
        },
        onPending: () => {
          if (!isCurrentLoginRun(loginRun)) {
            return false;
          }
          setLogin((prev) => ({ ...prev, status: "polling", error: "" }));
          return true;
        },
        onException: (error) => {
          if (!isCurrentLoginRun(loginRun)) {
            return;
          }
          clearPoller();
          setLogin({ status: "error", error: parseError(error) });
        },
      });
    },
    [clearPoller, isCurrentLoginRun, onRefresh, onSelect],
  );

  const beginLogin = useCallback(async () => {
    const loginRun = loginRunSeq.current + 1;
    loginRunSeq.current = loginRun;
    clearPoller();
    setLogin({ status: "waiting" });
    try {
      const start = await startCodexLogin();
      if (!isCurrentLoginRun(loginRun)) {
        return;
      }
      setLogin({ status: "waiting", start });
      if (start.login_url) {
        void openUrl(start.login_url);
      }
      const intervalSeconds = normalizeIntervalSeconds(start.interval_seconds, 2);
      startPolling(start.state, intervalSeconds, loginRun);
    } catch (err) {
      if (!isCurrentLoginRun(loginRun)) {
        return;
      }
      setLogin({ status: "error", error: parseError(err) });
    }
  }, [clearPoller, isCurrentLoginRun, startPolling]);

  useEffect(() => () => cancelLoginRun(), [cancelLoginRun]);

  return { login, beginLogin, resetLogin };
}
