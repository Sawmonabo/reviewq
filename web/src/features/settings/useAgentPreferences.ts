import { useState } from "react";
import {
  readFollowupPreference,
  readRecoveryPreference,
  saveFollowupPreference,
  saveRecoveryPreference,
} from "../../api/settings";
import type { FollowupPreferencesResponsePreferences } from "../../api/generated/contracts/followups";
import type { Preferences } from "../../api/generated/contracts/recovery";
import { useResource } from "../../hooks/useResource";

type AutomationDraft = {
  readonly recovery?: boolean;
  readonly automatic?: boolean;
  readonly autoReviewConcurrency?: number;
};

export function useAgentPreferences() {
  const recovery = useResource("recovery-preference", readRecoveryPreference, [
    { kind: "settings" },
  ]);
  const followups = useResource("followup-preference", readFollowupPreference, [
    { kind: "followup" },
  ]);
  const [savedRecovery, setSavedRecovery] = useState<Preferences>();
  const [savedFollowups, setSavedFollowups] = useState<FollowupPreferencesResponsePreferences>();
  const [draft, setDraft] = useState<AutomationDraft>({});
  const resume = latest(recovery.data, savedRecovery);
  const followup = latest(followups.data, savedFollowups);
  const recoveryChanged =
    draft.recovery !== undefined && draft.recovery !== resume?.autoResumeCodexReviews;
  const followupChanged =
    (draft.automatic !== undefined && draft.automatic !== followup?.automatic) ||
    (draft.autoReviewConcurrency !== undefined &&
      draft.autoReviewConcurrency !== followup?.autoReviewConcurrency);

  async function save() {
    try {
      if (resume && draft.recovery !== undefined && recoveryChanged) {
        const { data } = await saveRecoveryPreference({
          expectedRevision: resume.revision,
          autoResumeCodexReviews: draft.recovery,
        });
        setSavedRecovery(data);
        setDraft((current) => {
          const remaining = { ...current };
          delete remaining.recovery;
          return remaining;
        });
      }
      if (followup && followupChanged) {
        const { data } = await saveFollowupPreference({
          expectedRevision: followup.revision,
          submissionId: crypto.randomUUID(),
          ...(draft.automatic !== undefined ? { automatic: draft.automatic } : {}),
          ...(draft.autoReviewConcurrency !== undefined
            ? { autoReviewConcurrency: draft.autoReviewConcurrency }
            : {}),
        });
        setSavedFollowups(data);
      }
      setDraft({});
    } finally {
      recovery.retry();
      followups.retry();
    }
  }

  return {
    recovery,
    followups,
    automatic: draft.automatic ?? followup?.automatic,
    autoReviewConcurrency: draft.autoReviewConcurrency ?? followup?.autoReviewConcurrency,
    resume: draft.recovery ?? resume?.autoResumeCodexReviews,
    dirty: recoveryChanged || followupChanged,
    update: (change: AutomationDraft) => setDraft((current) => ({ ...current, ...change })),
    save,
  };
}

function latest<T extends { readonly revision: string }>(
  snapshot: T | undefined,
  saved: T | undefined,
) {
  return saved && (!snapshot || BigInt(saved.revision) > BigInt(snapshot.revision))
    ? saved
    : snapshot;
}
