import type {
  CatalogRequest,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
} from "../../api/generated/contracts/settings";

export type CatalogConnection = Omit<
  CatalogRequest,
  "kind" | "boardId" | "cursor" | "q" | "view" | "states"
>;

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
