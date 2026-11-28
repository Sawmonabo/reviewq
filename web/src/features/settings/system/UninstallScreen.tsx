import { useEffect, useState } from "react";
import { stillAnswers } from "../../../api/system";
import { Icon } from "../../../components/Icon";
import { Wordmark } from "../../../components/Wordmark";
import { useAppInstall } from "./useAppInstall";
import "./system.css";

const POLL_MS = 500;
const GIVE_UP_MS = 30_000;

export function UninstallScreen() {
  const asApp = useAppInstall().state === "standalone";
  const [done, setDone] = useState(false);

  // In a browser tab, forget the stopped page so a later server on this address never shows it.
  // The installed app keeps it: its icon then opens the page that says how to remove the app.
  useEffect(() => {
    if (!asApp) void forgetStoppedPage();
  }, [asApp]);

  // The uninstall job stops the service, so silence means it is gone. A service that still
  // answers after 30 seconds was not stopped; reloading brings the app back, and the job's
  // desktop notification says why.
  useEffect(() => {
    let active = true;
    let timer: number | undefined;
    const giveUp = Date.now() + GIVE_UP_MS;
    async function poll() {
      const answers = await stillAnswers();
      if (!active) return;
      if (!answers) setDone(true);
      else if (Date.now() >= giveUp) location.reload();
      else timer = window.setTimeout(() => void poll(), POLL_MS);
    }
    timer = window.setTimeout(() => void poll(), POLL_MS);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, []);

  return (
    <main className="system-screen">
      <div className="system-card" role="status">
        {done ? (
          <div className="system-mark done" aria-hidden="true">
            <Icon name="check" size={24} />
          </div>
        ) : (
          <div className="system-mark working" aria-hidden="true" />
        )}
        <h1 aria-label={done ? "ReviewQ is uninstalled" : "Uninstalling ReviewQ"}>
          {done ? (
            <>
              <Wordmark /> is uninstalled
            </>
          ) : (
            <>
              Uninstalling <Wordmark />
            </>
          )}
        </h1>
        <p className="system-lead">
          {done
            ? "You can close this window."
            : "This takes a few seconds. You can close this window."}
        </p>
        {asApp && (
          <p className="system-foot">
            To remove the app icon, right-click it and choose Uninstall.
          </p>
        )}
      </div>
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
