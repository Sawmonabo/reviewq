import { Component } from "react";
import type { ReactNode } from "react";
import { Notice } from "./PanelState";

export class PanelBoundary extends Component<
  { readonly children: ReactNode },
  { readonly failed: boolean }
> {
  override state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  override render() {
    return this.state.failed ? (
      <Notice>
        Could not open this panel.{" "}
        <button type="button" onClick={() => location.reload()}>
          Reload ReviewQ
        </button>
      </Notice>
    ) : (
      this.props.children
    );
  }
}
