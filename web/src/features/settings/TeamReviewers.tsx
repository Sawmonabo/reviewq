import { ListBox, ListBoxItem, Text } from "react-aria-components/ListBox";
import { useEffect, useId, useRef, useState } from "react";
import { browseCatalog } from "../../api/settings";
import type { CatalogItem } from "../../api/generated/contracts/settings";
import { Icon } from "../../components/Icon";
import { Modal } from "../../components/Modal";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError } from "../../lib/presentation";
import type { CatalogConnection } from "./scope-draft";

const MAX_REVIEWERS = 256;

export function TeamReviewers({
  active,
  connection,
  savedConnection,
  savedIds,
  ids,
  canLookUp,
  disabled,
  onChange,
  onConnections,
}: {
  readonly active: boolean;
  readonly connection: CatalogConnection;
  readonly savedConnection: CatalogConnection | undefined;
  readonly savedIds: readonly string[];
  readonly ids: readonly string[];
  readonly canLookUp: boolean;
  readonly disabled: boolean;
  readonly onChange: (ids: string[]) => void;
  readonly onConnections: () => void;
}) {
  const [picking, setPicking] = useState(false);
  const [chosen, setChosen] = useState<ReadonlyMap<string, CatalogItem>>(() => new Map());
  const add = useRef<HTMLButtonElement>(null);
  const saved = useResource(
    `team-reviewers:${savedConnection?.expectedRevision ?? ""}:${active}`,
    async (signal) => {
      const people = new Map<string, CatalogItem>();
      if (!active || !savedConnection || !savedIds.length) return { data: people, cursor: "" };
      let next: string | null = null;
      let cursor: string;
      do {
        const page = await browseCatalog(
          { ...savedConnection, kind: "reviewers", ...(next ? { cursor: next } : {}) },
          signal,
        );
        if (next && next === page.data.nextCursor)
          throw new Error("GitLab repeated a reviewer page.");
        for (const item of page.data.items) if (item.reviewer) people.set(item.id, item);
        next = page.data.nextCursor;
        cursor = page.cursor;
      } while (next);
      return { data: people, cursor };
    },
  );

  return (
    <section className="settings-reviewers" aria-labelledby="settings-team-reviewers">
      <div className="settings-inline">
        <h3 id="settings-team-reviewers">Team reviewers</h3>
        <button
          ref={add}
          type="button"
          disabled={disabled || !canLookUp || ids.length >= MAX_REVIEWERS}
          onClick={() => setPicking(true)}
        >
          Add reviewers
        </button>
      </div>
      <p className="meta">
        Comments and approvals from these GitLab users count as your team’s activity.
      </p>
      {!canLookUp && (
        <p className="meta">
          <button className="quiet-button" type="button" onClick={onConnections}>
            Connect GitLab
          </button>{" "}
          to look up reviewers.
        </p>
      )}
      {saved.error && <Notice retry={saved.retry}>{saved.error}</Notice>}
      {ids.length ? (
        <ul className="settings-reviewers-list">
          {ids.map((id) => {
            const person = saved.data?.get(id) ?? chosen.get(id);
            const name = person?.label ?? `GitLab user #${id}`;
            return (
              <li key={id}>
                <div>
                  <strong>{name}</strong>
                  {person && (
                    <small>
                      {detail(person)} · #{id}
                    </small>
                  )}
                </div>
                <button
                  type="button"
                  className="quiet-button"
                  aria-label={`Remove ${name} (${id})`}
                  disabled={disabled}
                  onClick={() => {
                    onChange(ids.filter((value) => value !== id));
                    if (canLookUp) add.current?.focus();
                  }}
                >
                  Remove
                </button>
              </li>
            );
          })}
        </ul>
      ) : (
        <p className="meta">No additional reviewers selected.</p>
      )}
      {picking && active && (
        <ReviewerPicker
          connection={connection}
          added={ids}
          onClose={() => setPicking(false)}
          onAdd={(people) => {
            setChosen(
              new Map([...chosen, ...people.map((person) => [person.id, person] as const)]),
            );
            onChange([...ids, ...people.map((person) => person.id)]);
            setPicking(false);
          }}
        />
      )}
    </section>
  );
}

