export function verifyReceipt({ decodeDeliveryReceipt, decodeFailure }, fixture) {
  const address = { submissionId: "send_42", runId: "run_active", sessionKey: "codex:active" };
  const receipt = decodeDeliveryReceipt(fixture.success, address);
  expect(receipt?.cursor === "9007199254740993", "preserves the exact Serde cursor");
  expect(receipt.data.state === "queued", "decodes the receipt state");
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

function expect(condition, description) {
  if (!condition) throw new Error(description);
}
