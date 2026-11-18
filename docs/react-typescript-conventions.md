# React and TypeScript conventions

One React browser entry composes the queue, runs, notifications, settings and review drawer. Rust supplies the projections and durable action receipts. Put new code beside its actual caller and keep the initial page small enough to open the queue without downloading rich document views. [Browser experience](specs/browser-experience.md) · [Web rules](../web/AGENTS.md)

## Where code belongs

Paths below are relative to `web/src/`. Create them as each production feature lands.

| Responsibility | Example | Ownership |
| --- | --- | --- |
| Browser entry, navigation, selection | `main.tsx`, `app/App.tsx`, `app/views/QueueView.tsx` | Mount once. The app confirms one selected run, composes sibling features and owns navigation callbacks. |
| API transport and wire shapes | `api/client.ts`, `api/history.ts`, `api/generated/contracts.ts` | Decode unknown data, enforce size and identity, return typed results/errors. Keep generated declarations under the contract owner's control. |
| Feature behavior | `features/review/ReviewDrawer.tsx`, `features/review/transcript/useHistory.ts`, `features/review/changes/patch-document.ts` | Keep single-feature hooks, ordinary functions, props and styles here. Multiple tabs within review may share a discussion component at their nearest common owner. |
| Shared JSX | `components/markdown/MarkdownContent.tsx` | Used by both review and ticket content. Keep a diagram-only hook beside the component that needs it. |
| Shared React behavior | `hooks/useScrollPosition.ts` | Use for shared stateful behavior such as maintaining independently scrolling panes. Each call has its own state unless connected to a shared owner. |
| Shared plain functions | `lib/view-preferences.ts`, `lib/submit-key.ts` | Functions can use browser APIs. Keep JSX, React hooks and application selection outside this layer. |

Use direct, case-correct imports. Shared layers cannot import screens or features. API does not import presentation. Shared hooks do not import components. Sibling features meet in the app through props and slots. Keep a helper private until a second consumer or a distinct responsibility justifies extraction. A missing directory does not call for a wrapper or empty index. [Enforced import graph](../web/dependency-cruiser.mjs)

## Names, types and file order

1. Static imports, grouped external before local where safe. Use `import type` for types. Preserve the order of side-effect imports, especially CSS.
2. Declare the module's types and props, then file-wide constants. A registry declared `as const` can precede its derived union. Use PascalCase for types, camelCase for ordinary values and functions, and upper snake case for fixed policy constants with units. Keep function-local values close to first use.
3. Put the named primary component, hook or public operation next, followed by private helpers. Define component and hook functions at module scope. Match filenames to exports, e.g. `ReviewDrawer.tsx` and `useHistory.ts`. Plain modules use kebab-case, e.g. `patch-document.ts`. Reserve `.tsx` for JSX.
4. Keep props local to their component. Use `type` for props, unions and derived shapes, `interface` for a capability implemented by interchangeable adapters, and `readonly` for inputs. Name events `onSelect` and handlers `handleSelect` where useful. Export only what has a consumer. Share a feature's types in a subject-named module.

For example, represent the state of opening a run with a discriminant, so a component can enable Send only when the service has confirmed the target.

```ts
type Selection =
  | { readonly state: "opening"; readonly requestedRunId: string }
  | { readonly state: "confirmed"; readonly runId: string; readonly leadSessionKey: string };
```

The actual handler captures run, lead, draft and submission ID when the user sends. A late receipt updates that captured target. Retain entered text after failure. Treat JSON, URL parameters, stored preferences and stream events as `unknown` until validated. A generated TypeScript type disappears at runtime. Keep decode and policy checks at the boundary that owns them. [TypeScript narrowing](https://www.typescriptlang.org/docs/handbook/2/narrowing.html) · [React state structure](https://react.dev/learn/choosing-the-state-structure)

## Rendering and cleanup

Derive display values in render. Keep selection, draft, scroll/expansion bookmarks and durable service state distinct. Key rows by stable provider/run/item identity. Pass `AbortSignal` to reads and check captured selection generation after each await. Clean up subscriptions, observers, listeners, portals and retained bodies on navigation. Use effects to synchronize with browser APIs or the service. Lazy-load rich Markdown, diagrams and feature bodies through their actual callers. [React effects](https://react.dev/learn/synchronizing-with-effects) · [Dependency decisions](dependency-decisions.md)

Let the TypeScript patch renderer own the selected file's code text and thread mount points. Render discussion children through React portals. Keep those text nodes in place for same-revision updates. Use one measured outer transcript list. Preserve the reader's selection, focus and anchor while earlier content arrives. Provide explicit previous/next and prompt jumps for material outside the mounted window. Use semantic controls, label asynchronous status and return focus to the invoking control. [Browser ownership and budgets](specs/browser-experience.md#documents-and-retained-memory)

## Checks

Run `make web-install` to install the checked-in pnpm lock. `make web-check` runs strict TypeScript for source and tests, type-aware ESLint and React Hooks rules, dependency-cruiser, Knip and Prettier. The config enforces PascalCase type names and type-only imports. Knip checks production reachability from `src/main.tsx`, with generated exports/types excepted. Intentional external entries need an actual build consumer. Naming semantics, filename spelling, section order, DOM ownership and keyboard behavior also need review. [Web scripts](../web/package.json) · [TypeScript settings](../web/tsconfig.json) · [Lint](../web/eslint.config.mjs) · [Knip](../web/knip.json)

Run `make web-build` for the production bundle and `make web-test` for browser tests. Inspect changes in a populated wide, narrow and phone view. For a reading interaction, check focus, Find, selection and scroll during actual background updates. For composer/navigation changes, check retained drafts and read back delivery in the intended run. Use the existing worked fixture and add a case only for a concrete uncovered regression. [Worked fixture](plans/01-foundation/fixtures.md) · [Browser qualification](plans/06-release/browser-qualification.md)
