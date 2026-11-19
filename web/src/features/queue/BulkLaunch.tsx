import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ApiResponseError, readOverview } from "../../api/client";
import type {
  HostProviderDto,
  RunPrepareRequest,
  RunStartRequest,
} from "../../api/generated/contracts";
import type { Catalog, Preview, Receipt } from "../../api/generated/contracts/launch";
import { launchReceipt, prepareRun, registerRun, runCatalog, startRun } from "../../api/launch";
import { readQueue, trackReview } from "../../api/workspace";
import { Icon } from "../../components/Icon";
import { Modal } from "../../components/Modal";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, readable } from "../../lib/presentation";
import { LaunchReceiptCard, LaunchReceipts } from "./LaunchReceipts";
import type { PendingLaunch } from "./pending-launches";
import {
  pendingLaunches,
  rememberPendingLaunch,
  settleLaunchReceipt,
  settleLaunchRefusal,
  watchPendingLaunches,
} from "./pending-launches";
import type { LaunchMode, LaunchTarget } from "./types";

type Attempt = {
  readonly request: RunStartRequest;
  readonly receipt?: Receipt;
  readonly error?: string;
  readonly refused?: boolean;
};
type Props = {
  readonly mode: LaunchMode;
  readonly targets: readonly LaunchTarget[];
  readonly settingsRevision: number;
  readonly onSettings: () => void;
  readonly onClose: () => void;
  readonly onOpen: (review: string, run: string) => void;
};

export default function BulkLaunch(props: Props) {
  if (props.mode === "receipts")
    return <LaunchReceipts onOpen={props.onOpen} onClose={props.onClose} />;
  return <LaunchEditor {...props} />;
}

