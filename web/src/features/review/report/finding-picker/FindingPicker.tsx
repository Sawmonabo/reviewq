import { useEffect, useId, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { Autocomplete } from "react-aria-components/Autocomplete";
import { Header, ListBox, ListBoxItem, ListBoxSection } from "react-aria-components/ListBox";
import { Input, TextField } from "react-aria-components/TextField";
import { Icon } from "../../../../components/Icon";
import type { ReaderDocument } from "../report-reader";
import "./picker.css";

const GAP_PX = 8;
const WIDTH_PX = 430;

// The `8 findings` control beside the heading of the section that holds the first card. It lists
// the cards the reader has read, blocking first, and jumps to the chosen one. Its label is drawn
// by CSS, so the report's text, selection, copy and Find stay as they were.
export function FindingPicker({
  reader,
  cards,
  busy,
  boundary,
  onJump,
}: {
  readonly reader: ReaderDocument;
  // The finding groups, in report order.
  readonly cards: readonly number[];
  readonly busy: boolean;
  // The reader's scrolling panel, which the popup fits inside.
  readonly boundary: HTMLElement | null;
  readonly onJump: (index: number) => void;
}) {
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [room, setRoom] = useState(WIDTH_PX);
  const [, setRead] = useState(0);
  const [failed, setFailed] = useState(false);
  const unread = cards.some((index) => !reader.finding(index));

  useEffect(() => {
    const read = reader.readFindings;
    if (!open || !unread || !read) return;
    const controller = new AbortController();
    read(cards, controller.signal).then(
      () => setRead((count) => count + 1),
      () => {
        if (!controller.signal.aborted) setFailed(true);
      },
    );
    return () => controller.abort();
  }, [open, unread, reader, cards]);

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
  const label = `${cards.length} findings`;

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className="finding-picker-trigger"
        popoverTarget={id}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`Jump to one of ${label}`}
        aria-busy={busy || undefined}
        data-report-jump=""
        data-label={label}
      >
        <Icon name="chevron" size={12} />
      </button>
      <section
        id={id}
        popover="auto"
        role="dialog"
        aria-label="Jump to a finding"
        className="finding-picker"
        style={{ "--finding-picker-width": `${room}px` } as CSSProperties}
        // Sized before it shows; filled once it shows, so the field can take focus.
        onBeforeToggle={(event) => {
          const end = trigger.current?.getBoundingClientRect().right;
          if (event.newState === "open" && boundary && end !== undefined)
            // The popup opens leftward from the control's end edge and stops short of the panel's.
            setRoom(Math.min(WIDTH_PX, end - boundary.getBoundingClientRect().left - GAP_PX));
        }}
        onToggle={(event) => {
          const next = event.newState === "open";
          if (next) {
            setQuery("");
            setFailed(false);
          }
          setOpen(next);
        }}
      >
        {open && (
          <>
            <Autocomplete inputValue={query} onInputChange={setQuery}>
              <TextField className="finding-picker-filter" aria-label="Filter findings" autoFocus>
                <Input placeholder="Type an ID, a word or a file…" autoComplete="off" />
              </TextField>
              {unread ? (
                <p className="finding-picker-state" role="status">
                  {failed
                    ? "The findings could not be read. Close and try again."
                    : "Reading the findings…"}
                </p>
              ) : (
                <ListBox
                  className="finding-picker-list"
                  aria-label="Findings"
                  autoFocus="first"
                  shouldFocusOnHover
                  onAction={(key) => {
                    document.getElementById(id)?.hidePopover();
                    onJump(Number(key));
                  }}
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
          </>
        )}
      </section>
    </>
  );
}
