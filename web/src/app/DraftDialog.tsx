import { useState, useSyncExternalStore } from "react";
import type { Tab } from "../api/generated/contracts/notifications";
import { Modal } from "../components/Modal";
import { Notice } from "../components/PanelState";
import type { DraftStore } from "../lib/drafts";
import { describeError } from "../lib/presentation";
import { questionDraftValues } from "../lib/question-draft";

export function DraftDialog({
  drafts,
  onClose,
  onOpen,
}: {
  readonly drafts: DraftStore;
  readonly onClose: () => void;
  readonly onOpen: (review: string, run: string, tab: Tab) => void;
}) {
  const state = useSyncExternalStore(drafts.subscribe, drafts.getSnapshot);
  const [notice, setNotice] = useState<string>();
  return (
    <Modal title="Unsent drafts" onClose={onClose}>
      <div className="stack">
        <p className="meta">
          Drafts stay in this tab until they’re sent, discarded, or the page reloads.
        </p>
        {state.full && (
          <Notice>Draft storage full. Shorten or discard a draft to make room.</Notice>
        )}
        {notice && <Notice>{notice}</Notice>}
        {!state.entries.length && <p>No unsent drafts.</p>}
        {state.entries.map((draft) => {
          const target = draftRoute(draft.key);
          const text = draftContent(draft.key, draft.text);
          return (
            <div className="surface-card stack" key={draft.key}>
              <strong>{draft.label}</strong>
              <p className="meta truncate">{text}</p>
              <div className="actions">
                {target && (
                  <button
                    type="button"
                    onClick={() => onOpen(target.review, target.run, target.tab)}
                  >
                    Open draft target
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => {
                    void navigator.clipboard
                      .writeText(text)
                      .catch((error: unknown) => setNotice(describeError(error)));
                  }}
                >
                  Copy
                </button>
                <button
                  type="button"
                  onClick={() => {
                    if (window.confirm(`Discard unsent draft for ${draft.label}?`))
                      drafts.set(draft.key, draft.label, "");
                  }}
                >
                  Discard
                </button>
              </div>
            </div>
          );
        })}
      </div>
    </Modal>
  );
}

function draftContent(key: string, text: string) {
  if (key.startsWith('["question",')) return questionDraftValues(text).join(", ");
  if (!key.startsWith('["quote",')) return text;
  try {
    const value: unknown = JSON.parse(text);
    if (value && typeof value === "object" && "text" in value && typeof value.text === "string")
      return value.text;
  } catch {
    /* Preserve the charged draft string. */
  }
  return text;
}

function draftRoute(key: string): { review: string; run: string; tab: Tab } | undefined {
  try {
    const value: unknown = JSON.parse(key);
    if (
      Array.isArray(value) &&
      (value[0] === "message" ||
        value[0] === "feedback" ||
        value[0] === "disposition" ||
        value[0] === "quote") &&
      typeof value[1] === "string" &&
      typeof value[2] === "string"
    ) {
      return {
        review: value[1],
        run: value[2],
        tab: value[0] === "message" ? "agent" : "findings",
      };
    }
  } catch {
    /* Draft keys are local to this tab. */
  }
}
