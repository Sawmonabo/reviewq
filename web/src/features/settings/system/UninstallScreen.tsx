import { useEffect } from "react";
import type { UninstallReceipt } from "../../../api/generated/contracts";
import { Wordmark } from "../../../components/Wordmark";
import { useAppInstall } from "./useAppInstall";
import "./system.css";

export function UninstallScreen({ receipt }: { readonly receipt: UninstallReceipt }) {
  const asApp = useAppInstall().state === "standalone";

  // In a browser tab, forget the stopped page so a later server on this address never shows it.
  // The installed app keeps it: its icon then opens the page that says how to remove the app.
  useEffect(() => {
    if (!asApp) void forgetStoppedPage();
  }, [asApp]);

  return (
    <main className="system-screen">
      <Wordmark />
      <h1>Uninstalling ReviewQ</h1>
      <p>
        This takes a few seconds. You can close this window.
        <br />
        If anything stops it, a desktop notification will say what is left.
      </p>
      <p className="meta">
        {receipt.deleteData
          ? "Your data will be deleted once everything else is removed."
          : `Your data will be kept at ${receipt.dataFolder}.`}
      </p>
      {asApp && (
        <p className="meta">To remove the ReviewQ app icon, right-click it and choose Uninstall.</p>
      )}
    </main>
  );
}

async function forgetStoppedPage() {
  try {
    await (await navigator.serviceWorker.getRegistration())?.unregister();
    for (const name of await caches.keys()) await caches.delete(name);
  } catch {
    // Nothing is left to show if the worker or its cache was never created.
  }
}
