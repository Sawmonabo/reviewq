import type { ReactNode } from "react";
import { Icon } from "../components/Icon";
import { Loading } from "../components/PanelState";

export function DetailHeader({
  ticket,
  title = "Review details",
  actions,
  onClose,
}: {
  readonly ticket?: string | undefined;
  readonly title?: string;
  readonly actions: ReactNode;
  readonly onClose: () => void;
}) {
  return (
    <header className="drawer-header spread">
      {ticket ? (
        <div>
          <h2>{ticket}</h2>
          <span className="row-tag" data-review-kind="thread_reply">
            No matching MR
          </span>
        </div>
      ) : (
        <h2>{title}</h2>
      )}
      <div className="actions drawer-header-actions">
        {actions}
        <button
          type="button"
          className={ticket ? "icon-button" : undefined}
          aria-label={ticket ? "Return to workspace" : "Close review"}
          onClick={onClose}
        >
          <Icon name={ticket ? "back" : "close"} />
        </button>
      </div>
    </header>
  );
}

export function LoadingDetail({
  header,
  label,
}: {
  readonly header: ReactNode;
  readonly label: string;
}) {
  return (
    <>
      {header}
      <Loading label={label} />
    </>
  );
}
