// Set once by the Uninstall box when the service accepts the uninstall. The app then shows only
// the uninstall screen.

const listeners = new Set<() => void>();
let started = false;

export function uninstallStarted() {
  return started;
}

export function onUninstallStarted(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function markUninstallStarted() {
  started = true;
  for (const listener of listeners) listener();
}
