# FrameNote

FrameNote is a local-first Tauri + React desktop player for watching a video and keeping a timestamped Markdown timeline beside it.

Open `recording.mp4` and FrameNote creates or reads `recording.md` in the same folder. The Markdown file is the canonical data source: bookmarks, generated timeline summaries, and any free-form writing remain readable and editable without FrameNote.

## What the MVP includes

- Native local file picker and system-webview video playback through a private byte-range server
- Adjacent Markdown sidecars with no video copying or mutation
- `N` to start an editable mark and `M` to end the nearest preceding open mark
- One-time import of OBS Hybrid MP4/MOV chapter markers as editable point bookmarks
- Direct local-network or internet-relay watch sessions with six-digit codes, host-served video, shared playback, marks, subtitles, and Markdown
- Clickable bookmarks and AI entries that seek the video
- Raw Markdown editor plus automatic reload when the app regains focus
- Playback shortcuts: Space, Left/Right (10 seconds), F, N (start mark), and M (end mark)
- Embedded audio-track discovery plus a live multi-track mixer with independent 0–200% levels
- Automatic playback-position saves (every 10 seconds, on pause, and before exit) with resume on reopen
- Configurable 30–300 second analysis chunks (60 seconds by default)
- Three representative FFmpeg frames per chunk
- Transcript context from an adjacent `.srt`/`.vtt`, or optional local `whisper-cli` + GGML model
- Local Ollama, direct Ollama Cloud, or native audio+vision analysis through Gemini
- Per-chunk persistence, progress, cancellation, and resume based on ranges already present in the Markdown
- Cancellable FFmpeg rough-cut export from every completed mark, with MOV/ProRes, MP4/H.264, or fast source-codec MKV presets
- All embedded or mixer-selected audio streams preserved as separate tracks, plus per-clip SRT subtitles and Resolve-friendly CSV manifests

## Prerequisites

- Node.js 20+
- Rust and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/)
- FFmpeg and FFprobe on `PATH` for multi-track audio mixing and AI analysis (`brew install ffmpeg` on macOS)
- Ollama for AI analysis; playback and notes work when it is absent

The default vision model is `gemma3:4b`. Install and start it with:

```sh
ollama pull gemma3:4b
ollama serve
```

FrameNote does not pull models automatically. You can select any installed Ollama model that accepts images in Analysis settings.

