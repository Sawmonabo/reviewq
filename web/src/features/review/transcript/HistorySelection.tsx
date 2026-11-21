import { describeError } from "../../../lib/presentation";

export function HistorySelection({
  onClear,
  onError,
}: {
  readonly onClear: () => void;
  readonly onError: (message: string) => void;
}) {
  return (
    <div className="timeline-controls">
      <button
        type="button"
        onClick={() => {
          const value = getSelection()?.toString();
          if (value)
            void navigator.clipboard
              .writeText(value)
              .catch((failure: unknown) => onError(describeError(failure)));
        }}
      >
        Copy selection
      </button>
      <button type="button" onClick={onClear}>
        Clear selection
      </button>
    </div>
  );
}
