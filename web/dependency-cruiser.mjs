export default {
  forbidden: [
    {
      name: "no-cycles",
      severity: "error",
      from: { path: "^src/" },
      to: { circular: true },
    },
    {
      name: "shared-code-does-not-import-screens",
      severity: "error",
      from: { path: "^src/(api|components|hooks|lib)/" },
      to: { path: "^src/(app/|features/|main\\.tsx$)" },
    },
    {
      name: "api-does-not-import-presentation",
      severity: "error",
      from: { path: "^src/api/" },
      to: { path: "^src/(components|hooks)/" },
    },
    {
      name: "lib-does-not-import-react-layers",
      severity: "error",
      from: { path: "^src/lib/" },
      to: { path: "^src/(components|hooks|api)/" },
    },
    {
      name: "shared-hooks-do-not-import-components",
      severity: "error",
      from: { path: "^src/hooks/" },
      to: { path: "^src/components/" },
    },
    {
      name: "features-do-not-import-shell",
      severity: "error",
      from: { path: "^src/features/" },
      to: { path: "^src/(app/|main\\.tsx$)" },
    },
    {
      name: "compose-features-in-shell",
      severity: "error",
      from: { path: "^src/features/([^/.]+)(?:/|[.])" },
      to: { path: "^src/features/", pathNot: "^src/features/$1(?:/|[.])" },
    },
  ],
  options: {
    doNotFollow: { path: "node_modules" },
    tsConfig: { fileName: "tsconfig.json" },
    tsPreCompilationDeps: true,
  },
};
