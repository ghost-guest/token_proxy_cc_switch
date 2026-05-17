import { act, renderHook } from "@testing-library/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCodexLogin } from "@/features/codex/use-codex-login";
import type {
  CodexAccountSummary,
  CodexLoginPollResponse,
  CodexLoginStartResponse,
} from "@/features/codex/types";

const apiMocks = vi.hoisted(() => {
  const startCodexLogin = vi.fn<() => Promise<CodexLoginStartResponse>>();
  const pollCodexLogin = vi.fn<(state: string) => Promise<CodexLoginPollResponse>>();
  const toastSuccess = vi.fn<(message: string) => void>();

  return {
    startCodexLogin,
    pollCodexLogin,
    toastSuccess,
  };
});

vi.mock("@/features/codex/api", () => ({
  startCodexLogin: apiMocks.startCodexLogin,
  pollCodexLogin: apiMocks.pollCodexLogin,
}));

vi.mock("sonner", () => ({
  toast: {
    success: apiMocks.toastSuccess,
  },
}));

const LOGIN_START: CodexLoginStartResponse = {
  state: "login-state",
  login_url: "https://codex.example.com/login",
  interval_seconds: 2,
};

const SECOND_LOGIN_START: CodexLoginStartResponse = {
  state: "second-login-state",
  login_url: "https://codex.example.com/second-login",
  interval_seconds: 2,
};

const ZERO_INTERVAL_LOGIN_START: CodexLoginStartResponse = {
  state: "zero-interval-state",
  login_url: "https://codex.example.com/zero-interval",
  interval_seconds: 0,
};

const LOGIN_PENDING: CodexLoginPollResponse = {
  state: "login-state",
  status: "waiting",
  account: null,
};

const LOGIN_ACCOUNT: CodexAccountSummary = {
  account_id: "codex-1",
  email: "codex@example.com",
  expires_at: "2026-05-17T00:00:00Z",
  status: "active",
  priority: 1,
};

const LOGIN_SUCCESS: CodexLoginPollResponse = {
  state: "login-state",
  status: "success",
  account: LOGIN_ACCOUNT,
};