For direct cloud analysis, select **Cloud** in Analysis settings, paste an [Ollama API key](https://docs.ollama.com/api/authentication), and choose a vision-capable cloud model. FrameNote uses the official `https://ollama.com/api` endpoint and bearer authentication. The key is stored in the operating system's secure credential store and is never written to local browser settings or Markdown. You can alternatively keep **Local** selected and use a cloud model through a locally installed Ollama that is signed in.

Direct Cloud model names omit the local routing suffix. For example, local Ollama uses `gemma4:31b-cloud`, while the direct API exposes `gemma4:31b`. FrameNote accepts either form in Cloud mode and maps `-cloud` or `:cloud` aliases automatically.

For native audio understanding, select **Audio + vision** and add a Gemini API key. FrameNote mixes every embedded audio stream in the chunk, extracts a temporary mono AAC sample, and sends it with the representative frames to `gemini-3.6-flash`. This lets the model jointly interpret multilingual speech—including English/Slovak switching—and relevant non-speech sounds without a separate Whisper transcript. Gemini and Ollama Cloud use separate entries in the operating system's secure credential store.

Optional transcription requires `whisper-cli` and a local GGML model. Enter the model's absolute path in Analysis settings. If an adjacent subtitle file exists, FrameNote uses it before Whisper and does not generate a transcript.

## Run locally

```sh
npm install
npm run tauri:dev
```

Web-only UI development is also available with `npm run dev`, but native file selection, sidecar writes, and analysis require Tauri.

## Watch together

Open a project and choose **Share → Create session**. Other FrameNote instances choose **Join session** and enter the six-digit code. Play, pause, seek, playback rate, marks, subtitles, AI entries, and raw Markdown updates propagate in both directions; the host remains the owner of the canonical adjacent sidecar.

**Local network** mode discovers the host over multicast DNS and connects directly over the same Wi-Fi/Ethernet network. It needs no FrameNote account, upload, signaling service, or relay. Multicast DNS must be allowed, and guest access ends when the host ends the session.

**Internet** mode uses the configured lightweight relay to tunnel control requests and media segments to the host. FrameNote does not preprocess or upload the source. When a guest needs a position, the host encodes one eight-second window around that position as one-second, 720p-or-smaller H.264/AAC segments (about 2 Mbps video). Those small segments stay below the relay's media bound, provide immediate read-ahead, and avoid starting a new FFmpeg process every second. Seeking jumps to the corresponding eight-second window rather than processing skipped footage. Host and guest segment caches are reused for seven days while the source is unchanged, so replaying a section does not encode or transfer it again. This prevents the audio-only/black-video failure caused by an unsupported source codec while keeping multi-hour raw footage practical. The original video is never modified, and the relay stores neither the project nor its segments.

Starting playback or seeking while playing uses a prepare/ready/commit barrier. Every active participant pauses at the target, buffers several seconds, and repeatedly acknowledges readiness before the host publishes a shared start time. Poll round-trip timing compensates for different computer clocks, and a participant that stalls during playback automatically starts a new barrier so everyone waits together. Pausing remains immediate. A guest that cannot buffer stays visibly in a waiting state instead of letting the other participants run ahead.

On macOS, allow FrameNote's Local Network permission when prompted. On Windows, allow FrameNote on private networks if Windows Defender Firewall prompts; public-network access is not needed. AI analysis, waveform extraction, and rough-cut export run on the host because the original file never gets copied to guests; their resulting sidecar entries appear for every peer.

## Verify and package

```sh
npm test
npm run build
cargo test --manifest-path src-tauri/Cargo.toml
npm run tauri:build
```

Native bundles are written under `src-tauri/target/release/bundle/`.

The included `.github/workflows/build-desktop.yml` produces Apple Silicon and Intel macOS DMGs plus a Windows x64 MSI/NSIS installer when manually dispatched or when a `v*` tag is pushed. It is also the repeatable cross-platform packaging check for machines not available locally.

## Sidecar format

FrameNote initializes a deliberately small Markdown structure:

```md
# recording.mp4

<!-- framenote:v1 -->
<!-- framenote:position seconds=83.250 -->

## Bookmarks

- [00:01:23.250–00:01:31.500] Important point <!-- framenote:bookmark:... start=83.250 end=91.500 -->

## AI timeline

- [00:01:00–00:02:00] The speaker demonstrates the export workflow. <!-- framenote:ai:... start=60 end=120 -->
```

The HTML comments carry stable IDs, exact ranges, and the latest playback position. They stay invisible in rendered Markdown and remain human-editable. Timeline markers are optional: human-authored `- [MM:SS] text` and `- [HH:MM:SS–HH:MM:SS] text` entries still render. Unknown headings, paragraphs, and Markdown are preserved. If you remove a FrameNote marker, edit that entry through the raw Markdown view.

On the first successful open of an MP4/MOV source, FrameNote asks FFprobe for embedded chapter markers such as those written by OBS Hybrid MP4. It adds each chapter as a named point bookmark and writes a hidden import receipt to the sidecar. That receipt prevents every later open from recreating markers you deliberately deleted, even if the source video changes. Removing the sidecar itself starts a new project and allows the one-time import again.

## Analysis behavior and privacy

Analysis runs chunk-by-chunk while the `<video>` element keeps playing. For each incomplete range, the Rust backend:

1. reads representative frames with FFmpeg;
2. includes overlapping companion subtitles, or optionally extracts temporary audio and runs local Whisper;
3. sends the frames and transcript text to the configured Ollama endpoint, or sends the actual AAC audio and frames together to Gemini;
4. appends one concise range entry to the sidecar immediately.

Temporary frames/audio live in an OS temporary directory and are removed after the chunk. Cancellation terminates active FFmpeg/Whisper work or the Ollama request. Already-written ranges remain intact, so Resume skips them. The original video is opened read-only and is never rewritten.

The default Ollama URL is loopback-only. In Ollama Cloud mode, sampled frames and transcript text are sent over HTTPS to Ollama Cloud. In Gemini mode, temporary AAC audio and frames are sent over HTTPS to Google's Gemini API. FrameNote makes the active data path explicit in settings, and cloud API keys are kept in the operating system's secure credential store.

Blue ranges on the playback seek bar mark analyzed chunks. Hover or keyboard-focus a blue range to preview its saved AI summary; click it to seek to the start of that chunk.

## Rough-cut export

Close one or more marks, then open **Export** in the player controls. Choose a destination, a media preset, and whether to preserve all embedded audio tracks or only the streams currently selected in the mixer. Audio streams remain separate; FrameNote never mixes them during export.

The Resolve preset creates frame-accurate MOV clips with ProRes 422 video and 24-bit PCM audio. The MP4 preset creates compact H.264/AAC clips. The source-codec MKV preset is fastest but its cut accuracy depends on source keyframes. Every run creates a new folder so existing exports are never overwritten.

Each media clip has a matching `.srt` file with clip-relative timestamps and speaker prefixes. `framenote_manifest.csv` retains the original source in/out points, while `framenote_subtitles.csv` retains relative and source timestamps, speaker names, language codes, and verbatim text. Import the media and SRT files into DaVinci Resolve; `README.txt` inside the export folder summarizes the handoff.

## Audio mixing and resume

Open the audio mixer from the waveform button in the player controls. Select one track to swap audio, or select several to hear them together. Each chosen stream has an independent level; **Select all** enables every embedded stream. FrameNote asks FFmpeg for a temporary fragmented AAC stream starting at the current playhead, so it never rewrites or creates a replacement video. Seeking, rate changes, play, and pause stay synchronized with the live mix.

Generated waveform peaks are cached in FrameNote's operating-system cache directory for seven days. Reopening an unchanged video reuses them immediately; changing the source file, changing the waveform extraction format in a future app version, expiry, or an unreadable cache entry triggers a clean rebuild. Stale waveform cache files are removed automatically when a video is opened, and the original media remains read-only.

The current playhead is written to the hidden `framenote:position` comment in the adjacent Markdown every 10 seconds, on pause, when changing videos, and before the window closes. Reopening the same video seeks to that position. Reaching the end resets it to zero.

Playback also uses loopback, but never leaves the machine: FrameNote binds a private HTTP server to `127.0.0.1` on a random port and registers each selected file under an unguessable session token. HTTP byte-range support lets the webview seek very large files without loading them into memory. The server is available only while FrameNote is running and the original file is opened read-only.

## Source player adaptation

The playback interaction model was adapted from the React player found at:

`/Users/filiplukovic/Documents/projects/ptr/notlify_monorepo/apps/frontend/src/components/player.tsx`

FrameNote retains the useful native video, keyboard transport, seek, volume, speed, and fullscreen patterns while removing Notlify's HLS servers, TMDB data, episodes, casting, subtitles menu, and remote progress APIs.

## Known codec behavior

Local playback support follows the operating system webview. MP4/H.264 is the safest cross-platform choice; a selected container or codec can still be rejected by the platform even when FFmpeg can analyze it. Shared sessions use on-demand HLS with short H.264/AAC compatibility segments, so guests do not depend on the original codec and multi-hour sources are never transcoded end-to-end. FrameNote never alters the source file.
