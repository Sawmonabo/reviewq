import { useEffect, useRef, useState } from "react";
import type { CheckoutSkillsRequest, Skill } from "../../api/generated/contracts";
import { checkoutSkills } from "../../api/settings";
import { Notice } from "../../components/PanelState";
import { describeError, readable } from "../../lib/presentation";

export function CheckoutSkill({ body, selected, onSelect }: {
  readonly body: CheckoutSkillsRequest;
  readonly selected: Skill | null;
  readonly onSelect: (skill: Skill | null) => void;
}) {
  const [state, setState] = useState<{ body: CheckoutSkillsRequest; skills?: readonly Skill[]; error?: string }>();
  const [pending, setPending] = useState(false);
  const request = useRef<AbortController | undefined>(undefined);
  const token = body.token.action === "replace" ? body.token.value : undefined;
  useEffect(() => () => request.current?.abort(), [token]);
  const previousToken = state?.body.token.action === "replace" ? state.body.token.value : undefined;
  const current = token === previousToken && state?.body.checkout === body.checkout ? state : undefined;

  async function browse() {
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setPending(true);
    try {
      const { data } = await checkoutSkills(body, controller.signal);
      if (!controller.signal.aborted) setState({ body, skills: data.skills });
    } catch (error) {
      if (!controller.signal.aborted) setState({ body, error: describeError(error) });
    } finally { if (request.current === controller) setPending(false); }
  }

  return <div className="stack">
    {current?.skills ? <label className="field">{readable(body.provider)} default skill
      <select value={selected?.path ?? ""} onChange={(event) => onSelect(current.skills?.find((skill) => skill.path === event.currentTarget.value) ?? null)}>
        <option value="">No default skill</option>
        {selected && !current.skills.some((skill) => skill.path === selected.path) && <option value={selected.path}>{selected.name} · Unavailable</option>}
        {current.skills.map((skill) => <option value={skill.path} key={skill.path}>{skill.name} · {skill.path}</option>)}
      </select>
    </label> : <p className="meta">{readable(body.provider)} default skill · {selected?.name ?? "No default selected"}</p>}
    {current?.error && <Notice>{current.error}</Notice>}
    <button type="button" disabled={pending} onClick={() => void browse()}>{pending ? "Reading checkout…" : `Browse ${readable(body.provider)} skills`}</button>
    {selected && !current?.skills && <button type="button" onClick={() => onSelect(null)}>Clear default skill</button>}
  </div>;
}
