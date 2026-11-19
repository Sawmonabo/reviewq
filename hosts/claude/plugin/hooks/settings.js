export async function currentSettings($, state) {
  if (!state.model || !state.effort) return undefined;
  const model = await $.session.model();
  const selected = await $.env.get("CLAUDE_CODE_EFFORT_LEVEL")
    ?? await $.settings.read({ key: "effortLevel" });
  if (model !== state.model || selected !== state.effort) return undefined;
  return { model, effort: selected };
}
