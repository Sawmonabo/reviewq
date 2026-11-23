import { execFile } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { parseArgs, promisify } from "node:util";
import { createServer as createViteServer } from "vite";
import { createDevBackend } from "./tools/dev/backend.mjs";
import { devApiProxy } from "./tools/dev/proxy.mjs";

const { values } = parseArgs({
  options: {
    profile: { type: "string" },
    installed: { type: "boolean" },
    validation: { type: "boolean" },
    "read-only": { type: "boolean" },
    "backend-port": { type: "string" },
    "native-port": { type: "string" },
  },
});
if (values.installed && values.profile)
  throw new Error("Choose the installed service or a separate development profile.");
if (
  !values.profile &&
  (values.validation || values["read-only"] || values["backend-port"] || values["native-port"])
)
  throw new Error("Backend options require --profile /absolute/dev/profile.");
const webPort = port(process.env.REVIEWQ_WEB_PORT ?? "5173");
const backendPort = port(values["backend-port"] ?? "0");
const nativePort = port(values["native-port"] ?? "0");

// Bind loopback once. Probing a wildcard port first can reserve it on WSL.
const http = createServer();
let address;
let backend;
const api = devApiProxy(
  () => address,
  () => backend?.origin(),
  values.installed || values.validation || values["read-only"],
);
const vite = await createViteServer({
  root: import.meta.dirname,
  server: {
    middlewareMode: { server: http },
    ws: { server: http },
    fs: { strict: true, allow: [import.meta.dirname] },
    proxy: { "/api/v1/": api.config },
  },
});

let terminal;
let stopping = false;
try {
  const listening = once(http, "listening");
  http.listen(webPort, "127.0.0.1");
  await listening;
  address = `http://127.0.0.1:${http.address().port}`;
  http.on("request", (request, response) => {
    if (api.allow(request, response)) vite.middlewares(request, response);
  });
  process.once("SIGINT", () => void close());
  process.once("SIGTERM", () => void close());
  console.log(`ReviewQ ${address}/`);
  if (values.installed) {
    const binary = join(homedir(), ".local/bin/reviewq");
    const { stdout } = await promisify(execFile)(binary, ["status"], { timeout: 5000 });
    const { browser, ready } = JSON.parse(stdout).data;
    if (!ready || typeof browser !== "string" || !/^127\.0\.0\.1:[1-9]\d{0,4}$/.test(browser))
      throw new Error("Start the installed ReviewQ service before opening its live frontend.");
    const origin = new URL(`http://${browser}`).origin;
    backend = { origin: () => origin };
    console.log(`Read-only preview of ${origin}. Sign in using the reviewq launcher.`);
  } else if (values.profile) {
    backend = await createDevBackend(resolve(import.meta.dirname, ".."), {
      profile: values.profile,
      backendPort,
      nativePort,
      validation: values.validation,
    });
    if (stopping) await backend.close();
    if (!stopping && process.stdin.isTTY && process.stdout.isTTY) {
      console.log("Press Enter for a one-time development browser link.");
      terminal = createInterface({ input: process.stdin });
      terminal.on("line", () => {
        void backend
          .browserLink(address)
          .then(console.log, (error) => console.error(error.message));
      });
    }
  }
} catch (error) {
  await close();
  throw error;
}

async function close() {
  if (stopping) return;
  stopping = true;
  terminal?.close();
  try {
    await backend?.close?.();
  } finally {
    await vite.close();
    http.closeAllConnections();
    http.close();
  }
}

function port(value) {
  if (
    !/^(?:0|[1-9]\d{3,4})$/.test(value) ||
    Number(value) < (value === "0" ? 0 : 1024) ||
    Number(value) > 65535
  )
    throw new Error("Ports must be 0 or an unprivileged TCP port (1024–65535).");
  return Number(value);
}
