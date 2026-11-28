import { useEffect, useState } from "react";
import type { CSSProperties } from "react";
import { Autocomplete } from "react-aria-components/Autocomplete";
import { Header, ListBox, ListBoxItem, ListBoxSection } from "react-aria-components/ListBox";
import { Input, TextField } from "react-aria-components/TextField";
import { Icon } from "../../../../components/Icon";
import { Popover } from "../../../../components/Popover";
import type { ReaderDocument } from "../report-reader";
import "./picker.css";

const GAP_PX = 8;
const WIDTH_PX = 430;

// The `8 findings` control beside the heading of the section that holds the first card. It lists
// every card, blocking first, and jumps to the chosen one. Its label is drawn by CSS, so the
// report's text, selection, copy and Find stay as they were.
export function FindingPicker({
  reader,
  cards,
  busy,
  boundary,
  onJump,
  onFallback,
}: {
  readonly reader: ReaderDocument;
  // The finding groups, in report order.
  readonly cards: readonly number[];
  readonly busy: boolean;
  // The reader's scrolling panel, which the popup fits inside.
  readonly boundary: HTMLElement | null;
  readonly onJump: (index: number) => void;
  // Opens the whole report, which has every card's text.
  readonly onFallback: () => void;
}) {
  const [room, setRoom] = useState(WIDTH_PX);
  const label = `${cards.length} findings`;
  return (
    <Popover
      label="Jump to a finding"
      className="finding-picker"
      style={{ "--finding-picker-width": `${room}px` } as CSSProperties}
      trigger={{
        className: "finding-picker-trigger",
        "aria-label": `Jump to one of ${label}`,
        "aria-busy": busy || undefined,
        "data-report-jump": "",
        "data-label": label,
        children: <Icon name="chevron" size={12} />,
      }}
      onBeforeOpen={(button) => {
        // The popup opens leftward from the control's end edge and stops short of the panel's.
        if (boundary)
          setRoom(
            Math.min(
              WIDTH_PX,
              button.getBoundingClientRect().right - boundary.getBoundingClientRect().left - GAP_PX,
            ),
          );
      }}
    >
      {(close) => (
        <FindingList
          reader={reader}
          cards={cards}
          onFallback={onFallback}
          onPick={(index) => {
            close();
            onJump(index);
          }}
        />
      )}
    </Popover>
  );
}

function FindingList({
  reader,
  cards,
  onFallback,
  onPick,
}: {
  readonly reader: ReaderDocument;
  readonly cards: readonly number[];
  readonly onFallback: () => void;
  readonly onPick: (index: number) => void;
}) {
  const [query, setQuery] = useState("");
  const unread = cards.some((index) => !reader.finding(index));
  // A windowed report whose layout lacks a card's text opens whole, as when a group cannot be
  // read, and the list fills from it.
  useEffect(() => {
    if (unread) onFallback();
  }, [unread, onFallback]);

  const needle = query.trim().toLowerCase();
  const rows = cards.flatMap((index) => {
    const card = reader.finding(index);
    if (!card) return [];
    const words = [card.id, card.title, card.category, card.location ?? ""].join("\n");
    return !needle || words.toLowerCase().includes(needle) ? [{ ...card, index }] : [];
  });
  const groups = [true, false]
    .map((blocking) => ({ blocking, rows: rows.filter((row) => row.blocking === blocking) }))
    .filter((group) => group.rows.length > 0);

  // Busy while the whole report opens, so a held refresh lets it in with focus still in the list.
  return (
    <div className="finding-picker-body" data-report-jump="" aria-busy={unread || undefined}>
      <Autocomplete inputValue={query} onInputChange={setQuery}>
        <TextField className="finding-picker-filter" aria-label="Filter findings" autoFocus>
          <Input placeholder="Type an ID, a word or a file…" autoComplete="off" />
        </TextField>
        {unread ? (
          <p className="finding-picker-state" role="status">
            Opening the whole report…
          </p>
        ) : (
          <ListBox
            className="finding-picker-list"
            aria-label="Findings"
            autoFocus="first"
            shouldFocusOnHover
            onAction={(key) => onPick(Number(key))}
            renderEmptyState={() => <p className="finding-picker-state">Nothing matches.</p>}
          >
            {groups.map(({ blocking, rows }) => (
              <ListBoxSection key={String(blocking)} id={String(blocking)}>
                <Header className="finding-picker-caption">
                  {blocking ? "Blocking" : "Nonblocking"} · {rows.length}
                </Header>
                {rows.map((row) => {
                  const detail = [row.category, row.location].filter(Boolean).join(" · ");
                  return (
                    <ListBoxItem
                      key={row.index}
                      id={row.index}
                      className="finding-picker-row"
                      textValue={`${row.id} ${row.title}`}
                      data-blocking={row.blocking}
                    >
                      <span className="finding-picker-bar" aria-hidden="true" />
                      <code>{row.id}</code>
                      <span className="finding-picker-text">
                        <strong>{row.title}</strong>
                        {detail && <small>{detail}</small>}
                      </span>
                      <span className="finding-picker-severity">{row.severity}</span>
                    </ListBoxItem>
                  );
                })}
              </ListBoxSection>
            ))}
          </ListBox>
        )}
      </Autocomplete>
      <footer className="finding-picker-keys" aria-hidden="true">
        <span>
          <kbd>↑</kbd> <kbd>↓</kbd> move
        </span>
        <span>
          <kbd>Enter</kbd> jump
        </span>
        <span>
          <kbd>Esc</kbd> close
        </span>
      </footer>
    </div>
  );
}
