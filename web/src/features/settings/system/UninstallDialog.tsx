import { useState } from "react";
import { lostReply, stillAnswers, uninstallReviewQ } from "../../../api/system";
import { Modal } from "../../../components/Modal";
import { Notice } from "../../../components/PanelState";
import { Wordmark } from "../../../components/Wordmark";
import { describeError } from "../../../lib/presentation";
import { DraftsLine } from "./DraftsLine";
import { markUninstallStarted } from "./uninstall-state";

export function UninstallDialog({
  dataSize,
  onClose,
}: {
  readonly dataSize: string | undefined;
  readonly onClose: () => void;
}) {
  const [deleteData, setDeleteData] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  async function uninstall() {
    setBusy(true);
    setError(undefined);
    try {
      await uninstallReviewQ({ deleteData });
      return markUninstallStarted();
    } catch (failure) {
      if (!lostReply(failure)) setError(describeError(failure));
      else if (await stillAnswers()) setError("Couldn’t reach ReviewQ. Try again.");
      // With no reply and no answer, the uninstall most likely started.
      else return markUninstallStarted();
    }
    setBusy(false);
  }

  return (
    <Modal
      title="Uninstall ReviewQ?"
      heading={
        <>
          Uninstall{" "}
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
          <button
            type="button"
            className="settings-danger"
            disabled={busy}
            onClick={() => void uninstall()}
          >
            {deleteData ? "Uninstall and delete data" : "Uninstall"}
          </button>
        </>
      }
    >
      <div className="stack">
        <p>
          Removes the background service, the <code>reviewq</code> command, and the Claude plugin
          and Codex hooks.
        </p>
        <div>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={deleteData}
              disabled={busy}
              onChange={(event) => setDeleteData(event.currentTarget.checked)}
            />
            Also delete my data{dataSize && ` (${dataSize})`}
          </label>
          <p className="meta settings-check-hint">
            Reviews, reports, settings and saved GitLab and Jira tokens.
            <br />
            Leave this unticked to reinstall later and keep everything.
          </p>
        </div>
        <p className="meta">
          Your Claude and Codex conversations and your GitLab merge requests are not touched.
        </p>
        <DraftsLine />
        {error && <Notice>{error}</Notice>}
      </div>
    </Modal>
  );
}
