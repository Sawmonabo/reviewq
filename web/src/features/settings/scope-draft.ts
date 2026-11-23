import type {
  CatalogRequest,
  Provider,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
  SettingsDraft,
} from "../../api/generated/contracts/settings";

export type CatalogConnection = Omit<
  CatalogRequest,
  "kind" | "boardId" | "cursor" | "q" | "view" | "states"
>;

export type ScopeOrigins = Record<Provider, string | undefined>;

export function sourceOrigin(value: string | null) {
  try {
    const url = new URL(value?.trim() ?? "");
    if ((url.protocol === "https:" || url.protocol === "http:") && url.href === `${url.origin}/`) {
      return url.origin;
    }
  } catch {
    // Connection inputs can be incomplete while editing.
  }
  return undefined;
}

export function draftOrigins(draft: SettingsDraft): ScopeOrigins {
  return { gitlab: sourceOrigin(draft.gitlab.origin), jira: sourceOrigin(draft.jira.site) };
}

export function clearProviderScope(draft: SettingsDraft, provider: Provider): SettingsDraft {
  if (provider === "jira") return { ...draft, boards: [] };
  return {
    ...draft,
    repositories: [],
    additionalReviewerIds: [],
    boards: draft.boards.map((board) => ({ ...board, projectIds: [] })),
  };
}

export function boardIncludesRepository(
  board: SaveSettingsBoardScope,
  repo: SaveSettingsRepositoryBinding,
) {
  return repo.boardSelection === "any" || board.projectIds.includes(repo.projectId);
}

export function toggleMembership(values: readonly string[], value: string, include: boolean) {
  return include ? [...new Set([...values, value])] : values.filter((entry) => entry !== value);
}

export function settingsFocus(id: string) {
  requestAnimationFrame(() => {
    const element = document.getElementById(id);
    if (!element) return;
    const details = element.closest("details");
    if (details) details.open = true;
    element.focus({ preventScroll: true });
    const scroller = element.closest<HTMLElement>(".modal-body");
    if (scroller)
      scroller.scrollTop +=
        element.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 14;
  });
}
