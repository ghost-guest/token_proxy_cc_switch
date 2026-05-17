import { act, renderHook } from "@testing-library/react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useKiroLogin } from "@/features/kiro/use-kiro-login";
import type {
  KiroAccountSummary,
  KiroLoginPollResponse,
  KiroLoginStartResponse,
} from "@/features/kiro/types";

const apiMocks = vi.hoisted(() => {
  const startKiroLogin = vi.fn<() => Promise<KiroLoginStartResponse>>();
  const pollKiroLogin = vi.fn<(state: string) => Promise<KiroLoginPollResponse>>();

  return {
    startKiroLogin,
    pollKiroLogin,
  };
});

vi.mock("@/features/kiro/api", () => ({
  startKiroLogin: apiMocks.startKiroLogin,
  pollKiroLogin: apiMocks.pollKiroLogin,
}));

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn<(message: string) => void>(),
  },
}));

const LOGIN_START: KiroLoginStartResponse = {
  state: "kiro-login-state",
  method: "google",
  login_url: "https://kiro.example.com/login",
  verification_uri: null,
  verification_uri_complete: null,
  user_code: null,
  interval_seconds: 3,
  expires_at: null,
};

const SECOND_LOGIN_START: KiroLoginStartResponse = {
  state: "second-kiro-login-state",
  method: "aws",
  login_url: "https://kiro.example.com/second-login",
  verification_uri: null,
  verification_uri_complete: null,
  user_code: null,
  interval_seconds: 3,
  expires_at: null,
};

const ZERO_INTERVAL_LOGIN_START: KiroLoginStartResponse = {
  state: "zero-interval-state",
  method: "google",
  login_url: "https://kiro.example.com/zero-interval",
  verification_uri: null,
  verification_uri_complete: null,
  user_code: null,
  interval_seconds: 0,
  expires_at: null,
};

const LOGIN_PENDING: KiroLoginPollResponse = {
  state: "kiro-login-state",
  status: "waiting",
  error: null,
  account: null,
};

const LOGIN_ACCOUNT: KiroAccountSummary = {
  account_id: "kiro-1",
  provider: "kiro",
  auth_method: "google",
  email: "kiro@example.com",
  expires_at: "2026-05-17T00:00:00Z",
  status: "active",
  priority: 1,
};

const LOGIN_SUCCESS: KiroLoginPollResponse = {
  state: "kiro-login-state",
  status: "success",
  error: null,
  account: LOGIN_ACCOUNT,
};

