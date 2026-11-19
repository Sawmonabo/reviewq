import type { ReviewOverview } from "../../api/generated/contracts";
import { Modal } from "../../components/Modal";
import { Notice } from "../../components/PanelState";
import { readable } from "../../lib/presentation";

type Attention = NonNullable<ReviewOverview["attention"]>;

export function ReviewAttention({
  attention,
  confirmLabel,
  stale,
  busy,
  source,
  onClose,
  onConfirm,
  onRefresh,
}: {
  readonly attention: Attention;
  readonly confirmLabel: string;
  readonly stale: boolean;
  readonly busy: boolean;
  readonly source: string | undefined;
  readonly onClose: () => void;
  readonly onConfirm: () => void;
  readonly onRefresh: () => void;
}) {
  return (
    <Modal
      title="Review follow-up evidence"
      busy={busy}
      onClose={onClose}
      footer={
        <>
          <button type="button" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="primary" disabled={stale || busy} onClick={onConfirm}>
            {confirmLabel}
          </button>
        </>
      }
    >
      <div className="stack">
        {stale && (
          <Notice>
            The review or its evidence changed while this dialog was open.
            <button type="button" disabled={busy} onClick={onRefresh}>
              Review current evidence
            </button>
          </Notice>
        )}
        {attention.source.length > 0 && (
          <section className="stack">
            <div className="spread">
              <h3>GitLab changes and discussions</h3>
              {source && (
                <a href={source} target="_blank" rel="noreferrer">
                  Open GitLab
                </a>
              )}
            </div>
            <ul>
              {attention.source.map((item) => (
                <li key={item.eventId}>
                  {item.label} · {readable(item.kind)}
                </li>
              ))}
            </ul>
          </section>
        )}
        {attention.sessions.length > 0 && (
          <section>
            <h3>Native session updates</h3>
            <ul>
              {attention.sessions.map((item) => (
                <li key={JSON.stringify([item.runId, item.sessionKey, item.eventId])}>
                  {item.label} · {readable(item.kind)}
                  <div className="meta">
                    Run <code>{item.runId}</code> · Session <code>{item.sessionKey}</code>
                  </div>
                </li>
              ))}
            </ul>
          </section>
        )}
        {attention.reports.length > 0 && (
          <section>
            <h3>Accepted findings updates</h3>
            <ul>
              {attention.reports.map((item) => (
                <li key={JSON.stringify([item.runId, item.attentionReceiptId])}>
                  Run <code>{item.runId}</code> · Report receipt <code>{item.receiptId}</code>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </Modal>
  );
}
