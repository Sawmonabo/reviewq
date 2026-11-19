import { useState } from "react";
import { historyPrompts } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { formatTime } from "../../../lib/presentation";

export function PromptNavigator({ target, generation, onJump }: {
  readonly target: SessionAddress;
  readonly generation: string | undefined;
  readonly onJump: (id: string, locator: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [overviewCursor, setOverview] = useState<string>();
  const [cursor, setCursor] = useState<string>();
  const [firstCursor, setFirstCursor] = useState<string>();
  const navigation = useResource(`prompts:${target.runId}:${target.sessionKey}:${generation}:${open}:${cursor ?? ""}:${overviewCursor ?? ""}`,
    (signal) => open ? historyPrompts(target, cursor ? { cursor, limit: 50 } : {
      maxGroups: 64, ...(overviewCursor ? { overviewCursor } : {}),
    }, signal) : Promise.resolve({ data: null, cursor: "" }));
  const page = navigation.data;
  return <details className="prompt-navigator" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>Prompts</summary>
    {open && <div className="stack prompt-pages">
      {navigation.error && <Notice retry={navigation.retry}>{navigation.error}</Notice>}
      {navigation.pending && <Loading label="Reading user prompts…" />}
      {page?.kind === "overview" && <>
        <p className="meta">{page.total} confirmed prompts</p>
        {page.groups.map((group) => <button type="button" key={group.cursor} onClick={() => { setFirstCursor(group.cursor); setCursor(group.cursor); }}>
          Prompts {group.first}–{group.last}</button>)}
        {overviewCursor && <button type="button" onClick={() => setOverview(undefined)}>First prompt groups</button>}
        {page.nextWindowCursor && <button type="button" onClick={() => setOverview(page.nextWindowCursor)}>More prompt groups</button>}
      </>}
      {page?.kind === "previews" && <>
        <button type="button" onClick={() => setCursor(undefined)}>All prompt groups</button>
        {page.items.map((item) => <button className="prompt-preview" type="button" title={item.timestamp ? formatTime(item.timestamp) : undefined}
          key={item.entryId} onClick={() => onJump(item.entryId, item.locator)}><strong>{item.ordinal}</strong> {item.text.slice(0, 160)}</button>)}
        {firstCursor !== cursor && <button type="button" onClick={() => setCursor(firstCursor)}>First previews</button>}
        {page.nextCursor && <button type="button" onClick={() => setCursor(page.nextCursor ?? undefined)}>More previews</button>}
      </>}
    </div>}
  </details>;
}
