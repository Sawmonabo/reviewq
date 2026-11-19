import { useEffect, useRef, useState } from "react";
import type { CheckoutSkillsRequest, Skill } from "../../api/generated/contracts";
import { checkoutSkills } from "../../api/settings";
import { Notice } from "../../components/PanelState";
import { describeError } from "../../lib/presentation";

export function CheckoutSkill({
  body,
  selected,
  onSelect,
}: {
  readonly body: CheckoutSkillsRequest;
  readonly selected: Skill | null;
  readonly onSelect: (skill: Skill | null) => void;
}) {
  const [state, setState] = useState<{ skills?: readonly Skill[]; error?: string }>();
  const [pending, setPending] = useState(false);
  const request = useRef<AbortController | undefined>(undefined);
  const provider = body.provider === "claude" ? "Claude Code" : "Codex";
  const canBrowse = Boolean(body.checkout.trim());
  const skills = state?.skills ?? [];
  useEffect(() => () => request.current?.abort(), []);

  async function browse() {
    if (!canBrowse || pending) return;
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setPending(true);
    try {
      const { data } = await checkoutSkills(body, controller.signal);
      if (!controller.signal.aborted) setState({ skills: data.skills });
    } catch (error) {
      if (!controller.signal.aborted) setState({ error: describeError(error) });
    } finally {
      if (request.current === controller) setPending(false);
    }
  }

  return (
    <div className="settings-provider-skill stack">
      <div className="settings-inline">
        <h4>{provider}</h4>
        <button
          type="button"
          className="quiet-button"
          aria-label={`Find ${provider} skills`}
          disabled={!canBrowse || pending}
          onClick={() => void browse()}
        >
          {pending ? "Reading checkout…" : "Find skills"}
        </button>
      </div>
      <label className="field">
        Default skill
        <select
          aria-label={`${provider} default skill`}
          disabled={!canBrowse || pending}
          value={selected?.path ?? ""}
          onChange={(event) =>
            onSelect(skills.find((skill) => skill.path === event.currentTarget.value) ?? null)
          }
        >
          <option value="">Choose at launch</option>
          {selected && !skills.some((skill) => skill.path === selected.path) && (
            <option value={selected.path}>
              {selected.name}
              {state?.skills && " · Unavailable"}
            </option>
          )}
          {skills.map((skill) => (
            <option value={skill.path} key={skill.path}>
              {skill.name} · {skill.path}
            </option>
          ))}
        </select>
      </label>
      {state?.error && <Notice>{state.error}</Notice>}
    </div>
  );
}
