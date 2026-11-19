import { lazy, Suspense, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties } from "react";
import { readOverview, readRun, resolveReview } from "../api/client";
import { startWorkspaceEvents } from "../api/events";
import type { Notification } from "../api/generated/contracts";
import type { Tab } from "../api/generated/contracts/notifications";
import { readNotifications } from "../api/workspace";
import { ActionMenu } from "../components/ActionMenu";
import { Icon } from "../components/Icon";
import { Modal } from "../components/Modal";
import { PanelBoundary } from "../components/PanelBoundary";
import { Loading, Notice } from "../components/PanelState";
import type { LaunchMode, LaunchTarget } from "../features/queue/types";
import { useResource } from "../hooks/useResource";
import { DraftStore } from "../lib/drafts";
import { describeError } from "../lib/presentation";
import { savedValue, saveValue } from "../lib/view-preferences";
import { readRoute, writeRoute } from "./selection";
import type { Route, View } from "./selection";
import { clampSplit, Splitter } from "./Splitter";
import { AttentionView } from "./views/AttentionView";
import { QueueView } from "./views/QueueView";
import { RunsView } from "./views/RunsView";
import "../styles/tokens.css";
import "../styles/base.css";
import "./workspace.css";

const ReviewDrawer = lazy(() => import("../features/review/ReviewDrawer"));
const SettingsPane = lazy(() => import("../features/settings/SettingsPane"));
const BulkLaunch = lazy(() => import("../features/queue/BulkLaunch"));
const MonitorPane = lazy(() => import("../features/review/followups/MonitorPane"));
const Tickets = lazy(() => import("../features/tickets/Tickets"));

const views = [
  { view: "queue", label: "Review queue", icon: "queue" },
  { view: "runs", label: "Review runs", icon: "runs" },
  { view: "notifications", label: "Notifications", icon: "inbox" },
] as const;

type Overlay =
  | { readonly kind: "launch"; readonly mode: LaunchMode; readonly targets: readonly LaunchTarget[] }
  | { readonly kind: "monitor"; readonly targets: readonly LaunchTarget[]; readonly pause: boolean }
  | { readonly kind: "drafts" };

