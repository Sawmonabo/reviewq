import { useState } from "react";
import { lostReply, quitReviewQ, stillAnswers } from "../../../api/system";
import { Modal } from "../../../components/Modal";
import { Notice } from "../../../components/PanelState";
import { Wordmark } from "../../../components/Wordmark";
import { describeError } from "../../../lib/presentation";
import { DraftsLine } from "./DraftsLine";

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
      heading={
        <>
          Quit{" "}
          <span className="title-name">
            <Wordmark />?
          </span>
        </>
      }
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
        <DraftsLine />
        <p className="meta">
          {startAtLogin
            ? "It starts again when you log in. To start it sooner, open the app and click Start."
            : "It stays off until you open the app and click Start."}
        </p>
        {error && <Notice>{error}</Notice>}
      </div>
    </Modal>
  );
}

// The service answers this navigation if it is still accepting; after that the worker serves its copy.
function showStopped() {
  location.replace("/stopped.html");
}
