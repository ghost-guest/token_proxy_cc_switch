import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { check } from "@tauri-apps/plugin-updater";
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  MAIN_WINDOW_VISIBLE_EVENT,
  __resetUpdateNotifierAutoCheckForTests,
  UpdateNotifier,
} from "@/features/update/UpdateNotifier";
import { UpdaterProvider, useUpdater } from "@/features/update/updater";

type TauriEventHandler = (event: { event: string; id: number; payload: unknown }) => void;

const {
  eventHandlers,
  listenMock,
  navigateMock,
  toastDismissMock,
  toastErrorMock,
  toastLoadingMock,
  toastMock,
} = vi.hoisted(() => ({
  eventHandlers: new Map<string, TauriEventHandler>(),
  listenMock: vi.fn<(event: string, handler: TauriEventHandler) => Promise<() => void>>(),
  navigateMock: vi.fn<() => Promise<void>>(),
  toastDismissMock: vi.fn<(id?: string | number) => void>(),
  toastErrorMock: vi.fn<(...args: unknown[]) => string>().mockReturnValue("error-toast"),
  toastLoadingMock: vi.fn<(...args: unknown[]) => string>().mockReturnValue("loading-toast"),
  toastMock: vi.fn<(...args: unknown[]) => string>().mockReturnValue("available-toast"),
}));

let consoleInfoMock: ReturnType<typeof vi.spyOn>;

async function createUpdateHandle(version: string) {
  const { Update: ActualUpdate } =
    await vi.importActual<typeof import("@tauri-apps/plugin-updater")>(
      "@tauri-apps/plugin-updater"
    );
  const update = new ActualUpdate({
    currentVersion: "0.1.0",
    date: "2026-05-17",
    rawJson: {},
    rid: 1,
    version,
  });
  const close = vi.spyOn(update, "close").mockResolvedValue(undefined);
  vi.spyOn(update, "downloadAndInstall").mockResolvedValue(undefined);

  return { close, update };
}

type TestUpdateHandle = Awaited<ReturnType<typeof createUpdateHandle>>;

function resolveUpdateCheckWith(updateHandle: TestUpdateHandle) {
  vi.mocked(check).mockResolvedValueOnce(updateHandle.update);
}

vi.mock("@tauri-apps/api/event", () => ({
  listen: listenMock,
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => navigateMock,
}));

vi.mock("sonner", () => ({
  toast: Object.assign(toastMock, {
    dismiss: toastDismissMock,
    error: toastErrorMock,
    loading: toastLoadingMock,
  }),
}));

function UpdaterHarness() {
  const { actions, state } = useUpdater();
  const { setAppProxyUrl } = actions;

  useEffect(() => {
    setAppProxyUrl("http://127.0.0.1:7890");
  }, [setAppProxyUrl]);

  return (
    <>
      <UpdateNotifier />
      <output data-testid="update-ready">{state.appProxyUrlReady ? "ready" : "pending"}</output>
      <output data-testid="update-status">{state.status}</output>
    </>
  );
}

function ManualUpdateHarness({ statusTestId }: { statusTestId: string }) {
  const { actions, state } = useUpdater();

  useEffect(() => {
    actions.setAppProxyUrl("http://127.0.0.1:7890");
  }, [actions]);

  return (
    <>
      <UpdateNotifier />
      <output data-testid={statusTestId}>{state.status}</output>
      <button
        type="button"
        onClick={() => {
          void actions.checkForUpdate({ source: "manual" });
        }}
      >
        check-now
      </button>
    </>
  );
}

function ProxyUrlStateHarness() {
  const { actions, state } = useUpdater();

  useEffect(() => {
    actions.setAppProxyUrl("http://127.0.0.1:7890");
  }, [actions]);

  return <output data-testid="app-proxy-url">{state.appProxyUrl}</output>;
}

