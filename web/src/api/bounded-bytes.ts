const UTF8 = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

export function decodeUtf8(bytes: Uint8Array): string {
  return UTF8.decode(bytes);
}

export async function readBoundedBytes(response: Response, limit: number): Promise<Uint8Array> {
  if (response.body === null || response.body.locked || response.bodyUsed) {
    throw new Error("ReviewQ returned no readable response.");
  }
  const reader = response.body.getReader();
  let bytes = new Uint8Array();
  let length = 0;
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      if (chunk.value.byteLength > limit - length) {
        await reader.cancel();
        throw new Error("ReviewQ returned a response larger than requested.");
      }
      const end = length + chunk.value.byteLength;
      if (end > bytes.byteLength) {
        const grown = new Uint8Array(Math.min(limit, Math.max(end, bytes.byteLength * 2)));
        grown.set(bytes);
        bytes = grown;
      }
      bytes.set(chunk.value, length);
      length = end;
    }
    return bytes.subarray(0, length);
  } finally {
    reader.releaseLock();
  }
}
