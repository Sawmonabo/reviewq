import * as Menu from "@radix-ui/react-dropdown-menu";
import type { ReactNode } from "react";

type Item = { readonly label: string; readonly run: () => void; readonly disabled?: boolean };

export function ActionMenu({
  label,
  trigger,
  items,
}: {
  readonly label: string;
  readonly trigger: ReactNode;
  readonly items: readonly Item[];
}) {
  return (
    <Menu.Root>
      <Menu.Trigger asChild>
        <button type="button" className="menu-trigger" aria-label={label}>
          {trigger}
        </button>
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Content className="action-menu" align="end" sideOffset={6} collisionPadding={12}>
          {items.map((item) => (
            <Menu.Item key={item.label} disabled={item.disabled ?? false} onSelect={item.run}>
              {item.label}
            </Menu.Item>
          ))}
        </Menu.Content>
      </Menu.Portal>
    </Menu.Root>
  );
}
