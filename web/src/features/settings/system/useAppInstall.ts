import { useSyncExternalStore } from "react";
import { installState, onInstallState, promptInstall } from "./install-offer";

export function useAppInstall() {
  return { state: useSyncExternalStore(onInstallState, installState), install: promptInstall };
}
