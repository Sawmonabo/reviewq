import { useEffect, useState } from "react";
import { readOverview, readQueue, readRun, refreshQueue, resolveReview } from "../api/client";
import type { QueuePage, ReviewOverview, RunDetail } from "../api/generated/contracts";
import { ReviewDrawer } from "../features/review/ReviewDrawer";
import { QueueView } from "./views/QueueView";
import "./workspace.css";

type Address = { readonly reviewId: string; readonly runId?: string };
type Overview = { readonly review: ReviewOverview; readonly run: RunDetail | undefined };

export function App() {
  const [queue, setQueue] = useState<QueuePage>();
  const [address, setAddress] = useState<Address | undefined>(readAddress);
  const [overview, setOverview] = useState<Overview>();
  const [error, setError] = useState<string>();
  const [refreshing, setRefreshing] = useState(false);
  const [queueGeneration, setQueueGeneration] = useState(0);
  const [selectionError, setSelectionError] = useState<string>();

  useEffect(() => {
    const controller = new AbortController();
    void readQueue(controller.signal).then(
      (response) => {
        if (!controller.signal.aborted) setQueue(response.data);
      },
      (reason: unknown) => {
        if (!controller.signal.aborted) setError(message(reason));
      },
    );
    return () => controller.abort();
  }, [queueGeneration]);

  useEffect(() => {
    if (!address) return;
    const controller = new AbortController();
    void loadOverview(address, controller.signal).then(
      (data) => {
        if (!controller.signal.aborted) setOverview(data);
      },
      (reason: unknown) => {
        if (!controller.signal.aborted) setSelectionError(message(reason));
      },
    );
    return () => controller.abort();
  }, [address]);

  useEffect(() => {
    function restoreAddress() {
      setOverview(undefined);
      setSelectionError(undefined);
      setAddress(readAddress());
    }
    window.addEventListener("popstate", restoreAddress);
    return () => window.removeEventListener("popstate", restoreAddress);
  }, []);

  async function handleRefresh() {
    setError(undefined);
    setRefreshing(true);
    try {
      await refreshQueue();
      setQueueGeneration((generation) => generation + 1);
    } catch (reason) {
      setError(message(reason));
    } finally {
      setRefreshing(false);
    }
  }

  function select(next: Address | undefined) {
    const url = new URL(window.location.href);
    if (next) url.searchParams.set("reviewId", next.reviewId);
    else url.searchParams.delete("reviewId");
    if (next?.runId) url.searchParams.set("runId", next.runId);
    else url.searchParams.delete("runId");
    window.history.pushState(window.history.state, "", url);
    setOverview(undefined);
    setSelectionError(undefined);
    setAddress(next);
  }

  return (
    <main className="workspace">
      <header className="workspace-toolbar">
        <h1>ReviewQ</h1>
        <button disabled={refreshing} onClick={() => void handleRefresh()}>
          {refreshing ? "Refreshing…" : "Refresh queue"}
        </button>
      </header>
      <section aria-label="Review queue">
        {error && (
          <p role="alert" className="workspace-error">
            {error}
          </p>
        )}
        <QueueView queue={queue} onReview={(reviewId) => select({ reviewId })} />
      </section>
      {selectionError && (
        <p role="alert" className="workspace-error">
          {selectionError}
        </p>
      )}
      {overview && address && (
        <ReviewDrawer
          overview={overview.review}
          run={overview.run}
          onRun={(runId) => select({ reviewId: address.reviewId, runId })}
          onClose={() => select(undefined)}
        />
      )}
    </main>
  );
}

function readAddress(): Address | undefined {
  const params = new URLSearchParams(window.location.search);
  const reviewId = params.get("reviewId");
  const runId = params.get("runId");
  if (!reviewId) return undefined;
  return runId === null ? { reviewId } : { reviewId, runId };
}

async function loadOverview(address: Address, signal: AbortSignal): Promise<Overview> {
  const [overview, selection] = await Promise.all([
    readOverview(address.reviewId, signal),
    resolveReview(address.reviewId, address.runId, signal),
  ]);
  const run =
    selection.data.runId === null
      ? undefined
      : (await readRun(address.reviewId, selection.data.runId, signal)).data;
  if (run && run.leadSessionKey !== selection.data.leadSessionKey) {
    throw new Error("The selected lead changed.");
  }
  return { review: overview.data, run };
}

function message(reason: unknown): string {
  return reason instanceof Error ? reason.message : "ReviewQ could not complete the request.";
}