export function App() {
  const [route, setRoute] = useState(readRoute);
  const [split, setSplit] = useState<number>(() => typeof savedValue("split") === "number" ? Number(savedValue("split")) : 50);
  const [railSmall, setRailSmall] = useState(savedValue("rail") === true);
  const [theme, setTheme] = useState(savedValue("theme") === "light" ? "light" : "dark");
  const [compact, setCompact] = useState(savedValue("compact") === true);
  const [settings, setSettings] = useState(false);
  const [revision, setRevision] = useState(0);
  const [overlay, setOverlay] = useState<Overlay>();
  const [selecting, setSelecting] = useState(false);
  const [offline, setOffline] = useState(!navigator.onLine);
  const [drafts] = useState(() => new DraftStore());
  const draftSnapshot = useSyncExternalStore(drafts.subscribe, drafts.getSnapshot);
  const panes = useRef<HTMLDivElement>(null);
  const workspace = useRef<HTMLElement>(null);
  const drawer = useRef<HTMLDivElement>(null);
  const opener = useRef<HTMLElement | undefined>(undefined);
  const { reviewId, runId, sessionKey, ticket } = route;
  const opened = Boolean(reviewId || ticket);
  const heading = views.find((item) => item.view === route.view)?.label ?? "Review queue";

  const overview = useResource(reviewId ?? "", (signal) => reviewId
    ? readOverview(reviewId, signal)
    : Promise.resolve({ data: null, cursor: "" }), reviewId ? [{ kind: "review", id: reviewId }, { kind: "run" }, { kind: "action" }] : []);
  const selected = useResource(JSON.stringify([reviewId, runId]), async (signal) => {
    if (!reviewId) return { data: null, cursor: "" };
    const selection = await resolveReview(reviewId, runId, signal);
    if (selection.data.runId === null) return { data: { run: undefined }, cursor: selection.cursor };
    const result = await readRun(reviewId, selection.data.runId, signal);
    if (result.data.leadSessionKey !== selection.data.leadSessionKey) throw new Error("The selected lead changed.");
    return { data: { run: result.data }, cursor: result.cursor };
  }, reviewId ? [{ kind: "run" }, { kind: "session" }] : []);
  const unread = useResource("rail-unread", (signal) => readNotifications({ view: "unread", limit: 1 }, signal), [{ kind: "notification" }]);

  useEffect(startWorkspaceEvents, []);
  useEffect(() => {
    function restore() { setRoute(readRoute()); setOverlay(undefined); }
    function network() { setOffline(!navigator.onLine); }
    window.addEventListener("popstate", restore);
    window.addEventListener("online", network);
    window.addEventListener("offline", network);
    return () => {
      window.removeEventListener("popstate", restore);
      window.removeEventListener("online", network);
      window.removeEventListener("offline", network);
    };
  }, []);
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.dataset.density = compact ? "compact" : "regular";
  }, [theme, compact]);
  useEffect(() => {
    const host = panes.current;
    if (!host) return;
    const observer = new ResizeObserver(() => {
      const width = host.clientWidth;
      if (width > 0) setSplit((value) => clampSplit(value, width));
    });
    observer.observe(host);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    const target = workspace.current;
    if (!target) return;
    const saved = savedValue(`scroll.${route.view}`);
    if (typeof saved === "number" && Number.isFinite(saved)) target.scrollTop = saved;
    function save() { if (target) saveValue(`scroll.${route.view}`, target.scrollTop); }
    target.addEventListener("scrollend", save);
    window.addEventListener("pagehide", save);
    return () => {
      target.removeEventListener("scrollend", save);
      window.removeEventListener("pagehide", save);
    };
  }, [route.view]);
  useEffect(() => {
    if (opened && window.matchMedia("(max-width: 700px)").matches) drawer.current?.focus();
  }, [opened]);

  function move(next: Route) {
    if (JSON.stringify(next) === JSON.stringify(route)) return;
    if (next.view !== route.view) {
      if (workspace.current) saveValue(`scroll.${route.view}`, workspace.current.scrollTop);
      setSelecting(false);
    }
    if (next.reviewId && next.reviewId !== reviewId && (reviewId || ticket)) resize(50);
    if (next.reviewId !== reviewId || next.runId !== runId || next.ticket !== ticket) setOverlay(undefined);
    writeRoute(next);
    setRoute(next);
  }
  function openReview(id: string, exactRun?: string, tab: Tab = "overview", child?: string) {
    if (!opened && document.activeElement instanceof HTMLElement) opener.current = document.activeElement;
    move({ view: route.view, reviewId: id, ...(exactRun ? { runId: exactRun } : {}), ...(child ? { sessionKey: child } : {}), tab });
  }
  function openTicket(key: string) {
    if (!opened && document.activeElement instanceof HTMLElement) opener.current = document.activeElement;
    move({ view: route.view, ticket: key, tab: "overview" });
  }
  function closeDrawer() {
    move({ view: route.view, tab: "overview" });
    requestAnimationFrame(() => {
      if (opener.current?.isConnected) opener.current.focus();
      else document.getElementById("view-title")?.focus();
    });
  }
  function openNotification(item: Notification) {
    openReview(item.reviewId, item.runId ?? undefined, item.tab, item.sessionKey ?? undefined);
  }
  function menuLaunch(mode: LaunchMode, targets: readonly LaunchTarget[] = []) {
    setOverlay({ kind: "launch", mode, targets });
  }
  function monitor(targets: readonly LaunchTarget[], pause: boolean) {
    setOverlay({ kind: "monitor", targets, pause });
  }
  function resize(next: number) { setSplit(next); saveValue("split", next); }
  function toggleDensity() { setCompact(!compact); saveValue("compact", !compact); }
  function toggleTheme() {
    const next = theme === "dark" ? "light" : "dark";
    setTheme(next); saveValue("theme", next);
  }
  function toggleRail() { setRailSmall(!railSmall); saveValue("rail", !railSmall); }

  return <main className={`workspace ${railSmall ? "rail-small" : ""} ${opened ? "has-detail" : ""}`}>
    <a className="skip-link" href="#view-title">Skip to workspace</a>
    <aside className="rail" aria-label="Workspace navigation">
      <div className="rail-brand"><span className="brand-mark"><Icon name="branch" size={18} /></span><span className="rail-label">ReviewQ</span></div>
      <nav>{views.map(({ view, label, icon }) => <button type="button" key={view} className="rail-item"
        title={label} aria-label={label} aria-current={view === route.view ? "page" : undefined}
        onClick={() => move({ view, tab: "overview" })}><Icon name={icon} /><span className="rail-label">{label}</span>
        {view === "notifications" && Boolean(unread.data?.unreadMrCount) && <span className="rail-count" aria-label={`${unread.data?.unreadMrCount} unread reviews`}>{unread.data?.unreadMrCount}</span>}
      </button>)}</nav>
      <div className="rail-footer">
        {draftSnapshot.full && <button type="button" className="rail-item" onClick={() => setOverlay({ kind: "drafts" })} title="Draft storage full"><Icon name="inbox" /><span className="rail-label">Draft storage full</span></button>}
        <button type="button" className="rail-item" title="Settings" aria-label="Settings" onClick={() => setSettings(true)}><Icon name="settings" /><span className="rail-label">Settings</span></button>
        <button type="button" className="rail-item rail-theme" title={theme === "dark" ? "Switch to light theme" : "Switch to dark theme"} onClick={toggleTheme}><Icon name="sun" /><span className="rail-label">{theme === "dark" ? "Light theme" : "Dark theme"}</span></button>
        <button type="button" className="rail-item rail-collapse" title={railSmall ? "Expand sidebar" : "Collapse sidebar"} onClick={toggleRail}><Icon name="collapse" /><span className="rail-label">Collapse sidebar</span></button>
      </div>
    </aside>
    <div className={`workspace-panes ${opened ? "has-detail" : ""}`} ref={panes} style={{ "--split": `${split}%` } as CSSProperties}>
      <section ref={workspace} className="workspace-view" aria-labelledby="view-title">
        <div className="workspace-inner">
          <header className="workspace-title">
            <h1 id="view-title" tabIndex={-1}>{heading}</h1>
            <div className="actions">
              {route.view !== "notifications" && <ActionMenu label="New review" trigger={<span className="button-label"><Icon name="plus" />New review <Icon name="chevron" size={13} /></span>} items={[
                { label: "Review MR", run: () => menuLaunch("pick") },
                { label: "Select MRs", run: () => setSelecting(true) },
                { label: "Register run", run: () => menuLaunch("register") },
              ]} />}
              <ActionMenu label="Workspace display" trigger={<Icon name="more" />} items={[
                { label: compact ? "Regular spacing" : "Compact spacing", run: toggleDensity },
                { label: theme === "dark" ? "Light theme" : "Dark theme", run: toggleTheme },
                { label: "Unsent drafts", run: () => setOverlay({ kind: "drafts" }) },
              ]} />
            </div>
          </header>
          {offline && <Notice>Offline. Showing the last available information.</Notice>}
          <PanelBoundary key={route.view}>
            {route.view === "queue" && <QueueView selectedReview={reviewId} revision={revision} selecting={selecting} onSelect={setSelecting} onReview={openReview} onTicket={openTicket} onSettings={() => setSettings(true)} onLaunch={menuLaunch} onMonitor={monitor} />}
            {route.view === "runs" && <RunsView selectedReview={reviewId} selecting={selecting} onSelect={setSelecting} onReview={openReview} onLaunch={menuLaunch} onMonitor={monitor} />}
            {route.view === "notifications" && <AttentionView onOpen={openNotification} onChange={unread.retry} />}
          </PanelBoundary>
        </div>
      </section>
      {opened && <>
        <Splitter host={panes} value={split} onChange={resize} />
        <div className="drawer-host" ref={drawer} tabIndex={-1} aria-label={ticket ? "Ticket details" : "Review details"} onKeyDown={(event) => {
          if (event.key === "Escape" && !event.defaultPrevented && !overlay && !settings) closeDrawer();
        }}>
          <PanelBoundary key={reviewId ?? ticket}>
            <Suspense fallback={<Loading label="Opening detail panel…" />}>
              {overview.data && reviewId ? <ReviewDrawer overview={overview.data} run={selected.data?.run} selectedSession={sessionKey}
                selectionError={selected.error} selectionPending={selected.pending} tab={route.tab} drafts={drafts}
                onReload={() => { overview.retry(); selected.retry(); }} onTab={(tab) => move({ ...route, tab })}
                onSession={(session) => {
                  const exactRun = selected.data?.run?.runId;
                  if (exactRun) openReview(reviewId, exactRun, "agent", session);
                }}
                onRun={(id) => openReview(reviewId, id, route.tab)}
                onClose={closeDrawer} onLaunch={menuLaunch} onMonitor={monitor} onSettings={() => setSettings(true)}
                ticket={<Tickets keys={overview.data.review.tickets.map((entry) => entry.key)} />}
              /> : reviewId ? <>
                <div className="drawer-header spread"><h2>Review details</h2><button type="button" aria-label="Close review" onClick={closeDrawer}><Icon name="close" /></button></div>
                {overview.error ? <Notice retry={overview.retry}>{overview.error} <code>{reviewId}</code></Notice> : <Loading label="Opening review…" />}
              </> : ticket && <div className="drawer-surface">
                <header className="drawer-header spread"><h2>{ticket}</h2><button type="button" className="icon-button" aria-label="Return to workspace" onClick={closeDrawer}><Icon name="back" /></button></header>
                <div className="drawer-body"><Tickets keys={[ticket]} /></div>
              </div>}
            </Suspense>
          </PanelBoundary>
        </div>
      </>}
    </div>
    <PanelBoundary>
      <Suspense fallback={null}>
        {overlay?.kind === "launch" && <BulkLaunch mode={overlay.mode} targets={overlay.targets} settingsRevision={revision} onSettings={() => setSettings(true)} onClose={() => setOverlay(undefined)}
          onOpen={(id, run) => { setOverlay(undefined); setSelecting(false); openReview(id, run, "agent"); }} />}
        {overlay?.kind === "monitor" && <MonitorPane targets={overlay.targets} pause={overlay.pause} onClose={() => setOverlay(undefined)} />}
        {settings && <SettingsPane onClose={() => setSettings(false)} onSave={() => { setRevision((value) => value + 1); overview.retry(); }} />}
      </Suspense>
    </PanelBoundary>
    {overlay?.kind === "drafts" && <Modal title="Unsent drafts" onClose={() => setOverlay(undefined)}>
      <div className="stack"><p className="meta">Drafts stay in this tab until they’re sent, discarded, or the page reloads.</p>
        {draftSnapshot.full && <Notice>Draft storage full. Shorten or discard a draft to make room.</Notice>}
        {!draftSnapshot.entries.length && <p>No unsent drafts.</p>}
        {draftSnapshot.entries.map((draft) => <div className="surface-card stack" key={draft.key}>
          <strong>{draft.label}</strong><p className="meta truncate">{draft.text}</p>
          <div className="actions">
            <button type="button" onClick={() => { void navigator.clipboard.writeText(draft.text).catch((error: unknown) => window.alert(describeError(error))); }}>Copy</button>
            <button type="button" onClick={() => {
              if (window.confirm(`Discard unsent draft for ${draft.label}?`)) drafts.set(draft.key, draft.label, "");
            }}>Discard</button>
          </div>
        </div>)}
      </div>
    </Modal>}
  </main>;
}
