import { useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { Descriptor, ReportSummary } from "../../../api/generated/contracts/reports";
import { reportContext, reportSection, saveDisposition } from "../../../api/reports";
import { Pagination } from "../../../components/Pagination";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useDraft } from "../../../hooks/useDraft";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { describeError, readable } from "../../../lib/presentation";

type Collection = "findings" | "changes" | "sections";
type Finding = Extract<Descriptor, { kind: "finding" }>;
type Saved = Extract<Descriptor, { kind: "disposition" }>;
type DispositionDraft = { readonly blocking: string; readonly publication: "" | "authorized" | "published" | "withheld"; readonly reason: string };

export function ReportDetails({ run, report, collection, current, drafts, reviewTitle, onSource }: {
  readonly run: RunDetail;
  readonly report: ReportSummary;
  readonly collection: Collection;
  readonly current: boolean;
  readonly drafts: DraftStore;
  readonly reviewTitle: string;
  readonly onSource: (offset: number) => void;
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [edits, setEdits] = useState<Readonly<Record<string, { value: Saved; version: string }>>>({});
  const context = useResource(`report-details:${run.runId}:${report.receiptId}:${collection}:${trail.at(-1) ?? ""}`, (signal) =>
    reportContext(run.runId, report.receiptId, collection, trail.at(-1), signal), [{ kind: "report", id: run.runId }]);
  const dispositions = useResource(`report-dispositions:${run.runId}:${report.receiptId}:${collection}`, (signal) => collection === "findings"
    ? reportContext(run.runId, report.receiptId, "dispositions", undefined, signal)
    : Promise.resolve({ data: null, cursor: "" }), [{ kind: "report", id: run.runId }]);
  const currentVersion = dispositions.data?.userDispositionVersion ?? context.data?.userDispositionVersion;
  const byId = new Map((dispositions.data?.items ?? []).flatMap((item) => item.kind === "disposition" ? [[item.findingId, item] as const] : []));

  return <div className="stack">
    {context.pending && <Loading label={`Reading report ${collection}…`} />}
    {context.error && <Notice retry={context.retry}>{context.error}</Notice>}
    {dispositions.error && <Notice retry={dispositions.retry}>{dispositions.error}</Notice>}
    {!context.data?.items.length && !context.pending && !context.error && <Empty title={`No recorded ${collection}`} />}
    {context.data?.items.map((item) => {
      if (item.kind === "change") return <article key={item.findingId} className="surface-card stack">
        <div className="spread"><strong>{item.findingId}</strong><span className="badge">{readable(item.conclusion)}</span></div>
        <p>{item.reason}</p>{item.checkedSha && <p className="meta">Checked at <code>{item.checkedSha}</code></p>}
        {item.discussionIds.length > 0 && <p className="meta">{item.discussionIds.length} linked discussions</p>}
      </article>;
      if (item.kind === "section") return <button className="report-version" key={item.source.offset} type="button" onClick={() => onSource(item.source.offset)}>
        <strong>{item.title}</strong><span className="meta">Open original source</span></button>;
      if (item.kind !== "finding") return null;
      const edit = edits[item.findingId];
      const saved = edit && (currentVersion === undefined || BigInt(edit.version) >= BigInt(currentVersion)) ? edit.value : byId.get(item.findingId);
      return <FindingCard key={item.findingId} item={item} report={report} run={run} saved={saved} drafts={drafts}
        version={edit && currentVersion !== undefined && BigInt(edit.version) > BigInt(currentVersion) ? edit.version : currentVersion}
        current={current} reviewTitle={reviewTitle} onSource={onSource} onSave={(value, version) => {
          setEdits((known) => ({ ...known, [item.findingId]: { value, version } })); context.retry(); dispositions.retry();
        }} onRefresh={() => { dispositions.retry(); context.retry(); }} />;
    })}
    <Pagination trail={trail} next={context.data?.nextCursor} onPage={setTrail} />
  </div>;
}

function FindingCard({ item, report, run, saved, drafts, version, current, reviewTitle, onSource, onSave, onRefresh }: {
  readonly item: Finding;
  readonly report: ReportSummary;
  readonly run: RunDetail;
  readonly saved: Saved | undefined;
  readonly drafts: DraftStore;
  readonly version: string | undefined;
  readonly current: boolean;
  readonly reviewTitle: string;
  readonly onSource: (offset: number) => void;
  readonly onSave: (value: Saved, version: string) => void;
  readonly onRefresh: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string>();
  const [stale, setStale] = useState(false);
  const [editedAt, setEditedAt] = useState<string>();
  const draftKey = JSON.stringify(["disposition", run.reviewId, run.runId, run.leadSessionKey, report.receiptId, report.sha256, item.findingId]);
  const label = `Disposition · ${item.findingId} · ${reviewTitle}`;
  const draft = useDraft(drafts, draftKey);
  const edit = decode(draft);
  const patch = item.source;
  const body = useResource(`finding-source:${report.receiptId}:${item.findingId}:${expanded}`, (signal) =>
    !patch || !expanded || !patch.length ? Promise.resolve({ data: null, cursor: "" }) :
      reportSection(run.runId, report.receiptId, report.sha256, patch.offset, signal, Math.min(patch.length, 12_000)));

  function change(next: DispositionDraft) {
    setNotice(drafts.set(draftKey, label, JSON.stringify(next)) ? undefined : "Draft storage full.");
  }

  async function commit() {
    if (!current || !version || busy || stale || !edit.reason.trim() || !edit.blocking && !edit.publication) return;
    const frozen = drafts.freeze(draftKey);
    if (!frozen) return;
    setBusy(true); setNotice(undefined);
    const captured = decode(frozen.text);
    try {
      const result = await saveDisposition(run.runId, { receiptId: report.receiptId, findingId: item.findingId,
        expectedVersion: editedAt ?? version, submissionId: crypto.randomUUID(), disposition: {
          reason: captured.reason.trim(), blocking: captured.blocking ? captured.blocking === "blocking" : null,
          publication: captured.publication || null,
        } });
      if (result.finding.kind === "disposition") onSave(result.finding, result.userDispositionVersion);
      drafts.finish(draftKey, true); setEditing(false); setNotice("Disposition saved.");
    } catch (error) {
      drafts.finish(draftKey, false); setNotice(describeError(error)); setStale(true); onRefresh();
    } finally { setBusy(false); }
  }

  return <article className="surface-card finding-card stack" aria-label={item.title}>
    <div className="spread"><div className="actions">
      <span className="ticket-key">{item.findingId}</span>{item.severity && <span className="badge amber">{item.severity}</span>}
      {(saved?.blocking ?? item.blocking) !== null && <span className={`badge ${(saved?.blocking ?? item.blocking) ? "rose" : ""}`}>{(saved?.blocking ?? item.blocking) ? "Blocking" : "Nonblocking"}</span>}
      {saved?.publication && <span className="badge">{readable(saved.publication)}</span>}
    </div>{current && version && <button type="button" onClick={() => { setEditing(!editing); setEditedAt(version); }}>Edit disposition</button>}</div>
    <h3>{item.title}</h3>
    {saved?.reason && <p className="meta">Local disposition · {saved.reason}</p>}
    {patch && <details onToggle={(event) => setExpanded(event.currentTarget.open)}><summary>Read authored finding</summary>
      {body.error && <Notice retry={body.retry}>{body.error}</Notice>}
      {expanded && body.pending && <Loading label="Reading finding evidence…" />}
      {body.data && <pre className="report-source">{body.data.text}</pre>}
      <button type="button" onClick={() => onSource(patch.offset)}>Open finding in original report</button>
    </details>}
    {notice && <Notice>{notice}</Notice>}
    {editing && current && <form className="stack disposition-form" onSubmit={(event) => { event.preventDefault(); void commit(); }}>
      <div className="fields">
        <label className="field">Blocking<select value={edit.blocking} onChange={(event) => change({ ...edit, blocking: event.currentTarget.value })}>
          <option value="">Keep existing</option><option value="blocking">Blocking</option><option value="nonblocking">Nonblocking</option></select></label>
        <label className="field">Publication status<select value={edit.publication} onChange={(event) => {
          const value = event.currentTarget.value;
          change({ ...edit, publication: value === "authorized" || value === "published" || value === "withheld" ? value : "" });
        }}><option value="">Keep existing</option><option value="withheld">Withheld</option><option value="authorized">Authorized</option><option value="published">Published</option></select></label>
      </div>
      <label className="field">Reason<textarea rows={2} required value={edit.reason} onChange={(event) => change({ ...edit, reason: event.currentTarget.value })} /></label>
      {stale && <button type="button" onClick={() => { setEditedAt(version); setStale(false); }}>Use refreshed disposition revision</button>}
      <div className="actions"><button type="submit" className="primary" disabled={busy || stale || !edit.reason.trim() || !edit.blocking && !edit.publication}>Save disposition</button>
        <button type="button" disabled={busy} onClick={() => setEditing(false)}>Close editor</button></div>
    </form>}
  </article>;
}

function decode(value: string): DispositionDraft {
  if (value) {
    try { return JSON.parse(value) as DispositionDraft; } catch { /* Either an empty draft or a current editor value. */ }
  }
  return { blocking: "", publication: "", reason: "" };
}
