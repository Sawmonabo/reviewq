import type { CatalogItem } from "../../../api/generated/contracts/settings";
import { readable } from "../../../lib/presentation";

export const pickerText = {
  projects: {
    title: "Add repository",
    search: "Search GitLab repositories",
    placeholder: "Name, path, project URL or ID",
    singular: "repository",
    plural: "repositories",
    source: "GitLab project",
    tracked: "Tracked repositories",
    limit: 256,
  },
  boards: {
    title: "Add Jira board",
    search: "Search Jira boards",
    placeholder: "Board name, project key or board ID",
    singular: "board",
    plural: "boards",
    source: "Jira board",
    tracked: "Tracked boards",
    limit: 128,
  },
  reviewers: {
    title: "Add team reviewers",
    search: "Search GitLab users",
    placeholder: "Name, @username, user ID or public email",
    singular: "reviewer",
    plural: "reviewers",
    source: "GitLab user",
    tracked: "Team reviewers",
    limit: 256,
  },
} as const;

export type PickerKind = keyof typeof pickerText;

export type PickerChoice = {
  readonly id: string;
  readonly label: string;
  readonly detail?: string | undefined;
};

function reviewerDetail(item: CatalogItem) {
  return [item.reviewer && `@${item.reviewer.username}`, item.reviewer?.publicEmail]
    .filter(Boolean)
    .join(" · ");
}

export function catalogChoice(kind: PickerKind, item: CatalogItem): PickerChoice | undefined {
  switch (kind) {
    case "projects": {
      if (!item.path) return;
      const label =
        item.label === item.path ? (item.path.split("/").at(-1) ?? item.label) : item.label;
      return { id: item.id, label, detail: item.path };
    }
    case "boards":
      return {
        id: item.id,
        label: item.label,
        detail: [item.path && `Project ${item.path}`, item.boardType && readable(item.boardType)]
          .filter(Boolean)
          .join(" · "),
      };
    case "reviewers":
      if (!item.reviewer) return;
      return item.label === item.reviewer.username
        ? { id: item.id, label: `@${item.label}`, detail: item.reviewer.publicEmail ?? undefined }
        : { id: item.id, label: item.label, detail: reviewerDetail(item) };
  }
}
