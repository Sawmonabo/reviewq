export function verifyReceipt({ decodeDeliveryReceipt, decodeFailure }, fixture) {
  const address = { submissionId: "send_42", runId: "run_active", sessionKey: "codex:active" };
  const receipt = decodeDeliveryReceipt(fixture.success, address);
  expect(receipt?.cursor === "9007199254740993", "preserves the exact Serde cursor");
  expect(
    receipt.data.state === "queued" && receipt.data.observedInput === null,
    "decodes the queued receipt without an observed input",
  );
  expect(
    decodeDeliveryReceipt(fixture.additive, address)?.cursor === receipt.cursor,
    "accepts an additive v1 field",
  );
  expect(
    !decodeDeliveryReceipt(fixture.success, { ...address, runId: "another_run" }),
    "rejects a different address",
  );

  const changed = (cursor, data = receipt.data) => JSON.stringify({ data, cursor });
  expect(
    decodeDeliveryReceipt(changed("9223372036854775807"), address),
    "accepts the maximum cursor",
  );
  expect(!decodeDeliveryReceipt(changed(9007199254740992), address), "rejects a numeric cursor");
  expect(
    !decodeDeliveryReceipt(changed("9223372036854775808"), address),
    "rejects cursor overflow",
  );
  expect(!decodeDeliveryReceipt(changed("01"), address), "rejects a leading zero");
  expect(
    !decodeDeliveryReceipt(changed(receipt.cursor, { ...receipt.data, state: "unknown" }), address),
    "rejects an incompatible delivery state",
  );
  expect(
    !decodeDeliveryReceipt(
      changed(receipt.cursor, { ...receipt.data, createdAt: "2026-02-31T00:00:00.000Z" }),
      address,
    ),
    "rejects an impossible calendar date",
  );

  const multibyteAddress = { ...address, sessionKey: "😀".repeat(128) };
  expect(
    decodeDeliveryReceipt(
      changed(receipt.cursor, { ...receipt.data, ...multibyteAddress }),
      multibyteAddress,
    ),
    "accepts the 512-byte identifier",
  );
  multibyteAddress.sessionKey += "x";
  expect(
    !decodeDeliveryReceipt(
      changed(receipt.cursor, { ...receipt.data, ...multibyteAddress }),
      multibyteAddress,
    ),
    "rejects more than 512 UTF-8 bytes",
  );

  const failure = decodeFailure(fixture.failure);
  expect(
    failure?.cursor === null && failure.error.code === "not_found",
    "reads the required null cursor",
  );
  expect(
    !decodeFailure(JSON.stringify({ error: failure.error })),
    "rejects an omitted failure cursor",
  );
}

