import type { AvailableSkill, Catalog } from "../../api/generated/contracts/launch";

type Choice = {
  readonly id: string;
  readonly label: string;
  readonly sources: ReadonlyMap<string, AvailableSkill>;
};

export function launchSkills(catalogs: readonly Catalog[]): Choice[] {
  const first = catalogs[0];
  if (!first) return [];
  const byCatalog = catalogs.map((catalog) => {
    const byName = new Map<string, Map<string, AvailableSkill>>();
    for (const skill of catalog.skills) {
      const paths = byName.get(skill.name) ?? new Map<string, AvailableSkill>();
      paths.set(skill.path, skill);
      byName.set(skill.name, paths);
    }
    return { reviewId: catalog.reviewId, byName };
  });
  const choices = new Map<string, Choice>();
  for (const skill of first.skills) {
    const id = JSON.stringify([skill.name, skill.path]);
    if (choices.has(id)) continue;
    const matches = byCatalog.map(({ reviewId, byName }) => ({
      reviewId,
      candidates: byName.get(skill.name),
    }));
    const exact = matches.every(({ candidates }) => candidates?.has(skill.path));
    if (!exact && !matches.every(({ candidates }) => candidates?.size === 1)) continue;
    const sources = new Map<string, AvailableSkill>();
    for (const { reviewId, candidates } of matches) {
      const matched = exact ? candidates?.get(skill.path) : candidates?.values().next().value;
      if (matched) sources.set(reviewId, matched);
    }
    const sameName = byCatalog[0]?.byName.get(skill.name);
    choices.set(id, {
      id,
      label: (sameName?.size ?? 0) > 1 ? `${skill.name} · ${skill.path}` : skill.name,
      sources,
    });
  }
  return [...choices.values()];
}

export function defaultLaunchSkill(choices: readonly Choice[], catalogs: readonly Catalog[]) {
  return choices.find(({ sources }) =>
    catalogs.every((catalog) => {
      const selected = sources.get(catalog.reviewId);
      return (
        selected &&
        selected.name === catalog.defaultSkill?.name &&
        selected.path === catalog.defaultSkill.path
      );
    }),
  );
}
