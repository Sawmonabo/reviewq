import { format } from "prettier";
import ts from "typescript";

export async function declarationModules(generated, header, features) {
  const formatted = await format(generated, { filepath: "contracts.ts" });
  const source = ts.createSourceFile("contracts.ts", formatted, ts.ScriptTarget.ESNext);
  const references = new Map();
  for (const declaration of source.statements) {
    if (!(ts.isTypeAliasDeclaration(declaration) || ts.isInterfaceDeclaration(declaration))) {
      throw new Error("Unexpected generated TypeScript declaration");
    }
    const names = new Set();
    references.set(declaration.name.text, names);
    ts.forEachChild(declaration, function collect(node) {
      if (ts.isTypeReferenceNode(node) && ts.isIdentifier(node.typeName)) {
        names.add(node.typeName.text);
      }
      ts.forEachChild(node, collect);
    });
  }
  const dependencies = referencedFeatures(references, features);
  const owners = new Map();
  const groups = new Map();
  for (const declaration of source.statements) {
    const name = declaration.name.text;
    const usedBy = dependencies.get(name);
    const feature = features[name] ?? (name === "Contracts" ? "registry" : uniqueOwner(name, usedBy));
    if (!groups.has(feature)) {
      groups.set(feature, { file: feature, declarations: [], imports: new Map() });
    }
    const group = groups.get(feature);
    owners.set(name, group);
    group.declarations.push(declaration);
  }
  const modules = Array.from(groups.values()).sort((left, right) => left.file.localeCompare(right.file));
  for (const group of modules) {
    for (const declaration of group.declarations) {
      for (const name of references.get(declaration.name.text)) {
        const owner = owners.get(name);
        if (owner && owner !== group) {
          const names = group.imports.get(owner) ?? new Set();
          names.add(name);
          group.imports.set(owner, names);
        }
      }
    }
  }
  return [
    [
      "contracts.ts",
      header + modules.map(({ file }) => 'export type * from "./contracts/' + file + '.js";').join("\n"),
    ],
    ...modules.map(({ file, imports, declarations }) => [
      "contracts/" + file + ".ts",
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
        ...declarations.map((declaration) => declaration.getText(source)),
      ].join("\n\n"),
    ]),
  ];
}

function uniqueOwner(name, owners) {
  if (!owners) {
    throw new Error("No schema owner for generated declaration " + name);
  }
  return owners.size === 1 ? owners.values().next().value : "common";
}

function referencedFeatures(references, roots) {
  const owners = new Map();
  for (const [root, feature] of Object.entries(roots)) {
    const visited = new Set();
    visitDefinition(root);

    function visitDefinition(name) {
      if (!references.has(name) || visited.has(name) || (name !== root && Object.hasOwn(roots, name))) return;
      visited.add(name);
      const usedBy = owners.get(name) ?? new Set();
      usedBy.add(feature);
      owners.set(name, usedBy);
      for (const child of references.get(name)) visitDefinition(child);
    }
  }
  return owners;
}
