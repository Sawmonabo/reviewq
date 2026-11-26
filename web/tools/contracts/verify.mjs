import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { build } from "vite";
import { verifyFirstReview, verifyReceipt } from "./cases.mjs";
import { verifyReportLayout } from "./report-layout.mjs";

const [scratch, mode] = process.argv.slice(2);
const decoderPath = join(scratch, "receipt-decoder.mjs");
const casesUrl = new URL("./cases.mjs", import.meta.url);
const additiveUrl = new URL(
  "../../../fixtures/contracts/delivery-receipt-additive.json",
  import.meta.url,
);
const [success, failure] = (await readFile(join(scratch, "receipt.ndjson"), "utf8")).split("\n");
const patchUrl = new URL("../../../fixtures/contracts/example.patch", import.meta.url);
const fixture = {
  success,
  failure,
  additive: await readFile(additiveUrl, "utf8"),
  payloads: await readPayloads(),
  expectedPatchETag: `"${createHash("sha256")
    .update(await readFile(patchUrl))
    .digest("hex")}"`,
};

await build({
  configFile: false,
  build: {
    outDir: scratch,
    emptyOutDir: false,
    lib: {
      entry: fileURLToPath(new URL("./receipt-decoder.ts", import.meta.url)),
      formats: ["es"],
      fileName: () => "receipt-decoder.mjs",
    },
  },
});
const decoder = await import(pathToFileURL(decoderPath).href);
verifyReceipt(decoder, fixture);
await verifyFirstReview(decoder, fixture);
console.log("First-review contracts passed in Node.");

// The browser reader recognises every part of the layout the Rust composer writes. An SSR
// build resolves the Markdown packages' Node variants, which load without a document.
const readerDir = join(scratch, "report-reader");
await build({
  configFile: false,
  publicDir: false,
  logLevel: "warn",
  ssr: { noExternal: true },
  build: {
    outDir: readerDir,
    emptyOutDir: false,
    ssr: true,
    rollupOptions: {
      input: {
        "parse-markdown": source("components/markdown/parse-markdown.ts"),
        "report-reader": source("features/review/report/report-reader.ts"),
      },
      output: { entryFileNames: "[name].mjs" },
    },
  },
});
const reader = {
  ...(await import(pathToFileURL(join(readerDir, "parse-markdown.mjs")).href)),
  ...(await import(pathToFileURL(join(readerDir, "report-reader.mjs")).href)),
};
verifyReportLayout(
  reader,
  await readFile(new URL("../../../fixtures/reviews/ledger/worked.md", import.meta.url), "utf8"),
);
console.log("The report reader recognises the composed layout.");

if (mode === "--browser") {
  await verifyInBrowser();
  console.log("First-review contracts passed in Chromium.");
}

function source(path) {
  return fileURLToPath(new URL(`../../src/${path}`, import.meta.url));
}

async function readPayloads() {
  const names = [
    "queue",
    "overview",
    "selection",
    "selectionUnstarted",
    "run",
    "patchPending",
    "patchA",
    "patchB",
    "history",
    "historyOversized",
    "observedA",
    "observedB",
    "sendRequest",
    "eventReplayCursor",
    "invalidate",
    "reset",
  ];
  const entries = await Promise.all(
    names.map(async (name) => [
      name,
      await readFile(join(scratch, "fixtures", `${name}.json`), "utf8"),
    ]),
  );
  return Object.fromEntries(entries);
}

async function verifyInBrowser() {
  const { chromium } = await import("@playwright/test");
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage();
    const origin = "http://127.0.0.1";
    await page.route(`${origin}/`, (route) =>
      route.fulfill({
        contentType: "text/html",
        body: "<!doctype html><title>Receipt contract</title>",
      }),
    );
    await page.route(`${origin}/cases.mjs`, (route) =>
      route.fulfill({ contentType: "text/javascript", path: fileURLToPath(casesUrl) }),
    );
    await page.route(`${origin}/receipt-decoder.mjs`, (route) =>
      route.fulfill({ contentType: "text/javascript", path: decoderPath }),
    );
    await page.goto(origin);
    await page.evaluate(async (wireFixture) => {
      const cases = await import("/cases.mjs");
      const decoder = await import("/receipt-decoder.mjs");
      cases.verifyReceipt(decoder, wireFixture);
      await cases.verifyFirstReview(decoder, wireFixture);
    }, fixture);
  } finally {
    await browser.close();
  }
}
