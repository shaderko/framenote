export type TimelineEntryType = "bookmark" | "ai" | "subtitle";

export interface TimelineEntry {
  id: string;
  type: TimelineEntryType;
  start: number;
  end?: number;
  text: string;
  editable: boolean;
  line: number;
  speaker?: string;
  language?: string;
  transcriptComplete?: boolean;
  source?: "embedded-chapter";
}

export interface AnalysisChunk {
  start: number;
  end: number;
}

export function planAnalysisChunks(
  duration: number,
  start: number,
  chunkSeconds: number,
  chunkCount: number,
): AnalysisChunk[] {
  if (!Number.isFinite(duration) || duration <= 0) return [];
  const safeStart = Math.max(0, Math.min(duration, Number.isFinite(start) ? start : 0));
  const safeSize = Math.max(10, Math.min(300, Number.isFinite(chunkSeconds) ? chunkSeconds : 60));
  const safeCount = Math.max(1, Math.min(50, Math.floor(Number.isFinite(chunkCount) ? chunkCount : 1)));
  const chunks: AnalysisChunk[] = [];
  for (let index = 0; index < safeCount; index += 1) {
    const chunkStart = safeStart + index * safeSize;
    if (chunkStart >= duration) break;
    chunks.push({ start: chunkStart, end: Math.min(duration, chunkStart + safeSize) });
  }
  return chunks;
}

export function timestampToSeconds(value: string): number | null {
  const parts = value.trim().split(":").map(Number);
  if (parts.some(Number.isNaN) || parts.length < 2 || parts.length > 3) return null;
  if (parts.length === 2) return parts[0] * 60 + parts[1];
  return parts[0] * 3600 + parts[1] * 60 + parts[2];
}

export function formatTime(value: number, compact = false): string {
  if (!Number.isFinite(value)) return compact ? "0:00" : "00:00:00";
  const total = Math.max(0, Math.round(value));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (compact && hours === 0) return `${minutes}:${String(seconds).padStart(2, "0")}`;
  return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

export function formatTimecode(value: number): string {
  const milliseconds = Math.max(0, Math.round((Number.isFinite(value) ? value : 0) * 1000));
  const hours = Math.floor(milliseconds / 3_600_000);
  const minutes = Math.floor((milliseconds % 3_600_000) / 60_000);
  const seconds = Math.floor((milliseconds % 60_000) / 1000);
  const fraction = milliseconds % 1000;
  return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}.${String(fraction).padStart(3, "0")}`;
}

export function nextSubtitleStart(playhead: number, cueStart: number, cueEnd: number): number {
  const safePlayhead = Math.max(0, Number.isFinite(playhead) ? playhead : 0);
  return safePlayhead >= cueStart && safePlayhead < cueEnd ? cueEnd : safePlayhead;
}

export function parseTimeline(markdown: string): TimelineEntry[] {
  let section: TimelineEntryType | null = null;
  const entries: TimelineEntry[] = [];

  markdown.split(/\r?\n/).forEach((line, lineNumber) => {
    const trimmed = line.trim();
    if (/^#{1,6}\s+/i.test(trimmed)) {
      const heading = trimmed.replace(/^#{1,6}\s+/, "").toLowerCase();
      section = heading.includes("bookmark") || heading === "notes"
        ? "bookmark"
        : heading.includes("subtitle") || heading.includes("transcript")
          ? "subtitle"
          : heading.includes("ai")
            ? "ai"
            : null;
      return;
    }

    const match = trimmed.match(
      /^[-*+]\s*\[(\d{1,2}:\d{2}(?::\d{2}(?:\.\d{1,3})?)?)(?:\s*[–—-]\s*(\d{1,2}:\d{2}(?::\d{2}(?:\.\d{1,3})?)?))?\]\s*(.*?)(?:\s*<!--\s*(.*?)\s*-->)?\s*$/,
    );
    if (!match) return;

    const start = timestampToSeconds(match[1]);
    const end = match[2] ? timestampToSeconds(match[2]) : null;
    if (start === null) return;

    const marker = match[4] ?? "";
    const bookmarkId = marker.match(/framenote:bookmark:([\w-]+)/i)?.[1];
    const aiId = marker.match(/framenote:ai:([\w-]+)/i)?.[1];
    const subtitleId = marker.match(/framenote:subtitle:([\w-]+)/i)?.[1];
    const markerStart = Number(marker.match(/\bstart=([\d.]+)/i)?.[1]);
    const markerEnd = Number(marker.match(/\bend=([\d.]+)/i)?.[1]);
    const speaker = marker.match(/\bspeaker="([^"]*)"/i)?.[1];
    const language = marker.match(/\blanguage="([^"]*)"/i)?.[1];
    const embeddedChapter = /\bsource=embedded-chapter\b/i.test(marker);
    const type: TimelineEntryType = bookmarkId || section === "bookmark"
      ? "bookmark"
      : subtitleId || section === "subtitle"
        ? "subtitle"
        : aiId || end !== null || section === "ai"
          ? "ai"
          : "bookmark";

    entries.push({
      id: bookmarkId ?? aiId ?? subtitleId ?? `line-${lineNumber}`,
      type,
      start: Number.isFinite(markerStart) ? markerStart : start,
      end: Number.isFinite(markerEnd) ? markerEnd : end ?? undefined,
      text: match[3].trim() || (type === "bookmark" ? "Untitled note" : type === "subtitle" ? "Untitled subtitle" : "Untitled summary"),
      editable: Boolean(bookmarkId || aiId || subtitleId),
      line: lineNumber,
      speaker,
      language,
      transcriptComplete: /\btranscript=complete\b/i.test(marker),
      source: embeddedChapter ? "embedded-chapter" : undefined,
    });
  });

  return entries.sort((a, b) => a.start - b.start || a.line - b.line);
}

export function isChunkCovered(entries: TimelineEntry[], start: number, end: number): boolean {
  return entries.some(
    (entry) => entry.type === "ai" && entry.end !== undefined && entry.start <= start + 1 && entry.end >= end - 1,
  );
}

export function isChunkTranscriptCovered(entries: TimelineEntry[], start: number, end: number): boolean {
  return entries.some(
    (entry) => entry.type === "ai" && entry.transcriptComplete && entry.end !== undefined && entry.start <= start + 1 && entry.end >= end - 1,
  );
}
