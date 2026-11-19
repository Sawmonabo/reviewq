import { format } from "prettier";
import ts from "typescript";

// Split on whole type boundaries and keep the public entry stable.
const DECLARATION_LINES = 600;

export async function declarationModules(generated, header) {
  const formatted = await format(generated, { filepath: "contracts.ts" });
  const source = ts.createSourceFile("contracts.ts", formatted, ts.ScriptTarget.ESNext);
  const owners = new Map();
  const groups = [];
  let group;
  for (const declaration of source.statements) {
    if (!(ts.isTypeAliasDeclaration(declaration) || ts.isInterfaceDeclaration(declaration))) {
      throw new Error("Unexpected generated TypeScript declaration");
    }
    const text = declaration.getText(source);
    const lines = text.split("\n").length + 2;
    if (!group || group.lines + lines > DECLARATION_LINES) {
      group = {
        file: "contracts-" + (groups.length + 1),
        declarations: [],
        lines: 0,
        imports: new Map(),
      };
      groups.push(group);
    }
    owners.set(declaration.name.text, group);
    group.declarations.push({ declaration, text });
    group.lines += lines;
  }
  if (groups.length === 1) {
    return [["contracts.ts", formatted]];
  }
  for (const group of groups) {
    for (const { declaration } of group.declarations) {
      ts.forEachChild(declaration, function collect(node) {
        if (ts.isTypeReferenceNode(node) && ts.isIdentifier(node.typeName)) {
          const name = node.typeName.text;
          const owner = owners.get(name);
          if (owner && owner !== group) {
            const names = group.imports.get(owner) ?? new Set();
            names.add(name);
            group.imports.set(owner, names);
          }
        }
        ts.forEachChild(node, collect);
      });
    }
  }
  return [
    [
      "contracts.ts",
      header + groups.map(({ file }) => 'export type * from "./' + file + '.js";').join("\n"),
    ],
    ...groups.map(({ file, imports, declarations }) => [
      file + ".ts",
      [
        header,
        ...Array.from(
          imports,
          ([owner, names]) =>
            "import type { " +
            Array.from(names).sort().join(", ") +
            ' } from "./' +
            owner.file +
            '.js";',
        ),
        ...declarations.map(({ text }) => text),
      ].join("\n\n"),
    ]),
  ];
}
