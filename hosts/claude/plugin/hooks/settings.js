export async function currentSettings($, state) {
  const { model: stepModel, effort: stepEffort } = state.turn;
  if (!stepModel || !stepEffort) return undefined;
  const model = await $.session.model();
  const selected = await $.env.get("CLAUDE_CODE_EFFORT_LEVEL")
    ?? await $.settings.read({ key: "effortLevel" });
  if (model !== stepModel || selected !== stepEffort) return undefined;
  return { model, effort: selected };
}
