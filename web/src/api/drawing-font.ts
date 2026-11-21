let fingerprint: string | undefined;

export function rememberDrawingFont(font: string) {
  fingerprint = font;
}

export function drawingFontHeaders() {
  return fingerprint ? { "X-ReviewQ-Drawing-Font": fingerprint } : undefined;
}
