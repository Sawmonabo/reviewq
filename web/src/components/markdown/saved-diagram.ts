export const SAVED_DIAGRAM_ID = "reviewq-saved-drawing";
export const MAX_SAVED_SVG_BYTES = 64 * 1024;
export const DRAWING_RECIPE = "mermaid-12.2.0-classic-dark-1";
const SVG_NAMESPACE = "http://www.w3.org/2000/svg";
const externalElement =
  /^(?:script|foreignObject|image|feImage|a|animate\w*|set|discard|handler|audio|video|iframe|link)$/i;
const openResource = /(?:@import\b|@font-face\b|https?:|image-set\s*\(|\\|\/\*)/i;
const cssUrl = /url\(\s*(["']?)(#[\w-]+)\1\s*\)/gi;
const fragment = /#([\w-]+)/g;
const ariaIds =
  /^(?:aria-labelledby|aria-describedby|aria-controls|aria-owns|aria-flowto|aria-activedescendant|aria-details|aria-errormessage)$/;
const keyframes = /(@keyframes\s+|(?:animation|animation-name)\s*:\s*)([-\w]+)/g;
const textProbe = "Review π 0123 mWWiq café é 漢字 🌱 العربية";
let fingerprint: Promise<string> | undefined;

export async function digestDrawing(source: string) {
  const bytes = new TextEncoder().encode(source);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

export function diagramFontIdentity() {
  fingerprint ??= measureFont();
  return fingerprint;
}

async function measureFont() {
  await document.fonts.ready;
  const canvas = document.createElement("canvas");
  canvas.width = 600;
  canvas.height = 100;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Cannot identify diagram fonts");
  const choices = [
    "14px Inter, Segoe UI, sans-serif",
    "bold 14px Inter, Segoe UI, sans-serif",
    "14px monospace",
  ];
  const metrics = choices.map((choice, index) => {
    ctx.font = choice;
    ctx.fillText(textProbe, 2, 25 + 25 * index);
    return ctx.measureText(textProbe).width;
  });
  return digestDrawing(JSON.stringify([navigator.platform, metrics, canvas.toDataURL()]));
}

export function normalizeDrawing(original: SVGSVGElement) {
  const root = original.cloneNode(true) as SVGSVGElement;
  const id = root.id;
  if (!id) return;
  const elements = [root, ...root.querySelectorAll("*")];
  const ids = new Map<string, string>([[id, SAVED_DIAGRAM_ID]]);
  for (const node of elements.slice(1)) {
    if (!node.id) continue;
    if (ids.has(node.id) || !/^[-\w]+$/.test(node.id)) return;
    const replacement = node.id.startsWith(id)
      ? node.id.replace(id, SAVED_DIAGRAM_ID)
      : `${SAVED_DIAGRAM_ID}-node-${ids.size}`;
    ids.set(node.id, replacement);
  }
  const replaceFragments = (value: string) =>
    value.replaceAll(fragment, (whole, target: string) => {
      const localized = ids.get(target);
      return localized ? `#${localized}` : whole;
    });
  const replaceUrls = (value: string) => value.replaceAll(cssUrl, replaceFragments);
  const replaceAnimations = (value: string) =>
    value.replaceAll(keyframes, (whole, property: string, name: string) => {
      if (name === "none") return whole;
      return `${property}${SAVED_DIAGRAM_ID}-keyframes-${name}`;
    });
  for (const node of elements) {
    if (node.id) node.id = ids.get(node.id)!;
    for (const attribute of [...node.attributes]) {
      if (ariaIds.test(attribute.localName)) {
        attribute.value = attribute.value
          .split(/\s+/)
          .map((key) => ids.get(key) ?? key)
          .join(" ");
      } else if (attribute.localName === "href") {
        attribute.value = replaceFragments(attribute.value);
      } else if (attribute.localName === "style" || /url\(/i.test(attribute.value)) {
        attribute.value = replaceAnimations(replaceUrls(attribute.value));
      }
    }
    if (node.localName === "style") {
      const stylesheet = replaceUrls(node.textContent ?? "").replace(
        /(^|[{}])([^{}]+)(?=\{)/g,
        (_, boundary: string, selector: string) => boundary + replaceFragments(selector),
      );
      node.textContent = replaceAnimations(stylesheet);
    }
  }

  const svg = new XMLSerializer().serializeToString(root);
  try {
    savedDiagram(svg, SAVED_DIAGRAM_ID);
    return svg;
  } catch {
    return;
  }
}

export function savedDiagram(svg: string, id: string) {
  if (
    new TextEncoder().encode(svg).length > MAX_SAVED_SVG_BYTES ||
    /<!DOCTYPE\b|<!ENTITY\b|<\?/i.test(svg)
  )
    throw new Error("Invalid saved diagram length or XML");
  const parsed = new DOMParser().parseFromString(svg, "image/svg+xml");
  if (parsed.querySelector("parsererror") || parsed.documentElement.id !== SAVED_DIAGRAM_ID)
    throw new Error("Invalid saved diagram root");
  const root = parsed.documentElement;
  const elements = [root, ...root.querySelectorAll("*")];
  const ids = new Set<string>();
  for (const node of elements) {
    if (node.namespaceURI !== SVG_NAMESPACE || externalElement.test(node.localName))
      throw new Error("Unexpected saved diagram node");
    if (node.id && (!node.id.startsWith(SAVED_DIAGRAM_ID) || ids.has(node.id)))
      throw new Error("Unscoped saved diagram ID");
    if (node.id) ids.add(node.id);
  }
  for (const node of elements) {
    for (const attribute of [...node.attributes]) {
      const { localName, value } = attribute;
      if (/^(?:on|src|base$|for$|data$)/i.test(localName))
        throw new Error("Unsafe diagram property");
      if (localName === "href" && (!value.startsWith("#") || !ids.has(value.slice(1))))
        throw new Error("External diagram reference");
      if (localName === "style" || /url\s*\(/i.test(value)) referenceCss(value, ids);
      if (
        localName === "id" ||
        localName === "href" ||
        ariaIds.test(localName) ||
        localName === "style" ||
        /url\s*\(/i.test(value)
      ) {
        attribute.value = value.replaceAll(SAVED_DIAGRAM_ID, id);
      }
    }
    if (node.localName === "style") {
      const stylesheet = node.textContent ?? "";
      referenceCss(stylesheet, ids);
      checkStyles(stylesheet);
      node.textContent = stylesheet.replaceAll(SAVED_DIAGRAM_ID, id);
    }
  }
  if (!(root instanceof SVGSVGElement)) throw new Error("Invalid saved diagram");
  const { width, height } = root.viewBox.baseVal;
  if (!Number.isFinite(width) || width <= 0 || !Number.isFinite(height) || height <= 0)
    throw new Error("Invalid saved diagram size");
  return { root, size: { width, height } };
}

function referenceCss(css: string, ids: ReadonlySet<string>) {
  const rest = css.replaceAll(cssUrl, (_, _quote: string, reference: string) => {
    if (!ids.has(reference.slice(1))) throw new Error("Unscoped diagram reference");
    return "";
  });
  if (openResource.test(rest) || /url\s*\(/i.test(rest)) throw new Error("External diagram style");
}

function checkStyles(stylesheet: string) {
  if (/@(?!keyframes\s)/i.test(stylesheet)) throw new Error("Unexpected diagram stylesheet");
  const sheet = new CSSStyleSheet();
  sheet.replaceSync(stylesheet);
  for (const rule of sheet.cssRules) {
    if (rule instanceof CSSKeyframesRule && rule.name.startsWith(`${SAVED_DIAGRAM_ID}-keyframes-`))
      continue;
    if (
      rule instanceof CSSStyleRule &&
      !rule.cssRules.length &&
      rule.selectorText
        .split(",")
        .every(
          (selector) =>
            /^#reviewq-saved-drawing(?:$|[.#:[ >])/.test(selector.trim()) && !/[+~]/.test(selector),
        )
    )
      continue;
    throw new Error("Unscoped diagram stylesheet");
  }
}
