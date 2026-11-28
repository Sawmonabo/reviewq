import { useDraftCount } from "../../../hooks/useDraftCount";

// The Quit and Uninstall boxes warn while this tab holds unsent drafts, which only live in memory.
export function DraftsLine() {
  const count = useDraftCount();
  if (count === 0) return null;
  return (
    <p>
      {count === 1
        ? "You have 1 unsent message to an agent. It will be lost."
        : `You have ${count} unsent messages to agents. They will be lost.`}
    </p>
  );
}
