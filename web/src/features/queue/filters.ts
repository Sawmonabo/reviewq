import type { SprintPair } from "../../api/generated/contracts";

export const ownerships = {
  others: "Others’ MRs",
  mine: "My MRs",
  all: "All MRs",
} as const;
export const states = {
  needs_review: "Needs review",
  all_open: "All open",
  followup: "Needs re-review",
  waiting_author: "Waiting on author",
  reviewed: "Reviewed · awaiting merge",
  archive: "Archive",
} as const;
export const sorts = {
  recent: "Recently updated",
  oldest: "Oldest updated",
  author: "Author A–Z",
  ticket: "Ticket number",
} as const;

export type QueueFilters = {
  readonly boardIds: readonly string[] | null;
  readonly projectIds: readonly string[] | null;
  readonly sprint: "active" | "any" | "none" | "selected";
  readonly sprintPairs: readonly string[];
  readonly ownership: keyof typeof ownerships;
  readonly authorId: string;
  readonly state: keyof typeof states;
  readonly sort: keyof typeof sorts;
  readonly q: string;
  readonly hideReviewed: boolean;
  readonly hideInProgress: boolean;
};

const defaultFilters: QueueFilters = {
  boardIds: null,
  projectIds: null,
  sprint: "active",
  sprintPairs: [],
  ownership: "others",
  authorId: "",
  state: "needs_review",
  sort: "recent",
  q: "",
  hideReviewed: false,
  hideInProgress: false,
};
export const clearedFilters: QueueFilters = {
  ...defaultFilters,
  ownership: "all",
  state: "all_open",
};

export function readFilters(raw: unknown): QueueFilters {
  if (!raw || typeof raw !== "object") return defaultFilters;
  const input = raw as Record<string, unknown>;
  const { state, ownership, sort, hideInProgress, hideReviewed } = input;
  const sprintPairs = selection(input.sprintPairs, /^[1-9]\d{0,18}:[1-9]\d{0,18}$/, 32) ?? [];
  return {
    boardIds: selection(input.boardIds, /^[1-9]\d{0,18}$/, 128),
    projectIds: selection(input.projectIds, /^[1-9]\d{0,18}$/, 256),
    authorId: numeric(input.authorId),
    sprint:
      input.sprint === "any" ||
      input.sprint === "none" ||
      (input.sprint === "selected" && sprintPairs.length > 0)
        ? input.sprint
        : "active",
    sprintPairs,
    q: typeof input.q === "string" ? input.q.slice(0, 200) : "",
    state:
      typeof state === "string" && Object.hasOwn(states, state)
        ? (state as keyof typeof states)
        : defaultFilters.state,
    ownership:
      typeof ownership === "string" && Object.hasOwn(ownerships, ownership)
        ? (ownership as keyof typeof ownerships)
        : defaultFilters.ownership,
    sort:
      typeof sort === "string" && Object.hasOwn(sorts, sort)
        ? (sort as keyof typeof sorts)
        : defaultFilters.sort,
    hideInProgress: hideInProgress === true,
    hideReviewed: hideReviewed === true,
  };
}

export function queueParams(filters: QueueFilters, cursor: string | undefined) {
  const { sprint, boardIds, projectIds, sprintPairs, authorId, ...rest } = filters;
  return {
    ...rest,
    cursor,
    boardId: boardIds,
    projectId: projectIds,
    scope: boardIds?.length === 0 || projectIds?.length === 0 ? "none" : undefined,
    authorId: authorId || undefined,
    sprint,
    sprintPair: sprint === "selected" ? sprintPairs : undefined,
  };
}

export function parseSprint(value: string): SprintPair | undefined {
  const [boardId, sprintId] = value.split(":");
  if (boardId && sprintId) return { boardId, sprintId };
}

export function filtersActive(filters: QueueFilters) {
  return (
    filters.boardIds !== null ||
    filters.projectIds !== null ||
    filters.sprint !== "active" ||
    filters.ownership !== clearedFilters.ownership ||
    filters.authorId !== "" ||
    filters.state !== clearedFilters.state ||
    filters.sort !== clearedFilters.sort ||
    filters.q !== "" ||
    filters.hideInProgress ||
    filters.hideReviewed
  );
}

function selection(value: unknown, pattern: RegExp, limit: number): readonly string[] | null {
  if (!Array.isArray(value)) return null;
  return [
    ...new Set(value.filter((id): id is string => typeof id === "string" && pattern.test(id))),
  ].slice(0, limit);
}

function numeric(value: unknown): string {
  return typeof value === "string" && /^\d{1,19}$/.test(value) ? value : "";
}
