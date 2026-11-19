export function submits(event: Pick<KeyboardEvent, "key" | "shiftKey" | "altKey" | "repeat" | "isComposing" | "keyCode">) {
  return event.key === "Enter" && !event.shiftKey && !event.altKey && !event.repeat && !event.isComposing && event.keyCode !== 229;
}