describe("update/UpdateNotifier", () => {
  beforeEach(() => {
    __resetUpdateNotifierAutoCheckForTests();
    eventHandlers.clear();
    consoleInfoMock = vi.spyOn(console, "info").mockImplementation(() => undefined);
    navigateMock.mockReset();
    toastDismissMock.mockClear();
    toastErrorMock.mockClear();
    toastErrorMock.mockReturnValue("error-toast");
    toastLoadingMock.mockClear();
    toastLoadingMock.mockReturnValue("loading-toast");
    toastMock.mockClear();
    toastMock.mockReturnValue("available-toast");
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue({ config: { app_proxy_url: null } });
    vi.mocked(check).mockReset();
    vi.mocked(check).mockResolvedValue(null);
    listenMock.mockReset();
    listenMock.mockImplementation(async (event, handler) => {
      eventHandlers.set(event, handler);
      return () => {
        eventHandlers.delete(event);
      };
    });
  });

  afterEach(() => {
    consoleInfoMock.mockRestore();
    cleanup();
  });

  it("checks for updates again when the main window becomes visible", async () => {
    render(
      <UpdaterProvider>
        <UpdaterHarness />
      </UpdaterProvider>
    );

    await waitFor(() => {
      expect(screen.getByTestId("update-status")).toHaveTextContent("uptodate");
    });
    expect(vi.mocked(check)).toHaveBeenCalledTimes(1);

    const handler = eventHandlers.get(MAIN_WINDOW_VISIBLE_EVENT);
    expect(handler).toBeDefined();
    handler?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 1, payload: null });

    await waitFor(() => {
      expect(vi.mocked(check)).toHaveBeenCalledTimes(2);
    });
    expect(vi.mocked(check)).toHaveBeenLastCalledWith(undefined);
  });

  it("does not wait for settings page to become ready before starting the startup update check", async () => {
    render(
      <UpdaterProvider>
        <UpdateNotifier />
      </UpdaterProvider>
    );

    await waitFor(() => {
      expect(vi.mocked(check)).toHaveBeenCalledTimes(1);
    });
    expect(vi.mocked(check)).toHaveBeenLastCalledWith(undefined);
  });

  it("uses app proxy url loaded by the updater provider for the startup update check", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      config: { app_proxy_url: "socks5h://127.0.0.1:7891" },
    });

    render(
      <UpdaterProvider>
        <UpdateNotifier />
      </UpdaterProvider>
    );

    await waitFor(() => {
      expect(vi.mocked(check)).toHaveBeenCalledTimes(1);
    });
    expect(vi.mocked(check)).toHaveBeenLastCalledWith({
      proxy: "socks5h://127.0.0.1:7891",
    });
  });

  it("does not let the startup config read overwrite a newer app proxy url", async () => {
    let resolveConfig:
      | ((value: { config: { app_proxy_url: string | null } }) => void)
      | null = null;
    vi.mocked(invoke).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveConfig = resolve;
        })
    );

    render(
      <UpdaterProvider>
        <ProxyUrlStateHarness />
      </UpdaterProvider>
    );

    await waitFor(() => {
      expect(screen.getByTestId("app-proxy-url")).toHaveTextContent("http://127.0.0.1:7890");
    });

    expect(resolveConfig).not.toBeNull();
    resolveConfig!({ config: { app_proxy_url: "socks5h://127.0.0.1:9999" } });

    await waitFor(() => {
      expect(screen.getByTestId("app-proxy-url")).toHaveTextContent("http://127.0.0.1:7890");
    });
  });

  it("does not start parallel update checks for repeated visible events", async () => {
    let resolveCheck: () => void = () => {
      throw new Error("update check promise was not created");
    };
    vi.mocked(check).mockImplementation(
      () =>
        new Promise<null>((resolve) => {
          resolveCheck = () => resolve(null);
        })
    );

    render(
      <UpdaterProvider>
        <UpdaterHarness />
      </UpdaterProvider>
    );

    await waitFor(() => {
      expect(screen.getByTestId("update-ready")).toHaveTextContent("ready");
    });

    const handler = eventHandlers.get(MAIN_WINDOW_VISIBLE_EVENT);
    expect(handler).toBeDefined();
    handler?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 1, payload: null });
    handler?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 2, payload: null });

    await waitFor(() => {
      expect(vi.mocked(check)).toHaveBeenCalledTimes(1);
    });

    resolveCheck();

    await waitFor(() => {
      expect(screen.getByTestId("update-status")).toHaveTextContent("uptodate");
    });

    handler?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 3, payload: null });

    await waitFor(() => {
      expect(vi.mocked(check)).toHaveBeenCalledTimes(2);
    });
  });

  it("does not run a manual and visible-window check in parallel", async () => {
    let resolveCheck: () => void = () => {
      throw new Error("update check promise was not created");
    };
    vi.mocked(check).mockImplementation(
      () =>
        new Promise<null>((resolve) => {
          resolveCheck = () => resolve(null);
        })
    );

    function ManualCheckHarness() {
      const { actions, state } = useUpdater();

      useEffect(() => {
        actions.setAppProxyUrl("http://127.0.0.1:7890");
      }, [actions]);

      return (
        <>
          <UpdateNotifier />
          <output data-testid="manual-ready">
            {state.appProxyUrlReady ? "ready" : "pending"}
          </output>
          <output data-testid="manual-status">{state.status}</output>
          <button
            type="button"
            onClick={() => {
              void actions.checkForUpdate({ source: "manual" });
            }}
          >
            manual-check
          </button>
        </>
      );
    }

    render(
      <UpdaterProvider>
        <ManualCheckHarness />
      </UpdaterProvider>
    );

    await waitFor(() => {
      expect(screen.getByTestId("manual-ready")).toHaveTextContent("ready");
    });

    if (vi.mocked(check).mock.calls.length > 0) {
      resolveCheck();
      await waitFor(() => {
        expect(screen.getByTestId("manual-status")).toHaveTextContent("uptodate");
      });
      vi.mocked(check).mockClear();
    }

    screen.getByRole("button", { name: "manual-check" }).click();
    eventHandlers
      .get(MAIN_WINDOW_VISIBLE_EVENT)
      ?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 1, payload: null });

    await waitFor(() => {
      expect(vi.mocked(check)).toHaveBeenCalledTimes(1);
    });

    resolveCheck();

    await waitFor(() => {
      expect(screen.getByTestId("manual-status")).toHaveTextContent("uptodate");
    });
  });

  it("closes the previous update handle before replacing it on a visible-window check", async () => {
    const firstUpdate = await createUpdateHandle("0.2.0");
    const secondUpdate = await createUpdateHandle("0.3.0");
    resolveUpdateCheckWith(firstUpdate);
    resolveUpdateCheckWith(secondUpdate);

    render(
      <UpdaterProvider>
        <ManualUpdateHarness statusTestId="available-status" />
      </UpdaterProvider>
    );

    screen.getByRole("button", { name: "check-now" }).click();

    await waitFor(() => {
      expect(screen.getByTestId("available-status")).toHaveTextContent("available");
    });

    const handler = eventHandlers.get(MAIN_WINDOW_VISIBLE_EVENT);
    expect(handler).toBeDefined();
    handler?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 1, payload: null });

    await waitFor(() => {
      expect(vi.mocked(check)).toHaveBeenCalledTimes(2);
    });
    expect(firstUpdate.close).toHaveBeenCalledTimes(1);
    expect(secondUpdate.close).not.toHaveBeenCalled();
    expect(screen.getByTestId("available-status")).toHaveTextContent("available");
  });

  it("dismisses stale available-update toast when a visible-window recheck finds no update", async () => {
    const firstUpdate = await createUpdateHandle("0.2.0");
    resolveUpdateCheckWith(firstUpdate);
    vi.mocked(check).mockResolvedValueOnce(null);

    render(
      <UpdaterProvider>
        <UpdaterHarness />
      </UpdaterProvider>
    );

    await waitFor(() => {
      expect(screen.getByTestId("update-ready")).toHaveTextContent("ready");
    });

    const handler = eventHandlers.get(MAIN_WINDOW_VISIBLE_EVENT);
    expect(handler).toBeDefined();
    if (vi.mocked(check).mock.calls.length === 0) {
      handler?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 1, payload: null });
    }

    await waitFor(() => {
      expect(screen.getByTestId("update-status")).toHaveTextContent("available");
    });
    await waitFor(() => {
      expect(toastMock).toHaveBeenCalledTimes(1);
    });
    handler?.({ event: MAIN_WINDOW_VISIBLE_EVENT, id: 2, payload: null });

    await waitFor(() => {
      expect(screen.getByTestId("update-status")).toHaveTextContent("uptodate");
    });
    await waitFor(() => {
      expect(toastDismissMock).toHaveBeenCalledWith("available-toast");
    });
  });
});
