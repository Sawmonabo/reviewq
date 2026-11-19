import type { CompactTicket } from "../../api/generated/contracts";

export type LaunchTarget = {
  readonly reviewId: string;
  readonly title: string;
  readonly projectPath: string;
  readonly iid: string;
  readonly sourceSha: string;
  readonly tickets: readonly CompactTicket[];
};

export type LaunchMode = "prepare" | "pick" | "register";

export type Launch = (mode: LaunchMode, targets?: readonly LaunchTarget[]) => void;

export type OpenReview = (reviewId: string, runId?: string) => void;
