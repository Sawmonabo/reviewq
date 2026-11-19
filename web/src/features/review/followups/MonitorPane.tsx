import { useState } from "react";
import { readPatchDiscussions, readPatchFiles } from "../../../api/changes";
import { readOverview } from "../../../api/client";
import type { FollowupConfigureRequest, PatchDiscussion } from "../../../api/generated/contracts";
import type { SelectedThread, Selection } from "../../../api/generated/contracts/followups";
import { changeFollowup, followupEnrollment } from "../../../api/review";
import { Modal } from "../../../components/Modal";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { describeError, formatTime, readable } from "../../../lib/presentation";
import { DiscussionThread } from "../DiscussionThread";

type Target = {
  readonly reviewId: string;
  readonly title: string;
  readonly projectPath: string;
  readonly iid: string;
};
type Command = {
  readonly reviewId: string;
  readonly action: "configure" | "pause" | "resume" | "review-now";
  readonly generation: string | null;
  readonly runId: string | null;
  readonly state: FollowupConfigureRequest["state"];
  readonly threads: FollowupConfigureRequest["threads"];
};

export default function MonitorPane({
  targets,
  pause,
  onClose,
}: {
  readonly targets: readonly Target[];
  readonly pause: boolean;
  readonly onClose: () => void;
}) {
  const [commands, setCommands] = useState<Readonly<Record<string, Command>>>({});
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<Readonly<Record<string, string>>>({});
  const [confirm, setConfirm] = useState(false);
  const selected = targets.flatMap((target) => {
    const command = commands[target.reviewId];
    return command ? [command] : [];
  });

  function update(command: Command | undefined, target: Target) {
    setCommands((previous) => {
      if (command) return { ...previous, [target.reviewId]: command };
      const result = { ...previous };
      delete result[target.reviewId];
      return result;
    });
  }

  async function dispatch() {
    if (busy || !selected.length) return;
    setBusy(true);
    try {
      for (const command of selected) {
        const body = { submissionId: crypto.randomUUID(), expectedGeneration: command.generation };
        try {
          const receipt =
            command.action === "configure" && command.runId
              ? await changeFollowup(command.reviewId, "configure", {
                  ...body,
                  runId: command.runId,
                  state: command.state,
                  threads: command.threads,
                })
              : await changeFollowup(command.reviewId, command.action, body);
          setResults((previous) => ({
            ...previous,
            [command.reviewId]: `Saved · ${readable(receipt.state)}${receipt.batchId ? ` · Batch ${receipt.batchId}` : ""}`,
          }));
        } catch (error) {
          setResults((previous) => ({ ...previous, [command.reviewId]: describeError(error) }));
        }
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title={pause ? "Pause auto-review" : "Auto-review updates"}
      busy={busy}
      onClose={onClose}
      footer={
        <>
          <button type="button" onClick={onClose} disabled={busy}>
            {Object.keys(results).length ? "Done" : "Cancel"}
          </button>
          {!Object.keys(results).length &&
            (confirm ? (
              <button
                type="button"
                className="primary"
                disabled={busy || !selected.length}
                onClick={() => void dispatch()}
              >
                Confirm {selected.length === 1 ? "review" : `${selected.length} reviews`}
              </button>
            ) : (
              <button
                type="button"
                className="primary"
                disabled={!selected.length}
                onClick={() => setConfirm(true)}
              >
                Preview changes
              </button>
            ))}
        </>
      }
    >
      <div className="stack">
        {confirm && (
          <Notice>
            {selected.length} of {targets.length} reviews selected for this change.{" "}
            {selected
              .map((item) => {
                const target = targets.find((target) => target.reviewId === item.reviewId);
                return `${target?.projectPath}!${target?.iid} · ${readable(item.action)}${item.threads.length ? ` ${item.threads.length} discussions` : ""}`;
              })
              .join(" · ")}
            {!busy && !Object.keys(results).length && (
              <button type="button" onClick={() => setConfirm(false)}>
                Edit selection
              </button>
            )}
          </Notice>
        )}
        {targets.map((target) => (
          <TargetMonitor
            key={target.reviewId}
            target={target}
            pause={pause}
            locked={confirm}
            onChoice={(command) => update(command, target)}
            result={results[target.reviewId]}
          />
        ))}
      </div>
    </Modal>
  );
}

function TargetMonitor({
  target,
  pause,
  locked,
  onChoice,
  result,
}: {
  readonly target: Target;
  readonly pause: boolean;
  readonly locked: boolean;
  readonly onChoice: (choice: Command | undefined) => void;
  readonly result: string | undefined;
}) {
  const [lead, setLead] = useState("");
  const [edited, setEdited] = useState<readonly string[]>();
  const [cursor, setCursor] = useState<string>();
  const [added, setAdded] = useState<readonly PatchDiscussion[]>([]);
  const [moreBusy, setMoreBusy] = useState(false);
  const [error, setError] = useState<string>();
  const resource = useResource(
    `monitor-configuration:${target.reviewId}:${pause}`,
    async (signal) => {
      const enrollment = await followupEnrollment(target.reviewId, signal);
      if (pause)
        return {
          cursor: enrollment.cursor,
          data: {
            enrollment: enrollment.data,
            runs: [],
            discussions: [],
            next: null,
            revision: "",
          },
        };
      const [overview, files] = await Promise.all([
        readOverview(target.reviewId, signal),
        readPatchFiles(target.reviewId, signal),
      ]);
      const discussions = await readPatchDiscussions(target.reviewId, files.data.revision, signal);
      return {
        cursor: enrollment.cursor,
        data: {
          enrollment: enrollment.data,
          runs: overview.data.runs.items,
          discussions: discussions.data.items,
          next: discussions.data.nextCursor,
          revision: files.data.revision,
        },
      };
    },
  );
  const value = resource.data;
  const monitor = value?.enrollment.monitor;
  const saved = savedSelections(monitor?.selections ?? []);
  const chosen = edited ?? [...saved.keys()];
  const unresolved = monitor?.selections.some((record) => record.selected && !record.discussionId);
  const selectedLead =
    lead || monitor?.runId || (value?.runs.length === 1 ? value.runs[0]?.runId : undefined) || "";
  const available = new Map(
    [...(value?.discussions ?? []), ...added]
      .filter((thread) => thread.notes.some((note) => note.ours && /^\d+$/.test(note.noteId)))
      .map((thread) => [thread.discussionId, thread] as const),
  );
  const unloaded = [...saved.values()].filter(
    (selection) => !available.has(selection.discussionId),
  );
  const next = cursor === undefined ? value?.next : cursor;
  const eligible = monitor?.state === "enabled" || monitor?.state === "paused";

  function select(ids: readonly string[], run: string, action: Command["action"] = "configure") {
    if (action === "configure" && (!run || unresolved)) {
      onChoice(undefined);
      return;
    }
    const threads: Selection[] = [];
    if (action === "configure") {
      for (const id of ids) {
        const previous = saved.get(id);
        const thread = available.get(id);
        if (previous) threads.push(previous);
        else if (thread) {
          threads.push({
            discussionId: id,
            originalNoteIds: thread.notes
              .filter((note) => note.ours && /^\d+$/.test(note.noteId))
              .map((note) => note.noteId),
          });
        } else {
          onChoice(undefined);
          return;
        }
      }
    }
    onChoice({
      reviewId: target.reviewId,
      action,
      generation: monitor?.generation ?? null,
      runId: run || null,
      state: monitor?.state === "paused" ? "paused" : "enabled",
      threads,
    });
  }

  function toggle(id: string, checked: boolean) {
    const ids = checked ? [...chosen, id] : chosen.filter((item) => item !== id);
    setEdited(ids);
    select(ids, selectedLead);
  }

  async function more() {
    if (!value || !next || moreBusy) return;
    setMoreBusy(true);
    setError(undefined);
    try {
      const page = await readPatchDiscussions(
        target.reviewId,
        value.revision,
        AbortSignal.timeout(12_000),
        next,
      );
      setAdded((existing) => [...existing, ...page.data.items].slice(0, 400));
      setCursor(page.data.nextCursor ?? "");
    } catch (error) {
      setError(describeError(error));
    } finally {
      setMoreBusy(false);
    }
  }

  return (
    <section className="surface-card stack">
      <div className="spread">
        <h3>{target.title}</h3>
        <code>
          {target.projectPath}!{target.iid}
        </code>
      </div>
      {resource.pending && <Loading label="Reading eligible lead runs and discussions…" />}
      {resource.error && <Notice retry={resource.retry}>{resource.error}</Notice>}
      {result && <Notice>{result}</Notice>}
      {monitor && (
        <p className="meta">
          Monitor {readable(monitor.state).toLowerCase()}
          {monitor.reason && ` · ${readable(monitor.reason)}`}
        </p>
      )}
      {unresolved && !pause && (
        <Notice>
          A saved note is missing its discussion reference. Refresh this review before editing
          selected discussions.
        </Notice>
      )}
      {value &&
        (pause ? (
          monitor?.state === "enabled" ? (
            <label>
              <input
                disabled={locked}
                type="checkbox"
                onChange={(event) => {
                  if (event.currentTarget.checked) select([], "", "pause");
                  else onChoice(undefined);
                }}
              />{" "}
              Pause checks on this review
            </label>
          ) : (
            <p className="meta">There is no enabled monitor to pause.</p>
          )
        ) : (
          <>
            {eligible && (
              <div className="actions">
                <button
                  type="button"
                  disabled={locked}
                  onClick={() =>
                    select([], selectedLead, monitor?.state === "paused" ? "resume" : "review-now")
                  }
                >
                  {monitor?.state === "paused" ? "Select resume" : "Select check now"}
                </button>
                {monitor?.selections.length ? (
                  <span className="meta">{monitor.selections.length} saved notes</span>
                ) : null}
              </div>
            )}
            {value.runs.length ? (
              <label className="field">
                Lead run
                <select
                  disabled={locked || unresolved}
                  value={selectedLead}
                  onChange={(event) => {
                    setLead(event.currentTarget.value);
                    select(chosen, event.currentTarget.value);
                  }}
                >
                  <option value="">Choose a lead</option>
                  {selectedLead && !value.runs.some((item) => item.runId === selectedLead) && (
                    <option value={selectedLead}>Saved lead</option>
                  )}
                  {value.runs.map((item) => (
                    <option key={item.runId} value={item.runId}>
                      {readable(item.provider)} ·{" "}
                      {item.startedAt ? formatTime(item.startedAt) : item.runId}
                    </option>
                  ))}
                </select>
              </label>
            ) : (
              <Empty title="A registered lead is required" />
            )}
            {unloaded.length > 0 && (
              <section className="stack" aria-label="Saved discussions outside this page">
                {unloaded.map((selection) => (
                  <label key={selection.discussionId}>
                    <input
                      disabled={locked || unresolved}
                      type="checkbox"
                      checked={chosen.includes(selection.discussionId)}
                      onChange={(event) =>
                        toggle(selection.discussionId, event.currentTarget.checked)
                      }
                    />{" "}
                    Saved discussion <code>{selection.discussionId}</code>
                  </label>
                ))}
              </section>
            )}
            {available.size ? (
              [...available.values()].map((thread) => (
                <div className="stack" key={thread.discussionId}>
                  <label>
                    <input
                      disabled={locked || unresolved}
                      type="checkbox"
                      checked={chosen.includes(thread.discussionId)}
                      onChange={(event) => toggle(thread.discussionId, event.currentTarget.checked)}
                    />{" "}
                    Monitor this discussion
                    {monitor?.selections.some(
                      (selection) =>
                        selection.discussionId === thread.discussionId && selection.selected,
                    ) && " · Currently monitored"}
                  </label>
                  <DiscussionThread thread={thread} />
                </div>
              ))
            ) : (
              <p className="meta">No eligible reviewer notes on this page.</p>
            )}
            {next && added.length < 400 && (
              <button type="button" disabled={moreBusy || locked} onClick={() => void more()}>
                More discussions
              </button>
            )}
            {error && <Notice>{error}</Notice>}
          </>
        ))}
    </section>
  );
}

function savedSelections(records: readonly SelectedThread[]) {
  const selected = new Map<string, Selection>();
  for (const record of records) {
    if (!record.selected || !record.discussionId) continue;
    const previous = selected.get(record.discussionId);
    selected.set(record.discussionId, {
      discussionId: record.discussionId,
      originalNoteIds: [...new Set([...(previous?.originalNoteIds ?? []), record.originalNoteId])],
      findingIds: [...new Set([...(previous?.findingIds ?? []), ...record.findingIds])],
    });
  }
  return selected;
}
