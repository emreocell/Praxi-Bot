import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "../api/client";
import type { ComposioApp, ComposioCatalog, ComposioStatus, ComposioToolkit } from "../api/types";
import { CATALOG_PAGE_SIZE } from "../lib/catalogOrder";
import { appendPage, pendingSlugs, upsertApp } from "../lib/composioApps";
import { openComposioLink } from "../lib/externalLink";
import { useDaemon } from "./DaemonContext";
import { humanLine } from "../lib/humanError";

/** How often a pending sign-in is checked, and when we stop asking. */
const POLL_MS = 2000;
const POLL_GIVE_UP_MS = 10 * 60 * 1000;
const SEARCH_DEBOUNCE_MS = 250;
/** While the daemon refreshes its catalog cache, the list is re-read now and then. */
const REFRESH_WATCH_MS = 3000;

export type LoginPhase = "idle" | "starting" | "waiting" | "failed";

/**
 * Composio state of the Apps modal: link status, connected apps, the catalog
 * (served instantly by the daemon from the bundled popular apps or its disk
 * cache), a poll for pending sign-ins and the one-time browser login.  Nothing
 * here ever sees an API key.
 */
export function useComposio(active: boolean, query: string, group: string) {
  const { client } = useDaemon();
  const [status, setStatus] = useState<ComposioStatus | null>(null);
  const [apps, setApps] = useState<ComposioApp[]>([]);
  const [items, setItems] = useState<ComposioToolkit[]>([]);
  const [meta, setMeta] = useState<Pick<ComposioCatalog, "source" | "refreshing" | "error"> | null>(null);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [pollErrors, setPollErrors] = useState<Record<string, string>>({});
  const [login, setLogin] = useState<LoginPhase>("idle");
  const seq = useRef(0);
  const loginAttempt = useRef(0);

  const reloadApps = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setApps(await client.composioApps());
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);

  const reloadStatus = useCallback(async () => {
    if (!client) {
      return null;
    }
    try {
      const s = await client.composioStatus();
      setStatus(s);
      setStatusError(null);
      return s;
    } catch (e) {
      setStatusError(humanLine(e));
      return null;
    }
  }, [client]);

  const loadCatalog = useCallback(
    async (search: string, grp: string, from: string | null) => {
      if (!client) {
        return;
      }
      const mine = ++seq.current;
      setLoading(true);
      try {
        const page = await client.composioToolkits({
          search: search.trim() || undefined,
          group: grp === "all" ? undefined : grp,
          cursor: from ?? undefined,
          limit: CATALOG_PAGE_SIZE
        });
        if (mine !== seq.current) {
          return;
        }
        setItems((prev) => (from ? appendPage(prev, page.items) : page.items));
        setMeta({ source: page.source, refreshing: page.refreshing, error: page.error });
        if (page.error === "unauthorized") {
          // The saved login is refused: the status carries `login_expired`, which the Connect flow reads.
          void reloadStatus();
        }
        setCursor(page.next_cursor ?? null);
        setError(null);
      } catch (e) {
        if (mine === seq.current) {
          setError(humanLine(e));
        }
      } finally {
        if (mine === seq.current) {
          setLoading(false);
        }
      }
    },
    [client, reloadStatus]
  );

  // Status and apps when the modal opens.
  useEffect(() => {
    if (!active) {
      return;
    }
    void Promise.all([reloadStatus(), reloadApps()]);
  }, [active, reloadApps, reloadStatus]);

  // The catalog follows search and chip (debounced).  The daemon answers from memory.
  useEffect(() => {
    if (!active) {
      return undefined;
    }
    setLoading(true);
    setCursor(null);
    const id = setTimeout(() => void loadCatalog(query, group, null), SEARCH_DEBOUNCE_MS);
    return () => {
      clearTimeout(id);
      seq.current += 1;
    };
  }, [active, query, group, status?.configured, loadCatalog]);

  // While the daemon refreshes the cache, pick the result up when it lands.
  const refreshing = meta?.refreshing === true ||
    (status?.configured === true && meta?.source === "bundled" && !meta.error);
  useEffect(() => {
    if (!active || !refreshing) {
      return undefined;
    }
    const id = setTimeout(() => void loadCatalog(query, group, null), REFRESH_WATCH_MS);
    return () => clearTimeout(id);
  }, [active, refreshing, query, group, items, loadCatalog]);

  // Poll pending sign-ins until they settle.
  const pending = pendingSlugs(apps).join(",");
  useEffect(() => {
    if (!active || !client || pending === "" || status?.login_expired) {
      return undefined;
    }
    const started = Date.now();
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      const expired = Date.now() - started >= POLL_GIVE_UP_MS;
      await Promise.all(pending.split(",").map(async (slug) => {
        try {
          const app = await client.composioApp(slug);
          if (!disposed) {
            setPollErrors((prev) => {
              if (!(slug in prev)) return prev;
              const next = { ...prev };
              delete next[slug];
              return next;
            });
            setApps((prev) => prev.some((a) => a.slug === slug && a.state === "pending")
              ? upsertApp(prev, expired && app.state === "pending" ? { ...app, state: "failed" } : app)
              : prev);
          }
        } catch (e) {
          if (!disposed) {
            setPollErrors((prev) => ({ ...prev, [slug]: humanLine(e) }));
            if (e instanceof ApiError && e.code === "composio_login_expired") void reloadStatus();
            if (expired) {
              setApps((prev) => prev.map((a) => a.slug === slug && a.state === "pending"
                ? { ...a, state: "failed" } : a));
            }
          }
        }
      }));
      if (!disposed && !expired) timer = setTimeout(() => void poll(), POLL_MS);
    };
    timer = setTimeout(() => void poll(), POLL_MS);
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [active, client, pending, reloadStatus, status?.login_expired]);

  // The one-time browser login: open the page, poll until Composio says linked.
  useEffect(() => {
    if (!active || !client || login !== "waiting") {
      return undefined;
    }
    const started = Date.now();
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    const attempt = loginAttempt.current;
    const poll = async () => {
      if (Date.now() - started > POLL_GIVE_UP_MS) {
        setLogin("failed");
        return;
      }
      try {
        const { state } = await client.composioLoginStatus();
        if (disposed || attempt !== loginAttempt.current) return;
        if (state === "linked") {
          await Promise.all([reloadStatus(), reloadApps()]);
          if (attempt === loginAttempt.current) setLogin("idle");
          return;
        }
        if (state === "failed" || state === "idle") {
          setLogin("failed");
          return;
        }
      } catch {
        // The next poll can recover a transient transport failure.
      }
      if (!disposed && attempt === loginAttempt.current) timer = setTimeout(() => void poll(), POLL_MS);
    };
    timer = setTimeout(() => void poll(), POLL_MS);
    return () => {
      disposed = true;
      clearTimeout(timer);
    };
  }, [active, client, login, reloadStatus, reloadApps]);

  /** Start the one-time account login, without starting any toolkit connection. */
  const startLogin = useCallback(
    async () => {
      if (!client) {
        return;
      }
      const attempt = ++loginAttempt.current;
      setLogin("starting");
      try {
        const r = await client.composioLoginStart();
        if (attempt !== loginAttempt.current) return;
        const opened = await openComposioLink(r.login_url, true);
        if (attempt === loginAttempt.current) setLogin(opened ? "waiting" : "failed");
      } catch {
        if (attempt === loginAttempt.current) setLogin("failed");
      }
    },
    [client]
  );

  const cancelLogin = useCallback(() => {
    loginAttempt.current += 1;
    setLogin("idle");
  }, []);

  return {
    status,
    statusError,
    pollErrors,
    apps,
    items,
    meta,
    hasMore: cursor !== null,
    loading: loading || refreshing,
    error,
    login,
    startLogin,
    cancelLogin,
    retryCatalog: () => loadCatalog(query, group, null),
    loadMore: () => cursor === null || loading || refreshing ? undefined : loadCatalog(query, group, cursor),
    setApps,
    reloadApps,
    reloadStatus
  };
}
