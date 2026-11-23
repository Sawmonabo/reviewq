import { spawn } from "node:child_process";
import { constants, watch } from "node:fs";
import { copyFile, lstat, mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve, sep } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";

const ROOT_INPUTS = new Set(["Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"]);
const DEBOUNCE_MS = 200;
const SHUTDOWN_MS = 15_000;
const STARTUP_MS = 30_000;

export async function createDevBackend(root, options) {
  const profile = await prepareProfile(root, options.profile);
  const scratch = await mkdtemp(join(tmpdir(), "reviewq-dev-"));
  const web = join(scratch, "web");
  await mkdir(web);
  await writeFile(join(web, "index.html"), "<title>ReviewQ development backend</title>");
  const watchers = [];
  let browserPort = options.backendPort;
  let generation = 0;
  let timer;
  let build;
  let building;
  let server;
  let disposed = false;

  function requestBuild() {
    generation++;
    schedule();
  }

  function schedule() {
    if (disposed || building) return;
    clearTimeout(timer);
    timer = setTimeout(() => {
      const requested = generation;
      building = rebuild(requested)
        .catch((error) => console.error(error.message))
        .finally(() => {
          building = undefined;
          if (requested !== generation) schedule();
        });
    }, DEBOUNCE_MS);
  }

  async function rebuild(requested) {
    console.log("Building the Rust backend from this checkout…");
    build = command(
      "cargo",
      [
        "build",
        "--locked",
        "-p",
        "reviewq",
        "--bin",
        "reviewq",
        "--message-format=json-render-diagnostics",
      ],
      { cwd: root, detached: true },
    );
    let artifact;
    try {
      artifact = await executable(build);
    } finally {
      build = undefined;
    }
    if (disposed || requested !== generation) return;
    if (!artifact) {
      console.log(
        server
          ? "Build failed. The previous backend is still running."
          : "Build failed. Save a Rust change to retry.",
      );
      return;
    }

    const directory = await mkdtemp(join(scratch, "build-"));
    const candidate = join(directory, "reviewq");
    try {
      await copyFile(artifact, candidate, constants.COPYFILE_FICLONE);
      if (disposed || requested !== generation) return;
      const previous = server?.binary;
      if (server) {
        const stopped = await stop(server);
        if (!stopped) throw new Error("The previous development backend has not stopped.");
        server = undefined;
      }
      if (disposed) return;
      try {
        await start(candidate);
      } catch (error) {
        if (previous && !disposed) {
          console.error("The new backend failed to start. Restoring the previous build.");
          await start(previous);
        }
        throw error;
      } finally {
        if (previous && previous !== server?.binary) await removeBuild(previous);
      }
    } finally {
      if (candidate !== server?.binary) await removeBuild(candidate);
    }
  }

  async function start(binary) {
    const flags = options.validation
      ? ["serve", "--validation", "--runtime-root", profile]
      : ["--profile", profile, "serve"];
    const running = command(binary, [
      ...flags,
      "--web",
      web,
      "--port",
      String(browserPort),
      "--native-port",
      String(options.nativePort),
    ]);
    server = { ...running, binary };
    const current = server;
    try {
      const origin = await ready(current);
      if (disposed) return;
      current.origin = origin;
      browserPort = Number(new URL(origin).port);
      console.log(`Rust backend ready on ${origin}`);
    } catch (error) {
      await stop(current, true);
      if (server === current) server = undefined;
      throw error;
    }
    void current.finished.then(() => {
      if (server !== current) return;
      current.origin = undefined;
      if (!disposed && !building)
        console.error("The development backend exited. Save a Rust change to restart it.");
    });
  }

  function follow(path, settings, changed) {
    const watcher = watch(join(root, path), settings, changed);
    watcher.on("error", (error) =>
      console.error(`Rust source watch failed for ${path}: ${error.message}`),
    );
    watchers.push(watcher);
  }

  try {
    follow(".", {}, (_kind, name) => {
      if (!name || ROOT_INPUTS.has(name)) requestBuild();
    });
    for (const path of ["crates", ".cargo", "hosts/shared/skills/reviewq-manage"])
      follow(path, { recursive: true }, requestBuild);
    requestBuild();
  } catch (error) {
    for (const watcher of watchers) watcher.close();
    await rm(scratch, { recursive: true, force: true });
    throw error;
  }

  return {
    origin: () => server?.origin,
    async close() {
      disposed = true;
      clearTimeout(timer);
      for (const watcher of watchers) watcher.close();
      if (build) signal(build, "SIGTERM", true);
      if (server) signal(server, "SIGTERM");
      if (build) await stop(build, true, true);
      await building;
      if (server) await stop(server, true);
      await rm(scratch, { recursive: true, force: true });
    },
  };
}

