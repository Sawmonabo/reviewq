import { useState } from "react";
import { readPatchDiscussions, readPatchFiles } from "../../../api/changes";
import { readOverview } from "../../../api/client";
import type { FollowupConfigureRequest, PatchDiscussion } from "../../../api/generated/contracts";
import { changeFollowup, followupEnrollment } from "../../../api/review";
import { Modal } from "../../../components/Modal";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { describeError, formatTime, readable } from "../../../lib/presentation";
import { DiscussionThread } from "../DiscussionThread";

type Target = { readonly reviewId: string; readonly title: string; readonly projectPath: string; readonly iid: string };
type Command = {
  readonly reviewId: string;
  readonly action: "configure" | "pause" | "resume" | "review-now";
  readonly generation: string | null;
  readonly runId: string | null;
  readonly threads: FollowupConfigureRequest["threads"];
};

export default function MonitorPane({ targets, pause, onClose }: { readonly targets: readonly Target[]; readonly pause: boolean; readonly onClose: () => void }) {
  const [commands, setCommands] = useState<Readonly<Record<string, Command>>>({});
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<Readonly<Record<string, string>>>({});
  const [confirm, setConfirm] = useState(false);
  const selected = targets.flatMap((target) => commands[target.reviewId] ? [commands[target.reviewId]] : []);

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
          const receipt = command.action === "configure" && command.runId
            ? await changeFollowup(command.reviewId, "configure", { ...body, runId: command.runId, state: "enabled", threads: command.threads })
            : await changeFollowup(command.reviewId, command.action, body);
          setResults((previous) => ({ ...previous, [command.reviewId]: `Saved · ${readable(receipt.state)}${receipt.batchId ? ` · Batch ${receipt.batchId}` : ""}` }));
        } catch (error) {
          setResults((previous) => ({ ...previous, [command.reviewId]: describeError(error) }));
        }
      }
    } finally { setBusy(false); }
  }

  return <Modal title={pause ? "Pause auto-review" : "Auto-review updates"} busy={busy} onClose={onClose}
    footer={<><button type="button" onClick={onClose} disabled={busy}>{Object.keys(results).length ? "Done" : "Cancel"}</button>
      {!Object.keys(results).length && (confirm
        ? <button type="button" className="primary" disabled={busy || !selected.length} onClick={() => void dispatch()}>Confirm {selected.length === 1 ? "review" : `${selected.length} reviews`}</button>
        : <button type="button" className="primary" disabled={!selected.length} onClick={() => setConfirm(true)}>Preview changes</button>)}
    </>}>
    <div className="stack">
      {confirm && <Notice>{selected.length} of {targets.length} reviews selected for this change. {selected.map((item) => {
        const target = targets.find((target) => target.reviewId === item.reviewId);
        return `${target?.projectPath}!${target?.iid} · ${readable(item.action)}${item.threads.length ? ` ${item.threads.length} discussions` : ""}`;
      }).join(" · ")}{!busy && !Object.keys(results).length && <button type="button" onClick={() => setConfirm(false)}>Edit selection</button>}</Notice>}
      {targets.map((target) => <TargetMonitor key={target.reviewId} target={target} pause={pause} locked={confirm}
        onChoice={(command) => update(command, target)} result={results[target.reviewId]} />)}
    </div>
  </Modal>;
}

