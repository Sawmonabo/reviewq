import { useEffect, useId, useRef } from "react";
import type { ReactNode } from "react";

export function Modal({ title, children, footer, onClose, busy = false }: {
  readonly title: string;
  readonly children: ReactNode;
  readonly footer?: ReactNode;
  readonly onClose: () => void;
  readonly busy?: boolean;
}) {
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const backdrop = useRef(false);
  useEffect(() => {
    const node = dialog.current;
    const opener = document.activeElement;
    node?.showModal();
    return () => {
      node?.close();
      if (opener instanceof HTMLElement && opener.isConnected) opener.focus();
    };
  }, []);
  return <dialog ref={dialog} className="modal" aria-labelledby={titleId} aria-busy={busy} onCancel={(event) => {
    event.preventDefault();
    if (!busy) onClose();
  }} onPointerDown={(event) => { backdrop.current = event.target === event.currentTarget; }} onClick={(event) => {
    if (backdrop.current && event.target === event.currentTarget && !busy) onClose();
    backdrop.current = false;
  }}>
    <div className="modal-frame">
      <header><h2 id={titleId}>{title}</h2><button type="button" className="icon-button" aria-label="Close dialog" disabled={busy} onClick={onClose}>×</button></header>
      <div className="modal-body">{children}</div>
      {footer && <footer>{footer}</footer>}
    </div>
  </dialog>;
}
