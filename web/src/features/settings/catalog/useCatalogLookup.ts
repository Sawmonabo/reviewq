import { useEffect, useEffectEvent, useMemo, useState } from "react";
import type {
  CatalogItem,
  CatalogKind,
  ProjectView,
} from "../../../api/generated/contracts/settings";
import { browseCatalog } from "../../../api/settings";
import { describeError } from "../../../lib/presentation";
import type { CatalogConnection } from "../scope-draft";

type Options = {
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly kind: CatalogKind;
  readonly browseKind?: CatalogKind | undefined;
  readonly query: string;
  readonly view?: ProjectView | undefined;
  readonly enabled: boolean;
};

type Request = {
  readonly scope: object;
  readonly cursor: string | undefined;
  readonly previous: readonly Visit[];
  readonly immediate: boolean;
  readonly forward: boolean;
};

type Visit = { readonly cursor: string | undefined; readonly ids: readonly string[] };

type Page = {
  readonly request: Request;
  readonly items: readonly CatalogItem[];
  readonly next: string | null;
};

type Result = {
  readonly request: Request;
  readonly page?: Page | undefined;
  readonly error?: string;
};

const empty: readonly CatalogItem[] = [];

export function useCatalogLookup({
  connection,
  connectionEpoch,
  kind,
  browseKind = kind,
  query,
  view,
  enabled,
}: Options) {
  const base = JSON.stringify([
    connection.provider,
    connection.expectedRevision,
    connectionEpoch,
    browseKind,
    view,
  ]);
  const scope = useMemo(() => ({ base, kind, query, enabled }), [base, kind, query, enabled]);
  const first = useMemo<Request>(
    () => ({ scope, cursor: undefined, previous: [], immediate: false, forward: false }),
    [scope],
  );
  const [requested, setRequested] = useState<Request>();
  const [result, setResult] = useState<Result>();
  const [browse, setBrowse] = useState<ReadonlyMap<string, readonly CatalogItem[]>>(
    () => new Map(),
  );
  const request = requested?.scope === scope ? requested : first;
  const page = result?.page?.request.scope === scope ? result.page : undefined;
  const pending = enabled && result?.request !== request;
  const error = result?.request === request ? result.error : undefined;

  const bindRead = useEffectEvent(() => {
    const body = {
      ...connection,
      kind: query ? kind : browseKind,
      ...(query ? { q: query } : {}),
      ...(view ? { view } : {}),
    };
    return (cursor: string | undefined, signal: AbortSignal) =>
      browseCatalog({ ...body, ...(cursor ? { cursor } : {}) }, signal);
  });

  useEffect(() => {
    if (!pending) return;
    const controller = new AbortController();
    const { signal } = controller;
    const read = bindRead();
    async function fetchPage() {
      try {
        const seen = new Set(request.previous.flatMap(({ ids }) => ids));
        const visited = new Set(request.previous.map(({ cursor }) => cursor));
        let cursor = request.cursor;
        for (let skipped = 0; ; skipped++) {
          const { data } = await read(cursor, signal);
          if (signal.aborted) return;
          const next = data.nextCursor;
          visited.add(cursor);
          if (next && visited.has(next)) throw new Error("The provider repeated a catalog page.");
          const items = [...new Map(data.items.map((item) => [item.id, item])).values()].filter(
            ({ id }) => !seen.has(id),
          );
          if (items.length || !request.forward) {
            setResult({ request, page: { request: { ...request, cursor }, items, next } });
            if (!query && !cursor) setBrowse((previous) => new Map(previous).set(base, items));
            return;
          }
          // A project-key page and a later board-name page can contain the same IDs.
          if (!next || skipped >= 2) {
            setResult((previous) => ({
              request,
              page: previous?.page ? { ...previous.page, next } : undefined,
            }));
            return;
          }
          cursor = next;
        }
      } catch (failure) {
        if (signal.aborted) return;
        setResult((previous) => ({
          request,
          page: previous?.page,
          error: describeError(failure),
        }));
      }
    }
    const timer = window.setTimeout(() => void fetchPage(), !request.immediate && query ? 300 : 0);
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [base, pending, query, request]);

  return {
    items: page?.items ?? empty,
    browseItems: browse.get(base) ?? empty,
    pending,
    error,
    page: page ? page.request.previous.length + 1 : 1,
    hasPrevious: Boolean(page?.request.previous.length),
    hasNext: Boolean(page?.next),
    search: () => setRequested({ ...first, immediate: true }),
    retry: () => setRequested({ ...request, immediate: true }),
    previous: () => {
      if (pending || !page?.request.previous.length) return;
      setRequested({
        scope,
        cursor: page.request.previous.at(-1)?.cursor,
        previous: page.request.previous.slice(0, -1),
        immediate: true,
        forward: false,
      });
    },
    next: (ids?: readonly string[]) => {
      if (pending || !page?.next) return;
      setRequested({
        scope,
        cursor: page.next,
        previous: [
          ...page.request.previous,
          { cursor: page.request.cursor, ids: ids ?? page.items.map(({ id }) => id) },
        ],
        immediate: true,
        forward: true,
      });
    },
  };
}
