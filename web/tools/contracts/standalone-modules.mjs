import { relative, dirname } from "node:path/posix";
import standaloneCode from "ajv/dist/standalone/index.js";
import ts from "typescript";

const SCHEMA_ID = "https://reviewq.invalid/contracts/v1.schema.json";
const RUNTIME_FILE = "validators/runtime.mjs";

// Ajv builds one scope for the entire registry and shares its schema references.
// Split the scope on its top-level declarations, preserving its dependency graph.
export function standaloneModules(ajv, schema, roots) {
  const aliases = equivalentRefs(schema.definitions);
  const canonicalRef = (ref) => aliases.get(ref) ?? ref;
  const validationSchema = JSON.parse(
    JSON.stringify(schema, (key, value) => (key === "$ref" ? canonicalRef(value) : value)),
  );
  ajv.addSchema(validationSchema, SCHEMA_ID);

  const definitions = Object.fromEntries(
    roots.map(([name, ref]) => [name, SCHEMA_ID + canonicalRef(ref)]),
  );
  const schemaNames = new Map(
    Object.entries(validationSchema.definitions).map(([name, definition]) => [definition, name]),
  );
  const fileFor = (name) =>
    `validators/schemas/${name
      .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
      .replaceAll("_", "-")
      .toLowerCase()}.mjs`;
  const validatorNames = new Map();
  for (const ref of Object.values(definitions)) registerFunction(ajv.getSchema(ref));
  const source = ts.createSourceFile(
    "standalone.mjs",
    standaloneCode(ajv, definitions),
    ts.ScriptTarget.ESNext,
    true,
    ts.ScriptKind.JS,
  );
  const declarations = readDeclarations(source);
  const referenced = new Set();
  for (const [name, declaration] of declarations) {
    declaration.file = validatorNames.get(name) ?? RUNTIME_FILE;
    declaration.dependencies = references(declaration.statement, declarations);
    declaration.dependencies.delete(name);
  }

  const exports = roots.map(([name, ref]) => {
    const symbol = ajv.getSchema(SCHEMA_ID + canonicalRef(ref)).source.validateName.toString();
    retain(symbol);
    return { name: `is${name}`, symbol, file: validatorNames.get(symbol) };
  });
  const files = new Map();
  for (const [name, declaration] of declarations) {
    if (!referenced.has(name)) continue;
    let file = files.get(declaration.file);
    if (!file) {
      file = { imports: new Map(), declarations: [], exports: [] };
      files.set(declaration.file, file);
    }
    file.declarations.push(declaration.statement.getText(source));
    file.exports.push(name);
    for (const dependency of declaration.dependencies) {
      const owner = declarations.get(dependency).file;
      if (owner === declaration.file) continue;
      const imported = file.imports.get(owner) ?? [];
      if (!imported.includes(dependency)) imported.push(dependency);
      file.imports.set(owner, imported);
    }
  }
  const modules = Array.from(files, ([file, parts]) => [
    file,
    [
      ...Array.from(parts.imports, ([path, names]) => {
        const localPath = relative(dirname(file), path);
        return `import { ${names.join(", ")} } from "${localPath.startsWith(".") ? localPath : `./${localPath}`}";`;
      }),
      ...parts.declarations,
      `export { ${parts.exports.join(", ")} };`,
    ].join("\n"),
  ]);
  return { exports, modules };

  function registerFunction(validate) {
    const symbol = validate.source.validateName.toString();
    if (validatorNames.has(symbol)) return;
    const owner = schemaNames.get(validate.schema);
    validatorNames.set(symbol, owner ? fileFor(owner) : `validators/internal/${symbol}.mjs`);
    for (const scopeName of validate.source.scopeValues.validate ?? []) {
      registerFunction(scopeName.value.ref);
    }
  }

  function retain(name) {
    if (referenced.has(name)) return;
    referenced.add(name);
    for (const dependency of declarations.get(name).dependencies) retain(dependency);
  }
}

function equivalentRefs(definitions) {
  const refs = new Map();
  const shapes = new Map();
  for (const [name, definition] of Object.entries(definitions)) {
    // Annotations do not affect validation. Keep the original names for types.
    const shape = { ...definition };
    delete shape.title;
    delete shape.description;
    const encoded = JSON.stringify(shape);
    const ref = `#/definitions/${name}`;
    const existing = shapes.get(encoded);
    if (existing) refs.set(ref, existing);
    else shapes.set(encoded, ref);
  }
  return refs;
}

function readDeclarations(source) {
  const declarations = new Map();
  for (const statement of source.statements) {
    if (ts.isExpressionStatement(statement) && ts.isStringLiteral(statement.expression)) continue;
    if (statement.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.ExportKeyword)) {
      continue;
    }
    const name = ts.isFunctionDeclaration(statement)
      ? statement.name
      : ts.isVariableStatement(statement) && statement.declarationList.declarations.length === 1
        ? statement.declarationList.declarations[0].name
        : null;
    if (!name || !ts.isIdentifier(name)) {
      throw new Error("Unexpected Ajv standalone statement: " + statement.getText(source));
    }
    declarations.set(name.text, { statement });
  }
  return declarations;
}

function references(statement, declarations) {
  const names = new Set();
  visit(statement);
  return names;

  function visit(node) {
    if (ts.isIdentifier(node) && declarations.has(node.text)) names.add(node.text);
    ts.forEachChild(node, visit);
  }
}