function LaunchEditor({
  mode,
  targets: initialTargets,
  settingsRevision,
  onSettings,
  onClose,
  onOpen,
}: Props) {
  const [targets, setTargets] = useState(initialTargets);
  const [provider, setProvider] = useState<HostProviderDto>("codex");
  const [skill, setSkill] = useState("");
  const [invocation, setInvocation] = useState<"skill" | "custom">("skill");
  const [args, setArgs] = useState("");
  const [prompt, setPrompt] = useState("");
  const [context, setContext] = useState("");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");
  const [search, setSearch] = useState("");
  const [url, setUrl] = useState("");
  const [showTrack, setShowTrack] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [nativeId, setNativeId] = useState("");
  const [preview, setPreview] = useState<{ revision: number; items: readonly Preview[] }>();
  const [attempts, setAttempts] = useState<readonly Attempt[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string>();
  const pending = useSyncExternalStore(watchPendingLaunches, pendingLaunches);
  const targetIds = new Set(targets.map((target) => target.reviewId));
  const earlier = pending.filter(
    (item) =>
      targetIds.has(item.reviewId) &&
      !attempts.some((attempt) => attempt.request.submissionId === item.submissionId),
  );
  const request = useRef<AbortController | undefined>(undefined);
  const register = mode === "register";
  const active = targets[0];
  const pick = useResource(
    `launch-picker:${settingsRevision}:${search}`,
    (signal) =>
      readQueue(
        { state: "all_open", ownership: "all", sprint: "any", q: search, limit: 30 },
        signal,
      ),
    [{ kind: "queue" }, { kind: "settings" }],
  );
  const catalogs = useResource(
    `catalog:${settingsRevision}:${provider}:${JSON.stringify(targets.map((target) => target.reviewId))}:${register}`,
    async (signal) => {
      if (!targets.length || register) return { data: [], cursor: "" };
      const results = await Promise.all(
        targets.map((target) => runCatalog({ reviewId: target.reviewId, provider }, signal)),
      );
      return { data: results.map((result) => result.data), cursor: results[0]?.cursor ?? "" };
    },
    [{ kind: "settings" }],
  );
  useEffect(() => () => request.current?.abort(), []);
  const skills = sharedSkills(catalogs.data ?? []);
  const defaultSkill = catalogs.data?.every(
    (catalog) => catalog.defaultSkill?.name === catalogs.data?.[0]?.defaultSkill?.name,
  )
    ? (catalogs.data?.[0]?.defaultSkill?.name ?? "")
    : "";
  const selectedSkill = skill || (skills.includes(defaultSkill) ? defaultSkill : "");
  const models = (catalogs.data?.[0]?.models ?? []).filter((candidate) =>
    catalogs.data?.every((entry) => entry.models.some((item) => item.id === candidate.id)),
  );
  const modelDef = models.find((item) => item.id === model);
  const efforts = (modelDef?.efforts ?? []).filter((value) =>
    catalogs.data?.every((catalog) =>
      catalog.models.find((entry) => entry.id === model)?.efforts.includes(value),
    ),
  );
  const currentPreview = preview?.revision === settingsRevision ? preview.items : undefined;

  function clearPreview() {
    setPreview(undefined);
    setNotice(undefined);
  }

  function selectTarget(target: LaunchTarget) {
    setTargets([target]);
    setSkill("");
    setModel("");
    setEffort("");
    clearPreview();
  }

  async function track() {
    if (!pick.data || !url.trim() || busy) return;
    setBusy(true);
    setNotice(undefined);
    const controller = new AbortController();
    request.current = controller;
    try {
      const tracked = await trackReview({
        expectedRevision: pick.data.settingsRevision,
        url: url.trim(),
      });
      const overview = await readOverview(tracked.data.reviewId, controller.signal);
      if (controller.signal.aborted) return;
      selectTarget(overview.data.review);
      setShowTrack(false);
      setUrl("");
      pick.retry();
    } catch (error) {
      if (!controller.signal.aborted) setNotice(describeError(error));
    } finally {
      setBusy(false);
    }
  }

  async function prepare() {
    if (busy || earlier.length || !catalogs.data?.length || !targets.length) return;
    setBusy(true);
    setNotice(undefined);
    const controller = new AbortController();
    request.current = controller;
    try {
      const source = await Promise.all(
        targets.map((target) => {
          const skillChoice = catalogs.data
            ?.find((catalog) => catalog.reviewId === target.reviewId)
            ?.skills.find((item) => item.name === selectedSkill);
          const resolvedPrompt = expandTicket(prompt, target);
          const resolvedContext = expandTicket(context, target);
          const body: RunPrepareRequest = {
            reviewId: target.reviewId,
            provider,
            context: resolvedContext || null,
            model: model || null,
            effort: effort || null,
            invocation:
              invocation === "custom"
                ? { kind: "custom", prompt: resolvedPrompt }
                : {
                    kind: "skill",
                    name: selectedSkill,
                    path: skillChoice?.path ?? null,
                    args: expandTicket(args, target) || null,
                  },
          };
          return prepareRun(body, controller.signal);
        }),
      );
      if (!controller.signal.aborted) setPreview({ revision: settingsRevision, items: source });
    } catch (error) {
      if (!controller.signal.aborted) setNotice(describeError(error));
    } finally {
      setBusy(false);
    }
  }

  async function start() {
    if (busy || earlier.length || !currentPreview || attempts.length) return;
    setBusy(true);
    setNotice(undefined);
    try {
      for (const preview of currentPreview) {
        const item: RunStartRequest = {
          reviewId: preview.reviewId,
          provider: preview.provider,
          sourceSha: preview.sourceSha,
          expectedSettingsRevision: preview.settingsRevision,
          invocation: preview.invocation,
          model: preview.model,
          effort: preview.effort,
          context: preview.context,
          submissionId: crypto.randomUUID(),
        };
        const reference: PendingLaunch = {
          reviewId: item.reviewId,
          submissionId: item.submissionId,
          title:
            targets.find((target) => target.reviewId === item.reviewId)?.title ?? item.reviewId,
        };
        if (!rememberPendingLaunch(reference)) {
          setNotice("A launch for this review still needs to be reconciled.");
          continue;
        }
        try {
          const receipt = await startRun(item);
          settleLaunchReceipt(receipt);
          setAttempts((known) => [...known, { request: item, receipt }]);
        } catch (error) {
          const refused = settleLaunchRefusal(reference, error);
          setAttempts((known) => [
            ...known,
            { request: item, error: describeError(error), refused },
          ]);
        }
      }
    } finally {
      setBusy(false);
    }
  }

  async function reconcile(attempt: Attempt) {
    if (busy) return;
    setBusy(true);
    const controller = new AbortController();
    request.current = controller;
    let updated: Attempt;
    try {
      const receipt = await launchReceipt(
        attempt.request.reviewId,
        attempt.request.submissionId,
        controller.signal,
      );
      settleLaunchReceipt(receipt);
      updated = { request: attempt.request, receipt };
    } catch (error) {
      updated = {
        request: attempt.request,
        error:
          error instanceof ApiResponseError && error.code === "not_found"
            ? "The service has no saved receipt for this submission yet."
            : describeError(error),
      };
    } finally {
      setBusy(false);
    }
    if (!controller.signal.aborted)
      setAttempts((current) =>
        current.map((item) =>
          item.request.submissionId === attempt.request.submissionId ? updated : item,
        ),
      );
  }

  async function registerSelected() {
    if (!active || !nativeId.trim() || busy) return;
    setBusy(true);
    setNotice(undefined);
    try {
      const result = await registerRun({
        reviewId: active.reviewId,
        provider,
        nativeId: nativeId.trim(),
      });
      onOpen(result.reviewId, result.runId);
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title={register ? "Register run" : targets.length > 1 ? "Prepare reviews" : "Review MR"}
      onClose={onClose}
      busy={busy}
      footer={
        <>
          <button type="button" disabled={busy} onClick={onClose}>
            {attempts.length ? "Done" : "Cancel"}
          </button>
          {register ? (
            <button
              type="button"
              className="primary"
              disabled={busy || !active || !nativeId.trim() || !advanced}
              onClick={() => void registerSelected()}
            >
              Register this run
            </button>
          ) : (
            !attempts.length &&
            (currentPreview ? (
              <button
                type="button"
                className="primary"
                disabled={busy || earlier.length > 0}
                onClick={() => void start()}
              >
                Start {targets.length === 1 ? "review" : `${targets.length} reviews`}
              </button>
            ) : (
              <button
                type="button"
                className="primary"
                disabled={
                  busy ||
                  earlier.length > 0 ||
                  !active ||
                  !catalogs.data?.length ||
                  (invocation === "skill" ? !selectedSkill : !prompt.trim())
                }
                onClick={() => void prepare()}
              >
                Preview {targets.length === 1 ? "review" : "reviews"}
              </button>
            ))
          )}
        </>
      }
    >
      <div className="stack">
        {notice && <Notice>{notice}</Notice>}
        {!register &&
          earlier.map((item) => (
            <LaunchReceiptCard key={item.submissionId} item={item} onOpen={onOpen} />
          ))}
        {register && (
          <div className="surface-card stack">
            <h3>Link from the native session</h3>
            <p>
              In Claude, use <code>/reviewq {active?.tickets[0]?.key ?? "<ticket or MR URL>"}</code>
              . In Codex, use{" "}
              <code>$reviewq {active?.tickets[0]?.key ?? "<ticket or MR URL>"}</code>.
            </p>
            <button type="button" onClick={() => setAdvanced(!advanced)} aria-expanded={advanced}>
              Advanced exact-session registration
            </button>
          </div>
        )}
        {(!register || advanced) && (
          <>
            {targets.length ? (
              <div className="surface-card stack">
                <div className="spread">
                  <h3>{targets.length === 1 ? "Target merge request" : "Target merge requests"}</h3>
                  {mode !== "prepare" && !attempts.length && (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => {
                        setTargets([]);
                        clearPreview();
                      }}
                    >
                      Choose another MR
                    </button>
                  )}
                </div>
                {targets.map((target) => (
                  <div key={target.reviewId}>
                    <span className="ticket-key">{target.tickets[0]?.key}</span>{" "}
                    <code>
                      {target.projectPath}!{target.iid} · {target.sourceSha.slice(0, 12)}
                    </code>
                    <div>{target.title}</div>
                  </div>
                ))}
              </div>
            ) : (
              <>
                <label className="search-field">
                  <Icon name="search" />
                  <input
                    aria-label="Find a merge request"
                    placeholder="Search merge requests"
                    value={search}
                    maxLength={200}
                    onChange={(event) => setSearch(event.currentTarget.value)}
                  />
                </label>
                {pick.pending && <Loading label="Finding merge requests…" />}
                {pick.error && <Notice retry={pick.retry}>{pick.error}</Notice>}
                <div className="stack">
                  {pick.data?.items.map(
                    (item) =>
                      item.kind === "mr" &&
                      item.mrState === "open" && (
                        <button
                          type="button"
                          key={item.reviewId}
                          onClick={() => selectTarget(item)}
                        >
                          <strong>
                            {item.tickets[0]?.key ?? item.projectPath} · !{item.iid}
                          </strong>{" "}
                          {item.title}
                        </button>
                      ),
                  )}
                </div>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => setShowTrack(!showTrack)}
                  aria-expanded={showTrack}
                >
                  Track MR by URL
                </button>
                {showTrack && (
                  <form
                    className="actions"
                    onSubmit={(event) => {
                      event.preventDefault();
                      void track();
                    }}
                  >
                    <input
                      aria-label="GitLab merge request URL"
                      placeholder="https://gitlab.example.com/team/repo/-/merge_requests/42"
                      type="url"
                      value={url}
                      onChange={(event) => setUrl(event.currentTarget.value)}
                    />
                    <button type="submit" disabled={busy || !pick.data || !url.trim()}>
                      Track merge request
                    </button>
                  </form>
                )}
              </>
            )}
            {active && (
              <label className="field">
                Provider
                <select
                  value={provider}
                  disabled={busy || attempts.length > 0}
                  onChange={(event) => {
                    setProvider(event.currentTarget.value === "claude" ? "claude" : "codex");
                    setSkill("");
                    setModel("");
                    setEffort("");
                    clearPreview();
                  }}
                >
                  <option value="codex">Codex</option>
                  <option value="claude">Claude</option>
                </select>
              </label>
            )}
          </>
        )}
        {register && advanced && active && (
          <label className="field">
            Exact native session ID
            <input
              value={nativeId}
              maxLength={200}
              onChange={(event) => setNativeId(event.currentTarget.value)}
            />
          </label>
        )}
        {!register && active && !attempts.length && (
          <>
            {catalogs.pending && (
              <Loading label="Reading the configured checkout and provider catalog…" />
            )}
            {catalogs.error && (
              <Notice retry={catalogs.retry}>
                {catalogs.error}{" "}
                <button type="button" onClick={onSettings}>
                  Repositories & agents
                </button>
              </Notice>
            )}
            {Boolean(catalogs.data?.length) && (
              <fieldset className="stack" disabled={busy}>
                <legend>Review instructions</legend>
                <div className="fields">
                  <label className="field">
                    Invocation
                    <select
                      value={invocation}
                      onChange={(event) => {
                        setInvocation(event.currentTarget.value === "custom" ? "custom" : "skill");
                        clearPreview();
                      }}
                    >
                      <option value="skill">Skill</option>
                      <option value="custom">Custom prompt</option>
                    </select>
                  </label>
                  <label className="field">
                    Model
                    <select
                      value={model}
                      onChange={(event) => {
                        setModel(event.currentTarget.value);
                        setEffort("");
                        clearPreview();
                      }}
                    >
                      <option value="">Provider default</option>
                      {models.map((item) => (
                        <option key={item.id} value={item.id}>
                          {item.label}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label className="field">
                    Effort
                    <select
                      value={effort}
                      onChange={(event) => {
                        setEffort(event.currentTarget.value);
                        clearPreview();
                      }}
                    >
                      <option value="">Provider default</option>
                      {efforts.map((value) => (
                        <option key={value}>{value}</option>
                      ))}
                    </select>
                  </label>
                </div>
                {invocation === "skill" ? (
                  <>
                    {skills.length ? (
                      <label className="field">
                        Skill
                        <select
                          value={selectedSkill}
                          onChange={(event) => {
                            setSkill(event.currentTarget.value);
                            clearPreview();
                          }}
                        >
                          <option value="">Choose a skill</option>
                          {skills.map((name) => (
                            <option key={name}>{name}</option>
                          ))}
                        </select>
                      </label>
                    ) : (
                      <Empty title="No shared skills in these checkouts">
                        <button
                          type="button"
                          onClick={() => {
                            setInvocation("custom");
                            clearPreview();
                          }}
                        >
                          Write a custom prompt
                        </button>
                      </Empty>
                    )}
                    <label className="field">
                      Skill arguments
                      <input
                        value={args}
                        onChange={(event) => {
                          setArgs(event.currentTarget.value);
                          clearPreview();
                        }}
                        placeholder="Use {ticket} for the current ticket key"
                      />
                    </label>
                  </>
                ) : (
                  <label className="field">
                    Prompt
                    <textarea
                      rows={5}
                      value={prompt}
                      onChange={(event) => {
                        setPrompt(event.currentTarget.value);
                        clearPreview();
                      }}
                    />
                  </label>
                )}
                <label className="field">
                  Additional context
                  <textarea
                    rows={3}
                    value={context}
                    onChange={(event) => {
                      setContext(event.currentTarget.value);
                      clearPreview();
                    }}
                  />
                </label>
              </fieldset>
            )}
          </>
        )}
        {preview && !currentPreview && (
          <Notice>Settings changed. Prepare a new preview before starting.</Notice>
        )}
        {currentPreview?.map((item) => (
          <details className="surface-card" open key={item.reviewId}>
            <summary>
              <strong>{targets.find((target) => target.reviewId === item.reviewId)?.title}</strong>{" "}
              · {item.sourceSha.slice(0, 12)}
            </summary>
            <div className="stack">
              <p className="meta">
                <code>{item.checkout}</code> · {readable(item.provider)}
                {item.model && ` · ${item.model}`}
                {item.effort && ` · ${item.effort}`}
              </p>
              <pre className="launch-preview">{item.prompt}</pre>
            </div>
          </details>
        ))}
        {attempts.map((attempt) => {
          const target = targets.find((item) => item.reviewId === attempt.request.reviewId);
          const run = attempt.receipt?.state === "confirmed" ? attempt.receipt.runId : null;
          return (
            <div className="surface-card stack" key={attempt.request.submissionId}>
              <strong>{target?.title ?? attempt.request.reviewId}</strong>
              <span className="badge">
                {run
                  ? "Started"
                  : attempt.refused || attempt.receipt?.state === "rejected"
                    ? "Rejected"
                    : "Start unconfirmed"}
              </span>
              {attempt.error && <Notice>{attempt.error}</Notice>}
              {attempt.receipt?.nativeId && <code>{attempt.receipt.nativeId}</code>}
              {run ? (
                <button type="button" onClick={() => onOpen(attempt.request.reviewId, run)}>
                  Open run
                </button>
              ) : (
                !attempt.refused &&
                attempt.receipt?.state !== "rejected" && (
                  <button type="button" disabled={busy} onClick={() => void reconcile(attempt)}>
                    Check saved launch receipt
                  </button>
                )
              )}
            </div>
          );
        })}
      </div>
    </Modal>
  );
}

function sharedSkills(catalogs: readonly Catalog[]) {
  const first = catalogs[0];
  if (!first) return [];
  return [...new Set(first.skills.map((skill) => skill.name))].filter((name) =>
    catalogs.every((catalog) => catalog.skills.filter((skill) => skill.name === name).length === 1),
  );
}

function expandTicket(text: string, target: LaunchTarget) {
  return target.tickets[0] ? text.replaceAll("{ticket}", target.tickets[0].key) : text;
}
