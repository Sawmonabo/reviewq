import { useId, useLayoutEffect, useRef, useState } from "react";
import type { FocusEvent } from "react";
import { Icon } from "../../components/Icon";
import { formatTime } from "../../lib/presentation";
import type { WorkspaceConnection } from "./useWorkspaceConnection";
import "./connection.css";

export function WorkspaceWarning({ connection }: { readonly connection: WorkspaceConnection }) {
  const id = useId();
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLElement>(null);
  const blurredWhileHidden = useRef(false);
  const [open, setOpen] = useState(false);
  const { status, issues, retry } = connection;
  const readFailed = issues.some(({ kind }) => kind === "read");

  useLayoutEffect(() => {
    const trigger = button.current;
    const popover = panel.current;
    return () => {
      const active = document.activeElement;
      const hadFocus =
        active === trigger ||
        popover?.contains(active) ||
        (active === document.body && blurredWhileHidden.current);
      if (!trigger || !hadFocus) return;
      const header = trigger.closest("header");
      const fallback = header?.querySelector("h1") ?? trigger.parentElement?.nextElementSibling;
      requestAnimationFrame(() => {
        if (document.activeElement !== document.body) return;
        const next = document.querySelector<HTMLElement>(".workspace-warning-button");
        const target = next ?? fallback;
        if (target instanceof HTMLElement && target.getClientRects().length)
          target.focus({ preventScroll: true });
      });
    };
  }, []);

  function rememberHiddenFocus({ relatedTarget }: FocusEvent<HTMLElement>) {
    // The media query can hide this trigger and blur it before React moves it.
    blurredWhileHidden.current =
      (relatedTarget === null || relatedTarget === document.body) &&
      button.current?.getClientRects().length === 0;
  }

  return (
    <>
      <button
        ref={button}
        onBlur={rememberHiddenFocus}
        className="workspace-warning-button"
        type="button"
        aria-label="Workspace connection warning"
        title="Workspace connection warning"
        aria-expanded={open}
        aria-haspopup="dialog"
        popoverTarget={id}
      >
        <Icon name="warning" size={17} />
      </button>
      <section
        ref={panel}
        onBlur={rememberHiddenFocus}
        id={id}
        tabIndex={-1}
        popover="auto"
        role="dialog"
        aria-labelledby={`${id}-title`}
        className="filter-popup workspace-warning-popup"
        onToggle={(event) => {
          const expanded = event.newState === "open";
          setOpen(expanded);
          if (expanded) event.currentTarget.focus({ preventScroll: true });
        }}
      >
        <h2 id={`${id}-title`}>Workspace connection</h2>
        {status.state === "reconnecting" && (
          <p>
            Live updates are reconnecting.
            {status.lastConnectedAt !== undefined && (
              <>
                <br />
                Last connected{" "}
                <time dateTime={new Date(status.lastConnectedAt).toISOString()}>
                  {formatTime(status.lastConnectedAt)}
                </time>
              </>
            )}
          </p>
        )}
        {readFailed && <p>ReviewQ couldn’t refresh some workspace data.</p>}
        {issues.length > 0 && (
          <dl>
            {issues.map(({ label, message, confirmedAt, kind }) => (
              <div key={label}>
                <dt>{label}</dt>
                <dd>
                  {message}
                  {confirmedAt !== undefined ? (
                    <>
                      {message && <br />}
                      {kind === "sync" ? "Last successful sync " : "Last loaded "}
                      <time dateTime={new Date(confirmedAt).toISOString()}>
                        {formatTime(confirmedAt)}
                      </time>
                    </>
                  ) : (
                    !message && "Waiting for data"
                  )}
                </dd>
              </div>
            ))}
          </dl>
        )}
        {retry && (
          <footer>
            <button type="button" className="button-label" onClick={retry}>
              <Icon name="refresh" size={14} /> Retry
            </button>
          </footer>
        )}
      </section>
    </>
  );
}
