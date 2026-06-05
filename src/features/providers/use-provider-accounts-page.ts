import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { listProviderAccountsPage } from "@/features/providers/api";
import type { ProviderAccountsPage } from "@/features/providers/types";
import { parseError } from "@/lib/error";

export const PROVIDER_ACCOUNTS_PAGE_SIZE = 10;
const PROVIDER_ACCOUNTS_SEARCH_DEBOUNCE_MS = 250;

export type ProviderAccountsPageFilters = {
  searchKeyword: string;
  providerFilter: "all" | "kiro" | "codex";
  statusFilter: "all" | "active" | "disabled" | "expired" | "invalid" | "cooling_down";
};

type ProviderAccountsPageStatus = "idle" | "loading" | "error";

function toProviderKind(value: ProviderAccountsPageFilters["providerFilter"]) {
  return value === "all" ? undefined : value;
}

function toStatus(value: ProviderAccountsPageFilters["statusFilter"]) {
  return value === "all" ? undefined : value;
}

function useDebouncedValue<T>(value: T, delayMs: number) {
  const [debouncedValue, setDebouncedValue] = useState(value);

  useEffect(() => {
    const timerId = window.setTimeout(() => {
      setDebouncedValue(value);
    }, delayMs);
    return () => window.clearTimeout(timerId);
  }, [delayMs, value]);

  return debouncedValue;
}

export function useProviderAccountsPage(filters: ProviderAccountsPageFilters) {
  const [page, setPage] = useState(1);
  const [snapshot, setSnapshot] = useState<ProviderAccountsPage | null>(null);
  const [status, setStatus] = useState<ProviderAccountsPageStatus>("loading");
  const [error, setError] = useState("");
  const requestSeq = useRef(0);
  const debouncedSearchKeyword = useDebouncedValue(
    filters.searchKeyword,
    PROVIDER_ACCOUNTS_SEARCH_DEBOUNCE_MS
  );
  const filterKey = `${debouncedSearchKeyword}|${filters.providerFilter}|${filters.statusFilter}`;
  const lastFilterKey = useRef(filterKey);

  const loadPage = useCallback(
    async (targetPage: number) => {
      const requestId = requestSeq.current + 1;
      requestSeq.current = requestId;
      setStatus("loading");
      setError("");
      try {
        const next = await listProviderAccountsPage({
          page: targetPage,
          pageSize: PROVIDER_ACCOUNTS_PAGE_SIZE,
          providerKind: toProviderKind(filters.providerFilter),
          status: toStatus(filters.statusFilter),
          search: debouncedSearchKeyword,
        });
        if (requestSeq.current !== requestId) {
          return;
        }
        setSnapshot(next);
        setStatus("idle");
      } catch (cause) {
        if (requestSeq.current !== requestId) {
          return;
        }
        setSnapshot(null);
        setStatus("error");
        setError(parseError(cause));
      }
    },
    [debouncedSearchKeyword, filters.providerFilter, filters.statusFilter]
  );

  useEffect(() => {
    if (lastFilterKey.current !== filterKey) {
      lastFilterKey.current = filterKey;
      if (page !== 1) {
        const resetTimerId = window.setTimeout(() => {
          setPage(1);
        }, 0);
        return () => window.clearTimeout(resetTimerId);
      }
    }

    const timerId = window.setTimeout(() => {
      void loadPage(page);
    }, 0);
    return () => window.clearTimeout(timerId);
  }, [filterKey, loadPage, page]);

  const total = snapshot?.total ?? 0;
  const totalPages = useMemo(
    () => Math.max(1, Math.ceil(total / PROVIDER_ACCOUNTS_PAGE_SIZE)),
    [total]
  );

  const refresh = useCallback(async () => {
    await loadPage(page);
  }, [loadPage, page]);

  const resetPage = useCallback(() => {
    setPage(1);
  }, []);

  const onPrevPage = useCallback(() => {
    setPage((current) => Math.max(1, current - 1));
  }, []);

  const onNextPage = useCallback(() => {
    setPage((current) => Math.min(totalPages, current + 1));
  }, [totalPages]);

  return {
    items: snapshot?.items ?? [],
    statusCounts: snapshot?.status_counts,
    total,
    page,
    pageSize: PROVIDER_ACCOUNTS_PAGE_SIZE,
    totalPages,
    loading: status === "loading",
    error,
    refresh,
    resetPage,
    onPrevPage,
    onNextPage,
  };
}
