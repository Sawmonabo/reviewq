import { useEffect, useRef, useState } from "react";
import type { CheckoutSkillsRequest, Skill } from "../../api/generated/contracts";
import { checkoutSkills } from "../../api/settings";
import { Notice } from "../../components/PanelState";
import { describeError, readable } from "../../lib/presentation";

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
  useEffect(() => () => request.current?.abort(), []);

  async function browse() {
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
    <div className="stack">
      {state?.skills ? (
        <label className="field">
          {readable(body.provider)} default skill
          <select
            value={selected?.path ?? ""}
            onChange={(event) =>
              onSelect(
                state.skills?.find((skill) => skill.path === event.currentTarget.value) ?? null,
              )
            }
          >
            <option value="">No default skill</option>
            {selected && !state.skills.some((skill) => skill.path === selected.path) && (
              <option value={selected.path}>{selected.name} · Unavailable</option>
            )}
            {state.skills.map((skill) => (
              <option value={skill.path} key={skill.path}>
                {skill.name} · {skill.path}
              </option>
            ))}
          </select>
        </label>
      ) : (
        <p className="meta">
          {readable(body.provider)} default skill · {selected?.name ?? "No default selected"}
        </p>
      )}
      {state?.error && <Notice>{state.error}</Notice>}
      <button type="button" disabled={pending} onClick={() => void browse()}>
        {pending ? "Reading checkout…" : `Browse ${readable(body.provider)} skills`}
      </button>
      {selected && !state?.skills && (
        <button type="button" onClick={() => onSelect(null)}>
          Clear default skill
        </button>
      )}
    </div>
  );
}
