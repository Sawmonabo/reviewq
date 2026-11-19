import { useEffect, useRef, useState } from "react";
import { browseCatalog } from "../../api/settings";
import type { CatalogItem, CatalogKind, CatalogRequest, ProjectView } from "../../api/generated/contracts/settings";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { describeError } from "../../lib/presentation";

type Props = {
  readonly label: string;
  readonly scope: Omit<CatalogRequest, "kind" | "cursor" | "q" | "view">;
  readonly kind: CatalogKind;
  readonly unavailable: readonly string[];
  readonly onAdd: (item: CatalogItem) => void;
};

export function CatalogPicker({ label, scope, kind, unavailable, onAdd }: Props) {
  const [open, setOpen] = useState(false);
  const [q, setQuery] = useState("");
  const [view, setView] = useState<ProjectView>("member");
  const [forced, setForced] = useState(0);
  const [page, setPage] = useState<{ scope: string; query: string; items: readonly CatalogItem[]; next: string | null }>();
  const [error, setError] = useState<{ query: string; message: string }>();
  const [busy, setBusy] = useState(false);
  const current = useRef<AbortController | undefined>(undefined);
  const search = useRef<HTMLInputElement>(null);
  const first = useRef<HTMLButtonElement>(null);
  const scopeKey = JSON.stringify(scope);
  const query = JSON.stringify([scopeKey, q, view]);
  const bound = page?.query === query ? page : undefined;
  const visible = bound?.items.filter((item) => !unavailable.includes(item.id));

  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    current.current?.abort();
    current.current = controller;
    const timer = window.setTimeout(() => {
      setBusy(true);
      const body = JSON.parse(scopeKey) as typeof scope;
      void browseCatalog({ ...body, kind, q, ...(kind === "projects" ? { view } : {}) }, controller.signal).then(
        ({ data }) => {
          if (!controller.signal.aborted) { setPage({ scope: scopeKey, query, items: data.items, next: data.nextCursor }); setError(undefined); setBusy(false); }
        },
        (failure: unknown) => {
          if (!controller.signal.aborted) { setError({ query, message: describeError(failure) }); setBusy(false); }
        },
      );
    }, forced || q.length < 2 ? 0 : 300);
    return () => { controller.abort(); current.current?.abort(); window.clearTimeout(timer); };
  }, [open, forced, scopeKey, q, kind, view, query]);

  async function more() {
    if (!bound?.next || busy) return;
    const controller = new AbortController();
    current.current?.abort();
    current.current = controller;
    setBusy(true);
    try {
      const { data } = await browseCatalog({ ...scope, kind, q, cursor: bound.next, ...(kind === "projects" ? { view } : {}) }, controller.signal);
      if (!controller.signal.aborted) setPage({ scope: scopeKey, query, items: [...bound.items, ...data.items].slice(0, 400), next: data.nextCursor });
    } catch (failure) {
      if (!controller.signal.aborted) setError({ query, message: describeError(failure) });
    } finally { if (!controller.signal.aborted) setBusy(false); }
  }

  return <div className="catalog-picker stack" onKeyDown={(event) => {
    if (event.key === "Escape" && open) { event.stopPropagation(); setOpen(false); }
  }}>
    <button type="button" aria-expanded={open} onClick={() => {
      setOpen(!open);
      if (!open) requestAnimationFrame(() => search.current?.focus());
    }}>{label}</button>
    {open && <section className="surface-card stack" aria-label={label}>
      <form className="actions" onSubmit={(event) => { event.preventDefault(); setForced((count) => count + 1); }}>
        <input ref={search} aria-label={`Search ${label.toLowerCase()}`} placeholder="Search by name" value={q} maxLength={200}
          onChange={(event) => { setQuery(event.currentTarget.value); setForced(0); }}
          onKeyDown={(event) => { if (event.key === "ArrowDown") { event.preventDefault(); first.current?.focus(); } }} />
        {kind === "projects" && <select aria-label="Repository visibility" value={view} onChange={(event) => setView(event.currentTarget.value as ProjectView)}>
          <option value="member">Member projects</option><option value="starred">Starred projects</option><option value="all">All visible projects</option>
        </select>}
        <button type="submit">Search</button>
      </form>
      {busy && !bound && <Loading label="Looking up choices…" />}
      {error?.query === query && <Notice retry={() => setForced((count) => count + 1)}>{error.message}</Notice>}
      {visible?.length === 0 && <Empty title={q ? "No matching choices" : "No available choices"} />}
      {visible && <div className="catalog-results">{visible.map((item, index) => <button type="button" key={item.id} ref={index === 0 ? first : undefined} onClick={() => onAdd(item)}>
        <strong>{item.label}</strong><span className="meta">{item.path ?? item.state ?? item.boardType ?? item.id}</span><span className="catalog-add">Add</span>
      </button>)}</div>}
      {bound?.next && bound.items.length < 400 && <button type="button" disabled={busy} onClick={() => void more()}>More choices</button>}
    </section>}
  </div>;
}
