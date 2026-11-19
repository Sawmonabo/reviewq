import { useEffect, useState } from "react";
import type { ReactNode } from "react";

export function Loading({ label }: { readonly label: string }) {
  const [delay, setDelay] = useState(0);
  useEffect(() => {
    const brief = window.setTimeout(() => setDelay(1), 180);
    const longer = window.setTimeout(() => setDelay(2), 3000);
    return () => {
      window.clearTimeout(brief);
      window.clearTimeout(longer);
    };
  }, []);
  return <div className="panel-loading" role="status" aria-label={label} aria-busy="true">
    {delay > 0 && <span className="spinner" aria-hidden="true" />}
    {delay > 1 && <span>{label}</span>}
  </div>;
}

export function Empty({ title, children }: { readonly title: string; readonly children?: ReactNode }) {
  return <div className="empty-state"><h3>{title}</h3>{children}</div>;
}

export function Notice({ children, retry }: { readonly children: ReactNode; readonly retry?: () => void }) {
  return <div className="notice" role="status"><span>{children}</span>{retry && <button type="button" onClick={retry}>Retry</button>}</div>;
}
