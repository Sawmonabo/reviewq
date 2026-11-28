import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { createReadStream } from "node:fs";
import {
  chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  readdirSync, realpathSync, renameSync, rmSync, writeFileSync,
} from "node:fs";
import { join, resolve } from "node:path";
import { homedir, tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { notices } from "./licenses.mjs";

const root = fileURLToPath(new URL("../../", import.meta.url));
const run = (program, args, cwd = root) =>
  execFileSync(program, args, { cwd, encoding: "utf8", maxBuffer: 24 * 1024 * 1024 }).trim();
const digest = (bytes) => createHash("sha256").update(bytes).digest("hex");
const hashFile = async (path) => {
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest("hex");
};
const json = (value) => JSON.stringify(value) + "\n";
const clean = () => !run("git", ["status", "--porcelain"]);

function files(root, prefix = "") {
  const inventory = [];
  for (const name of readdirSync(join(root, prefix)).sort()) {
    if (!/^[A-Za-z0-9._@+-]+$/.test(name)) throw new Error(`Unsupported package filename ${name}`);
    const relative = prefix ? `${prefix}/${name}` : name;
    const meta = lstatSync(join(root, relative));
    if (meta.isDirectory()) inventory.push(...files(root, relative));
    else if (meta.isFile()) inventory.push(relative);
    else throw new Error(`Expected a regular package file at ${relative}`);
  }
  return inventory;
}

function put(path, bytes) {
  writeFileSync(path, bytes, { mode: 0o600, flag: "wx" });
}

function copyTree(from, to) {
  mkdirSync(to, { recursive: true, mode: 0o700 });
  for (const path of files(from)) {
    const target = join(to, path);
    mkdirSync(resolve(target, ".."), { recursive: true, mode: 0o700 });
    copyFileSync(join(from, path), target);
    chmodSync(target, 0o600);
  }
}

async function hostVersion(hosts, version) {
  const pluginPath = join(hosts, "claude/plugin/.claude-plugin/plugin.json");
  const marketPath = join(hosts, "claude/.claude-plugin/marketplace.json");
  const source = await Promise.all(files(join(hosts, "claude")).sort().map(async (path) =>
    [path, await hashFile(join(hosts, "claude", path))],
  ));
  const nativeVersion = `${version.split("+", 1)[0]}+reviewq.${digest(json(source)).slice(0, 20)}`;
  const plugin = JSON.parse(readFileSync(pluginPath, "utf8"));
  const market = JSON.parse(readFileSync(marketPath, "utf8"));
  const entry = market.plugins.find((item) => item.name === "reviewq");
  if (plugin.name !== "reviewq" || market.name !== "reviewq" || entry?.source !== "./plugin") {
    throw new Error("Invalid ReviewQ Claude plugin or marketplace identity");
  }
  plugin.version = nativeVersion;
  market.metadata = { ...market.metadata, version: nativeVersion };
  entry.version = nativeVersion;
  writeFileSync(pluginPath, json(plugin));
  writeFileSync(marketPath, json(market));
}

function validateClaude(marketplace) {
  const selected = process.env.REVIEWQ_PACKAGE_CLAUDE;
  const executable = selected || join(homedir(), ".local/bin/claude");
  if (!existsSync(executable)) {
    if (selected) throw new Error("REVIEWQ_PACKAGE_CLAUDE does not exist");
    console.log("Claude validation unavailable. Set REVIEWQ_PACKAGE_CLAUDE to select the host.");
    return;
  }
  const config = mkdtempSync(join(tmpdir(), "reviewq-claude-validate."));
  try {
    const verify = (args) => execFileSync(resolve(executable), args, {
      cwd: config, env: { HOME: config, CLAUDE_CONFIG_DIR: config, PATH: process.env.PATH },
      encoding: "utf8", timeout: 25_000,
    }).trim();
    const version = verify(["--version"]);
    for (const directory of [marketplace, join(marketplace, "plugin")]) {
      const result = JSON.parse(verify(["plugin", "validate", "--strict", "--json", directory]));
      if (result.success !== true) throw new Error("Claude strict validation failed");
    }
    console.log(`Claude strict marketplace and plugin validation passed (${version})`);
  } finally {
    rmSync(config, { recursive: true, force: true });
  }
}

function checkWeb(web, inventory) {
  for (const entry of ["index.html", "diagram-producer.html"]) {
    const index = readFileSync(join(web, entry), "utf8");
    if (!/<script\b[^>]*type="module"/.test(index)) throw new Error(`Missing production module entry ${entry}`);
    for (const [, reference] of index.matchAll(/\b(?:src|href)="(\/[^"]+)"/g)) {
      if (!inventory.has(reference.slice(1).split(/[?#]/, 1)[0])) {
        throw new Error(`Missing web entry reference ${reference}`);
      }
    }
  }
  for (const entry of ["stopped.html", "worker.js", "manifest.webmanifest"]) {
    if (!inventory.has(entry)) throw new Error(`Missing web file ${entry}`);
  }
  // The service swaps this one literal to show the stopped page's Start button.
  if (readFileSync(join(web, "stopped.html"), "utf8").split('data-door="off"').length !== 2) {
    throw new Error('stopped.html must contain data-door="off" exactly once');
  }
  for (const path of inventory) {
    if (!path.endsWith(".js")) continue;
    const code = readFileSync(join(web, path), "utf8");
    for (const [, target] of code.matchAll(/(?:import\(\s*|from\s*)["']\.\/([A-Za-z0-9_@.+-]+\.(?:js|css))["']/g)) {
      const nearby = path.slice(0, path.lastIndexOf("/") + 1) + target;
      if (!inventory.has(nearby)) throw new Error(`Missing lazy web asset ${nearby}`);
    }
  }
}

async function main() {
  if (!clean()) throw new Error("Package an exact committed source tree");
  const source = run("git", ["rev-parse", "HEAD"]);
  const platform = run("rustc", ["-vV"]).match(/^host: (.+)$/m)?.[1];
  if (!platform || !/^[A-Za-z0-9_-]+-linux-[A-Za-z0-9_-]+$/.test(platform)) {
    throw new Error("The current target is not a supported native Linux target");
  }
  const metadata = JSON.parse(run("cargo", ["metadata", "--locked", "--format-version", "1", "--filter-platform", platform]));
  const application = metadata.packages.find((item) => item.name === "reviewq");
  const target = metadata.target_directory;
  const dist = resolve(root, "web/dist");
  const requested = process.argv[2] ? resolve(process.argv[2]) : join(target, "package");
  if (process.argv.length > 3) throw new Error("Usage: assemble.mjs [output-parent]");
  mkdirSync(requested, { recursive: true, mode: 0o700 });
  if (lstatSync(requested).isSymbolicLink()) throw new Error("Package output is a symlink");
  // The bundle check takes only a canonical path, and a build driver may link the target folder.
  const output = realpathSync(requested);
  const directory = join(output, `reviewq-${application.version}-${platform}`);
  const stage = mkdtempSync(join(output, ".reviewq-package."));
  try {
    const binary = join(stage, "bin/reviewq");
    mkdirSync(join(stage, "bin"), { mode: 0o700 });
    copyFileSync(join(target, "release/reviewq"), binary);
    chmodSync(binary, 0o700);
    copyTree(dist, join(stage, "web"));
    checkWeb(join(stage, "web"), new Set(files(join(stage, "web"))));
    copyTree(join(root, "hosts"), join(stage, "hosts"));
    await hostVersion(join(stage, "hosts"), application.version);
    validateClaude(join(stage, "hosts/claude"));
    copyTree(join(root, "packaging"), join(stage, "packaging"));
    notices(metadata, application.id, root, join(stage, "licenses"), run);
    const inventory = await Promise.all(files(stage).sort().map(async (path) => {
      const absolute = join(stage, path);
      const meta = lstatSync(absolute);
      return { path, mode: meta.mode & 0o777, size: meta.size, sha256: await hashFile(absolute) };
    }));
    const manifest = json({ format: 1, version: application.version, target: platform, source,
      protocol: 1, store: 2, files: inventory });
    put(join(stage, "manifest.json"), manifest);
    if (!clean() || source !== run("git", ["rev-parse", "HEAD"])) throw new Error("Source changed while packaging");
    if (run(binary, ["--version"], stage) !== `reviewq ${application.version}`) throw new Error("Wrong staged binary version");
    run(binary, ["--help"], stage);
    run(binary, ["internal", "verify-bundle", stage], stage);
    const elf = run("readelf", ["--wide", "--program-headers", binary], stage);
    const loader = elf.match(/Requesting program interpreter: ([^\]]+)\]/)?.[1];
    console.log(loader ? `Platform loader ${loader}` : "Static executable (no ELF interpreter)");
    if (existsSync(directory)) {
      if (lstatSync(directory).isSymbolicLink() ||
          readFileSync(join(directory, "manifest.json"), "utf8") !== manifest) {
        throw new Error(`A different release already exists at ${directory}`);
      }
      run(binary, ["internal", "verify-bundle", directory], stage);
    } else {
      renameSync(stage, directory);
    }
    console.log(`${directory}\nBundle SHA-256 ${digest(manifest)}`);
  } finally {
    rmSync(stage, { recursive: true, force: true });
  }
}

await main();
