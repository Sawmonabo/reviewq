/** The companion's identity for one Claude Code session: written once per generation. */
export type ReviewqCompanion = {
  id: string;
  generation: string;
  proof: string;
};

/** The companion's counters for one generation, each only rising. */
export type ReviewqProgress = {
  id: string;
  generation: string;
  sequence: number;
  childSequence: number;
  submitted: number;
  promptSubmitted?: number;
};

/** The delivery offer one copy of the module took, as `id:fence`. */
export type ReviewqDelivery = {
  id: string;
  offer: string;
  copy: string;
};

/** The generation's turn as its copies left it; each handler writes only what its event changed. */
export type ReviewqTurn = {
  id: string;
  generation: string;
  turnId?: string;
  activeTurn?: string | null;
  step?: number | null;
  model?: string;
  effort?: string | null;
  phase?: "working" | "idle" | "failed" | null;
  question?: string | null;
  lastPrompt?: string;
  lastPromptTurn?: string | null;
  lastPromptManual?: boolean;
};

/** The copy of the module that opened the companion last. */
export type ReviewqCopy = {
  id: string;
  copy: string;
};

declare module "claude-code" {
  interface PluginState {
    reviewq: {
      companion: ReviewqCompanion;
      progress: ReviewqProgress;
      delivery: ReviewqDelivery;
      turn: ReviewqTurn;
      copy: ReviewqCopy;
    };
  }
}
