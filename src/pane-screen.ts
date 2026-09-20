import type { PreviewPart } from "./transcript-preview";

/** Crop a terminal snapshot, never reflow its columns. Follow the bottom by default. */
export function paneScreen(text: string, width: number, rows: number, offsetFromBottom = 0) {
  const source = text.split("\n");
  // Empty rows below the last rendered content need not hide an input/approval prompt.
  while (source.length && !source.at(-1)!.trim()) source.pop();
  const capacity = Math.max(1, rows);
  const offset = Math.min(Math.max(0, offsetFromBottom), Math.max(0, source.length - capacity));
  const end = Math.max(0, source.length - offset);
  const start = Math.max(0, end - capacity);
  const columns = Math.max(1, width);
  let clipped = false;
  const lines: PreviewPart[][] = source.slice(start, end).map(line => {
    const trimmed = line.trimEnd();
    if (Bun.stringWidth(trimmed) <= columns) return [{ text: trimmed, match: false }];
    clipped = true;
    let result = "", cells = 0;
    for (const { segment } of new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(trimmed)) {
      const size = Bun.stringWidth(segment);
      if (cells + size > columns - 1) break;
      result += segment; cells += size;
    }
    return [{ text: `${result}…`, match: false }];
  });
  return { lines, offset, start, end, totalRows: source.length, clipped };
}
