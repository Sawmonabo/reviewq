import type { Tab } from "../api/generated/contracts/notifications";
import { savedValue, saveValue } from "../lib/view-preferences";

type View = "queue" | "runs" | "notifications";
export type Route = {
  readonly view: View;
  readonly reviewId?: string;
  readonly runId?: string;
  readonly sessionKey?: string;
  readonly ticket?: string;
  readonly notificationId?: string;
  readonly tab: Tab;
};

export function readRoute(): Route {
  const query = new URLSearchParams(location.search);
  const savedView = savedValue("view");
  const view = query.get("view") ?? savedView;
  const tab = query.get("tab");
  const notificationId = identifier(query.get("notificationId"));
  if (view === "notifications" && notificationId)
    return { view: "notifications", tab: "overview", notificationId };
  const reviewId = identifier(query.get("reviewId"));
  const runId = identifier(query.get("runId"));
  const sessionKey = identifier(query.get("sessionKey"));
  const ticket = identifier(query.get("ticket"));
  return {
    view: view === "runs" || view === "notifications" ? view : "queue",
    tab: tab === "agent" || tab === "findings" || tab === "changes" ? tab : "overview",
    ...(reviewId ? { reviewId } : {}),
    ...(reviewId && runId ? { runId } : {}),
    ...(reviewId && runId && sessionKey ? { sessionKey } : {}),
    ...(ticket ? { ticket } : {}),
  };
}

export function writeRoute(route: Route, replace = false) {
  const url = new URL(location.href);
  if (!replace && !url.searchParams.has("view")) {
    // Back must restore this entry's view even after navigation changes the saved preference.
    url.searchParams.set("view", readRoute().view);
    history.replaceState(history.state, "", url);
  }
  for (const key of [
    "view",
    "tab",
    "reviewId",
    "runId",
    "sessionKey",
    "ticket",
    "notificationId",
  ] as const) {
    const value = key === "tab" && route.notificationId ? undefined : route[key];
    if (value) url.searchParams.set(key, value);
    else url.searchParams.delete(key);
  }
  if (route.notificationId) url.hash = encodeURIComponent(notificationAnchor(route.notificationId));
  else if (url.hash.startsWith("#notification-")) url.hash = "";
  if (replace) history.replaceState(history.state, "", url);
  else history.pushState(null, "", url);
  saveValue("view", route.view);
}

export function openNotificationRoute(notificationId: string) {
  const id = identifier(notificationId);
  if (!id) return;
  const current = readRoute();
  writeRoute(
    { view: "notifications", tab: "overview", notificationId: id },
    current.notificationId === id,
  );
  window.dispatchEvent(new PopStateEvent("popstate"));
}

export function observeRoute(onRoute: (route: Route) => void) {
  function restore() {
    onRoute(readRoute());
  }
  function restoreCachedPage(event: PageTransitionEvent) {
    if (event.persisted) restore();
  }
  window.addEventListener("popstate", restore);
  window.addEventListener("pageshow", restoreCachedPage);
  return () => {
    window.removeEventListener("popstate", restore);
    window.removeEventListener("pageshow", restoreCachedPage);
  };
}

export function notificationAnchor(id: string) {
  return `notification-${id}`;
}

export function settingsScope(route: Route): string {
  return JSON.stringify([
    route.view,
    route.reviewId,
    route.runId,
    route.sessionKey,
    route.ticket,
    route.tab,
  ]);
}

export function restoreSettings(route: Route) {
  return !route.notificationId && savedValue("settings.open") === settingsScope(route);
}

function identifier(value: string | null): string | undefined {
  return value && /^[^\p{Cc}]{1,256}$/u.test(value) ? value : undefined;
}
