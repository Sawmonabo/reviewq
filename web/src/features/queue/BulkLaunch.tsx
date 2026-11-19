import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { KeyboardEvent } from "react";
import { readOverview } from "../../api/client";
import type {
  HostProviderDto,
  ReviewOverview,
  RunPrepareRequest,
  RunStartRequest,
} from "../../api/generated/contracts";
import type { Preview, Receipt } from "../../api/generated/contracts/launch";
import { prepareRun, registerRun, runCatalog, startRun } from "../../api/launch";
import { readQueue, trackReview } from "../../api/workspace";
import { Modal } from "../../components/Modal";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, readable } from "../../lib/presentation";
import { submits } from "../../lib/submit-key";
import { LaunchReceiptCard, LaunchReceipts } from "./LaunchReceipts";
import { isMergeRequestUrl, LaunchTargetPicker } from "./LaunchTargetPicker";
import { defaultLaunchSkill, launchSkills } from "./launch-skills";
import { RegistrationGuide } from "./RegistrationGuide";
import type { PendingLaunch } from "./pending-launches";
import {
  pendingLaunches,
  rememberPendingLaunch,
  settleLaunchReceipt,
  settleLaunchRefusal,
  watchPendingLaunches,
} from "./pending-launches";
import type { LaunchMode, LaunchTarget } from "./types";
import "./launch.css";

type Attempt = {
  readonly request: RunStartRequest;
  readonly receipt?: Receipt;
  readonly error?: string;
  readonly refused?: boolean;
};
type Props = {
  readonly mode: LaunchMode;
  readonly targets: readonly LaunchTarget[];
  readonly openOverview: ReviewOverview | null | undefined;
  readonly settingsRevision: number;
  readonly onSettings: () => void;
  readonly onClose: () => void;
  readonly onOpen: (review: string, run: string) => void;
};

export default function BulkLaunch(props: Props) {
  if (props.mode === "receipts")
    return <LaunchReceipts onOpen={props.onOpen} onClose={props.onClose} />;
  const scope = JSON.stringify([props.mode, props.targets.map((target) => target.reviewId)]);
  return <LaunchEditor key={scope} {...props} />;
}

