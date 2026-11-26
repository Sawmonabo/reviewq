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

declare module "claude-code" {
  interface PluginState {
    reviewq: {
      companion: ReviewqCompanion;
      progress: ReviewqProgress;
      delivery: ReviewqDelivery;
    };
  }
}
