import { useEffect } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { Icon } from "../../components/Icon";

const pages = [
  {
    id: "connections",
    title: "Connections",
    icon: "plug",
    group: "Workspace",
    description: "Manage the accounts ReviewQ uses for merge requests, tickets and discussions.",
  },
  {
    id: "scope",
    title: "Review scope",
    icon: "board",
    group: null,
    description:
      "Choose which tickets enter the queue and whose GitLab activity counts as your team’s.",
  },
  {
    id: "repositories",
    title: "Repositories",
    icon: "folder",
    group: null,
    description: "Choose a checkout, review skills and Jira boards for each GitLab project.",
  },
  {
    id: "agents",
    title: "Agents",
    icon: "automation",
    group: "Review workflow",
    description: "Control how registered reviews resume and how follow-ups start.",
  },
  {
    id: "appearance",
    title: "Appearance",
    icon: "palette",
    group: "Preferences",
    description: "Choose how your review queue and reports look in this browser.",
  },
] as const;

export type Page = (typeof pages)[number]["id"];

export function savedPage(value: unknown): Page {
  return pages.find(({ id }) => id === value)?.id ?? "connections";
}

export function SettingsNavigation({
  active,
  onSelect,
}: {
  readonly active: Page;
  readonly onSelect: (page: Page) => void;
}) {
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      document.getElementById(`settings-tab-${active}`)?.scrollIntoView({
        block: "nearest",
        inline: "nearest",
      });
    });
    return () => cancelAnimationFrame(frame);
  }, [active]);

  function navigate(event: KeyboardEvent<HTMLDivElement>) {
    const current = pages.findIndex(({ id }) => id === active);
    const last = pages.length - 1;
    let next: number;
    switch (event.key) {
      case "ArrowUp":
      case "ArrowLeft":
        next = current === 0 ? last : current - 1;
        break;
      case "ArrowDown":
      case "ArrowRight":
        next = current === last ? 0 : current + 1;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = last;
        break;
      default:
        return;
    }
    event.preventDefault();
    const id = pages[next]?.id;
    if (!id) return;
    onSelect(id);
    document.getElementById(`settings-tab-${id}`)?.focus();
  }

  return (
    <div className="settings-sidebar">
      <div
        className="settings-tabs"
        role="tablist"
        aria-label="Settings pages"
        onKeyDown={navigate}
      >
        {pages.map(({ id, title, icon, group }) => (
          <div role="presentation" key={id}>
            {group && (
              <p className="settings-tab-group" aria-hidden="true">
                {group}
              </p>
            )}
            <button
              type="button"
              role="tab"
              id={`settings-tab-${id}`}
              aria-selected={active === id}
              aria-controls={`settings-page-${id}`}
              tabIndex={active === id ? 0 : -1}
              onClick={() => onSelect(id)}
            >
              <Icon name={icon} size={17} />
              {title}
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

export function SettingsPage({
  page,
  active,
  action,
  children,
}: {
  readonly page: Page;
  readonly active: Page;
  readonly action?: ReactNode;
  readonly children: ReactNode;
}) {
  const heading = pages.find(({ id }) => id === page);
  return (
    <section
      className="settings-page"
      id={`settings-page-${page}`}
      role="tabpanel"
      tabIndex={0}
      aria-labelledby={`settings-tab-${page}`}
      hidden={page !== active}
    >
      <header className="settings-page-heading">
        <div>
          <h3>{heading?.title}</h3>
          <p className="meta">{heading?.description}</p>
        </div>
        {action}
      </header>
      {children}
    </section>
  );
}
