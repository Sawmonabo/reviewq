import { useId, useState } from "react";
import type { ButtonHTMLAttributes, CSSProperties, ReactNode } from "react";
import "./popover.css";

type Trigger = Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  "id" | "type" | "popoverTarget" | "aria-haspopup" | "aria-expanded"
> & { readonly [data: `data-${string}`]: string | boolean | undefined };

// A button that opens a dialog in the top layer, anchored below it. The dialog's content mounts
// once it shows, so a field in it can take focus, and unmounts when it hides. Esc and a click
// outside hide it and return focus to the button; so does `close`.
export function Popover({
  label,
  className,
  style,
  trigger,
  onBeforeOpen,
  children,
}: {
  // The dialog's accessible name.
  readonly label: string;
  readonly className: string;
  readonly style?: CSSProperties;
  readonly trigger: Trigger;
  // Runs before the dialog shows, such as to size it from the button's place.
  readonly onBeforeOpen?: (button: HTMLElement) => void;
  readonly children: (close: () => void) => ReactNode;
}) {
  const id = useId();
  const triggerId = `${id}-trigger`;
  const [open, setOpen] = useState(false);
  const button = () => document.getElementById(triggerId);
  function close() {
    document.getElementById(id)?.hidePopover();
    button()?.focus({ preventScroll: true });
  }
  return (
    <>
      <button
        {...trigger}
        id={triggerId}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        popoverTarget={id}
      />
      <section
        id={id}
        popover="auto"
        role="dialog"
        aria-label={label}
        className={`popover ${className}`}
        style={style}
        onBeforeToggle={(event) => {
          const opener = button();
          if (event.newState === "open" && opener) onBeforeOpen?.(opener);
        }}
        onToggle={(event) => setOpen(event.newState === "open")}
      >
        {open && children(close)}
      </section>
    </>
  );
}
