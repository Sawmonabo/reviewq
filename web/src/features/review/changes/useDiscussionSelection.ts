import { useEffect, useEffectEvent, useRef, useState } from "react";
import { readDiscussionDestination } from "../../../api/discussion-search";
import { watchResources } from "../../../api/events";
import type {
  DiscussionDestination,
  DiscussionSearchMatch,
} from "../../../api/generated/contracts";
import { describeError } from "../../../lib/presentation";
import { highlightNote } from "./note-highlight";

type Confirmed = { readonly value: DiscussionDestination; readonly hit: string };

const readingKeys = new Set(["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "]);

export function useDiscussionSelection(
  reviewId: string,
  revision: string | undefined,
  open: (note: DiscussionDestination) => void,
) {
  const active = useRef<AbortController>(null);
  const recheck = useRef<AbortController>(null);
  const listener = useRef<ReturnType<typeof watchResources>>(undefined);
  const [generation, setGeneration] = useState(0);
  const [authority, setAuthority] = useState(0);
  const [error, setError] = useState<string>();
  const [selected, setSelected] = useState<Confirmed>();
  const invalidate = useEffectEvent((authorityChanged: boolean) => {
    setGeneration((value) => value + 1);
    if (authorityChanged) setAuthority((value) => value + 1);
    active.current?.abort();
    recheck.current?.abort();
    highlightNote();
    setSelected(undefined);
    setError(undefined);
  });
  const resume = useEffectEvent(() => {
    setGeneration((value) => value + 1);
    active.current?.abort();
    recheck.current?.abort();
    if (!selected || !revision || selected.value.revision !== revision) return;
    const controller = new AbortController();
    recheck.current = controller;
    function clearConfirmed() {
      if (controller.signal.aborted) return;
      highlightNote();
      setSelected(undefined);
    }
    void readDiscussionDestination(reviewId, revision, selected.hit, controller.signal, true).then(
      ({ data, cursor }) => {
        if (controller.signal.aborted) return;
        listener.current?.snapshot(cursor);
        const previous = selected.value.thread;
        if (
          data.thread.outdated !== previous.outdated ||
          JSON.stringify(data.thread.anchor) !== JSON.stringify(previous.anchor)
        )
          clearConfirmed();
      },
      clearConfirmed,
    );
  });

  useEffect(() => {
    const subscription = watchResources([{ kind: "discussions", id: reviewId }], (reason) => {
      if (reason === "visible") resume();
      else invalidate(reason === "reset");
    });
    const scope = watchResources(
      [{ kind: "settings" }, { kind: "changes", id: reviewId }],
      (reason) => {
        if (reason === "invalidate") invalidate(true);
      },
    );
    listener.current = subscription;
    return () => {
      subscription.close();
      scope.close();
    };
  }, [reviewId]);

  useEffect(() => {
    const events = new AbortController();
    function interrupt(event: Event) {
      if (!event.isTrusted || (event instanceof KeyboardEvent && !readingKeys.has(event.key)))
        return;
      active.current?.abort();
    }
    window.addEventListener("wheel", interrupt, { passive: true, signal: events.signal });
    window.addEventListener("pointerdown", interrupt, { passive: true, signal: events.signal });
    window.addEventListener("keydown", interrupt, { signal: events.signal });
    return () => {
      events.abort();
      active.current?.abort();
      recheck.current?.abort();
      highlightNote();
    };
  }, [reviewId, revision]);

  return {
    generation,
    authority,
    selected: selected?.value.revision === revision ? selected?.value : undefined,
    error,
    confirm: async (hit: DiscussionSearchMatch) => {
      active.current?.abort();
      recheck.current?.abort();
      if (!revision) return;
      const controller = new AbortController();
      active.current = controller;
      setError(undefined);
      try {
        const result = await readDiscussionDestination(
          reviewId,
          revision,
          hit.hit,
          controller.signal,
        );
        if (result.data.matched !== hit.snippet.slice(hit.highlightStart, hit.highlightEnd))
          throw new Error("The discussion changed. Search again.");
        controller.signal.throwIfAborted();
        listener.current?.snapshot(result.cursor);
        highlightNote();
        setSelected({ value: result.data, hit: hit.hit });
        open(result.data);
      } catch (error) {
        if (!controller.signal.aborted) {
          setSelected(undefined);
          setError(describeError(error));
        }
      } finally {
        if (active.current === controller) active.current = null;
      }
    },
    clear: () => {
      active.current?.abort();
      recheck.current?.abort();
      highlightNote();
      setSelected(undefined);
      setError(undefined);
    },
    cancel: () => active.current?.abort(),
  };
}
