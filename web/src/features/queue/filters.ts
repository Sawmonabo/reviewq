import type { SprintPair } from "../../api/generated/contracts";

export const ownerships = {
  others: "Other authors",
  mine: "My MRs",
  all: "All ownership",
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
  recent: "Most recent",
  oldest: "Oldest update",
  author: "Author",
  ticket: "Ticket key",
} as const;

export type QueueFilters = {
  readonly boardId: string;
  readonly projectId: string;
  readonly sprint: string;
  readonly ownership: keyof typeof ownerships;
  readonly authorId: string;
  readonly state: keyof typeof states;
  readonly sort: keyof typeof sorts;
  readonly q: string;
  readonly hideReviewed: boolean;
  readonly hideInProgress: boolean;
};

export const defaultFilters: QueueFilters = {
  boardId: "",
  projectId: "",
  sprint: "",
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
  return {
    boardId: numeric(input.boardId),
    projectId: numeric(input.projectId),
    authorId: numeric(input.authorId),
    sprint:
      typeof input.sprint === "string" && /^(any|active|none|\d+:\d+)$/.test(input.sprint)
        ? input.sprint
        : "",
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
  const { sprint, boardId, projectId, authorId, ...rest } = filters;
  const pair = sprint.includes(":");
  return {
    ...rest,
    cursor,
    boardId: boardId || undefined,
    projectId: projectId || undefined,
    authorId: authorId || undefined,
    sprint: (pair ? "selected" : sprint) || undefined,
    sprintPair: pair ? sprint : undefined,
  };
}

export function parseSprint(value: string): SprintPair | undefined {
  const [boardId, sprintId] = value.split(":");
  if (boardId && sprintId) return { boardId, sprintId };
}

function numeric(value: unknown): string {
  return typeof value === "string" && /^\d{1,19}$/.test(value) ? value : "";
}
