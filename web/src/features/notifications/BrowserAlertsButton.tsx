import { useState, useSyncExternalStore } from "react";
import { Modal } from "../../components/Modal";
import {
  browserAlertPermission,
  onAlertPermission,
  requestAlertPermission,
} from "./browser-alerts";

export function BrowserAlertsButton() {
  const permission = useSyncExternalStore(onAlertPermission, browserAlertPermission);
  const [help, setHelp] = useState(false);
  if (permission === "unsupported" || permission === "granted") return null;
  return (
    <>
      <button
        type="button"
        title="Let desktop alerts bring you back to this ReviewQ tab."
        onClick={() => {
          if (permission === "denied") setHelp(true);
          else void requestAlertPermission().catch(() => setHelp(true));
        }}
      >
        Enable browser alerts
      </button>
      {help && (
        <Modal title="Browser alerts" onClose={() => setHelp(false)}>
          <p>
            Allow notifications in this site's browser settings so alerts can open in your existing
            ReviewQ tab.
          </p>
        </Modal>
      )}
    </>
  );
}
