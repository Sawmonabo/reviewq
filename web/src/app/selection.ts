import type { Tab } from "../api/generated/contracts/notifications";
import { savedValue, saveValue } from "../lib/view-preferences";

export type View = "queue" | "runs" | "notifications";
export type Route = {
  readonly view: View;
  readonly reviewId?: string;
  readonly runId?: string;
  readonly sessionKey?: string;
  readonly ticket?: string;
  readonly tab: Tab;
};

export function readRoute(): Route {
  const query = new URLSearchParams(location.search);
  const savedView = savedValue("view");
  const view = query.get("view") ?? savedView;
  const tab = query.get("tab");
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
  for (const key of ["view", "tab", "reviewId", "runId", "sessionKey", "ticket"] as const) {
    const value = route[key];
    if (value) url.searchParams.set(key, value);
    else url.searchParams.delete(key);
  }
  if (replace) history.replaceState(history.state, "", url);
  else history.pushState(null, "", url);
  saveValue("view", route.view);
}

function identifier(value: string | null): string | undefined {
  return value && /^[^\p{Cc}]{1,256}$/u.test(value) ? value : undefined;
}
