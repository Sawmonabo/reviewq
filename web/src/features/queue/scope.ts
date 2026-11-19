import type { SettingsView } from "../../api/generated/contracts";
import type { QueueFilters } from "./filters";

export function resolveQueueScope(filters: QueueFilters, settings: SettingsView) {
  const boards = settings.boards.filter((board) => board.enabled);
  const boardIds = retained(filters.boardIds, new Set(boards.map((board) => board.boardId)));
  const chosen =
    boardIds === null ? boards : boards.filter((board) => boardIds.includes(board.boardId));
  const projects = new Set(chosen.flatMap((board) => board.projectIds));
  const repositories = settings.repositories.filter(
    (repository) =>
      !boards.length ||
      (chosen.length > 0 &&
        (repository.boardSelection === "any" || projects.has(repository.projectId))),
  );
  const projectIds = retained(
    filters.projectIds,
    new Set(repositories.map((repo) => repo.projectId)),
  );
  const scrum = chosen.filter((board) => board.type === "scrum");
  const scrumIds = new Set(scrum.map((board) => board.boardId));
  const sprintPairs = filters.sprintPairs.filter((pair) => scrumIds.has(pair.split(":")[0] ?? ""));
  const sprint =
    chosen.length > 0 && !scrum.length
      ? "any"
      : filters.sprint === "selected" && !sprintPairs.length
        ? "active"
        : filters.sprint;
  return {
    boards,
    repositories,
    scrum,
    filters: { ...filters, boardIds, projectIds, sprint, sprintPairs } satisfies QueueFilters,
  };
}

function retained(selected: readonly string[] | null, available: ReadonlySet<string>) {
  return selected === null ? null : selected.filter((id) => available.has(id));
}