export async function verifyFirstReview(
  { firstReviewDecoders: read, decodeDeliveryReceipt, findObservedInput },
  { payloads, expectedPatchETag },
) {
  const reviewId = "rq-alpha";
  const runId = "run-active";
  const sessionKey = "codex:active";
  const queue = read.queue(payloads.queue);
  const overview = read.overview(payloads.overview);
  const selection = read.selection(payloads.selection, { reviewId, runId });
  const run = read.run(payloads.run);

  expect(queue?.data.items[0]?.reviewId === reviewId, "reads the queued review");
  expect(queue.data.items[1]?.kind === "unmatched_ticket", "reads a Jira-only queue row");
  expect(queue.data.nextCursor === null, "reads the required nullable page cursor");
  expect(
    overview?.data.review.reviewId === reviewId &&
      overview.data.runs.items[0]?.runId === runId &&
      overview.data.checkpointId === "displayed-frontier",
    "reads overview with its checkpoint and run",
  );
  expect(
    selection?.data.runId === runId &&
      selection.data.leadSessionKey === sessionKey &&
      run?.data.reviewId === reviewId &&
      run.data.leadSessionKey === sessionKey,
    "reads the exact scoped lead and run",
  );
  expect(
    read.selection(payloads.selectionUnstarted, { reviewId })?.data.leadSessionKey === null &&
      !read.selection(payloads.selectionUnstarted, { reviewId, runId }) &&
      !read.selection(payloads.selection, { reviewId: "rq-beta" }) &&
      !read.selection(payloads.selection, { reviewId, runId: "run-other" }),
    "requires the requested review and any explicit run before enabling actions",
  );
  expect(
    !read.selection(JSON.stringify({ ...selection, data: { ...selection.data, runId: null } }), {
      reviewId,
    }) &&
      !read.selection(
        JSON.stringify({ ...selection, data: { ...selection.data, leadSessionKey: null } }),
        { reviewId },
      ),
    "rejects either half of an identified lead",
  );

  const patchAddress = { reviewId, revision: "patch-a" };
  const pending = read.patch(payloads.patchPending, patchAddress)?.data.items[0];
  const fileA = read.patch(payloads.patchA, patchAddress)?.data.items[0];
  const fileB = read.patch(payloads.patchB, { reviewId, revision: "patch-b" })?.data.items[0];
  expect(pending?.patchETag === null, "reads a file before its digest is known");
  expect(
    fileA?.patchETag === expectedPatchETag &&
      fileB?.patchETag === expectedPatchETag &&
      fileA.fileId === fileB.fileId &&
      fileA.revision !== fileB.revision,
    "keeps revision-specific anchors when the patch bytes are identical",
  );
  expect(!read.patch(payloads.patchB, patchAddress), "rejects the same bytes at another revision");

  const address = { runId, sessionKey };
  const encoder = new TextEncoder();
  const history = await read.readHistory(new Response(encoder.encode(payloads.history)), address);
  expect(
    history?.data.runId === runId &&
      history.data.sessionKey === sessionKey &&
      history.data.items[0]?.timestamp === null &&
      history.data.items[0]?.parts[0]?.kind === "assistant_text",
    "reads native session history with unknown time",
  );
  const oversized = encoder.encode(payloads.historyOversized);
  let offset = 0;
  let cancelled = false;
  const stream = new ReadableStream({
    pull(controller) {
      if (offset >= oversized.byteLength) {
        controller.close();
        return;
      }
      controller.enqueue(oversized.subarray(offset, offset + 32 * 1024));
      offset += 32 * 1024;
    },
    cancel() {
      cancelled = true;
    },
  });
  expect(
    payloads.historyOversized.length < 256 * 1024 &&
      oversized.byteLength > 256 * 1024 &&
      !read.history(oversized, address) &&
      !(await read.readHistory(new Response(stream), address)) &&
      cancelled,
    "rejects the encoded page before decoding or collecting the oversized stream",
  );
  const observedA = decodeDeliveryReceipt(payloads.observedA, {
    ...address,
    submissionId: "send_identical_a",
  });
  const observedB = decodeDeliveryReceipt(payloads.observedB, {
    ...address,
    submissionId: "send_identical_b",
  });
  const entryA = observedA && findObservedInput(observedA.data, history.data);
  const entryB = observedB && findObservedInput(observedB.data, history.data);
  expect(
    entryA?.entryId === "native-input-a" &&
      entryB?.entryId === "native-input-b" &&
      entryA.turnId === entryB.turnId &&
      entryA.parts[0]?.text === entryB.parts[0]?.text &&
      !findObservedInput(observedA.data, { ...history.data, sessionKey: "codex:other" }),
    "matches identical sends by their exact confirmed native entry within the selected session",
  );
  expect(
    !decodeDeliveryReceipt(
      JSON.stringify({ ...observedA, data: { ...observedA.data, observedInput: null } }),
      { ...address, submissionId: "send_identical_a" },
    ),
    "rejects started receipts without matched native input",
  );
  const message = read.sendRequest(payloads.sendRequest);
  expect(message?.submissionId === "send_42", "reads the exact serialized mutation request");
  expect(
    !read.sendRequest(JSON.stringify({ ...message, actor: "checkout" })),
    "rejects a client-supplied actor",
  );
  expect(
    read.eventReplayCursor(payloads.eventReplayCursor) === "9007199254740993",
    "preserves the SSE replay cursor",
  );
  const invalidate = read.invalidate(payloads.invalidate);
  expect(
    invalidate?.resources[0]?.id === runId &&
      invalidate.resources[1]?.kind === "queue" &&
      !Object.hasOwn(invalidate.resources[1], "id"),
    "reads the exact invalidation and omitted resource ID",
  );
  expect(read.reset(payloads.reset)?.reason === "cursor_expired", "reads the terminal reset");
}

function expect(condition, description) {
  if (!condition) throw new Error(description);
}
