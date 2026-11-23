import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import type { HistoryPageResponse } from "../../../../api/generated/contracts";
import type { Form } from "../../../../api/generated/contracts/questions";
import { historyPage } from "../../../../api/history";
import type { SessionAddress } from "../../../../api/history";
import { describeError } from "../../../../lib/presentation";
import { questionKey } from "./question-key";

const ORIGINAL_UNAVAILABLE = "The original question is unavailable in the conversation.";

export function useQuestionJump(
  target: SessionAddress,
  viewport: RefObject<HTMLDivElement | null>,
  forms: readonly Form[],
) {
  const anchors = useRef(new Map<HTMLElement, string>());
  const observer = useRef<IntersectionObserver>(null);
  const request = useRef<AbortController>(null);
  const currentForms = useRef(forms);
  const [visible, setVisible] = useState<ReadonlySet<string>>(() => new Set());
  const [jumping, setJumping] = useState(false);

  useLayoutEffect(() => {
    currentForms.current = forms;
  }, [forms]);

  const cancel = useCallback(() => request.current?.abort(), []);
  const observe = useCallback((element: HTMLElement, key: string) => {
    const known = anchors.current;
    known.set(element, key);
    observer.current?.observe(element);
    return () => {
      known.delete(element);
      observer.current?.unobserve(element);
      if (![...known.values()].includes(key))
        setVisible((values) => withVisibility(values, key, false));
    };
  }, []);

  useEffect(() => {
    const host = viewport.current;
    if (!host) return;
    const known = anchors.current;
    const watch = new IntersectionObserver(
      (entries) => {
        setVisible((previous) => {
          let next = previous;
          for (const entry of entries) {
            const key = known.get(entry.target as HTMLElement);
            if (key) next = withVisibility(next, key, entry.intersectionRect.height > 0);
          }
          return next;
        });
      },
      { root: host },
    );
    observer.current = watch;
    for (const element of known.keys()) watch.observe(element);
    const inputs = ["wheel", "touchstart", "pointerdown", "keydown"];
    for (const input of inputs) host.addEventListener(input, cancel, { passive: true });
    window.addEventListener("pagehide", cancel);
    return () => {
      watch.disconnect();
      observer.current = null;
      cancel();
      for (const input of inputs) host.removeEventListener(input, cancel);
      window.removeEventListener("pagehide", cancel);
    };
  }, [cancel, viewport]);

  const offscreen = forms.find(
    (form) => typeof form.locator === "string" && !visible.has(questionKey(form)),
  );
  const detached = forms.find(
    (form) => form.mode === "native" && !form.locator && !visible.has(questionKey(form)),
  );

  async function jump(
    form: Form,
    trigger: HTMLButtonElement,
    choose: (page: HistoryPageResponse, entryId: string, locator: string) => boolean,
    beforeReveal: () => void,
    onError: (message: string | undefined) => void,
  ) {
    if (request.current || typeof form.locator !== "string") return;
    const controller = new AbortController();
    request.current = controller;
    const { signal } = controller;
    const key = questionKey(form);
    const locator = form.locator;
    const stillPending = () =>
      currentForms.current.some((item) => questionKey(item) === key && item.locator === locator);
    setJumping(true);
    onError(undefined);
    try {
      const page = await historyPage(target, { aroundId: form.itemId, locator }, signal);
      signal.throwIfAborted();
      if (!stillPending()) return;
      if (
        page.data.reset ||
        !page.data.items.some(
          (entry) =>
            entry.entryId === form.itemId &&
            entry.turnId === form.turnId &&
            entry.parts.some(
              (part) => part.kind === "user_question" && part.requestId === form.requestId,
            ),
        )
      )
        throw new Error(ORIGINAL_UNAVAILABLE);
      if (!choose(page, form.itemId, locator))
        throw new Error("Clear the active selection before jumping to the question.");
      const deadline = performance.now() + 1200;
      while (performance.now() < deadline) {
        await new Promise(requestAnimationFrame);
        signal.throwIfAborted();
        if (!stillPending()) return;
        const host = viewport.current;
        const node = [...anchors.current].find(
          ([node, id]) => id === key && host?.contains(node),
        )?.[0];
        if (!host || !node) continue;
        const frame = host.getBoundingClientRect();
        const destination = node.getBoundingClientRect();
        if (!frame.height || !destination.height) continue;
        beforeReveal();
        host.scrollTo({ top: host.scrollTop + destination.top - frame.top - 76 });
        if (document.activeElement === trigger)
          (node.querySelector<HTMLElement>("button") ?? host).focus({ preventScroll: true });
        return;
      }
      throw new Error(ORIGINAL_UNAVAILABLE);
    } catch (error) {
      if (!signal.aborted) onError(describeError(error));
    } finally {
      if (request.current === controller) {
        request.current = null;
        setJumping(false);
      }
    }
  }

  return { offscreen, detached, jumping, observe, jump, cancel };
}

function withVisibility(values: ReadonlySet<string>, key: string, visible: boolean) {
  if (values.has(key) === visible) return values;
  const next = new Set(values);
  if (visible) next.add(key);
  else next.delete(key);
  return next;
}
