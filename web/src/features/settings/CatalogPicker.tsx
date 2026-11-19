import { useEffect, useEffectEvent, useRef, useState } from "react";
import { browseCatalog } from "../../api/settings";
import type {
  CatalogItem,
  CatalogKind,
  CatalogRequest,
  ProjectView,
} from "../../api/generated/contracts/settings";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { describeError } from "../../lib/presentation";

type Props = {
  readonly label: string;
  readonly scope: Omit<CatalogRequest, "kind" | "cursor" | "q" | "view">;
  readonly kind: CatalogKind;
  readonly inputId?: string;
  readonly disabled?: boolean;
  readonly connectionEpoch: number;
  readonly unavailable: readonly string[];
  readonly onAdd: (item: CatalogItem) => void;
};

export function CatalogPicker({
  label,
  scope,
  kind,
  inputId,
  disabled = false,
  unavailable,
  onAdd,
  connectionEpoch,
}: Props) {
  const [open, setOpen] = useState(false);
  const [q, setQuery] = useState("");
  const [view, setView] = useState<ProjectView>("member");
  const [forced, setForced] = useState(0);
  const [page, setPage] = useState<{
    query: string;
    items: readonly CatalogItem[];
    next: string | null;
    cursor: string | undefined;
    previous: readonly (string | undefined)[];
  }>();
  const [error, setError] = useState<{ query: string; message: string }>();
  const [busy, setBusy] = useState(false);
  const current = useRef<AbortController | undefined>(undefined);
  const search = useRef<HTMLInputElement>(null);
  const first = useRef<HTMLButtonElement>(null);
  const scopeKey = JSON.stringify([
    scope.expectedRevision,
    scope.provider,
    scope.boardId,
    connectionEpoch,
  ]);
  function readPage(cursor: string | undefined, signal: AbortSignal) {
    return browseCatalog(
      {
        ...scope,
        kind,
        q,
        ...(cursor ? { cursor } : {}),
        ...(kind === "projects" ? { view } : {}),
      },
      signal,
    );
  }
  const lookup = useEffectEvent((signal: AbortSignal) => readPage(undefined, signal));
  const query = JSON.stringify([scopeKey, q, view]);
  const bound = page?.query === query ? page : undefined;
  const visible = bound?.items.filter((item) => !unavailable.includes(item.id));
  const searchLabel =
    kind === "boards"
      ? "Search boards"
      : kind === "projects"
        ? "Search repositories"
        : "Search saved filters";

  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    current.current?.abort();
    current.current = controller;
    const timer = window.setTimeout(
      () => {
        setBusy(true);
        void lookup(controller.signal).then(
          ({ data }) => {
            if (!controller.signal.aborted) {
              setPage({
                query,
                items: data.items,
                next: data.nextCursor,
                cursor: undefined,
                previous: [],
              });
              setError(undefined);
              setBusy(false);
            }
          },
          (failure: unknown) => {
            if (!controller.signal.aborted) {
              setError({ query, message: describeError(failure) });
              setBusy(false);
            }
          },
        );
      },
      forced || q.length < 2 ? 0 : 300,
    );
    return () => {
      controller.abort();
      current.current?.abort();
      window.clearTimeout(timer);
    };
  }, [open, forced, scopeKey, q, kind, view, query]);

  async function movePage(cursor: string | undefined, previous: readonly (string | undefined)[]) {
    if (busy || !bound) return;
    const controller = new AbortController();
    current.current?.abort();
    current.current = controller;
    setBusy(true);
    try {
      const { data } = await readPage(cursor, controller.signal);
      if (!controller.signal.aborted) {
        setPage({ query, items: data.items, next: data.nextCursor, cursor, previous });
        setError(undefined);
      }
    } catch (failure) {
      if (!controller.signal.aborted) setError({ query, message: describeError(failure) });
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return (
    <div
      className="catalog-picker stack"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          setOpen(false);
          search.current?.focus();
        }
      }}
    >
      <div className="settings-search">
        <input
          ref={search}
          id={inputId}
          aria-label={searchLabel}
          placeholder={`${searchLabel}…`}
          value={q}
          disabled={disabled}
          maxLength={200}
          onChange={(event) => {
            const value = event.currentTarget.value;
            setQuery(value);
            setForced(0);
            if (value.trim().length >= 2) setOpen(true);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown") {
              event.preventDefault();
              if (first.current) first.current.focus();
              else setOpen(true);
            } else if (event.key === "Enter") {
              event.preventDefault();
              setOpen(true);
              setForced((count) => count + 1);
            }
          }}
        />
        {kind === "projects" && (
          <select
            aria-label="Repository visibility"
            value={view}
            disabled={disabled}
            onChange={(event) => {
              const value = event.currentTarget.value;
              if (value === "member" || value === "starred" || value === "all") setView(value);
              setOpen(true);
            }}
          >
            <option value="member">Member projects</option>
            <option value="starred">Starred projects</option>
            <option value="all">All visible projects</option>
          </select>
        )}
        <button
          type="button"
          disabled={disabled}
          aria-expanded={open}
          aria-label={label}
          onClick={() => {
            setOpen(!open);
            if (!open) requestAnimationFrame(() => search.current?.focus());
          }}
        >
          Browse
        </button>
      </div>
      {open && (
        <section className="catalog-choices stack" aria-label={label}>
          {busy && !bound && <Loading label="Looking up choices…" />}
          {error?.query === query && (
            <Notice retry={() => setForced((count) => count + 1)}>{error.message}</Notice>
          )}
          {visible?.length === 0 && (
            <Empty title={q ? "No matching choices" : "No available choices"} />
          )}
          {visible && (
            <div className="catalog-results">
              {visible.map((item, index) => (
                <button
                  type="button"
                  key={item.id}
                  disabled={disabled}
                  ref={index === 0 ? first : undefined}
                  onClick={() => onAdd(item)}
                >
                  <strong>{item.label}</strong>
                  <span className="meta">
                    {item.path ?? item.state ?? item.boardType ?? item.id}
                  </span>
                  <span className="catalog-add">Add</span>
                </button>
              ))}
            </div>
          )}
          {bound && (bound.next || bound.previous.length > 0) && (
            <div className="actions">
              <button
                type="button"
                disabled={busy || disabled || !bound.previous.length}
                onClick={() => void movePage(bound.previous.at(-1), bound.previous.slice(0, -1))}
              >
                Previous choices
              </button>
              <button
                type="button"
                disabled={busy || disabled || !bound.next}
                onClick={() =>
                  bound.next && void movePage(bound.next, [...bound.previous, bound.cursor])
                }
              >
                More choices
              </button>
            </div>
          )}
        </section>
      )}
    </div>
  );
}
