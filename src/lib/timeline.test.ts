import { describe, expect, it } from "vitest";
import { formatTime, formatTimecode, isChunkCovered, isChunkTranscriptCovered, nextSubtitleStart, parseTimeline, planAnalysisChunks, timestampToSeconds } from "./timeline";

describe("timeline Markdown", () => {
  it("parses bookmarks and AI ranges without discarding human Markdown", () => {
    const entries = parseTimeline(`# Talk\n\nFree-form intro.\n\n## Bookmarks\n- [00:01:08] Key argument <!-- framenote:bookmark:note-1 -->\n\n## AI timeline\n- [00:00:00–00:01:00] The speaker introduces the topic. <!-- framenote:ai:ai-1 start=0 end=60 -->`);
    expect(entries).toHaveLength(2);
    expect(entries[0]).toMatchObject({ type: "ai", start: 0, end: 60 });
    expect(entries[1]).toMatchObject({ type: "bookmark", start: 68, text: "Key argument" });
  });

  it("supports human-authored entries without internal markers", () => {
    const entries = parseTimeline("## Notes\n- [04:12] Review this manually");
    expect(entries[0]).toMatchObject({ type: "bookmark", start: 252, editable: false });
  });

  it("keeps completed mark ranges in the bookmarks timeline", () => {
    const [entry] = parseTimeline("## Bookmarks\n- [00:00:10.250–00:00:14.500] Useful moment <!-- framenote:bookmark:mark-1 start=10.25 end=14.5 -->");
    expect(entry).toMatchObject({ type: "bookmark", start: 10.25, end: 14.5, text: "Useful moment" });
  });

  it("identifies imported chapter markers as point bookmarks", () => {
    const [entry] = parseTimeline("## Bookmarks\n- [00:02:03.250] Unnamed 1 <!-- framenote:bookmark:embedded-1-123250 start=123.250 source=embedded-chapter -->");
    expect(entry).toMatchObject({
      type: "bookmark",
      start: 123.25,
      text: "Unnamed 1",
      source: "embedded-chapter",
    });
    expect(entry.end).toBeUndefined();
  });

  it("parses editable subtitle cues and speaker metadata", () => {
    const [entry] = parseTimeline(`## Subtitles\n- [00:00:02–00:00:06] Poďte už do lobby. <!-- framenote:subtitle:cue-1 start=2.25 end=5.5 speaker="Speaker 1" language="sk" -->`);
    expect(entry).toMatchObject({
      type: "subtitle",
      start: 2.25,
      end: 5.5,
      text: "Poďte už do lobby.",
      speaker: "Speaker 1",
      language: "sk",
      editable: true,
    });
  });

  it("recognizes resumable completed chunks", () => {
    const [entry] = parseTimeline("## AI timeline\n- [00:00:00–00:01:00] Opening");
    expect(isChunkCovered([entry], 0, 60)).toBe(true);
    expect(isChunkCovered([entry], 60, 120)).toBe(false);
  });

  it("distinguishes legacy summaries from transcript-complete ranges", () => {
    const legacy = parseTimeline("## AI timeline\n- [00:00:00–00:01:00] Opening <!-- framenote:ai:old start=0 end=60 -->");
    const complete = parseTimeline("## AI timeline\n- [00:00:00–00:01:00] Opening <!-- framenote:ai:new start=0 end=60 transcript=complete -->");
    expect(isChunkCovered(legacy, 0, 60)).toBe(true);
    expect(isChunkTranscriptCovered(legacy, 0, 60)).toBe(false);
    expect(isChunkTranscriptCovered(complete, 0, 60)).toBe(true);
  });

  it("formats and parses long timestamps", () => {
    expect(timestampToSeconds("01:02:03")).toBe(3723);
    expect(timestampToSeconds("01:02:03.250")).toBe(3723.25);
    expect(formatTime(3723)).toBe("01:02:03");
    expect(formatTimecode(3723.25)).toBe("01:02:03.250");
    expect(formatTime(63, true)).toBe("1:03");
    expect(parseTimeline("## Subtitles\n- [00:00:02.250–00:00:05.500] Precise cue")[0]).toMatchObject({ start: 2.25, end: 5.5 });
  });

  it("plans a bounded analysis range from the exact playhead", () => {
    expect(planAnalysisChunks(260, 37.5, 60, 5)).toEqual([
      { start: 37.5, end: 97.5 },
      { start: 97.5, end: 157.5 },
      { start: 157.5, end: 217.5 },
      { start: 217.5, end: 260 },
    ]);
  });

  it("starts a tab-created cue at the current cue end or at the playhead", () => {
    expect(nextSubtitleStart(12, 10, 14.5)).toBe(14.5);
    expect(nextSubtitleStart(40.25, 10, 14.5)).toBe(40.25);
  });
});
