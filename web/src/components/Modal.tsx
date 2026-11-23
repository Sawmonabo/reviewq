import { useEffect, useId, useRef } from "react";
import type { ReactNode, RefObject } from "react";

type DialogProps = {
  readonly title: string;
  readonly eyebrow?: string;
  readonly headingMark?: ReactNode;
  readonly headerActions?: ReactNode;
  readonly initialFocus?: RefObject<HTMLElement | null>;
  readonly className?: string;
  readonly nonmodal?: boolean;
  readonly children: ReactNode;
  readonly onClose: () => void;
  readonly busy?: boolean;
};

export function Modal({
  children,
  footer,
  ...dialog
}: DialogProps & { readonly footer?: ReactNode }) {
  return (
    <ModalFrame {...dialog}>
      <div className="modal-body">{children}</div>
      {footer && <footer>{footer}</footer>}
    </ModalFrame>
  );
}

export function ModalFrame({
  title,
  eyebrow,
  headingMark,
  headerActions,
  initialFocus,
  className,
  nonmodal = false,
  children,
  onClose,
  busy = false,
}: DialogProps) {
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const backdrop = useRef(false);
  useEffect(() => {
    const node = dialog.current;
    const opener = document.activeElement;
    if (nonmodal) node?.show();
    else node?.showModal();
    (initialFocus?.current ?? closeButton.current)?.focus({ preventScroll: true });
    return () => {
      const active = document.activeElement;
      node?.close();
      if (
        opener instanceof HTMLElement &&
        opener.isConnected &&
        (!nonmodal || active === document.body || node?.contains(active))
      ) {
        opener.focus({ preventScroll: true });
      }
    };
  }, [initialFocus, nonmodal]);
  const close = (
    <button
      ref={closeButton}
      type="button"
      className="icon-button"
      aria-label="Close dialog"
      disabled={busy}
      onClick={onClose}
    >
      ×
    </button>
  );
  return (
    <dialog
      ref={dialog}
      className={className ? `modal ${className}` : "modal"}
      aria-labelledby={titleId}
      aria-busy={busy}
      onCancel={(event) => {
        event.preventDefault();
        event.stopPropagation();
        if (!busy) onClose();
      }}
      onKeyDown={(event) => {
        if (
          nonmodal &&
          event.key === "Escape" &&
          !event.defaultPrevented &&
          !busy &&
          !event.currentTarget.querySelector(":popover-open")
        ) {
          event.preventDefault();
          event.stopPropagation();
          onClose();
        }
      }}
      onPointerDown={(event) => {
        backdrop.current = event.target === event.currentTarget;
      }}
      onClick={(event) => {
        if (backdrop.current && event.target === event.currentTarget && !busy) onClose();
        backdrop.current = false;
      }}
    >
      <div className="modal-frame">
        <header>
          <div>
            {eyebrow && <p className="modal-eyebrow">{eyebrow}</p>}
            <h2 id={titleId}>{title}</h2>
            {headingMark}
          </div>
          {headerActions ? (
            <div className="actions modal-header-actions">
              {headerActions}
              {close}
            </div>
          ) : (
            close
          )}
        </header>
        {children}
      </div>
    </dialog>
  );
}