function LaunchEditor({
  mode,
  targets: initialTargets,
  openOverview,
  settingsRevision,
  onSettings,
  onClose,
  onOpen,
}: Props) {
  const [targets, setTargets] = useState(initialTargets);
  const [provider, setProvider] = useState<HostProviderDto>(
    mode === "register" ? "claude" : "codex",
  );
  const [skill, setSkill] = useState<string>();
  const [preferredInvocation, setInvocation] = useState<"skill" | "custom">();
  const [editedArgs, setArgs] = useState<string>();
  const [prompt, setPrompt] = useState("");
  const [context, setContext] = useState("");
  const [model, setModel] = useState("");
  const [effort, setEffort] = useState("");
  const [search, setSearch] = useState("");
  const [url, setUrl] = useState("");
  const [choosing, setChoosing] = useState(mode === "pick" && initialTargets.length === 0);
  const [advanced, setAdvanced] = useState(false);
  const [nativeId, setNativeId] = useState("");
  const registering = useRef(false);
  const [preview, setPreview] = useState<{ revision: number; items: readonly Preview[] }>();
  const [attempts, setAttempts] = useState<readonly Attempt[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string>();
  const pending = useSyncExternalStore(watchPendingLaunches, pendingLaunches);
  const [noReceipt, setNoReceipt] = useState<ReadonlySet<string>>(() => new Set());
  const [earlierResults, setEarlierResults] = useState<
    ReadonlyArray<{ item: PendingLaunch; receipt: Receipt }>
  >([]);
  const targetIds = new Set(targets.map((target) => target.reviewId));
  const earlier = pending.filter(
    (item) =>
      targetIds.has(item.reviewId) &&
      !attempts.some((attempt) => attempt.request.submissionId === item.submissionId),
  );
  const prior = earlierResults.filter((record) => targetIds.has(record.item.reviewId));
  const confirmedEarlier = prior.some(
    ({ receipt }) => receipt.state === "confirmed" && receipt.runId,
  );
  const blocked =
    confirmedEarlier || earlier.some((item) => item.request || !noReceipt.has(item.submissionId));

  const request = useRef<AbortController | undefined>(undefined);
  const register = mode === "register";
  const pickMode = choosing || register;
  const pick = useResource(
    `launch-picker:${settingsRevision}:${search}`,
    (signal) =>
      readQueue(
        { state: "all_open", ownership: "all", sprint: "any", q: search, limit: 30 },
        signal,
      ),
    [{ kind: "queue" }, { kind: "settings" }],
  );
  const choices = (pick.data?.items ?? []).flatMap((item) =>
    item.kind === "mr" && item.mrState === "open" ? [item] : [],
  );
  const active = targets[0] ?? (pickMode && !search ? choices[0] : undefined);
  const args =
    editedArgs ??
    (targets.length > 1
      ? "{ticket}"
      : (active?.tickets.map((ticket) => ticket.key).join(" ") ?? ""));
  const ticket = active?.tickets[0]?.key;
  const uniqueTicket =
    register &&
    ticket &&
    !search &&
    pick.data?.nextCursor === null &&
    pick.data.sync.mode === "connected" &&
    !pick.data.sync.stale &&
    !pick.data.sync.error &&
    choices.filter((choice) => choice.tickets.some(({ key }) => key === ticket)).length === 1 &&
    choices.some((choice) => choice.reviewId === active?.reviewId)
      ? ticket
      : undefined;
  const knownUrl =
    targets[0]?.webUrl ??
    (openOverview && openOverview.review.reviewId === active?.reviewId
      ? openOverview.webUrl
      : undefined);
  const missingUrlId = register && !uniqueTicket && !knownUrl ? active?.reviewId : undefined;
  const registrationUrl = useResource(`registration-url:${missingUrlId ?? ""}`, async (signal) => {
    if (!missingUrlId) return { data: undefined, cursor: "" };
    const { data, cursor } = await readOverview(missingUrlId, signal);
    return { data: data.webUrl, cursor };
  });
  const catalogs = useResource(
    `catalog:${settingsRevision}:${provider}:${JSON.stringify(targets.map((target) => target.reviewId))}:${pickMode}`,
    async (signal) => {
      if (!targets.length || pickMode) return { data: [], cursor: "" };
      const results = await Promise.all(
        targets.map((target) => runCatalog({ reviewId: target.reviewId, provider }, signal)),
      );
      return { data: results.map((result) => result.data), cursor: results[0]?.cursor ?? "" };
    },
    [{ kind: "settings" }],
  );
  useEffect(() => () => request.current?.abort(), []);
  const skills = launchSkills(catalogs.data ?? []);
  const defaultSkill = defaultLaunchSkill(skills, catalogs.data ?? []);
  const invocation = preferredInvocation ?? (defaultSkill ? "skill" : "custom");
  const selectedSkill =
    skill === undefined ? defaultSkill : skills.find((choice) => choice.id === skill);
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

  function submitFromField(event: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>) {
    if (event.defaultPrevented || !submits(event.nativeEvent)) return;
    event.preventDefault();
    if (currentPreview) void start();
    else void prepare();
  }

  function clearPreview() {
    setPreview(undefined);
    setNotice(undefined);
  }

  function selectTarget(target: LaunchTarget) {
    setTargets([target]);
    if (target.reviewId !== active?.reviewId) setNativeId("");
    setSkill(undefined);
    setModel("");
    setEffort("");
    clearPreview();
  }

  async function track() {
    if (!pick.data || !isMergeRequestUrl(url) || busy) return;
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
      selectTarget({ ...overview.data.review, webUrl: overview.data.webUrl });
      setUrl("");
      pick.retry();
    } catch (error) {
      if (!controller.signal.aborted) setNotice(describeError(error));
    } finally {
      setBusy(false);
    }
  }

  async function prepare() {
    if (
      busy ||
      blocked ||
      !catalogs.data?.length ||
      !targets.length ||
      (invocation === "skill" ? !selectedSkill : !prompt.trim())
    )
      return;
    setBusy(true);
    setNotice(undefined);
    const controller = new AbortController();
    request.current = controller;
    try {
      const source = await Promise.all(
        targets.map((target) => {
          const skillChoice = selectedSkill?.sources.get(target.reviewId);
          const resolvedPrompt = expandTicket(prompt, target);
          const resolvedContext = expandTicket(context, target);
          const selectedInvocation: RunPrepareRequest["invocation"] =
            invocation === "custom"
              ? { kind: "custom", prompt: resolvedPrompt }
              : skillChoice && {
                  kind: "skill",
                  name: skillChoice.name,
                  path: skillChoice.path,
                  args: expandTicket(args, target) || null,
                };
          if (!selectedInvocation) {
            throw new Error("The selected skill is unavailable in a target checkout.");
          }
          const body: RunPrepareRequest = {
            reviewId: target.reviewId,
            provider,
            context: resolvedContext || null,
            model: model || null,
            effort: effort || null,
            invocation: selectedInvocation,
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
    if (busy || blocked || !currentPreview || attempts.length) return;
    setBusy(true);
    setNotice(undefined);
    try {
      for (const preview of currentPreview) {
        const previous = earlier.find((item) => item.reviewId === preview.reviewId);
        if (previous && (previous.request || !noReceipt.has(previous.submissionId))) continue;
        const item: RunStartRequest = {
          reviewId: preview.reviewId,
          provider: preview.provider,
          sourceSha: preview.sourceSha,
          expectedSettingsRevision: preview.settingsRevision,
          invocation: preview.invocation,
          model: preview.model,
          effort: preview.effort,
          context: preview.context,
          submissionId: previous?.submissionId ?? crypto.randomUUID(),
        };
        const reference: PendingLaunch = {
          reviewId: item.reviewId,
          submissionId: item.submissionId,
          request: item,
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

  async function registerSelected() {
    if (!advanced || !active || !nativeId.trim() || busy || registering.current) return;
    registering.current = true;
    const controller = new AbortController();
    request.current = controller;
    const field =
      document.activeElement instanceof HTMLInputElement ? document.activeElement : null;
    setBusy(true);
    setNotice(undefined);
    try {
      const result = await registerRun({
        reviewId: active.reviewId,
        provider,
        nativeId: nativeId.trim(),
      });
      if (!controller.signal.aborted) onOpen(result.reviewId, result.runId);
    } catch (error) {
      if (controller.signal.aborted) return;
      setNotice(describeError(error));
      if (field)
        requestAnimationFrame(() => {
          if (field.isConnected && document.activeElement === document.body) {
            field.focus({ preventScroll: true });
          }
        });
    } finally {
      registering.current = false;
      setBusy(false);
    }
  }

  return (
    <Modal
      className="work-pane launch-pane"
      nonmodal
      title={
        register
          ? "Register review work"
          : choosing
            ? "New review run"
            : targets.length > 1
              ? `Launch ${targets.length} reviews`
              : "Launch MR session"
      }
      onClose={onClose}
      busy={busy}
      footer={
        !choosing && (
          <>
            <button type="button" disabled={busy} onClick={onClose}>
              {attempts.length ? "Done" : "Back"}
            </button>
            {register
              ? advanced &&
                nativeId.trim() && (
                  <button
                    type="button"
                    className="primary"
                    disabled={busy || !active || !nativeId.trim() || !advanced}
                    onClick={() => void registerSelected()}
                  >
                    {busy ? "Registering…" : "Register session"}
                  </button>
                )
              : !attempts.length &&
                (currentPreview ? (
                  <button
                    type="button"
                    className="primary"
                    disabled={busy || blocked}
                    onClick={() => void start()}
                  >
                    {earlier.length > 0
                      ? "Retry the prepared submission"
                      : `Start ${targets.length === 1 ? "review" : `${targets.length} reviews`}`}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="primary"
                    disabled={
                      busy ||
                      blocked ||
                      !active ||
                      !catalogs.data?.length ||
                      (invocation === "skill" ? !selectedSkill : !prompt.trim())
                    }
                    onClick={() => void prepare()}
                  >
                    Preview {targets.length === 1 ? "review" : "reviews"}
                  </button>
                ))}
          </>
        )
      }
    >
      <div className="launch-body stack">
        <div className="launch-heading">
          <h3>
            {register
              ? "Connect your review session."
              : choosing
                ? "Choose a merge request"
                : targets.length > 1
                  ? "Review selected MRs"
                  : "Use your existing workflow."}
          </h3>
          <p className="meta">
            {register
              ? "Run the command in the conversation working on this MR."
              : choosing
                ? "Then choose your agent, skill and review context."
                : targets.map((item) => item.tickets[0]?.key ?? `!${item.iid}`).join(" · ")}
          </p>
        </div>
        {notice && <Notice>{notice}</Notice>}
        {!register &&
          earlier.map((item) => (
            <LaunchReceiptCard
              key={item.submissionId}
              item={item}
              onOpen={onOpen}
              onNoReceipt={() => setNoReceipt((known) => new Set([...known, item.submissionId]))}
              onReceipt={(receipt) => {
                setNoReceipt(
                  (values) => new Set([...values].filter((id) => id !== item.submissionId)),
                );
                if (
                  receipt.state === "rejected" ||
                  (receipt.state === "confirmed" && receipt.runId)
                ) {
                  setEarlierResults((known) => [...known, { item, receipt }]);
                }
              }}
            />
          ))}
        {!register &&
          prior.map(({ item, receipt }) => (
            <LaunchReceiptCard
              key={item.submissionId}
              item={item}
              onOpen={onOpen}
              initialReceipt={receipt}
            />
          ))}
        {pickMode && (
          <LaunchTargetPicker
            choices={choices}
            active={active}
            search={search}
            pending={pick.pending}
            error={pick.error}
            retry={pick.retry}
            disabled={busy}
            url={url}
            onSearch={(value) => {
              setSearch(value);
              if (value) {
                setTargets([]);
                setNativeId("");
              }
              clearPreview();
            }}
            onSelect={selectTarget}
            {...(choosing
              ? {
                  onPrepare: () => {
                    if (!active) return;
                    setTargets([active]);
                    setChoosing(false);
                  },
                }
              : {})}
            onUrl={setUrl}
            onTrack={() => void track()}
          />
        )}
        {!choosing && (register || active) && (
          <label className="field">
            Agent provider
            <select
              value={provider}
              disabled={busy || attempts.length > 0}
              onChange={(event) => {
                setProvider(event.currentTarget.value === "claude" ? "claude" : "codex");
                setNativeId("");
                setSkill(undefined);
                setModel("");
                setEffort("");
                clearPreview();
              }}
            >
              <option value="codex">Codex</option>
              <option value="claude">Claude Code</option>
            </select>
          </label>
        )}
        {registrationUrl.error && (
          <Notice retry={registrationUrl.retry}>{registrationUrl.error}</Notice>
        )}
        {register && (
          <RegistrationGuide
            key={JSON.stringify([provider, active?.reviewId])}
            provider={provider}
            target={uniqueTicket ?? knownUrl ?? registrationUrl.data}
            selected={Boolean(active)}
            advanced={advanced}
            nativeId={nativeId}
            onAdvanced={setAdvanced}
            onNativeId={(value) => {
              if (!targets.length && active) setTargets([active]);
              setNativeId(value);
            }}
            onRegister={() => void registerSelected()}
            disabled={busy}
          />
        )}
        {!pickMode && targets.length > 0 && !attempts.length && (
          <div className="launch-target stack">
            {targets.map((target) => (
              <div key={target.reviewId}>
                <span className="ticket-key">{target.tickets[0]?.key}</span>
                <strong>{target.tickets[0]?.title ?? target.title}</strong>
                <small>
                  {catalogs.data?.find((catalog) => catalog.reviewId === target.reviewId)
                    ?.checkout ?? target.projectPath}{" "}
                  · <code>{target.sourceSha}</code>
                </small>
              </div>
            ))}
            {mode === "pick" && (
              <div className="actions">
                <button
                  type="button"
                  className="quiet-button"
                  disabled={busy}
                  onClick={() => {
                    setChoosing(true);
                    clearPreview();
                  }}
                >
                  Choose another MR
                </button>
              </div>
            )}
          </div>
        )}
        {!pickMode && active && !attempts.length && (
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
              <fieldset className="launch-instructions stack" disabled={busy}>
                <legend className="sr-only">Review instructions</legend>
                <div className="launch-modes" role="group" aria-label="Launch type">
                  <button
                    type="button"
                    aria-pressed={invocation === "skill"}
                    onClick={() => {
                      setInvocation("skill");
                      clearPreview();
                    }}
                  >
                    Existing skill
                  </button>
                  <button
                    type="button"
                    aria-pressed={invocation === "custom"}
                    onClick={() => {
                      setInvocation("custom");
                      clearPreview();
                    }}
                  >
                    Custom prompt
                  </button>
                </div>
                {invocation === "skill" ? (
                  <>
                    {skills.length ? (
                      <label className="field">
                        Skill
                        <select
                          value={selectedSkill?.id ?? ""}
                          onChange={(event) => {
                            setSkill(event.currentTarget.value);
                            clearPreview();
                          }}
                        >
                          <option value="">Choose a skill</option>
                          {skills.map((choice) => (
                            <option key={choice.id} value={choice.id}>
                              {choice.label}
                            </option>
                          ))}
                        </select>
                      </label>
                    ) : (
                      <Empty
                        title={
                          targets.length === 1
                            ? "No skills in this checkout"
                            : "No shared skills in these checkouts"
                        }
                      >
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
                    {selectedSkill && (
                      <details className="launch-disclosure launch-skill-source">
                        <summary>Selected skill source</summary>
                        <ul>
                          {targets.map((target) => {
                            const source = selectedSkill.sources.get(target.reviewId);
                            return (
                              <li key={target.reviewId}>
                                {targets.length > 1 && (
                                  <span>
                                    {target.projectPath} !{target.iid}
                                  </span>
                                )}
                                <code>{source?.path}</code>
                                {source?.description && <p>{source.description}</p>}
                              </li>
                            );
                          })}
                        </ul>
                      </details>
                    )}
                    <label className="field">
                      Skill arguments
                      <input
                        value={args}
                        onKeyDown={submitFromField}
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
                    Your prompt
                    <textarea
                      rows={6}
                      placeholder="Describe the work you want this agent to do…"
                      value={prompt}
                      onKeyDown={submitFromField}
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
                    rows={5}
                    placeholder="Add background, a specific concern, or instructions for this invocation…"
                    value={context}
                    onKeyDown={submitFromField}
                    onChange={(event) => {
                      setContext(event.currentTarget.value);
                      clearPreview();
                    }}
                  />
                </label>
                <div className="launch-models fields">
                  <label className="field">
                    Lead model
                    <select
                      value={model}
                      onChange={(event) => {
                        setModel(event.currentTarget.value);
                        setEffort("");
                        clearPreview();
                      }}
                    >
                      <option value="">Use CLI default</option>
                      {models.map((item) => (
                        <option key={item.id} value={item.id}>
                          {item.label}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label className="field">
                    Lead effort
                    <select
                      value={effort}
                      disabled={!efforts.length}
                      onChange={(event) => {
                        setEffort(event.currentTarget.value);
                        clearPreview();
                      }}
                    >
                      <option value="">Use CLI default</option>
                      {efforts.map((value) => (
                        <option key={value}>{value}</option>
                      ))}
                    </select>
                  </label>
                </div>{" "}
              </fieldset>
            )}
          </>
        )}
        {preview && !currentPreview && (
          <Notice>Settings changed. Prepare a new preview before starting.</Notice>
        )}
        {currentPreview?.map((item) => (
          <details className="launch-preview-panel" open key={item.reviewId}>
            <summary>
              <strong>{targets.find((target) => target.reviewId === item.reviewId)?.title}</strong>{" "}
              · <code>{item.sourceSha}</code>
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
          const item = {
            reviewId: attempt.request.reviewId,
            submissionId: attempt.request.submissionId,
            title: target?.title ?? attempt.request.reviewId,
            request: attempt.request,
          };
          return attempt.refused ? (
            <Notice key={item.submissionId}>
              {item.title} · {attempt.error}
            </Notice>
          ) : (
            <LaunchReceiptCard
              key={item.submissionId}
              item={item}
              onOpen={onOpen}
              initialReceipt={attempt.receipt}
              initialNotice={attempt.error}
            />
          );
        })}
      </div>
    </Modal>
  );
}

function expandTicket(text: string, target: LaunchTarget) {
  return target.tickets[0] ? text.replaceAll("{ticket}", target.tickets[0].key) : text;
}
