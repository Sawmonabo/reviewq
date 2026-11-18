import { execFileSync } from "node:child_process";
import { join, relative } from "node:path";

const allowed = {
  "reviewq-domain": [],
  "reviewq-protocol": [],
  "reviewq-app": ["reviewq-domain"],
  "reviewq-store": ["reviewq-app", "reviewq-domain"],
  "reviewq-providers": ["reviewq-app", "reviewq-domain"],
  "reviewq-transport": ["reviewq-app", "reviewq-domain", "reviewq-protocol"],
  reviewq: ["reviewq-app", "reviewq-protocol", "reviewq-store", "reviewq-providers", "reviewq-transport"],
};

const metadata = JSON.parse(
  execFileSync("cargo", ["metadata", "--locked", "--no-deps", "--format-version", "1"], { encoding: "utf8" }),
);
const packages = metadata.packages.filter((pkg) => metadata.workspace_members.includes(pkg.id));
const memberNames = new Set(packages.map((pkg) => pkg.name));
const errors = [];
for (const pkg of packages) {
  const actualManifest = relative(metadata.workspace_root, pkg.manifest_path);
  const expectedManifest = join("crates", pkg.name, "Cargo.toml");
  if (actualManifest !== expectedManifest) {
    errors.push(`${pkg.name} must use ${expectedManifest}, found ${actualManifest}`);
  }
  if (!Object.hasOwn(allowed, pkg.name)) {
    errors.push(`Choose an import policy for new workspace member ${pkg.name}`);
    continue;
  }
  for (const dep of pkg.dependencies) {
    if (memberNames.has(dep.name) && !allowed[pkg.name].includes(dep.name)) {
      errors.push(`${pkg.name} may not import ${dep.name} (${dep.kind ?? "normal"})`);
    }
    if (pkg.name === "reviewq-domain" && !memberNames.has(dep.name) && dep.kind !== "dev") {
      errors.push(`Keep ${dep.name} outside the dependency-free domain`);
    }
    if (["rusqlite", "libsqlite3-sys"].includes(dep.name) && pkg.name !== "reviewq-store") {
      errors.push(`Only reviewq-store owns the SQLite driver, found ${dep.name} in ${pkg.name}`);
    }
  }
}
if (errors.length) {
  console.error(errors.join("\n"));
  process.exitCode = 1;
}
