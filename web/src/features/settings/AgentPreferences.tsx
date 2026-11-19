import { useState } from "react";
import {
  readFollowupPreference,
  readRecoveryPreference,
  saveFollowupPreference,
  saveRecoveryPreference,
} from "../../api/settings";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError } from "../../lib/presentation";

export function AgentPreferences() {
  const recovery = useResource("recovery-preference", readRecoveryPreference, [
    { kind: "settings" },
  ]);
  const followup = useResource("followup-preference", readFollowupPreference, [
    { kind: "followup" },
  ]);
  const [resume, setResume] = useState<boolean>();
  const [automatic, setAutomatic] = useState<boolean>();
  const [concurrency, setConcurrency] = useState<string>();
  const [busy, setBusy] = useState("");
  const [notice, setNotice] = useState<string>();
  const recovering = resume ?? recovery.data?.autoResumeCodexReviews ?? false;
  const scheduling = automatic ?? followup.data?.automatic ?? false;
  const count = Number(concurrency ?? followup.data?.autoReviewConcurrency);

  async function save(kind: "recovery" | "automatic" | "concurrency") {
    if (busy) return;
    setBusy(kind);
    setNotice(undefined);
    try {
      if (kind === "recovery" && recovery.data) {
        const { data } = await saveRecoveryPreference({
          expectedRevision: recovery.data.revision,
          autoResumeCodexReviews: recovering,
        });
        setResume(data.autoResumeCodexReviews);
        recovery.retry();
        setNotice(
          data.autoResumeCodexReviews
            ? "Automatic Codex recovery enabled."
            : "Automatic Codex recovery disabled.",
        );
      } else if (followup.data) {
        const { data } = await saveFollowupPreference({
          expectedRevision: followup.data.revision,
          submissionId: crypto.randomUUID(),
          ...(kind === "automatic" ? { automatic: scheduling } : { autoReviewConcurrency: count }),
        });
        if (kind === "automatic") setAutomatic(data.automatic);
        else setConcurrency(String(data.autoReviewConcurrency));
        followup.retry();
        setNotice(
          kind === "automatic"
            ? `Automatic follow-ups ${data.automatic ? "enabled" : "disabled"}.`
            : `Saved maximum of ${data.autoReviewConcurrency} concurrent follow-ups.`,
        );
      }
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      setBusy("");
    }
  }

  return (
    <section className="settings-section stack" aria-labelledby="settings-agents">
      <h3 id="settings-agents">Agents</h3>
      {notice && <Notice>{notice}</Notice>}
      {recovery.error && <Notice retry={recovery.retry}>{recovery.error}</Notice>}
      {followup.error && <Notice retry={followup.retry}>{followup.error}</Notice>}
      {recovery.pending && followup.pending && <Loading label="Reading agent preferences…" />}
      {recovery.data && (
        <div className="surface-card stack">
          <div className="spread">
            <label>
              <input
                type="checkbox"
                checked={recovering}
                disabled={Boolean(busy)}
                onChange={(event) => setResume(event.currentTarget.checked)}
              />{" "}
              Automatically resume interrupted reviews
            </label>
            <button
              type="button"
              disabled={Boolean(busy) || recovering === recovery.data.autoResumeCodexReviews}
              onClick={() => void save("recovery")}
            >
              Save recovery
            </button>
          </div>
          <p className="meta">
            Resumes eligible Codex reviews. Turning this off stops eligible queued recoveries.
          </p>
        </div>
      )}
      {followup.data && (
        <div className="surface-card stack">
          <div className="spread">
            <label>
              <input
                type="checkbox"
                checked={scheduling}
                disabled={Boolean(busy)}
                onChange={(event) => setAutomatic(event.currentTarget.checked)}
              />{" "}
              Automatically review replies to my review comments
            </label>
            <button
              type="button"
              disabled={Boolean(busy) || scheduling === followup.data.automatic}
              onClick={() => void save("automatic")}
            >
              Save follow-ups
            </button>
          </div>
          <div className="spread">
            <label className="field">
              Concurrent follow-ups (1–8)
              <input
                type="number"
                min={1}
                max={8}
                step={1}
                value={concurrency ?? followup.data.autoReviewConcurrency}
                onChange={(event) => setConcurrency(event.currentTarget.value)}
              />
            </label>
            <button
              type="button"
              disabled={
                Boolean(busy) ||
                !Number.isInteger(count) ||
                count < 1 ||
                count > 8 ||
                count === followup.data.autoReviewConcurrency
              }
              onClick={() => void save("concurrency")}
            >
              Save concurrency
            </button>
          </div>
          <p className="meta">
            The limit applies to newly started checks. Working reviews can finish.
          </p>
        </div>
      )}
    </section>
  );
}