describe("kiro/use-kiro-login", () => {
  afterEach(() => {
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("resetLogin ignores a late login start response", async () => {
    let resolveStart: ((response: KiroLoginStartResponse) => void) | undefined;
    apiMocks.startKiroLogin.mockReturnValueOnce(
      new Promise<KiroLoginStartResponse>((resolve) => {
        resolveStart = resolve;
      })
    );

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useKiroLogin({ onRefresh }));

    act(() => {
      void result.current.beginLogin("google");
    });
    expect(result.current.login.status).toBe("waiting");

    act(() => {
      result.current.resetLogin();
    });

    await act(async () => {
      if (!resolveStart) {
        throw new Error("Missing login start resolver");
      }
      resolveStart(LOGIN_START);
      await Promise.resolve();
    });

    expect(result.current.login.status).toBe("idle");
    expect(openUrl).not.toHaveBeenCalled();
    expect(apiMocks.pollKiroLogin).not.toHaveBeenCalled();
  });

  it("resetLogin clears the active poller", async () => {
    vi.useFakeTimers();
    apiMocks.startKiroLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollKiroLogin.mockResolvedValue(LOGIN_PENDING);

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useKiroLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin("google");
    });
    expect(openUrl).toHaveBeenCalledWith(LOGIN_START.login_url);

    act(() => {
      result.current.resetLogin();
    });

    await act(async () => {
      vi.advanceTimersByTime((LOGIN_START.interval_seconds ?? 3) * 1000);
      await Promise.resolve();
    });

    expect(result.current.login.status).toBe("idle");
    expect(apiMocks.pollKiroLogin).not.toHaveBeenCalled();
  });

  it("resetLogin ignores an in-flight poll response", async () => {
    vi.useFakeTimers();
    let resolvePoll: ((response: KiroLoginPollResponse) => void) | undefined;
    apiMocks.startKiroLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollKiroLogin.mockReturnValueOnce(
      new Promise<KiroLoginPollResponse>((resolve) => {
        resolvePoll = resolve;
      })
    );

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useKiroLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin("google");
    });
    await act(async () => {
      vi.advanceTimersByTime((LOGIN_START.interval_seconds ?? 3) * 1000);
      await Promise.resolve();
    });
    expect(apiMocks.pollKiroLogin).toHaveBeenCalledTimes(1);

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
  });

  it("waits for the current poll before scheduling the next poll", async () => {
    vi.useFakeTimers();
    let resolveFirstPoll: ((response: KiroLoginPollResponse) => void) | undefined;
    apiMocks.startKiroLogin.mockResolvedValueOnce(LOGIN_START);
    apiMocks.pollKiroLogin
      .mockReturnValueOnce(
        new Promise<KiroLoginPollResponse>((resolve) => {
          resolveFirstPoll = resolve;
        })
      )
      .mockResolvedValue(LOGIN_PENDING);

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useKiroLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin("google");
    });
    await act(async () => {
      vi.advanceTimersByTime((LOGIN_START.interval_seconds ?? 3) * 1000);
      await Promise.resolve();
    });
    expect(apiMocks.pollKiroLogin).toHaveBeenCalledTimes(1);

    await act(async () => {
      vi.advanceTimersByTime((LOGIN_START.interval_seconds ?? 3) * 3000);
      await Promise.resolve();
    });
    expect(apiMocks.pollKiroLogin).toHaveBeenCalledTimes(1);

    await act(async () => {
      if (!resolveFirstPoll) {
        throw new Error("Missing poll resolver");
      }
      resolveFirstPoll(LOGIN_PENDING);
      await Promise.resolve();
    });
    expect(result.current.login.status).toBe("polling");

    await act(async () => {
      vi.advanceTimersByTime((LOGIN_START.interval_seconds ?? 3) * 1000);
      await Promise.resolve();
    });
    expect(apiMocks.pollKiroLogin).toHaveBeenCalledTimes(2);
  });

  it("ignores an older start response after a newer login starts", async () => {
    let resolveFirstStart: ((response: KiroLoginStartResponse) => void) | undefined;
    let resolveSecondStart: ((response: KiroLoginStartResponse) => void) | undefined;
    apiMocks.startKiroLogin
      .mockReturnValueOnce(
        new Promise<KiroLoginStartResponse>((resolve) => {
          resolveFirstStart = resolve;
        })
      )
      .mockReturnValueOnce(
        new Promise<KiroLoginStartResponse>((resolve) => {
          resolveSecondStart = resolve;
        })
      );

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useKiroLogin({ onRefresh }));

    act(() => {
      void result.current.beginLogin("google");
      void result.current.beginLogin("aws");
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

  it("clamps zero interval seconds to one second", async () => {
    vi.useFakeTimers();
    apiMocks.startKiroLogin.mockResolvedValueOnce(ZERO_INTERVAL_LOGIN_START);
    apiMocks.pollKiroLogin.mockResolvedValue(LOGIN_PENDING);

    const onRefresh = vi.fn<() => Promise<void>>().mockResolvedValue(undefined);
    const { result } = renderHook(() => useKiroLogin({ onRefresh }));

    await act(async () => {
      await result.current.beginLogin("google");
    });
    await act(async () => {
      vi.advanceTimersByTime(999);
      await Promise.resolve();
    });
    expect(apiMocks.pollKiroLogin).not.toHaveBeenCalled();

    await act(async () => {
      vi.advanceTimersByTime(1);
      await Promise.resolve();
    });
    expect(apiMocks.pollKiroLogin).toHaveBeenCalledTimes(1);
  });
});
