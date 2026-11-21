import type { HostProviderDto } from "../../../api/generated/contracts/common";
import { readable } from "../../../lib/presentation";
import { deliveryLabels } from "../delivery-labels";
import { HistoryTimestamp } from "./HistoryTimestamp";
import { ProviderMark } from "./ProviderMark";
import type { VisibleRow } from "./timeline-rows";

type MessageRow = Extract<VisibleRow, { kind: "native" | "live" | "delivery" }>;
type Author = { readonly label: string; readonly symbol: "A" | "RQ" | "Y" };

export function messageAuthor(row: MessageRow, child: boolean): Author | undefined {
  if (
    child &&
    row.entry?.parts.length &&
    row.entry.parts.every((part) => part.kind === "assignment")
  )
    return { label: "Assignment", symbol: "A" };
  const user = row.entry
    ? row.entry.parts.some((part) => part.kind === "user_prompt")
    : Boolean(row.delivery);
  if (!user) return;
  if (row.delivery?.purpose === "thread_followup")
    return { label: "ReviewQ follow-up", symbol: "RQ" };
  if (row.delivery?.purpose === "codex_recovery") return { label: "ReviewQ", symbol: "RQ" };
  return { label: child ? "Incoming message" : "You", symbol: "Y" };
}

export function HistoryEntryHeading({
  row,
  author,
  provider,
}: {
  readonly row: MessageRow;
  readonly author: Author | undefined;
  readonly provider: HostProviderDto;
}) {
  const timestamp = row.entry?.timestamp ?? row.delivery?.createdAt;
  return (
    <header className="timeline-message-heading">
      <span className="timeline-symbol" aria-hidden="true">
        {author?.symbol ?? <ProviderMark provider={provider} />}
      </span>
      <strong>{author?.label ?? readable(provider)}</strong>
      {timestamp && <HistoryTimestamp timestamp={timestamp} delivery={!row.entry?.timestamp} />}
      {row.delivery && (
        <span className="delivery-status" data-state={row.delivery.state}>
          {deliveryLabels[row.delivery.state]}
        </span>
      )}
      {row.provisional !== undefined && <span className="badge green">Working</span>}
    </header>
  );
}
