import type { HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import type { HistoryToolState } from "../../../api/generated/contracts/history";
import { historyKey } from "./history-window";

type CallPart = Extract<HistoryPart, { kind: "tool_call" }>;
type ToolPart = Extract<HistoryPart, { kind: "tool_call" | "tool_result" }>;

export type ToolKind =
  | "command"
  | "read"
  | "search"
  | "edit"
  | "web"
  | "image"
  | "agent"
  | "message"
  | "plan"
  | "question"
  | "plugin"
  | "other";

export type ToolCall = {
  readonly label: string;
  readonly nativeName?: string;
  readonly detail?: string;
};

export type ToolCatalog = ReadonlyMap<string, ToolCall>;

export type ToolSummary = {
  readonly families: readonly { readonly label: string; readonly count: number }[];
  readonly count: number;
  readonly failed: number;
  readonly running: number;
  readonly partial: boolean;
};

export type ToolRun = {
  readonly kind: "tool-run";
  readonly id: string;
  readonly anchorId: string;
  readonly members: readonly string[];
  readonly summary: ToolSummary;
  readonly open: boolean;
};

export type ReadTarget = { readonly entryId: string; readonly callId: string };
export type ReadPack = {
  readonly kind: "read-pack";
  readonly id: string;
  readonly anchorId: string;
  readonly first: ReadTarget;
  readonly files: readonly (ReadTarget & { readonly label: string })[];
  readonly summary: ToolSummary;
};

type NativeRow = {
  readonly kind: "native";
  readonly id: string;
  readonly entry: HistoryEntry;
  readonly grouped: boolean;
};

export type ActivityRow = NativeRow | ToolRun | ReadPack;

type PartGroup =
  | { readonly kind: "prose"; readonly part: HistoryPart }
  | {
      readonly kind: "tool";
      readonly callId: string;
      readonly call: ToolCall;
      readonly state: HistoryToolState;
      readonly parts: readonly ToolPart[];
    };

export function toolCatalog(entries: readonly HistoryEntry[]) {
  const known = new Map<string, ToolCall>();
  for (const entry of entries) {
    for (const part of entry.parts) {
      if (part.kind === "tool_call") known.set(callKey(entry, part.callId), describeCall(part));
    }
  }
  return known;
}

export function activityRows(
  entries: readonly HistoryEntry[],
  known: ToolCatalog,
  expandedRuns: ReadonlyMap<string, boolean>,
  expandedReads: ReadonlySet<string>,
  expandedCalls: ReadonlySet<string>,
  clippedStart: boolean,
  clippedEnd: boolean,
  groupKeys: ReadonlySet<string>,
): ActivityRow[] {
  const rows: ActivityRow[] = [];
  for (let index = 0; index < entries.length;) {
    const first = entries[index];
    if (!first) break;
    if (!canGroup(first, known)) {
      rows.push(native(first));
      index += 1;
      continue;
    }

    let end = index + 1;
    while (end < entries.length) {
      const next = entries[end];
      if (!next || next.turnId !== first.turnId || !canGroup(next, known)) break;
      end += 1;
    }
    const run = entries.slice(index, end);
    const anchorId =
      run.find((item) => groupKeys.has(`tools:${item.entryId}`))?.entryId ?? first.entryId;
    const id = `tools:${anchorId}`;
    const clipped = (index === 0 && clippedStart) || (end === entries.length && clippedEnd);
    const summary = summarize(run, known, clipped);
    if (summary.count > 1) {
      const members = run.map((entry) => entry.entryId);
      const override = members.find((entryId) => expandedRuns.has(entryId));
      const open =
        override !== undefined
          ? expandedRuns.get(override) === true
          : summary.running > 0 ||
            run.some((entry) =>
              entry.parts.some(
                (part) => isTool(part) && expandedCalls.has(openKey(entry.entryId, part.callId)),
              ),
            );
      rows.push({ kind: "tool-run", id, anchorId, members, summary, open });
      if (open) rows.push(...expandedRows(run, known, expandedReads, clipped, groupKeys));
    } else {
      rows.push(...run.map((entry) => native(entry)));
    }
    index = end;
  }
  return rows;
}

export function toolOnly(entry: HistoryEntry) {
  return entry.parts.length > 0 && entry.parts.every(isTool);
}

export function groupParts(entry: HistoryEntry, parts: readonly HistoryPart[], known: ToolCatalog) {
  const groups: PartGroup[] = [];
  const local = new Map(
    parts.flatMap((part) =>
      part.kind === "tool_call" ? [[part.callId, describeCall(part)] as const] : [],
    ),
  );
  for (const part of parts) {
    if (!isTool(part)) {
      groups.push({ kind: "prose", part });
      continue;
    }
    const previous = groups.at(-1);
    const sameCall = previous?.kind === "tool" && previous.callId === part.callId;
    const call = local.get(part.callId) ??
      known.get(callKey(entry, part.callId)) ?? { label: "ToolCall" };
    const state = part.state === "unknown" && sameCall ? previous.state : part.state;
    const group: PartGroup = {
      kind: "tool",
      callId: part.callId,
      call,
      state,
      parts: sameCall ? [...previous.parts, part] : [part],
    };
    if (sameCall) groups[groups.length - 1] = group;
    else groups.push(group);
  }
  return groups;
}

export function toolKind(label: string): ToolKind {
  if (label === "CommandExecution" || label === "CodeExecution") return "command";
  if (label === "FileRead") return "read";
  if (label.endsWith("Search") && !label.startsWith("Web")) return "search";
  if (label.endsWith("Edit")) return "edit";
  if (label.startsWith("Web")) return "web";
  if (label.startsWith("Image")) return "image";
  if (label === "AgentMessage" || label === "AgentFollowup") return "message";
  if (label.startsWith("Agent")) return "agent";
  if (label === "Plan" || label === "Task") return "plan";
  if (label === "UserQuestion") return "question";
  if (label === "MCP" || label === "McpToolCall" || label === "DynamicToolCall") return "plugin";
  return "other";
}

export function openKey(entryId: string, callId: string) {
  return JSON.stringify([entryId, callId]);
}

function expandedRows(
  run: readonly HistoryEntry[],
  known: ToolCatalog,
  expandedReads: ReadonlySet<string>,
  clipped: boolean,
  groupKeys: ReadonlySet<string>,
) {
  const rows: ActivityRow[] = [];
  for (let index = 0; index < run.length;) {
    const first = run[index];
    if (!first) break;
    if (!onlyReads(first, known)) {
      rows.push(native(first, true));
      index += 1;
      continue;
    }

    let end = index + 1;
    while (end < run.length) {
      const next = run[end];
      if (!next || !onlyReads(next, known)) break;
      end += 1;
    }
    const reads = run.slice(index, end);
    const anchorId =
      reads.find((item) => groupKeys.has(`reads:${item.entryId}`))?.entryId ?? first.entryId;
    const id = `reads:${anchorId}`;
    const summary = summarize(reads, known, clipped && (index === 0 || end === run.length));
    const part = first.parts.find(isTool);
    if (part && summary.count > 1 && !reads.some((entry) => expandedReads.has(entry.entryId))) {
      const files = new Map<string, ReadTarget & { readonly label: string }>();
      for (const entry of reads) {
        for (const item of entry.parts) {
          if (item.kind !== "tool_call") continue;
          const label = known.get(callKey(entry, item.callId))?.detail;
          if (label && !files.has(label))
            files.set(label, { entryId: entry.entryId, callId: item.callId, label });
        }
      }
      rows.push({
        kind: "read-pack",
        id,
        anchorId,
        first: { entryId: first.entryId, callId: part.callId },
        files: [...files.values()],
        summary,
      });
    } else rows.push(...reads.map((entry) => native(entry, true)));
    index = end;
  }
  return rows;
}

function native(entry: HistoryEntry, grouped = false): NativeRow {
  return { kind: "native", id: historyKey(entry), entry, grouped };
}

function isTool(part: HistoryPart): part is ToolPart {
  return part.kind === "tool_call" || part.kind === "tool_result";
}

function canGroup(entry: HistoryEntry, known: ToolCatalog) {
  return (
    !entry.partsCursor &&
    toolOnly(entry) &&
    entry.parts.every(
      (part) => !isTool(part) || known.get(callKey(entry, part.callId))?.label !== "UserQuestion",
    )
  );
}

function onlyReads(entry: HistoryEntry, known: ToolCatalog) {
  return entry.parts.every(
    (part) => isTool(part) && known.get(callKey(entry, part.callId))?.label === "FileRead",
  );
}

function summarize(
  entries: readonly HistoryEntry[],
  known: ToolCatalog,
  partial: boolean,
): ToolSummary {
  const calls = new Map<string, { label: string; state: HistoryToolState }>();
  for (const entry of entries) {
    for (const part of entry.parts) {
      if (!isTool(part)) continue;
      const key = callKey(entry, part.callId);
      const call = known.get(key);
      if (!call) partial = true;
      const state = part.state === "unknown" ? (calls.get(key)?.state ?? part.state) : part.state;
      calls.set(key, { label: call?.label ?? "ToolCall", state });
    }
  }
  const families = new Map<string, number>();
  let failed = 0;
  let running = 0;
  for (const { label, state } of calls.values()) {
    families.set(label, (families.get(label) ?? 0) + 1);
    if (state === "failed") failed += 1;
    if (state === "running") running += 1;
  }
  return {
    families: [...families].map(([label, count]) => ({ label, count })),
    count: calls.size,
    failed,
    running,
    partial,
  };
}

function callKey(entry: HistoryEntry, callId: string) {
  return JSON.stringify([entry.turnId, callId]);
}

function describeCall(part: CallPart): ToolCall {
  const label = part.label || "ToolCall";
  const nativeName = part.name === label ? undefined : part.name;
  const detail = argumentDetail(part.text, toolKind(label));
  return { label, ...(nativeName ? { nativeName } : {}), ...(detail ? { detail } : {}) };
}

function argumentDetail(text: string, kind: ToolKind) {
  const value = text.trim();
  if (!value) return undefined;
  if (!value.startsWith("{"))
    return kind === "command" ? value.split("\n", 1)[0]?.slice(0, 180) : undefined;
  try {
    const parsed: unknown = JSON.parse(value);
    if (!parsed || typeof parsed !== "object") return undefined;
    for (const key of [
      "file_path",
      "filePath",
      "path",
      "command",
      "cmd",
      "query",
      "pattern",
      "url",
    ] as const) {
      if (key in parsed) {
        const field: unknown = (parsed as Record<string, unknown>)[key];
        if (typeof field === "string" && field.trim()) return field.trim().slice(0, 200);
      }
    }
  } catch {
    // A truncated native argument has no reliable structured summary.
  }
  return undefined;
}
