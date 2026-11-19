import * as Menu from "@radix-ui/react-dropdown-menu";
import { Icon } from "../../components/Icon";

export function QueueExclusions({
  reviewed,
  inProgress,
  reviewedCount,
  inProgressCount,
  onReviewed,
  onInProgress,
}: {
  readonly reviewed: boolean;
  readonly inProgress: boolean;
  readonly reviewedCount: number | undefined;
  readonly inProgressCount: number | undefined;
  readonly onReviewed: (checked: boolean) => void;
  readonly onInProgress: (checked: boolean) => void;
}) {
  const count = Number(reviewed) + Number(inProgress);
  const filters = [
    {
      label: "Reviewed",
      description: "Comments posted, approved, or an agent report ready",
      checked: reviewed,
      count: reviewedCount,
      change: onReviewed,
    },
    {
      label: "Being reviewed",
      description: "Agent working or waiting for your input",
      checked: inProgress,
      count: inProgressCount,
      change: onInProgress,
    },
  ];
  return (
    <Menu.Root modal={false}>
      <Menu.Trigger type="button" className="filter-control" aria-label="Hide reviews">
        Hide
        {count > 0 && <span className="hide-count">{count}</span>}
        <Icon name="chevron" size={12} />
      </Menu.Trigger>
      <Menu.Portal>
        <Menu.Content
          className="action-menu queue-exclusions"
          align="start"
          sideOffset={6}
          collisionPadding={16}
        >
          {filters.map((filter) => (
            <Menu.CheckboxItem
              key={filter.label}
              aria-label={`Hide ${filter.label.toLowerCase()}`}
              checked={filter.checked}
              onCheckedChange={filter.change}
              onSelect={(event) => event.preventDefault()}
            >
              <span className="exclusion-check" aria-hidden="true">
                <Menu.ItemIndicator>
                  <Icon name="check" size={11} />
                </Menu.ItemIndicator>
              </span>
              <span className="exclusion-copy">
                <strong>
                  {filter.label}
                  {filter.count !== undefined && <small>{filter.count}</small>}
                </strong>
                <span>{filter.description}</span>
              </span>
            </Menu.CheckboxItem>
          ))}
        </Menu.Content>
      </Menu.Portal>
    </Menu.Root>
  );
}
