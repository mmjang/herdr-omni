import { transcriptTerms } from "./sessions";

export const TRANSCRIPT_PREVIEW_ROWS = 6;
export interface PreviewPart { text: string; match: boolean }

/** Wrap by terminal cells, then center the visible window on a literal match. */
export function transcriptPreview(excerpt: string, query: string, width: number, rows: number): PreviewPart[][] {
  const text = excerpt.replace(/[\x00-\x09\x0b-\x1f\x7f-\x9f]/g, " ");
  const lower = text.toLowerCase();
  const matches = new Set<number>();
  const terms = transcriptTerms(query);
  let focus = 0, paragraphOffset = 0;
  for (const paragraph of lower.split("\n\n")) {
    if (terms.length && terms.every(term => paragraph.includes(term))) {
      focus = paragraphOffset + Math.min(...terms.map(term => paragraph.indexOf(term)));
      break;
    }
    paragraphOffset += paragraph.length + 2;
  }
  for (const term of terms) {
    let start = lower.indexOf(term);
    while (start >= 0) {
      for (let index = start; index < start + term.length; index++) matches.add(index);
      start = lower.indexOf(term, start + term.length);
    }
  }
  const lines: PreviewPart[][] = [[]];
  let cells = 0, focusLine = 0;
  for (const part of new Intl.Segmenter(undefined, { granularity: "grapheme" }).segment(text)) {
    const size = Bun.stringWidth(part.segment);
    if (part.segment === "\n" || cells + size > Math.max(1, width)) { lines.push([]); cells = 0; }
    if (part.segment === "\n") continue;
    if (part.index <= focus) focusLine = lines.length - 1;
    lines.at(-1)!.push({ text: part.segment, match: matches.has(part.index) });
    cells += size;
  }
  const start = Math.max(0, focusLine - Math.floor(rows / 3));
  return lines.slice(start, start + rows);
}