function TargetMonitor({ target, pause, locked, onChoice, result }: {
  readonly target: Target;
  readonly pause: boolean;
  readonly locked: boolean;
  readonly onChoice: (choice: Command | undefined) => void;
  readonly result: string | undefined;
}) {
  const [lead, setLead] = useState("");
  const [chosen, setChosen] = useState<readonly string[]>([]);
  const [cursor, setCursor] = useState<string>();
  const [added, setAdded] = useState<readonly PatchDiscussion[]>([]);
  const [moreBusy, setMoreBusy] = useState(false);
  const [error, setError] = useState<string>();
  const resource = useResource(`monitor-configuration:${target.reviewId}:${pause}`, async (signal) => {
    const enrollment = await followupEnrollment(target.reviewId, signal);
    if (pause) return { cursor: enrollment.cursor, data: { enrollment: enrollment.data, runs: [], discussions: [], next: null, revision: "" } };
    const [overview, files] = await Promise.all([readOverview(target.reviewId, signal), readPatchFiles(target.reviewId, signal)]);
    const discussions = await readPatchDiscussions(target.reviewId, files.data.revision, signal);
    return { cursor: enrollment.cursor, data: { enrollment: enrollment.data, runs: overview.data.runs.items,
      discussions: discussions.data.items, next: discussions.data.nextCursor, revision: files.data.revision } };
  });
  const value = resource.data;
  const monitor = value?.enrollment.monitor;
  const selectedLead = lead || monitor?.runId || (value?.runs.length === 1 ? value.runs[0]?.runId : undefined) || "";
  const available = [...(value?.discussions ?? []), ...added].filter((thread) => thread.notes.some((note) => note.ours && /^\d+$/.test(note.noteId)));
  const next = cursor ?? value?.next;
  const eligible = monitor?.state === "enabled" || monitor?.state === "paused";

  function select(ids: readonly string[], run: string, action: Command["action"] = "configure") {
    const threads = available.filter((thread) => ids.includes(thread.discussionId)).map((thread) => ({
      discussionId: thread.discussionId, originalNoteIds: thread.notes.filter((note) => note.ours && /^\d+$/.test(note.noteId)).map((note) => note.noteId),
      findingIds: [...new Set(monitor?.selections.filter((record) => record.discussionId === thread.discussionId).flatMap((record) => record.findingIds))],
    }));
    if (action === "configure" && (!run || !threads.length)) { onChoice(undefined); return; }
    onChoice({ reviewId: target.reviewId, action, generation: monitor?.generation ?? null, runId: run || null, threads });
  }

  async function more() {
    if (!value || !next || moreBusy) return;
    setMoreBusy(true); setError(undefined);
    try {
      const page = await readPatchDiscussions(target.reviewId, value.revision, AbortSignal.timeout(12_000), next);
      setAdded((existing) => [...existing, ...page.data.items].slice(0, 400));
      setCursor(page.data.nextCursor ?? "");
    } catch (error) { setError(describeError(error)); }
    finally { setMoreBusy(false); }
  }

  return <section className="surface-card stack">
    <div className="spread"><h3>{target.title}</h3><code>{target.projectPath}!{target.iid}</code></div>
    {resource.pending && <Loading label="Reading eligible lead runs and discussions…" />}
    {resource.error && <Notice retry={resource.retry}>{resource.error}</Notice>}
    {result && <Notice>{result}</Notice>}
    {monitor && <p className="meta">Monitor {readable(monitor.state).toLowerCase()}{monitor.reason && ` · ${readable(monitor.reason)}`}</p>}
    {value && (pause ? monitor?.state === "enabled" ? <label><input disabled={locked} type="checkbox" onChange={(event) => {
      if (event.currentTarget.checked) select([], "", "pause"); else onChoice(undefined);
    }} /> Pause checks on this review</label> : <p className="meta">There is no enabled monitor to pause.</p> : <>
      {eligible && <div className="actions">
        <button type="button" disabled={locked} onClick={() => select([], selectedLead, monitor?.state === "paused" ? "resume" : "review-now")}>{monitor?.state === "paused" ? "Select resume" : "Select check now"}</button>
        {monitor?.selections.length ? <span className="meta">{monitor.selections.length} saved notes</span> : null}
      </div>}
      {value.runs.length ? <label className="field">Lead run<select disabled={locked} value={selectedLead} onChange={(event) => {
        setLead(event.currentTarget.value); select(chosen, event.currentTarget.value);
      }}><option value="">Choose a lead</option>
        {value.runs.map((item) => <option key={item.runId} value={item.runId}>{readable(item.provider)} · {item.startedAt ? formatTime(item.startedAt) : item.runId}</option>)}</select></label>
        : <Empty title="A registered lead is required" />}
      {available.length ? available.map((thread) => <div className="stack" key={thread.discussionId}>
        <label><input disabled={locked} type="checkbox" checked={chosen.includes(thread.discussionId)} onChange={(event) => {
          const ids = event.currentTarget.checked ? [...chosen, thread.discussionId] : chosen.filter((id) => id !== thread.discussionId);
          setChosen(ids); select(ids, selectedLead);
        }} /> Monitor this discussion{monitor?.selections.some((selection) => selection.discussionId === thread.discussionId && selection.selected) && " · Currently monitored"}</label>
        <DiscussionThread thread={thread} />
      </div>) : <p className="meta">No eligible reviewer notes on this page.</p>}
      {next && added.length < 400 && <button type="button" disabled={moreBusy || locked} onClick={() => void more()}>More discussions</button>}
      {error && <Notice>{error}</Notice>}
    </>)}
  </section>;
}
