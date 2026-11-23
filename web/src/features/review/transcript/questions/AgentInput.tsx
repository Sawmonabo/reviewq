import { useCallback, useLayoutEffect, useRef } from "react";
import type { ComponentProps } from "react";
import { Composer } from "../../Composer";
import { questionKey } from "./question-key";
import { QuestionPrompt } from "./QuestionPrompt";
import type { Questions } from "./useQuestions";

export function AgentInput({
  composer,
  questions,
}: {
  readonly composer?: ComponentProps<typeof Composer>;
  readonly questions: Questions;
}) {
  const blocking = questions.blocking[0];
  const key = blocking && questionKey(blocking.form);
  const shell = useRef<HTMLDivElement>(null);
  const prompt = useRef<HTMLDivElement>(null);
  const message = useRef<HTMLDivElement>(null);
  const priorMessageFocus = useRef<HTMLElement>(null);
  const promptHadFocus = useRef(false);
  const trackPrompt = useCallback((node: HTMLDivElement | null) => {
    if (!node) return;
    prompt.current = node;
    return () => {
      promptHadFocus.current = node.contains(document.activeElement);
      prompt.current = null;
    };
  }, []);

  useLayoutEffect(() => {
    const wrapper = message.current;
    const active = document.activeElement;
    const composerOwnedFocus = active instanceof HTMLElement && wrapper?.contains(active);
    if (composerOwnedFocus) priorMessageFocus.current = active;
    const restore = promptHadFocus.current && active === document.body;
    promptHadFocus.current = false;
    if (wrapper) wrapper.hidden = Boolean(key);
    if (key && (composerOwnedFocus || restore)) {
      prompt.current?.querySelector<HTMLElement>("form")?.focus({ preventScroll: true });
    } else if (!key && restore) {
      const prior = priorMessageFocus.current;
      const destination =
        (prior && !prior.matches(":disabled") ? prior : wrapper?.querySelector("textarea")) ??
        shell.current?.closest(".agent-panel")?.querySelector<HTMLElement>(".timeline-viewport");
      destination?.focus({ preventScroll: true });
    }
    if (!key) priorMessageFocus.current = null;
  }, [key]);

  return (
    <div className="agent-input-slot" ref={shell}>
      {blocking && (
        <div key={key} className="blocking-question" ref={trackPrompt}>
          <QuestionPrompt
            form={blocking.form}
            questions={questions}
            error={blocking.error}
            {...(questions.blocking.length > 1
              ? { queue: `1 of ${questions.blocking.length}` }
              : {})}
          />
        </div>
      )}
      {composer && (
        <div className="agent-input" ref={message}>
          <Composer {...composer} disabled={Boolean(blocking)} />
        </div>
      )}
    </div>
  );
}