describe("codex/use-codex-login", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("resetLogin ignores a late login start response", async () => {
    let resolveStart: ((response: CodexLoginStartResponse) => void) | undefined;
    apiMocks.startCodexLogin.mockReturnValueOnce(
      new Promise<CodexLoginStartResponse>((resolve) => {
        resolveStart = resolve;
      })
    );

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useCodexLogin({ onRefresh }));

    act(() => {
      void result.current.beginLogin();
    });
    expect(result.current.login.status).toBe("waiting");

    act(() => {
      result.current.resetLogin();
    });
    expect(result.current.login.status).toBe("idle");

    await act(async () => {
      if (!resolveStart) {
        throw new Error("Missing login start resolver");
      }
      resolveStart(LOGIN_START);
      await Promise.resolve();
    });

    expect(result.current.login.status).toBe("idle");
    expect(openUrl).not.toHaveBeenCalled();
    expect(apiMocks.pollCodexLogin).not.toHaveBeenCalled();
  });

  it("resetLogin clears the active poller", async () => {
    vi.useFakeTimers();
    apiMocks.startCodexLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollCodexLogin.mockResolvedValue(LOGIN_PENDING);

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useCodexLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin();
    });
    expect(openUrl).toHaveBeenCalledWith(LOGIN_START.login_url);

    act(() => {
      result.current.resetLogin();
    });

    await act(async () => {
      vi.advanceTimersByTime(LOGIN_START.interval_seconds * 1000);
      await Promise.resolve();
    });

    expect(result.current.login.status).toBe("idle");
    expect(apiMocks.pollCodexLogin).not.toHaveBeenCalled();
  });

  it("resetLogin ignores an in-flight poll response", async () => {
    vi.useFakeTimers();
    let resolvePoll: ((response: CodexLoginPollResponse) => void) | undefined;
    apiMocks.startCodexLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollCodexLogin.mockReturnValueOnce(
      new Promise<CodexLoginPollResponse>((resolve) => {
        resolvePoll = resolve;
      })
    );

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useCodexLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin();
    });
    await act(async () => {
      vi.advanceTimersByTime(LOGIN_START.interval_seconds * 1000);
      await Promise.resolve();
    });
    expect(apiMocks.pollCodexLogin).toHaveBeenCalledTimes(1);

    act(() => {
      result.current.resetLogin();
    });
    await act(async () => {
      if (!resolvePoll) {
        throw new Error("Missing poll resolver");
      }
      resolvePoll(LOGIN_SUCCESS);
      await Promise.resolve();
    });

    expect(result.current.login.status).toBe("idle");
    expect(onRefresh).not.toHaveBeenCalled();
    expect(apiMocks.toastSuccess).not.toHaveBeenCalled();
  });

  it("waits for the current poll before scheduling the next poll", async () => {
    vi.useFakeTimers();
    let resolveFirstPoll: ((response: CodexLoginPollResponse) => void) | undefined;
    apiMocks.startCodexLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollCodexLogin
      .mockReturnValueOnce(
        new Promise<CodexLoginPollResponse>((resolve) => {
          resolveFirstPoll = resolve;
        })
      )
      .mockResolvedValue(LOGIN_PENDING);

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useCodexLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin();
    });
    await act(async () => {
      vi.advanceTimersByTime(LOGIN_START.interval_seconds * 1000);
      await Promise.resolve();
    });
    expect(apiMocks.pollCodexLogin).toHaveBeenCalledTimes(1);

    await act(async () => {
      vi.advanceTimersByTime(LOGIN_START.interval_seconds * 3000);
      await Promise.resolve();
    });
    expect(apiMocks.pollCodexLogin).toHaveBeenCalledTimes(1);

    await act(async () => {
      if (!resolveFirstPoll) {
        throw new Error("Missing poll resolver");
      }
      resolveFirstPoll(LOGIN_PENDING);
      await Promise.resolve();
    });
    expect(result.current.login.status).toBe("polling");

    await act(async () => {
      vi.advanceTimersByTime(LOGIN_START.interval_seconds * 1000);
      await Promise.resolve();
    });
    expect(apiMocks.pollCodexLogin).toHaveBeenCalledTimes(2);
  });

  it("clamps zero interval seconds to one second", async () => {
    vi.useFakeTimers();
    apiMocks.startCodexLogin.mockResolvedValueOnce(ZERO_INTERVAL_LOGIN_START);
    apiMocks.pollCodexLogin.mockResolvedValue(LOGIN_PENDING);

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useCodexLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin();
    });
    await act(async () => {
      vi.advanceTimersByTime(999);
      await Promise.resolve();
    });
    expect(apiMocks.pollCodexLogin).not.toHaveBeenCalled();

    await act(async () => {
      vi.advanceTimersByTime(1);
      await Promise.resolve();
    });
    expect(apiMocks.pollCodexLogin).toHaveBeenCalledTimes(1);
  });

  it("ignores an older start response after a newer login starts", async () => {
    let resolveFirstStart: ((response: CodexLoginStartResponse) => void) | undefined;
    let resolveSecondStart: ((response: CodexLoginStartResponse) => void) | undefined;
    apiMocks.startCodexLogin
      .mockReturnValueOnce(
        new Promise<CodexLoginStartResponse>((resolve) => {
          resolveFirstStart = resolve;
        })
      )
      .mockReturnValueOnce(
        new Promise<CodexLoginStartResponse>((resolve) => {
          resolveSecondStart = resolve;
        })
      );

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useCodexLogin({ onRefresh }));

    act(() => {
      void result.current.beginLogin();
      void result.current.beginLogin();
    });

    await act(async () => {
      if (!resolveFirstStart || !resolveSecondStart) {
        throw new Error("Missing login start resolver");
      }
      resolveFirstStart(LOGIN_START);
      await Promise.resolve();
      resolveSecondStart(SECOND_LOGIN_START);
      await Promise.resolve();
    });

    expect(result.current.login.start?.state).toBe(SECOND_LOGIN_START.state);
    expect(openUrl).toHaveBeenCalledTimes(1);
    expect(openUrl).toHaveBeenCalledWith(SECOND_LOGIN_START.login_url);
  });

  it("resetLogin blocks success side effects after refresh has started", async () => {
    vi.useFakeTimers();
    let resolveRefresh: (() => void) | undefined;
    apiMocks.startCodexLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollCodexLogin.mockResolvedValueOnce(LOGIN_SUCCESS);

    const onRefresh = vi.fn<() => Promise<void>>().mockReturnValue(
      new Promise<void>((resolve) => {
        resolveRefresh = resolve;
      })
    );
    const onSelect = vi.fn<(accountId: string) => void>();
    const { result } = renderHook(() => useCodexLogin({ onRefresh, onSelect }));

    await act(async () => {
      await result.current.beginLogin();
    });
    await act(async () => {
      vi.advanceTimersByTime(LOGIN_START.interval_seconds * 1000);
      await Promise.resolve();
    });
    expect(result.current.login.status).toBe("success");
    expect(onRefresh).toHaveBeenCalledWith(LOGIN_ACCOUNT.account_id);

    act(() => {
      result.current.resetLogin();
    });
    await act(async () => {
      if (!resolveRefresh) {
        throw new Error("Missing refresh resolver");
      }
      resolveRefresh();
      await Promise.resolve();
    });

    expect(result.current.login.status).toBe("idle");
    expect(apiMocks.toastSuccess).not.toHaveBeenCalled();
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("keeps the current login run active until success", async () => {
    vi.useFakeTimers();
    apiMocks.startCodexLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollCodexLogin.mockResolvedValueOnce(LOGIN_SUCCESS);

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useCodexLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin();
    });
    await act(async () => {
      vi.advanceTimersByTime(LOGIN_START.interval_seconds * 1000);
      await Promise.resolve();
    });

    expect(result.current.login.status).toBe("success");
    expect(onRefresh).toHaveBeenCalledWith(LOGIN_ACCOUNT.account_id);
    expect(apiMocks.toastSuccess).toHaveBeenCalledTimes(1);
  });
});
