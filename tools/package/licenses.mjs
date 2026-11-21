import { chmodSync, copyFileSync, lstatSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";

function cargoPackages(metadata, application) {
  const resolved = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const visited = new Set();
  const pending = [application];
  while (pending.length) {
    const id = pending.pop();
    if (visited.has(id)) continue;
    visited.add(id);
    for (const dependency of resolved.get(id).deps) {
      if (dependency.dep_kinds.some(({ kind }) => kind !== "dev")) pending.push(dependency.pkg);
    }
  }
  return metadata.packages
    .filter((pkg) => pkg.source && visited.has(pkg.id))
    .map((pkg) => ({
      ecosystem: "cargo", name: pkg.name, version: pkg.version, license: pkg.license,
      root: dirname(pkg.manifest_path), explicit: pkg.license_file, git: pkg.source.startsWith("git+"),
    }));
}

function nodePackages(root, run) {
  const [app] = JSON.parse(run("pnpm", ["list", "--prod", "--depth", "Infinity", "--json"], join(root, "web")));
  const visited = new Set();
  const packages = [];
  const pending = [app];
  while (pending.length) {
    const tree = pending.pop();
    const entries = Object.values({ ...tree.dependencies, ...tree.optionalDependencies });
    for (const item of entries) {
      if (!item.path || visited.has(item.path)) continue;
      visited.add(item.path);
      const value = JSON.parse(readFileSync(join(item.path, "package.json"), "utf8"));
      packages.push({ ecosystem: "npm", name: value.name, version: value.version,
        license: value.license ?? null, root: item.path, explicit: null });
      pending.push(item);
    }
  }
  return packages;
}

function licenseFiles(root) {
  return readdirSync(root).sort()
    .filter((file) => /^(?:licen[cs]e|copying|copyright|notice)(?:[.-]|$)/i.test(file))
    .map((file) => [file, join(root, file)])
    .filter(([, path]) => lstatSync(path).isFile());
}

export function notices(metadata, application, root, output, run) {
  mkdirSync(output, { mode: 0o700 });
  const packages = [
    ...cargoPackages(metadata, application),
    ...nodePackages(root, run),
    { ecosystem: "source", name: "ai-sidekicks-block-cutter",
      version: "85975b3d8475b9d4c3a31d4bc4de4c5baaf3b42d", license: "Apache-2.0",
      root: join(root, "crates/reviewq-app/src/reports/display") },
    { ecosystem: "source", name: "agent-overflow-markdown-boundary",
      version: "cba52327fe1e8ad2b43097b884a3c96d0c40aa52", license: "MIT",
      root: join(root, "web/src/components/markdown/boundary") },
  ];
  const entries = new Map();
  for (const pkg of packages) {
    const name = `${pkg.ecosystem}-${pkg.name.replaceAll("/", "-")}-${pkg.version}`;
    if (!/^[A-Za-z0-9._@+-]+$/.test(name)) throw new Error("Invalid dependency identity");
    if (entries.has(name)) continue;
    const found = new Map(licenseFiles(pkg.root));
    // Git workspace members can inherit the license at the checkout root.
    if (pkg.git && !pkg.explicit && found.size === 0) {
      const checkout = run("git", ["rev-parse", "--show-toplevel"], pkg.root);
      for (const [file, path] of licenseFiles(checkout)) found.set(file, path);
    }
    if (pkg.explicit) found.set(basename(pkg.explicit), join(pkg.root, pkg.explicit));
    const texts = [];
    for (const [file, from] of found) {
      if (!/^[A-Za-z0-9._@+-]+$/.test(file)) continue;
      const directory = join(output, name);
      mkdirSync(directory, { recursive: true, mode: 0o700 });
      const into = join(directory, file);
      copyFileSync(from, into);
      chmodSync(into, 0o600);
      texts.push(`${name}/${file}`);
    }
    entries.set(name, { ecosystem: pkg.ecosystem, name: pkg.name, version: pkg.version,
      license: pkg.license, texts });
  }
  const ordered = [...entries].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0);
  writeFileSync(join(output, "DEPENDENCIES.json"), JSON.stringify(ordered.map(([, value]) => value), null, 2) + "\n",
    { mode: 0o600, flag: "wx" });
}