function ReviewerPicker({
  connection,
  added,
  onClose,
  onAdd,
}: {
  readonly connection: CatalogConnection;
  readonly added: readonly string[];
  readonly onClose: () => void;
  readonly onAdd: (people: readonly CatalogItem[]) => void;
}) {
  const [query, setQuery] = useState("");
  const [composing, setComposing] = useState<string | null>(null);
  const [selected, setSelected] = useState<ReadonlyMap<string, CatalogItem>>(() => new Map());
  const [supplement, setSupplement] = useState<{
    term: string;
    items: readonly CatalogItem[];
    next: string | null;
  }>();
  const [moreError, setMoreError] = useState<string>();
  const [loadingMore, setLoadingMore] = useState(false);
  const more = useRef<AbortController>(null);
  const search = useRef<HTMLInputElement>(null);
  const results = useRef<HTMLDivElement>(null);
  const listId = useId();
  const term = query.trim();
  const canSearch = Array.from(term).length >= 2 || /^\d$/u.test(term);
  const first = useResource(`reviewer-search:${term}`, async (signal) => {
    if (!canSearch) return { data: { items: [], nextCursor: null }, cursor: "" };
    await new Promise<void>((resolve) => {
      const timer = window.setTimeout(resolve, 250);
      signal.addEventListener(
        "abort",
        () => {
          window.clearTimeout(timer);
          resolve();
        },
        { once: true },
      );
    });
    signal.throwIfAborted();
    return browseCatalog({ ...connection, kind: "reviewers", q: term }, signal);
  });
  const page = supplement?.term === term ? supplement : undefined;
  const items = [
    ...new Map(
      (page?.items ?? first.data?.items ?? [])
        .filter((item) => item.reviewer)
        .map((item) => [item.id, item]),
    ).values(),
  ];
  const next = page ? page.next : first.data?.nextCursor;
  const remaining = MAX_REVIEWERS - added.length;

  useEffect(() => {
    return () => more.current?.abort();
  }, []);

  async function readMore(cursor: string) {
    more.current?.abort();
    const controller = new AbortController();
    more.current = controller;
    setLoadingMore(true);
    setMoreError(undefined);
    try {
      const { data } = await browseCatalog(
        { ...connection, kind: "reviewers", q: term, cursor },
        controller.signal,
      );
      if (controller.signal.aborted) return;
      if (cursor === data.nextCursor) throw new Error("GitLab repeated a reviewer search page.");
      setSupplement({ term, items: [...items, ...data.items], next: data.nextCursor });
    } catch (error) {
      if (!controller.signal.aborted) setMoreError(describeError(error));
    } finally {
      if (!controller.signal.aborted) setLoadingMore(false);
    }
  }

  function updateQuery(value: string) {
    more.current?.abort();
    setLoadingMore(false);
    setMoreError(undefined);
    setSupplement(undefined);
    setQuery(value);
  }

  function enabledOptions() {
    return Array.from(
      results.current?.querySelectorAll<HTMLElement>(
        '[role="option"]:not([aria-disabled="true"])',
      ) ?? [],
    );
  }

  return (
    <Modal
      title="Add team reviewers"
      className="reviewer-picker"
      initialFocus={search}
      onClose={onClose}
      footer={
        <>
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="primary"
            disabled={!selected.size}
            onClick={() => onAdd([...selected.values()])}
          >
            {selected.size > 1 ? `Add ${selected.size} reviewers` : "Add reviewer"}
          </button>
        </>
      }
    >
      <div className="stack">
        <label className="field">
          Search GitLab users
          <input
            ref={search}
            type="search"
            autoComplete="off"
            maxLength={200}
            placeholder="Name, @username, user ID or public email"
            aria-controls={items.length ? listId : undefined}
            value={composing ?? query}
            onChange={(event) => {
              if (composing !== null) setComposing(event.currentTarget.value);
              else updateQuery(event.currentTarget.value);
            }}
            onCompositionStart={(event) => setComposing(event.currentTarget.value)}
            onCompositionEnd={(event) => {
              setComposing(null);
              updateQuery(event.currentTarget.value);
            }}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing || composing !== null) return;
              if (!["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) return;
              const options = enabledOptions();
              const option = event.key === "ArrowUp" ? options.at(-1) : options[0];
              if (option) {
                event.preventDefault();
                option.focus();
              }
            }}
          />
        </label>
        <div
          ref={results}
          onKeyDownCapture={(event) => {
            if (event.key === "ArrowUp" && enabledOptions()[0] === event.target) {
              event.preventDefault();
              event.stopPropagation();
              search.current?.focus();
            }
          }}
        >
          {items.length > 0 && (
            <ListBox
              id={listId}
              className="reviewer-results"
              aria-label="GitLab search results"
              aria-busy={loadingMore || undefined}
              items={items}
              selectionMode="multiple"
              selectionBehavior="toggle"
              selectedKeys={new Set(items.filter(({ id }) => selected.has(id)).map(({ id }) => id))}
              onSelectionChange={(keys) => {
                const visible = new Set(items.map(({ id }) => id));
                const selection = new Map(
                  [...selected].filter(
                    ([id]) => !visible.has(id) || keys === "all" || keys.has(id),
                  ),
                );
                for (const item of items) {
                  if (selection.size >= remaining) break;
                  if (!added.includes(item.id) && (keys === "all" || keys.has(item.id))) {
                    selection.set(item.id, item);
                  }
                }
                setSelected(selection);
              }}
            >
              {(item) => (
                <ListBoxItem
                  id={item.id}
                  className="reviewer-option"
                  textValue={item.label}
                  isDisabled={
                    added.includes(item.id) ||
                    (selected.size >= remaining && !selected.has(item.id))
                  }
                >
                  {({ isSelected }) => (
                    <>
                      <span className="reviewer-check" aria-hidden="true">
                        {isSelected && <Icon name="check" size={13} />}
                      </span>
                      <span className="reviewer-option-text">
                        <Text slot="label" elementType="strong">
                          {item.label}
                        </Text>
                        <Text slot="description" elementType="small">
                          {detail(item)} · GitLab #{item.id}
                        </Text>
                      </span>
                      {added.includes(item.id) && <small className="meta">Already added</small>}
                    </>
                  )}
                </ListBoxItem>
              )}
            </ListBox>
          )}
        </div>
        {first.error && <Notice retry={first.retry}>{first.error}</Notice>}
        {canSearch && first.pending && <Loading label="Searching GitLab…" />}
        {!first.error && !first.pending && !items.length && (
          <p className="meta" role="status">
            {canSearch
              ? "No matching GitLab users."
              : "Enter a name, handle, user ID or public email."}
          </p>
        )}
        {moreError && <Notice>{moreError}</Notice>}
        {next && (
          <button type="button" disabled={loadingMore} onClick={() => void readMore(next)}>
            {loadingMore
              ? "Loading more results…"
              : moreError
                ? "Retry more results"
                : "More results"}
          </button>
        )}
        {selected.size > 0 && (
          <ul className="reviewer-selection" aria-label="Selected reviewers">
            {[...selected.values()].map((item) => (
              <li key={item.id}>
                <button
                  type="button"
                  aria-label={`Remove ${item.label} from selection`}
                  onClick={() => {
                    const selection = new Map(selected);
                    selection.delete(item.id);
                    setSelected(selection);
                  }}
                >
                  {item.label} <Icon name="close" size={12} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Modal>
  );
}

function detail(item: CatalogItem) {
  return [item.reviewer && `@${item.reviewer.username}`, item.reviewer?.publicEmail]
    .filter(Boolean)
    .join(" · ");
}
