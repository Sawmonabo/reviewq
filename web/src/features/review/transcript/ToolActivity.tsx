import type { HistoryToolState } from "../../../api/generated/contracts/history";
import { HistoryTimestamp } from "./HistoryTimestamp";
import { toolKind } from "./tool-activity";
import type {
  ReadPack,
  ReadTarget,
  ToolCall,
  ToolKind,
  ToolRun,
  ToolSummary,
} from "./tool-activity";
import "./tool-activity.css";

const glyphs: Readonly<Record<ToolKind, string>> = {
  command: "M4 5h16v14H4z M7 9l3 3-3 3m6 0h4",
  read: "M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12Z M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z",
  search: "M16 10a6 6 0 1 1-12 0 6 6 0 0 1 12 0Zm-1 5 5 5",
  edit: "M13 3H5v18h14v-7M13 3v6h6m-9 7 7-7 3 3-7 7h-3Z",
  web: "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0Zm-18 0h18M12 3a9 9 0 0 0 0 18m0-18a9 9 0 0 1 0 18",
  image: "M3 4h18v16H3zM3 17l6-6 4 4 3-3 5 5M8 8h.01",
  agent: "M5 7h14v12H5z M12 3v4M2 12v4m20-4v4M9 16h6m-6-5h.01m6 0h.01",
  message: "M4 4h16v12H9l-5 4z M8 8h8m-8 4h5",
  question: "M4 4h16v12H9l-5 4z M10 8a2 2 0 0 1 4 0c0 2-2 1-2 3m0 2h.01",
  plan: "m3 7 2 2 3-4m3 2h10M3 15l2 2 3-4m3 2h10",
  plugin: "M8 3v5m8-5v5M6 8h12v4a6 6 0 0 1-6 6v4m0-4a6 6 0 0 1-6-6Z",
  other: "M14 4a6 6 0 0 0-7 7l-4 4a3 3 0 0 0 4 4l4-4a6 6 0 0 0 7-7l-4 4-4-4Z",
};

export function ToolRunHeader({
  run,
  onToggle,
}: {
  readonly run: ToolRun;
  readonly onToggle: () => void;
}) {
  return (
    <button
      type="button"
      className="transcript-tool-run"
      aria-expanded={run.open}
      onClick={onToggle}
    >
      <ToolGlyph kind="other" />
      <span className="tool-run-families">
        {run.summary.families.map(({ label, count }) => (
          <span className="tool-style" data-tool-kind={toolKind(label)} key={label}>
            <span className="tool-name">{label}</span>
            <CallCount count={count} partial={run.summary.partial} />
          </span>
        ))}
      </span>
      <GroupStatus summary={run.summary} />
      <span className="tool-chevron" aria-hidden="true">
        ›
      </span>
    </button>
  );
}

export function FileReadPack({
  pack,
  onInspect,
}: {
  readonly pack: ReadPack;
  readonly onInspect: (target: ReadTarget) => void;
}) {
  return (
    <div className="transcript-read-pack">
      <div className="tool-heading tool-style" data-tool-kind="read">
        <ToolGlyph kind="read" />
        <span className="tool-heading-main">
          <span className="tool-name">FileRead</span>
          <CallCount count={pack.summary.count} partial={pack.summary.partial} />
        </span>
        <GroupStatus summary={pack.summary} />
      </div>
      <button
        type="button"
        className="tool-inspect-link quiet-button"
        onClick={() => onInspect(pack.first)}
      >
        Inspect calls{pack.files.length > 6 ? ` · ${pack.files.length - 6} more files` : ""}
      </button>
      {pack.files.length > 0 && (
        <span className="tool-read-files">
          {pack.files.slice(0, 6).map((file) => (
            <button
              type="button"
              className="quiet-button"
              key={file.label}
              title={file.label}
              onClick={() => onInspect(file)}
            >
              {file.label}
            </button>
          ))}
        </span>
      )}
    </div>
  );
}

export function ToolHeading({
  call,
  state,
  timestamp,
}: {
  readonly call: ToolCall;
  readonly state: HistoryToolState;
  readonly timestamp?: string | null | undefined;
}) {
  const kind = toolKind(call.label);
  return (
    <span className="tool-heading tool-style" data-tool-kind={kind}>
      <ToolGlyph kind={kind} />
      <span className="tool-heading-main">
        <span className="tool-name">{call.label}</span>
        {(call.detail || call.nativeName) && (
          <span className="tool-description" title={call.detail || call.nativeName}>
            {call.detail || call.nativeName}
          </span>
        )}
      </span>
      <span className="tool-heading-meta">
        {state === "failed" && <span className="tool-failed">Failed</span>}
        {state === "running" && <span className="tool-running">Running</span>}
        {timestamp && <HistoryTimestamp timestamp={timestamp} />}
      </span>
    </span>
  );
}

function ToolGlyph({ kind }: { readonly kind: ToolKind }) {
  return (
    <span className="tool-icon" aria-hidden="true">
      <svg
        width="16"
        height="16"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d={glyphs[kind]} />
      </svg>
    </span>
  );
}

function CallCount({ count, partial }: { readonly count: number; readonly partial: boolean }) {
  return (
    <span className="tool-count" title={partial ? "Calls shown in this history window" : undefined}>
      × {count}
      {partial && "+"}
    </span>
  );
}

function GroupStatus({ summary }: { readonly summary: ToolSummary }) {
  return (
    <>
      {summary.failed > 0 && (
        <span className="tool-failed" role="status">
          {summary.failed} failed
        </span>
      )}
      {summary.running > 0 && (
        <span className="tool-running" role="status">
          {summary.running} running
        </span>
      )}
    </>
  );
}
