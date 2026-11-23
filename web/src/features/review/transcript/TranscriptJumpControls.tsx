import { DetachedQuestion } from "./questions/DetachedQuestion";
import { questionKey } from "./questions/question-key";
import type { QuestionDestination, useQuestionJump } from "./questions/useQuestionJump";
import type { Questions } from "./questions/useQuestions";

export function TranscriptJumpControls({
  jump,
  questions,
  latestLabel,
  onLatest,
  onChoose,
  onReveal,
  onError,
}: {
  readonly jump: ReturnType<typeof useQuestionJump>;
  readonly questions: Questions;
  readonly latestLabel: string | undefined;
  readonly onLatest: () => void;
  readonly onChoose: (destination: QuestionDestination) => boolean;
  readonly onReveal: () => void;
  readonly onError: (message: string | undefined) => void;
}) {
  const { offscreen, detached, jumping } = jump;
  return (
    <div className="timeline-jump-controls">
      {detached && (
        <DetachedQuestion key={questionKey(detached)} form={detached} questions={questions} />
      )}
      {offscreen && (
        <button
          type="button"
          className="question-jump quiet-button"
          aria-disabled={jumping || undefined}
          onClick={(event) =>
            void jump.jump(offscreen, event.currentTarget, onChoose, onReveal, onError)
          }
        >
          Jump to question
        </button>
      )}
      {latestLabel && (
        <button type="button" className="timeline-jump" onClick={onLatest}>
          <span aria-hidden="true">↓ </span>
          {latestLabel}
        </button>
      )}
    </div>
  );
}