async function prepareProfile(root, input) {
  if (!isAbsolute(input)) throw new Error("Use an absolute path for the development profile.");
  const profile = await canonicalPath(resolve(input));
  const web = await realpath(join(root, "web"));
  if (within(profile, web))
    throw new Error("Put the development profile outside the web source directory.");
  const data = process.env.XDG_DATA_HOME;
  const installed = await canonicalPath(
    resolve(data && isAbsolute(data) ? data : join(homedir(), ".local/share"), "reviewq"),
  );
  if (within(profile, installed))
    throw new Error("Use a profile separate from the installed ReviewQ profile.");
  if ((await exists(join(profile, "current"))) || (await exists(join(profile, "releases"))))
    throw new Error("Use a separate profile for development, outside the installed release.");
  if (await exists(join(profile, "service.json")))
    throw new Error(
      "The profile already has a service descriptor. Stop its existing service first.",
    );
  return profile;
}

function within(path, directory) {
  return path === directory || path.startsWith(directory + sep);
}

async function canonicalPath(path) {
  try {
    return await realpath(path);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    const parent = dirname(path);
    if (parent === path) throw error;
    return join(await canonicalPath(parent), basename(path));
  }
}

async function exists(path) {
  try {
    await lstat(path);
    return true;
  } catch (error) {
    if (error.code === "ENOENT") return false;
    throw error;
  }
}

function command(binary, args, options = {}) {
  const child = spawn(binary, args, { ...options, stdio: ["ignore", "pipe", "inherit"] });
  let error;
  const finished = new Promise((resolve) => {
    child.once("error", (failure) => {
      error = failure;
    });
    child.once("close", (status) => resolve({ status, error }));
  });
  return { child, finished };
}

async function executable(process) {
  let binary;
  for await (const line of createInterface({ input: process.child.stdout, crlfDelay: Infinity })) {
    const event = JSON.parse(line);
    if (event.reason === "compiler-message" && event.message.rendered)
      console.error(event.message.rendered.trimEnd());
    if (event.reason === "compiler-artifact" && event.target.name === "reviewq" && event.executable)
      binary = event.executable;
  }
  const result = await process.finished;
  if (result.error) throw new Error(`Could not start Cargo: ${result.error.message}`);
  return result.status === 0 ? binary : undefined;
}

async function ready(process) {
  const output = createInterface({ input: process.child.stdout, crlfDelay: Infinity });
  try {
    return await Promise.race([
      waitForOrigin(output),
      process.finished.then(({ error }) => {
        throw new Error(
          error
            ? `Could not start ReviewQ: ${error.message}`
            : "The development backend exited before readiness.",
        );
      }),
      delay(STARTUP_MS, undefined, { ref: false }).then(() => {
        throw new Error("The development backend did not start in 30 seconds.");
      }),
    ]);
  } finally {
    output.close();
    process.child.stdout.resume();
  }
}

async function waitForOrigin(output) {
  for await (const line of output) {
    const match = /^ReviewQ (http:\/\/127\.0\.0\.1:[1-9]\d{0,4})\/$/.exec(line);
    if (match) return new URL(match[1]).origin;
  }
  throw new Error("The development backend did not report its loopback address.");
}

function signal(process, value, group = false) {
  if (process.child.exitCode !== null || process.child.signalCode !== null) return;
  if (group && process.child.pid) {
    try {
      globalThis.process.kill(-process.child.pid, value);
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  } else {
    process.child.kill(value);
  }
}

async function stop(process, force = false, group = false) {
  signal(process, "SIGTERM", group);
  const stopped = await Promise.race([
    process.finished.then(() => true),
    delay(SHUTDOWN_MS, false, { ref: false }),
  ]);
  if (!stopped && force) {
    signal(process, "SIGKILL", group);
    await process.finished;
    return true;
  }
  return stopped;
}

async function removeBuild(binary) {
  await rm(dirname(binary), { recursive: true, force: true });
}
