import { useState } from "react";
import { lostReply, quitReviewQ, stillAnswers } from "../../../api/system";
import { Modal } from "../../../components/Modal";
import { Notice } from "../../../components/PanelState";
import { describeError } from "../../../lib/presentation";

export function QuitDialog({
  startAtLogin,
  onClose,
}: {
  readonly startAtLogin: boolean;
  readonly onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  async function quit() {
    setBusy(true);
    setError(undefined);
    try {
      await quitReviewQ();
      return showStopped();
    } catch (failure) {
      if (!lostReply(failure)) setError(describeError(failure));
      else if (await stillAnswers()) setError("Couldn’t reach ReviewQ. Try again.");
      else return showStopped();
    }
    setBusy(false);
  }

  return (
    <Modal
      title="Quit ReviewQ?"
      className="settings-confirm"
      onClose={onClose}
      busy={busy}
      footer={
        <>
          <button type="button" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <button type="button" className="primary" disabled={busy} onClick={() => void quit()}>
            Quit
          </button>
        </>
      }
    >
      <div className="stack">
        <p>Monitoring, alerts and follow-ups stop.</p>
        <p className="meta">
          {startAtLogin
            ? "It will start again when you log in. To start it sooner, open ReviewQ and click Start ReviewQ."
            : "It stays off until you open ReviewQ and click Start ReviewQ."}
        </p>
        {error && <Notice>{error}</Notice>}
      </div>
    </Modal>
  );
}

// While the service drains it serves this page itself; once stopped, the worker serves its copy.
function showStopped() {
  location.replace("/stopped.html");
}
