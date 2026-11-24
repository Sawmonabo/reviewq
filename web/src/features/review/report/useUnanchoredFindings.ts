import { useMemo } from "react";
import type { RefObject } from "react";
import type { Descriptor, ReportSummary } from "../../../api/generated/contracts/reports";
import { useResource } from "../../../hooks/useResource";
import { contextItems } from "./report-context";
import { finishReading } from "./report-position";
import { unanchoredBlock } from "./report-presentation";
import { withUnanchored } from "./report-reader";
import type { ReaderDocument } from "./report-reader";
import { isRetired } from "./report-structure";
import type { AcceptedFindingChange } from "./report-structure";

type Finding = Extract<Descriptor, { kind: "finding" }>;

// Every active finding gets a card, including one carried from an earlier version
// or listed in the findings ledger without a heading in this report's text. The
// report renders first; the cards join it when the findings arrive, and refresh
// after a disposition save.
export function useUnanchoredFindings(
  runId: string,
  report: ReportSummary,
  admitted: boolean,
  host: RefObject<HTMLElement | null>,
  reader: ReaderDocument | undefined,
  changes: ReadonlyMap<string, AcceptedFindingChange> | undefined,
) {
  const wanted = admitted && report.unanchoredFindingCount > 0;
  const findings = useResource(
    JSON.stringify(["unanchored-findings", runId, report.receiptId, report.sha256, wanted]),
    async (signal) => {
      if (!wanted) return { cursor: "", data: null };
      const { items, cursor } = await contextItems(
        runId,
        report,
        "findings",
        (item): item is Finding => item.kind === "finding",
        null,
        signal,
      );
      // Rows keep their identity and the reader's place when the cards join, but a card
      // inserted inside a native selection would join it, so wait while the reader holds one.
      await finishReading(host.current?.querySelector(".report-blocks") ?? null, signal, false);
      signal.throwIfAborted();
      return { cursor, data: items };
    },
    wanted ? [{ kind: "report", id: runId }] : [],
  );
  const items = findings.data;
  const withCards = useMemo(() => {
    if (!reader || !items) return reader;
    const cards = new Set(reader.groups.flatMap((group) => group.findingId ?? []));
    const blocks = [...items.values()].flatMap((item) => {
      const change = changes?.get(item.findingId);
      return cards.has(item.findingId) || isRetired(change) ? [] : [unanchoredBlock(item, change)];
    });
    return withUnanchored(reader, blocks, changes);
  }, [reader, items, changes]);
  return { reader: withCards, error: Boolean(findings.error), retry: findings.retry };
}
