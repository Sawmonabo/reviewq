import { useRef, useState } from "react";
import type { CatalogItem } from "../../../api/generated/contracts/settings";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import type { CatalogConnection } from "../scope-draft";
import { useCatalogLookup } from "./useCatalogLookup";

export function SavedFilterPicker({
  connection,
  connectionEpoch,
  current,
  disabled,
  onAdd,
}: {
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly current: string | null | undefined;
  readonly disabled: boolean;
  readonly onAdd: (item: CatalogItem) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const search = useRef<HTMLInputElement>(null);
  const results = useRef<HTMLDivElement>(null);
  const lookup = useCatalogLookup({
    connection,
    connectionEpoch,
    kind: "filters",
    query: query.trim(),
    enabled: open && !disabled,
  });
  const visible = lookup.items.filter((item) => item.id !== current);

  return (
    <div
      className="catalog-picker stack"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.preventDefault();
          event.stopPropagation();
          setOpen(false);
          search.current?.focus();
          return;
        }
        if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
        const choices = Array.from(
          results.current?.querySelectorAll<HTMLButtonElement>("button:enabled") ?? [],
        );
        const index = choices.findIndex((button) => button === event.target);
        if (event.target !== search.current && index < 0) return;
        if (index < 0 && event.key === "ArrowUp") return;
        event.preventDefault();
        if (!open) {
          setOpen(true);
        } else {
          const next = event.key === "ArrowDown" ? index + 1 : index - 1;
          (next < 0 ? search.current : choices[next])?.focus();
        }
      }}
    >
      <div className="settings-search">
        <input
          ref={search}
          aria-label="Search saved filters"
          placeholder="Search saved filters…"
          value={query}
          maxLength={200}
          onChange={(event) => {
            const value = event.currentTarget.value;
            setQuery(value);
            if (value.trim().length >= 2) setOpen(true);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.nativeEvent.isComposing) {
              event.preventDefault();
              setOpen(true);
              lookup.search();
            }
          }}
        />
        <button
          type="button"
          aria-expanded={open}
          aria-label="Browse saved filters"
          onClick={() => {
            setOpen(!open);
            if (!open) search.current?.focus();
          }}
        >
          Browse
        </button>
      </div>
      {open && (
        <section className="catalog-choices stack" aria-label="Browse saved filters">
          {lookup.pending && !visible.length && <Loading label="Looking up choices…" />}
          {lookup.error && <Notice retry={lookup.retry}>{lookup.error}</Notice>}
          {!lookup.pending && !lookup.error && !visible.length && (
            <Empty title={query ? "No matching choices" : "No available choices"} />
          )}
          <div className="catalog-results" ref={results}>
            {visible.map((item) => (
              <button type="button" key={item.id} onClick={() => onAdd(item)}>
                <strong>{item.label}</strong>
                <span className="meta">{item.path ?? item.state ?? item.id}</span>
                <span className="catalog-add">Add</span>
              </button>
            ))}
          </div>
          {(lookup.hasPrevious || lookup.hasNext) && (
            <div className="actions">
              <button
                type="button"
                disabled={lookup.pending || !lookup.hasPrevious}
                onClick={lookup.previous}
              >
                Previous choices
              </button>
              <button
                type="button"
                disabled={lookup.pending || !lookup.hasNext}
                onClick={() => lookup.next()}
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
