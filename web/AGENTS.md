# Web rules

- Follow the [React and TypeScript conventions](../docs/react-typescript-conventions.md) for module ownership, names, types and rendering.
- Keep layout, styling, and interaction behavior consistent. When changing one to fix a bug or usability issue, identify the problem the change solves.
- Put navigation views in `src/app/views/`, API access in `src/api/`, and feature-specific code beside its consumer. Use `src/components/`, `src/hooks/`, and `src/lib/` for components, hooks, and plain functions shared across features. Follow the [import rules](dependency-cruiser.mjs).
- Match component and view filenames to their PascalCase exports, such as `ReviewDrawer.tsx`. Match custom hook filenames to their camelCase exports, such as `useHistory.ts`. Use kebab-case for plain modules, such as `submit-key.ts`.
- Inspect visual changes in a populated view. Use data fixtures or a seeded generator when needed.
- Run automated browser checks headlessly against a dedicated ReviewQ profile. Reuse its test cookie across cold browser processes unless the check exercises first authorization. Keep the user’s browser and installed profile out of the test workflow.
- Preserve scroll position and text selection when live content arrives while reading. Verify keyboard focus, draft retention, and delivery to the selected run when changing navigation or the composer.
- Run `make web-check` for static checks, `make web-build` for the production bundle, and `make web-test` for browser tests.
