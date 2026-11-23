import { useRef, useState } from "react";
import { browseCatalog } from "../../api/settings";
import type { CatalogItem } from "../../api/generated/contracts/settings";
import { Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { CatalogDialog } from "./catalog/CatalogDialog";
import { catalogChoice } from "./catalog/picker-items";
import type { CatalogConnection } from "./scope-draft";

export function TeamReviewers({
  active,
  browsing,
  connection,
  connectionEpoch,
  suggestReviewers,
  savedConnection,
  savedIds,
  ids,
  canLookUp,
  disabled,
  onChange,
  onConnections,
  onBrowse,
  onDismissCatalog,
}: {
  readonly active: boolean;
  readonly browsing: boolean;
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly suggestReviewers: boolean;
  readonly savedConnection: CatalogConnection | undefined;
  readonly savedIds: readonly string[];
  readonly ids: readonly string[];
  readonly canLookUp: boolean;
  readonly disabled: boolean;
  readonly onChange: (ids: string[]) => void;
  readonly onConnections: () => void;
  readonly onBrowse: () => void;
  readonly onDismissCatalog: () => void;
}) {
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
  function reviewer(id: string) {
    const item = saved.data?.get(id) ?? chosen.get(id);
    return (item && catalogChoice("reviewers", item)) || { id, label: `GitLab user #${id}` };
  }

  return (
    <section className="settings-reviewers" aria-labelledby="settings-team-reviewers">
      <div className="settings-inline">
        <h3 id="settings-team-reviewers">Team reviewers</h3>
        <button
          ref={add}
          type="button"
          aria-haspopup="dialog"
          disabled={disabled || !canLookUp}
          onClick={(event) => {
            event.currentTarget.focus({ preventScroll: true });
            onBrowse();
          }}
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
            const person = reviewer(id);
            return (
              <li key={id}>
                <div>
                  <strong>{person.label}</strong>
                  {person.label !== `GitLab user #${id}` && (
                    <small>
                      {person.detail && `${person.detail} · `}#{id}
                    </small>
                  )}
                </div>
                <button
                  type="button"
                  className="quiet-button"
                  aria-label={`Remove ${person.label} (${id})`}
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
      {browsing && active && (
        <CatalogDialog
          kind="reviewers"
          connection={connection}
          connectionEpoch={connectionEpoch}
          canBrowse={suggestReviewers}
          added={ids}
          tracked={ids.map(reviewer)}
          onClose={onDismissCatalog}
          onAdd={(people) => {
            setChosen(
              (known) =>
                new Map([...known, ...people.map((person) => [person.id, person] as const)]),
            );
            onChange([...new Set([...ids, ...people.map((person) => person.id)])]);
          }}
        />
      )}
    </section>
  );
}
