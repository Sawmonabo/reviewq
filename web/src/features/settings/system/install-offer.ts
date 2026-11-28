// Imported by main.tsx: the browser offers installation once, soon after load, long before the
// lazy Settings pane exists. This module is the only listener for these events.

export type InstallState = "offered" | "standalone" | "installed" | "chromium" | "other";

type InstallPromptEvent = Event & {
  readonly prompt: () => Promise<void>;
  readonly userChoice: Promise<unknown>;
};

const changed = new EventTarget();
const standalone = window.matchMedia("(display-mode: standalone)");
let offer: InstallPromptEvent | undefined;
let installed = false;

window.addEventListener("beforeinstallprompt", (event) => {
  event.preventDefault();
  offer = event as InstallPromptEvent;
  notify();
});
window.addEventListener("appinstalled", () => {
  offer = undefined;
  installed = true;
  notify();
});

export function installState(): InstallState {
  if (standalone.matches) return "standalone";
  if (installed) return "installed";
  if (offer) return "offered";
  return "onbeforeinstallprompt" in window ? "chromium" : "other";
}

export function onInstallState(listener: () => void) {
  changed.addEventListener("change", listener);
  standalone.addEventListener("change", listener);
  return () => {
    changed.removeEventListener("change", listener);
    standalone.removeEventListener("change", listener);
  };
}

// Call directly from the click: the browser shows its box only for a user gesture, once per offer.
// A dismissed offer is spent; the button returns only if the browser offers again.
export function promptInstall() {
  const current = offer;
  if (!current) return;
  current.prompt().catch(() => {});
  void current.userChoice
    .catch(() => {})
    .then(() => {
      if (offer === current) offer = undefined;
      notify();
    });
}

function notify() {
  changed.dispatchEvent(new Event("change"));
}
