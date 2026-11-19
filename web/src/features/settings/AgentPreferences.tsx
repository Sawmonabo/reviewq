import { useRef, useState } from "react";
import {
  readFollowupPreference,
  readRecoveryPreference,
  saveFollowupPreference,
  saveRecoveryPreference,
} from "../../api/settings";
import type { FollowupPreferencesResponsePreferences } from "../../api/generated/contracts/followups";
import type { Preferences } from "../../api/generated/contracts/recovery";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError } from "../../lib/presentation";

export function AgentPreferences() {
  const recovery = useResource("recovery-preference", readRecoveryPreference, [
    { kind: "settings" },
  ]);
  const followups = useResource("followup-preference", readFollowupPreference, [
    { kind: "followup" },
  ]);
  const [savedRecovery, setSavedRecovery] = useState<Preferences>();
  const [savedFollowups, setSavedFollowups] = useState<FollowupPreferencesResponsePreferences>();
  const [concurrency, setConcurrency] = useState<string>();
  const [busy, setBusy] = useState(false);
  const pending = useRef(false);
  const [notice, setNotice] = useState<string>();
  const resume = latest(recovery.data, savedRecovery);
  const followup = latest(followups.data, savedFollowups);
  const count = Number(concurrency ?? followup?.autoReviewConcurrency);

  async function save(
    change: { recovery: boolean } | { automatic: boolean } | { autoReviewConcurrency: number },
  ) {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setNotice(undefined);
    try {
      if ("recovery" in change) {
        if (!resume) return;
        const { data } = await saveRecoveryPreference({
          expectedRevision: resume.revision,
          autoResumeCodexReviews: change.recovery,
        });
        setSavedRecovery(data);
        recovery.retry();
      } else if (followup) {
        const { data } = await saveFollowupPreference({
          ...change,
          expectedRevision: followup.revision,
          submissionId: crypto.randomUUID(),
        });
        setSavedFollowups(data);
        if ("autoReviewConcurrency" in change) setConcurrency(undefined);
        followups.retry();
      }
    } catch (error) {
      setNotice(describeError(error));
      recovery.retry();
      followups.retry();
    } finally {
      pending.current = false;
      setBusy(false);
    }
  }

  return (
    <section
      className="settings-section agent-preferences"
      aria-labelledby="settings-agents"
      aria-busy={recovery.pending || followups.pending}
    >
      <h3 id="settings-agents">Agents</h3>
      {notice && <Notice>{notice}</Notice>}
      {recovery.error && <Notice retry={recovery.retry}>{recovery.error}</Notice>}
      {followups.error && <Notice retry={followups.retry}>{followups.error}</Notice>}
      {recovery.pending && followups.pending && <Loading label="Reading agent preferences…" />}
      {resume && (
        <div className="settings-agent-choice">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={resume.autoResumeCodexReviews}
              disabled={busy}
              onChange={(event) => void save({ recovery: event.currentTarget.checked })}
            />
            Automatically resume interrupted reviews
          </label>
          <p className="meta">
            Continue registered reviews after temporary usage or capacity interruptions.
          </p>
        </div>
      )}
      {followup && (
        <>
          <div className="settings-agent-choice">
            <label className="settings-check">
              <input
                type="checkbox"
                checked={followup.automatic}
                disabled={busy}
                onChange={(event) => void save({ automatic: event.currentTarget.checked })}
              />
              Automatically review replies to my review comments
            </label>
            <p className="meta">
              Monitor your published threads until you approve the MR. Disable individual MRs from
              their actions menu.
            </p>
          </div>
          <div className="settings-concurrency">
            <label className="field" htmlFor="maximum-follow-ups">
              Maximum simultaneous follow-ups
            </label>
            <div className="actions">
              <input
                id="maximum-follow-ups"
                type="number"
                min={1}
                max={8}
                step={1}
                value={concurrency ?? followup.autoReviewConcurrency}
                onChange={(event) => setConcurrency(event.currentTarget.value)}
              />
              <button
                type="button"
                disabled={
                  busy ||
                  !Number.isInteger(count) ||
                  count < 1 ||
                  count > 8 ||
                  count === followup.autoReviewConcurrency
                }
                onClick={() => void save({ autoReviewConcurrency: count })}
              >
                Save
              </button>
            </div>
            <p className="meta">
              More slots start queued follow-ups. Fewer slots let active reviews finish.
            </p>
          </div>
        </>
      )}
    </section>
  );
}

function latest<T extends { readonly revision: string }>(
  snapshot: T | undefined,
  saved: T | undefined,
) {
  return saved && (!snapshot || BigInt(saved.revision) > BigInt(snapshot.revision))
    ? saved
    : snapshot;
}
