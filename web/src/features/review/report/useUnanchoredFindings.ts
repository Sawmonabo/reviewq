import { useMemo } from "react";
import type { RefObject } from "react";
import type { Descriptor, ReportSummary } from "../../../api/generated/contracts/reports";
import { useResource } from "../../../hooks/useResource";
import { contextItems } from "./report-context";
import { finishReading } from "./report-position";
import { expectedLedgerCards, ledgerFindings, unanchoredBlock } from "./report-presentation";
import { withUnanchored } from "./report-reader";
import type { ReaderDocument } from "./report-reader";
import type { AcceptedFindingChange } from "./report-structure";

type Finding = Extract<Descriptor, { kind: "finding" }>;

// Every active finding gets a card, including one carried from an earlier version,
// listed in the findings ledger without a heading in this report's text, or headed
// inside an element such as `<details>`. The report renders first; the cards join it
// when the findings arrive, and refresh after a disposition save. A summary that
// expects cards starts the read beside the report's; a finding count above the
// text's own cards starts it once the text is formatted.
export function useUnanchoredFindings(
  runId: string,
  report: ReportSummary,
  admitted: boolean,
  host: RefObject<HTMLElement | null>,
  reader: ReaderDocument | undefined,
  changes: ReadonlyMap<string, AcceptedFindingChange> | undefined,
  base: string | undefined,
) {
  const expected = reader
    ? expectedLedgerCards(report, reader.groups, changes)
    : report.unanchoredFindingCount;
  const wanted = admitted && expected > 0;
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
    const blocks = ledgerFindings(reader.groups, items.values(), changes).map((item) =>
      unanchoredBlock(item, changes?.get(item.findingId), base),
    );
    return withUnanchored(reader, blocks, changes);
  }, [reader, items, changes, base]);
  return {
    reader: withCards,
    expected,
    error: Boolean(findings.error),
    retry: findings.retry,
  };
}
