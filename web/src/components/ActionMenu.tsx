import * as Menu from "@radix-ui/react-dropdown-menu";
import { Fragment } from "react";
import type { ReactNode } from "react";

type Item = {
  readonly label: string;
  readonly run: () => void;
  readonly disabled?: boolean;
  readonly separatorBefore?: boolean;
  readonly trailing?: ReactNode;
};

export function ActionMenu({
  label,
  trigger,
  items,
  contentClassName,
}: {
  readonly label: string;
  readonly trigger: ReactNode;
  readonly items: readonly Item[];
  readonly contentClassName?: string;
}) {
  return (
    <Menu.Root>
      <Menu.Trigger asChild>
        <button type="button" className="menu-trigger" aria-label={label}>
          {trigger}
        </button>
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Content
          className={contentClassName ? `action-menu ${contentClassName}` : "action-menu"}
          align="end"
          sideOffset={6}
          collisionPadding={12}
        >
          {items.map((item) => (
            <Fragment key={item.label}>
              {item.separatorBefore && <Menu.Separator />}
              <Menu.Item disabled={item.disabled ?? false} onSelect={item.run}>
                {item.label}
                {item.trailing}
              </Menu.Item>
            </Fragment>
          ))}
        </Menu.Content>
      </Menu.Portal>
    </Menu.Root>
  );
}
