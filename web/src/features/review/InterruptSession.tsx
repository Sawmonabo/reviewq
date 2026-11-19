import { useRef, useState } from "react";
import { interrupt, readInterruptTarget } from "../../api/review";
import { Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, readable } from "../../lib/presentation";

export function InterruptSession({
  runId,
  sessionKey,
  nativeId,
  className,
}: {
  readonly runId: string;
  readonly sessionKey: string;
  readonly nativeId: string;
  readonly className?: string;
}) {
  const current = useResource(
    JSON.stringify(["interrupt", runId, sessionKey]),
    (signal) => readInterruptTarget(runId, sessionKey, signal),
    [
      { kind: "session", id: sessionKey },
      { kind: "run", id: runId },
    ],
  );
  const sending = useRef(false);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string>();
  const [interrupted, setInterrupted] = useState<string>();
  const turn = current.error || current.data?.turnId === interrupted ? null : current.data?.turnId;

  async function stop() {
    if (
      !turn ||
      sending.current ||
      !window.confirm(`Interrupt turn ${turn} in native session ${nativeId}?`)
    )
      return;
    sending.current = true;
    setBusy(true);
    setNotice(undefined);
    try {
      const result = await interrupt(runId, sessionKey, { expectedTurnId: turn });
      if (result.state === "interrupted") setInterrupted(turn);
      setNotice(`Interrupt · ${readable(result.state)}`);
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      sending.current = false;
      setBusy(false);
      current.retry();
    }
  }

  return (
    <>
      {turn && (
        <button type="button" className={className} disabled={busy} onClick={() => void stop()}>
          Interrupt session
        </button>
      )}
      {notice && <Notice>{notice}</Notice>}
      {current.error && (
        <Notice retry={current.retry}>
          Could not check interrupt availability. {current.error}
        </Notice>
      )}
    </>
  );
}
