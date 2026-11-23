import { Header, ListBox, ListBoxItem, ListBoxSection, Text } from "react-aria-components/ListBox";
import type { Selection } from "react-aria-components/ListBox";
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type { CatalogItem, ProjectView } from "../../../api/generated/contracts/settings";
import { Icon } from "../../../components/Icon";
import { Modal } from "../../../components/Modal";
import { describeError } from "../../../lib/presentation";
import type { CatalogConnection } from "../scope-draft";
import { catalogChoice, pickerText } from "./picker-items";
import type { PickerChoice, PickerKind } from "./picker-items";
import { useCatalogLookup } from "./useCatalogLookup";
import "./picker.css";

type Option = PickerChoice & { readonly item?: CatalogItem };

const repositoryViews = {
  member: "Member projects",
  starred: "Starred projects",
  all: "All visible projects",
} as const;

export function CatalogDialog({
  kind,
  connection,
  connectionEpoch,
  added,
  tracked,
  canBrowse = true,
  onAdd,
  onClose,
}: {
  readonly kind: PickerKind;
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly added: readonly string[];
  readonly tracked: readonly PickerChoice[];
  readonly canBrowse?: boolean;
  readonly onAdd: (items: readonly CatalogItem[], signal: AbortSignal) => void | Promise<void>;
  readonly onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const [composing, setComposing] = useState<string | null>(null);
  const [view, setView] = useState<ProjectView>("member");
  const [selected, setSelected] = useState<ReadonlyMap<string, CatalogItem>>(() => new Map());
  const [confirming, setConfirming] = useState(false);
  const [addError, setAddError] = useState<string>();
  const search = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const confirmRequest = useRef<AbortController>(null);
  const pageButton = useCallback((node: HTMLButtonElement | null) => {
    if (!node) return;
    return () => {
      if (document.activeElement === node) search.current?.focus({ preventScroll: true });
    };
  }, []);
  const listId = useId();
  const term = query.trim();
  const canSearch = Array.from(term).length >= 2 || /^[0-9]$/u.test(term);
  const lookup = useCatalogLookup({
    connection,
    connectionEpoch,
    kind,
    browseKind: kind === "reviewers" ? "reviewerSuggestions" : kind,
    query: term,
    view: kind === "projects" ? view : undefined,
    enabled: term ? canSearch : canBrowse,
  });
  const text = pickerText[kind];
  const addedIds = new Set(added);
  const remaining = text.limit - addedIds.size;
  const options = new Map<string, Option>();
  for (const item of lookup.items) {
    const choice = catalogChoice(kind, item);
    if (choice) options.set(item.id, { ...choice, item });
  }
  if (lookup.page === 1 && (term || lookup.pending || lookup.error)) {
    for (const item of lookup.browseItems) {
      if (options.has(item.id)) continue;
      const choice = catalogChoice(kind, item);
      if (choice && matchesQuery(choice, term)) options.set(item.id, { ...choice, item });
    }
  }
  const trackedOptions =
    lookup.page > 1 || (kind === "projects" && view === "starred")
      ? []
      : tracked
          .filter((choice) => !options.has(choice.id) && matchesQuery(choice, term))
          .slice(0, 24);
  const visibleSelected = new Set([...selected.keys()].filter((id) => options.has(id)));
  const hasOptions = options.size + trackedOptions.length > 0;
  const paged = lookup.hasPrevious || lookup.hasNext;
  const status = confirming
    ? "Checking Jira board details…"
    : lookup.pending
      ? term
        ? "Searching…"
        : "Loading choices…"
      : paged
        ? `Page ${lookup.page}`
        : hasOptions
          ? ""
          : term && canSearch
            ? `No matching ${text.plural}.`
            : term
              ? "Enter at least two characters to search."
              : canBrowse
                ? `No ${text.plural} available.`
                : `Search to find ${text.plural}.`;

  useEffect(() => () => confirmRequest.current?.abort(), []);
  useLayoutEffect(() => {
    if (list.current) list.current.scrollTop = 0;
  }, [term, view, lookup.page]);

  function enabledResults() {
    return Array.from(
      list.current?.querySelectorAll<HTMLElement>('[role="option"]:not([aria-disabled="true"])') ??
        [],
    );
  }

  function dismiss() {
    confirmRequest.current?.abort();
    onClose();
  }

  function searchKey(event: KeyboardEvent<HTMLInputElement>) {
    if (event.nativeEvent.isComposing || composing !== null) return;
    if (!["ArrowDown", "ArrowUp", "Enter"].includes(event.key)) return;
    const results = enabledResults();
    const focus = event.key === "ArrowUp" ? results.at(-1) : results[0];
    if (event.key === "Enter" || focus) event.preventDefault();
    if (focus) focus.focus();
    else if (event.key === "Enter" && (canSearch || (!term && canBrowse))) lookup.search();
  }

  function choose(keys: Selection) {
    if (confirming) return;
    setAddError(undefined);
    setSelected((previous) => {
      const next = new Map(
        [...previous].filter(([id]) => !options.has(id) || keys === "all" || keys.has(id)),
      );
      for (const option of options.values()) {
        if (next.size >= remaining) break;
        if (option.item && !addedIds.has(option.id) && (keys === "all" || keys.has(option.id))) {
          next.set(option.id, option.item);
        }
      }
      return next;
    });
  }

  async function confirm() {
    if (confirmRequest.current || !selected.size) return;
    const items = [...selected.values()].filter(({ id }) => !addedIds.has(id));
    if (!items.length || items.length > remaining) return;
    const controller = new AbortController();
    confirmRequest.current = controller;
    setConfirming(true);
    setAddError(undefined);
    try {
      await onAdd(items, controller.signal);
      if (!controller.signal.aborted) onClose();
    } catch (failure) {
      if (!controller.signal.aborted) setAddError(describeError(failure));
    } finally {
      confirmRequest.current = null;
      if (!controller.signal.aborted) setConfirming(false);
    }
  }

  function renderOption(option: Option) {
    const current = addedIds.has(option.id);
    const outsideView = option.item?.outsideView === true && view !== "all";
    return (
      <ListBoxItem
        key={option.id}
        id={option.id}
        className="catalog-option"
        textValue={option.label}
        isDisabled={
          confirming ||
          current ||
          !option.item ||
          (selected.size >= remaining && !selected.has(option.id))
        }
      >
        {({ isSelected }) => (
          <>
            <span className="catalog-check" aria-hidden="true" data-added={current || undefined}>
              {(isSelected || current) && <Icon name="check" size={12} />}
            </span>
            <span className="catalog-option-text">
              <Text slot="label" elementType="strong">
                {option.label}
              </Text>
              <Text slot="description" elementType="small">
                {option.detail && <span>{option.detail}</span>}
                {option.label !== `${text.source} #${option.id}` && (
                  <span>
                    {text.source} #{option.id}
                  </span>
                )}
                {outsideView && <span>Outside {repositoryViews[view].toLowerCase()}</span>}
                {current && <span className="catalog-added">Already added</span>}
              </Text>
            </span>
          </>
        )}
      </ListBoxItem>
    );
  }

  return (
    <Modal
      title={text.title}
      className="catalog-dialog"
      initialFocus={search}
      onClose={dismiss}
      footer={
        <>
          <button type="button" onClick={dismiss}>
            Cancel
          </button>
          <button
            type="button"
            className="primary"
            disabled={!selected.size}
            aria-disabled={confirming || undefined}
            onClick={() => void confirm()}
          >
            {selected.size <= 1 ? `Add ${text.singular}` : `Add ${selected.size} ${text.plural}`}
          </button>
        </>
      }
    >
      <div
        className="catalog-dialog-body stack"
        onKeyDownCapture={(event) => {
          if (event.key !== "Escape" || event.nativeEvent.isComposing || composing !== null) return;
          if (event.target !== search.current && !list.current?.contains(event.target as Node))
            return;
          event.preventDefault();
          event.stopPropagation();
          dismiss();
        }}
      >
        <label className="field">
          {text.search}
          <span className="catalog-search-input">
            <Icon name="search" size={15} />
            <input
              ref={search}
              type="search"
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="none"
              spellCheck={false}
              maxLength={200}
              placeholder={text.placeholder}
              readOnly={confirming}
              aria-controls={hasOptions ? listId : undefined}
              value={composing ?? query}
              onChange={(event) => {
                if (composing !== null) setComposing(event.currentTarget.value);
                else setQuery(event.currentTarget.value);
              }}
              onCompositionStart={(event) => setComposing(event.currentTarget.value)}
              onCompositionEnd={(event) => {
                setComposing(null);
                setQuery(event.currentTarget.value);
              }}
              onKeyDown={searchKey}
            />
          </span>
        </label>
        {kind === "projects" && (
          <label className="field catalog-view">
            Repository visibility
            <select
              value={view}
              disabled={confirming}
              onChange={(event) => {
                const value = event.currentTarget.value;
                if (value === "member" || value === "starred" || value === "all") setView(value);
              }}
            >
              {Object.entries(repositoryViews).map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </select>
          </label>
        )}
        <div
          className="catalog-dialog-results"
          onKeyDownCapture={(event) => {
            if (event.key === "ArrowUp" && enabledResults()[0] === event.target) {
              event.preventDefault();
              event.stopPropagation();
              search.current?.focus();
            }
          }}
        >
          {hasOptions ? (
            <ListBox
              id={listId}
              ref={list}
              className="catalog-dialog-list"
              aria-label={`${text.source} results`}
              aria-busy={lookup.pending || undefined}
              selectionMode="multiple"
              selectionBehavior="toggle"
              selectedKeys={visibleSelected}
              onSelectionChange={choose}
            >
              {[...options.values()].map(renderOption)}
              {trackedOptions.length > 0 && (
                <ListBoxSection id="tracked">
                  <Header className="catalog-tracked">{text.tracked}</Header>
                  {trackedOptions.map(renderOption)}
                </ListBoxSection>
              )}
            </ListBox>
          ) : (
            <p className="catalog-placeholder meta" role="status">
              {lookup.error ? "" : status}
            </p>
          )}
        </div>
        <div className="catalog-feedback">
          <p className="meta" role="status">
            {addError || lookup.error || (hasOptions ? status : "")}
          </p>
          {lookup.error && (
            <button ref={pageButton} type="button" disabled={confirming} onClick={lookup.retry}>
              Retry search
            </button>
          )}
          {paged && (
            <div className="catalog-pages">
              <button
                ref={pageButton}
                type="button"
                aria-disabled={confirming || lookup.pending || !lookup.hasPrevious}
                onClick={() => {
                  if (!confirming) lookup.previous();
                }}
              >
                Previous page
              </button>
              <button
                ref={pageButton}
                type="button"
                aria-disabled={confirming || lookup.pending || !lookup.hasNext}
                onClick={() => {
                  if (!confirming) lookup.next([...options.keys()]);
                }}
              >
                Next page
              </button>
            </div>
          )}
        </div>
        {selected.size > 0 && (
          <ul className="catalog-selection" aria-label={`Selected ${text.plural}`}>
            {[...selected.values()].map((item) => (
              <li key={item.id}>
                <button
                  type="button"
                  aria-label={`Remove ${item.label} (${text.source} #${item.id}) from selection`}
                  disabled={confirming}
                  onClick={() => {
                    search.current?.focus({ preventScroll: true });
                    setSelected((previous) => {
                      const next = new Map(previous);
                      next.delete(item.id);
                      return next;
                    });
                    setAddError(undefined);
                  }}
                >
                  {catalogChoice(kind, item)?.label} <small>#{item.id}</small>
                  <Icon name="close" size={12} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Modal>
  );
}

function matchesQuery(choice: PickerChoice, query: string) {
  if (!query) return true;
  if (/^[0-9]+$/u.test(query)) return choice.id === query;
  const term = query.toLowerCase();
  return [choice.label, choice.detail].some((value) => value?.toLowerCase().includes(term));
}
