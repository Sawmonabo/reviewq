// Slim type subset for boundary detection only.
//
// Vendored from incremark/packages/core/src/types — only the shapes
// BoundaryDetector / detector.ts touch. The upstream module also
// exports ParsedBlock, ParserOptions, etc., which depend on
// mdast/micromark and aren't needed for boundary splitting.
//
// See ./LICENSE-incremark for the incremark MIT license.

export interface BlockContext {
  readonly inFencedCode: boolean;
  readonly fenceChar?: string | undefined;
  readonly fenceLength?: number | undefined;
  readonly listDepth: number;
  readonly blockquoteDepth: number;
  readonly inContainer: boolean;
  readonly containerMarkerLength?: number | undefined;
  readonly containerName?: string | undefined;
  readonly containerDepth: number;
  readonly inList: boolean;
  readonly listOrdered?: boolean | undefined;
  readonly listIndent?: number | undefined;
  readonly listMayEnd?: boolean | undefined;
  readonly inFootnote?: boolean | undefined;
  readonly footnoteIdentifier?: string | undefined;
}

export interface ContainerConfig {
  marker?: string;
  minMarkerLength?: number;
  allowedNames?: string[];
}

export interface ContainerMatch {
  name: string;
  markerLength: number;
  isEnd: boolean;
}
