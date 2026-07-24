import {
  AudioLines,
  AudioWaveform,
  BrainCircuit,
  Captions,
  Check,
  CircleAlert,
  Cloud,
  Clock3,
  Code2,
  Copy,
  Download,
  FileText,
  Film,
  FolderOpen,
  HardDrive,
  KeyRound,
  Maximize2,
  Pause,
  PanelRightClose,
  PanelRightOpen,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  RotateCw,
  Save,
  Scissors,
  Settings2,
  Share2,
  Sparkles,
  Square,
  Trash2,
  Users,
  Volume2,
  VolumeX,
  Wifi,
  WifiOff,
  X,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { formatTime, formatTimecode, isChunkCovered, isChunkTranscriptCovered, nextSubtitleStart, parseTimeline, planAnalysisChunks, timestampToSeconds, type TimelineEntry } from "./lib/timeline";

interface SidecarDocument {
  videoPath: string;
  videoName: string;
  sidecarPath: string;
  markdown: string;
  playbackPosition: number;
}

interface RecentProject {
  videoPath: string;
  videoName: string;
  sidecarPath: string;
  lastOpenedAt: number;
  playbackPosition: number;
  duration?: number;
}

interface AudioTrackInfo {
  streamIndex: number;
  label: string;
  language?: string;
  codec: string;
  channels?: number;
}

interface MediaRegistration {
  url: string;
  mixBaseUrl: string;
  audioTracks: AudioTrackInfo[];
  frameRate?: number;
}

interface CollaborationSession {
  mode: "host" | "guest";
  code: string;
  participantCount: number;
  videoName: string;
  displayName: string;
  clientId: string;
  participants: string[];
}

interface JoinCollaborationResult {
  document: SidecarDocument;
  mediaRegistration: MediaRegistration;
  session: CollaborationSession;
  transport: CollaborationTransport;
}

interface CollaborationTransport {
  position: number;
  playing: boolean;
  playbackRate: number;
  emittedAt?: number;
}

interface CollaborationEvent {
  sequence: number;
  senderId: string;
  kind: "transport" | "document";
  payload: Record<string, unknown>;
}

interface CollaborationPollResult {
  events: CollaborationEvent[];
  participantCount: number;
  participants: string[];
  connected: boolean;
}

interface PrecisionSeekAnchor {
  pointerId: number;
  startX: number;
  startY: number;
  startTime: number;
  width: number;
}

interface PrecisionSeekFeedback {
  time: number;
  scale: number;
  lift: number;
}

interface WaveformData {
  samplesPerSecond: number;
  peaks: number[];
}

interface WaveformSelection {
  start: number;
  end: number;
  viewStart: number;
  viewEnd: number;
}

interface SubtitleTimingOverride {
  id: string;
  start: number;
  end: number;
}

interface AddBookmarkResult {
  document: SidecarDocument;
  entryId: string;
}

interface OllamaStatus {
  available: boolean;
  modelAvailable: boolean;
  message: string;
  models: string[];
}

interface AnalysisChunkResult {
  summary: string;
  frameCount: number;
  transcriptSource: string;
  transcriptCues: TranscriptCue[];
  transcriptComplete: boolean;
}

interface TranscriptCue {
  startSeconds: number;
  endSeconds: number;
  text: string;
  speaker: string;
  language: string;
}

interface SubtitleDraft {
  start: number;
  end: number;
  text: string;
  speaker: string;
  language: string;
}

interface AnalysisConfig {
  provider: "local" | "cloud" | "gemini";
  ollamaUrl: string;
  model: string;
  apiKey: string;
  chunkSeconds: number;
  chunkCount: number;
  frameCount: number;
  whisperModelPath: string;
}

type AnalysisPhase = "idle" | "running" | "paused" | "complete" | "error";
type CredentialPhase = "idle" | "loading" | "saving" | "saved" | "error";
type TimelineFilter = "all" | "bookmark" | "ai" | "subtitle";
type ExportPreset = "resolve" | "mp4" | "source";
type ExportPhase = "idle" | "running" | "complete" | "error" | "cancelled";

interface ExportClipResult {
  fileName: string;
  subtitleFileName: string;
  videoPath: string;
  subtitlePath: string;
}

interface ExportManifestClip {
  fileName: string;
  subtitleFileName: string;
  startSeconds: number;
  endSeconds: number;
  label: string;
}

const DEFAULT_CONFIG: AnalysisConfig = {
  provider: "local",
  ollamaUrl: "http://127.0.0.1:11434",
  model: "gemma3:4b",
  apiKey: "",
  chunkSeconds: 60,
  chunkCount: 5,
  frameCount: 4,
  whisperModelPath: "",
};

const OLLAMA_CLOUD_URL = "https://ollama.com/api";
const RECENT_PROJECTS_KEY = "framenote:recent-projects";
const MAX_RECENT_PROJECTS = 6;

const DEMO_MARKDOWN = `# Studio interview.mp4

<!-- framenote:v1 -->

## Bookmarks

- [00:01:08] Strong explanation of the core idea <!-- framenote:bookmark:demo-note -->
- [00:03:42] Pull this quote for the written recap <!-- framenote:bookmark:demo-quote -->

## AI timeline

- [00:00:00–00:01:00] The speaker introduces the project and outlines the problem it is designed to solve. <!-- framenote:ai:demo-ai-1 start=0 end=60 -->
- [00:01:00–00:02:00] A close-up demonstration shows the first workflow while the speaker explains the design choices. <!-- framenote:ai:demo-ai-2 start=60 end=120 -->
- [00:02:00–00:03:00] The discussion shifts to practical constraints, including local data ownership and offline use. <!-- framenote:ai:demo-ai-3 start=120 end=180 -->

## Subtitles

- [00:01:20–00:01:24] This is the part that stays completely local. <!-- framenote:subtitle:demo-sub-1 start=80 end=84 speaker="Speaker 1" language="en" -->
- [00:01:24–00:01:28] Presne tak, video sa nikam neposiela. <!-- framenote:subtitle:demo-sub-2 start=84 end=88 speaker="Speaker 2" language="sk" -->
`;

const IS_TAURI = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const IS_DEMO = import.meta.env.DEV && new URLSearchParams(window.location.search).has("demo");

function readRecentProjects(): RecentProject[] {
  try {
    const value = JSON.parse(localStorage.getItem(RECENT_PROJECTS_KEY) ?? "[]");
    if (!Array.isArray(value)) return [];
    return value
      .filter((item): item is RecentProject => (
        typeof item?.videoPath === "string"
        && typeof item?.videoName === "string"
        && typeof item?.sidecarPath === "string"
        && Number.isFinite(item?.lastOpenedAt)
      ))
      .slice(0, MAX_RECENT_PROJECTS);
  } catch {
    return [];
  }
}

function saveRecentProjects(projects: RecentProject[]): void {
  localStorage.setItem(RECENT_PROJECTS_KEY, JSON.stringify(projects.slice(0, MAX_RECENT_PROJECTS)));
}

function recentOpenedLabel(timestamp: number): string {
  const elapsed = Math.max(0, Date.now() - timestamp);
  if (elapsed < 60_000) return "Just opened";
  if (elapsed < 3_600_000) return `${Math.max(1, Math.floor(elapsed / 60_000))}m ago`;
  if (elapsed < 86_400_000) return `${Math.max(1, Math.floor(elapsed / 3_600_000))}h ago`;
  if (elapsed < 172_800_000) return "Yesterday";
  return new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(timestamp);
}

function readSavedConfig(): AnalysisConfig {
  try {
    const saved = {
      ...DEFAULT_CONFIG,
      ...JSON.parse(localStorage.getItem("framenote:analysis") ?? "{}"),
      apiKey: "",
    };
    return {
      ...saved,
      ollamaUrl: saved.provider === "cloud"
        ? OLLAMA_CLOUD_URL
        : saved.provider === "gemini"
          ? "https://generativelanguage.googleapis.com/v1beta"
          : saved.ollamaUrl,
    };
  } catch {
    return DEFAULT_CONFIG;
  }
}

function errorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Something unexpected happened.";
}

function matchesAnalysisResumeState(phase: AnalysisPhase): boolean {
  return phase === "running" || phase === "paused" || phase === "error";
}

function precisionSeekScale(lift: number): number {
  return Math.max(0.008, 10 ** (-Math.max(0, lift) / 95));
}

function App() {
  const videoRef = useRef<HTMLVideoElement>(null);
  const mixAudioRef = useRef<HTMLAudioElement>(null);
  const playerRef = useRef<HTMLDivElement>(null);
  const cancelRequestedRef = useRef(false);
  const activeJobRef = useRef<string | null>(null);
  const noticeTimerRef = useRef<number | null>(null);
  const documentRef = useRef<SidecarDocument | null>(null);
  const currentTimeRef = useRef(0);
  const mixAnchorRef = useRef(0);
  const resumePositionRef = useRef(0);
  const closingRef = useRef(false);
  const subtitleTransitionRef = useRef(false);
  const activeExportJobRef = useRef<string | null>(null);
  const exportCancelRequestedRef = useRef(false);
  const precisionSeekRef = useRef<PrecisionSeekAnchor | null>(null);
  const frameSeekTimerRef = useRef<number | null>(null);
  const credentialLoadedProviderRef = useRef<AnalysisConfig["provider"] | null>(null);
  const credentialStoredValueRef = useRef("");
  const credentialOperationRef = useRef(0);
  const credentialSaveTimerRef = useRef<number | null>(null);
  const remoteTransportUntilRef = useRef(0);
  const lastSharedMarkdownRef = useRef("");
  const collaborationPollFailuresRef = useRef(0);
  const initialRemotePlayingRef = useRef(false);

  const [document, setDocument] = useState<SidecarDocument | null>(() =>
    IS_DEMO
      ? {
          videoPath: "demo://studio-interview.mp4",
          videoName: "Studio interview.mp4",
          sidecarPath: "/Videos/Studio interview.md",
          markdown: DEMO_MARKDOWN,
          playbackPosition: 82,
        }
      : null,
  );
  const [recentProjects, setRecentProjects] = useState<RecentProject[]>(readRecentProjects);
  const [videoUrl, setVideoUrl] = useState<string | null>(null);
  const [mediaRegistration, setMediaRegistration] = useState<MediaRegistration | null>(null);
  const [duration, setDuration] = useState(IS_DEMO ? 438 : 0);
  const [currentTime, setCurrentTime] = useState(IS_DEMO ? 82 : 0);
  const [isPlaying, setIsPlaying] = useState(false);
  const [volume, setVolume] = useState(() => Number(localStorage.getItem("framenote:volume") ?? 0.85));
  const [isMuted, setIsMuted] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1);
  const [captionsVisible, setCaptionsVisible] = useState(true);
  const [mixerOpen, setMixerOpen] = useState(false);
  const [mixerActive, setMixerActive] = useState(false);
  const [trackLevels, setTrackLevels] = useState<Record<number, number>>({});
  const [mixState, setMixState] = useState<"off" | "loading" | "ready" | "error">("off");
  const [filter, setFilter] = useState<TimelineFilter>("all");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [rawMode, setRawMode] = useState(false);
  const [rawDraft, setRawDraft] = useState(document?.markdown ?? "");
  const [rawDirty, setRawDirty] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [config, setConfig] = useState<AnalysisConfig>(readSavedConfig);
  const [ollamaStatus, setOllamaStatus] = useState<OllamaStatus | null>(
    IS_DEMO
      ? { available: true, modelAvailable: true, message: "Connected · gemma3:4b is ready", models: ["gemma3:4b"] }
      : null,
  );
  const [checkingOllama, setCheckingOllama] = useState(false);
  const [credentialPhase, setCredentialPhase] = useState<CredentialPhase>("idle");
  const [analysisOpen, setAnalysisOpen] = useState(false);
  const [analysisStart, setAnalysisStart] = useState(IS_DEMO ? 82 : 0);
  const [analysisPhase, setAnalysisPhase] = useState<AnalysisPhase>("idle");
  const [analysisCursor, setAnalysisCursor] = useState(0);
  const [analysisTotal, setAnalysisTotal] = useState(0);
  const [analysisDetail, setAnalysisDetail] = useState("Ready to build a private local timeline.");
  const [exportOpen, setExportOpen] = useState(false);
  const [exportDestination, setExportDestination] = useState("");
  const [exportPreset, setExportPreset] = useState<ExportPreset>("resolve");
  const [exportAudioMode, setExportAudioMode] = useState<"all" | "selected">("all");
  const [exportPhase, setExportPhase] = useState<ExportPhase>("idle");
  const [exportCursor, setExportCursor] = useState(0);
  const [exportTotal, setExportTotal] = useState(0);
  const [exportDetail, setExportDetail] = useState("Choose a folder and export every completed mark.");
  const [exportOutputDirectory, setExportOutputDirectory] = useState("");
  const [notice, setNotice] = useState<{ tone: "info" | "error"; text: string } | null>(null);
  const [opening, setOpening] = useState(false);
  const [precisionSeek, setPrecisionSeek] = useState<PrecisionSeekFeedback | null>(null);
  const [frameSeek, setFrameSeek] = useState<PrecisionSeekFeedback | null>(null);
  const [waveform, setWaveform] = useState<WaveformData | null>(null);
  const [waveformPhase, setWaveformPhase] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [waveformOpen, setWaveformOpen] = useState(false);
  const [waveformZoom, setWaveformZoom] = useState(() => {
    const saved = Number(localStorage.getItem("framenote:waveform-zoom") ?? 8);
    return Number.isFinite(saved) && saved > 0 ? saved : 8;
  });
  const [waveformSelection, setWaveformSelection] = useState<WaveformSelection | null>(null);
  const [subtitleTimingOverride, setSubtitleTimingOverride] = useState<SubtitleTimingOverride | null>(null);
  const [timelineCollapsed, setTimelineCollapsed] = useState(false);
  const [collaboration, setCollaboration] = useState<CollaborationSession | null>(null);
  const [collaborationOpen, setCollaborationOpen] = useState(false);
  const [collaborationPhase, setCollaborationPhase] = useState<"idle" | "hosting" | "joining" | "connected" | "reconnecting">("idle");
  const [joinCode, setJoinCode] = useState("");
  const [displayName, setDisplayName] = useState(() => localStorage.getItem("framenote:display-name") || "Editor");

  const selectedAudioTracks = useMemo(
    () => mediaRegistration?.audioTracks.filter((track) => trackLevels[track.streamIndex] !== undefined) ?? [],
    [mediaRegistration?.audioTracks, trackLevels],
  );

  const entries = useMemo(() => parseTimeline(document?.markdown ?? ""), [document?.markdown]);
  const subtitleEntries = useMemo(() => entries.filter((entry) => entry.type === "subtitle"), [entries]);
  const editingSubtitle = useMemo(() => {
    const entry = subtitleEntries.find((candidate) => candidate.id === editingId);
    if (!entry || entry.end === undefined) return null;
    return subtitleTimingOverride?.id === entry.id
      ? { id: entry.id, start: subtitleTimingOverride.start, end: subtitleTimingOverride.end }
      : { id: entry.id, start: entry.start, end: entry.end };
  }, [editingId, subtitleEntries, subtitleTimingOverride]);
  const completedMarks = useMemo(
    () => entries.filter((entry) => entry.type === "bookmark" && entry.end !== undefined && entry.end > entry.start),
    [entries],
  );
  const openMark = useMemo(
    () => entries
      .filter((entry) => entry.type === "bookmark" && entry.end === undefined && entry.source !== "embedded-chapter")
      .sort((left, right) => right.start - left.start)[0] ?? null,
    [entries],
  );
  const activeSubtitles = useMemo(
    () => captionsVisible
      ? subtitleEntries.filter((entry) => entry.start <= currentTime + 0.08 && currentTime < (entry.end ?? entry.start + 4))
      : [],
    [captionsVisible, currentTime, subtitleEntries],
  );
  const filteredEntries = useMemo(
    () => entries.filter((entry) => filter === "all" || entry.type === filter),
    [entries, filter],
  );
  const plannedChunks = useMemo(
    () => planAnalysisChunks(duration, analysisStart, config.chunkSeconds, config.chunkCount),
    [analysisStart, config.chunkCount, config.chunkSeconds, duration],
  );
  const progress = analysisTotal ? analysisCursor / analysisTotal : 0;

  const showNotice = useCallback((text: string, tone: "info" | "error" = "info") => {
    setNotice({ text, tone });
    if (noticeTimerRef.current) window.clearTimeout(noticeTimerRef.current);
    noticeTimerRef.current = window.setTimeout(() => setNotice(null), 4500);
  }, []);

  const applyDocument = useCallback((next: SidecarDocument) => {
    setDocument(next);
    setRawDraft(next.markdown);
    setRawDirty(false);
  }, []);

  const updateRecentProjects = useCallback((update: (projects: RecentProject[]) => RecentProject[]) => {
    setRecentProjects((current) => {
      const next = update(current).slice(0, MAX_RECENT_PROJECTS);
      saveRecentProjects(next);
      return next;
    });
  }, []);

  const recordRecentProject = useCallback((next: SidecarDocument) => {
    updateRecentProjects((current) => [{
      videoPath: next.videoPath,
      videoName: next.videoName,
      sidecarPath: next.sidecarPath,
      lastOpenedAt: Date.now(),
      playbackPosition: next.playbackPosition,
      duration: current.find((project) => project.videoPath === next.videoPath)?.duration,
    }, ...current.filter((project) => project.videoPath !== next.videoPath)]);
  }, [updateRecentProjects]);

  const updateRecentPlayback = useCallback((videoPath: string, playbackPosition: number, mediaDuration?: number) => {
    updateRecentProjects((current) => current.map((project) => project.videoPath === videoPath
      ? {
          ...project,
          playbackPosition,
          duration: Number.isFinite(mediaDuration) && (mediaDuration ?? 0) > 0 ? mediaDuration : project.duration,
        }
      : project));
  }, [updateRecentProjects]);

  useEffect(() => {
    documentRef.current = document;
  }, [document]);

  useEffect(() => {
    if (!document || !IS_TAURI) return;
    if (collaboration?.mode === "guest") {
      setWaveform(null);
      setWaveformPhase("idle");
      return;
    }
    let current = true;
    setWaveform(null);
    setWaveformPhase("loading");
    void invoke<WaveformData>("extract_waveform", { videoPath: document.videoPath })
      .then((result) => {
        if (!current) return;
        setWaveform(result);
        setWaveformPhase("ready");
      })
      .catch(() => {
        if (!current) return;
        setWaveform(null);
        setWaveformPhase("error");
      });
    return () => {
      current = false;
    };
  }, [collaboration?.mode, document?.videoPath]);

  useEffect(() => () => {
    if (frameSeekTimerRef.current) window.clearTimeout(frameSeekTimerRef.current);
  }, []);

  useEffect(() => {
    localStorage.setItem("framenote:waveform-zoom", String(waveformZoom));
  }, [waveformZoom]);

  const persistApiKey = useCallback(async (provider: AnalysisConfig["provider"], apiKey: string) => {
    if (!IS_TAURI || provider === "local" || credentialLoadedProviderRef.current !== provider) return true;
    if (credentialSaveTimerRef.current) {
      window.clearTimeout(credentialSaveTimerRef.current);
      credentialSaveTimerRef.current = null;
    }
    const normalized = apiKey.trim();
    if (credentialStoredValueRef.current === normalized) {
      setCredentialPhase("saved");
      return true;
    }
    const operation = ++credentialOperationRef.current;
    setCredentialPhase("saving");
    try {
      await invoke("save_api_key", { provider, apiKey: normalized });
      if (credentialOperationRef.current === operation) {
        credentialStoredValueRef.current = normalized;
        setCredentialPhase("saved");
      }
      return true;
    } catch (error) {
      if (credentialOperationRef.current === operation) setCredentialPhase("error");
      showNotice(errorMessage(error), "error");
      return false;
    }
  }, [showNotice]);

  useEffect(() => {
    const provider = config.provider;
    const operation = ++credentialOperationRef.current;
    credentialLoadedProviderRef.current = null;
    credentialStoredValueRef.current = "";
    if (credentialSaveTimerRef.current) {
      window.clearTimeout(credentialSaveTimerRef.current);
      credentialSaveTimerRef.current = null;
    }
    if (provider === "local" || !IS_TAURI) {
      setConfig((current) => current.provider === provider && current.apiKey
        ? { ...current, apiKey: "" }
        : current);
      setCredentialPhase("idle");
      return;
    }

    setCredentialPhase("loading");
    setConfig((current) => current.provider === provider
      ? { ...current, apiKey: "" }
      : current);
    void invoke<string | null>("load_api_key", { provider })
      .then((apiKey) => {
        if (credentialOperationRef.current !== operation) return;
        const savedKey = apiKey ?? "";
        credentialLoadedProviderRef.current = provider;
        credentialStoredValueRef.current = savedKey;
        setConfig((current) => current.provider === provider
          ? { ...current, apiKey: savedKey }
          : current);
        setCredentialPhase("saved");
      })
      .catch((error) => {
        if (credentialOperationRef.current !== operation) return;
        credentialLoadedProviderRef.current = provider;
        setCredentialPhase("error");
        showNotice(errorMessage(error), "error");
      });
  }, [config.provider, showNotice]);

  useEffect(() => {
    if (!IS_TAURI || config.provider === "local" || credentialLoadedProviderRef.current !== config.provider) return;
    const normalized = config.apiKey.trim();
    if (normalized === credentialStoredValueRef.current) return;
    setCredentialPhase("saving");
    credentialSaveTimerRef.current = window.setTimeout(() => {
      credentialSaveTimerRef.current = null;
      void persistApiKey(config.provider, config.apiKey);
    }, 300);
    return () => {
      if (credentialSaveTimerRef.current) {
        window.clearTimeout(credentialSaveTimerRef.current);
        credentialSaveTimerRef.current = null;
      }
    };
  }, [config.apiKey, config.provider, persistApiKey]);

  const checkOllama = useCallback(async () => {
    if (!IS_TAURI) return ollamaStatus;
    if (config.provider !== "local" && !config.apiKey.trim()) {
      const status: OllamaStatus = {
        available: false,
        modelAvailable: false,
        message: `Enter a ${config.provider === "gemini" ? "Gemini" : "Ollama Cloud"} API key to connect.`,
        models: [],
      };
      setOllamaStatus(status);
      return status;
    }
    setCheckingOllama(true);
    try {
      if (config.provider !== "local") await persistApiKey(config.provider, config.apiKey);
      const status = config.provider === "gemini"
        ? await invoke<OllamaStatus>("check_gemini", { model: config.model, apiKey: config.apiKey })
        : await invoke<OllamaStatus>("check_ollama", {
            ollamaUrl: config.ollamaUrl,
            model: config.model,
            apiKey: config.apiKey || null,
          });
      setOllamaStatus(status);
      return status;
    } catch (error) {
      const status: OllamaStatus = {
        available: false,
        modelAvailable: false,
        message: errorMessage(error),
        models: [],
      };
      setOllamaStatus(status);
      return status;
    } finally {
      setCheckingOllama(false);
    }
  }, [config.apiKey, config.model, config.ollamaUrl, config.provider, ollamaStatus, persistApiKey]);

  useEffect(() => {
    const { apiKey: _sessionOnlyKey, ...savedConfig } = config;
    localStorage.setItem("framenote:analysis", JSON.stringify(savedConfig));
  }, [config]);

  useEffect(() => {
    if (!IS_DEMO) setOllamaStatus(null);
  }, [config.model, config.ollamaUrl, config.provider]);

  useEffect(() => {
    localStorage.setItem("framenote:volume", String(volume));
    if (videoRef.current) videoRef.current.volume = volume;
    if (mixAudioRef.current) mixAudioRef.current.volume = volume;
  }, [volume]);

  useEffect(() => {
    if (videoRef.current) videoRef.current.muted = isMuted || mixerActive;
    if (mixAudioRef.current) mixAudioRef.current.muted = isMuted;
  }, [isMuted, mixerActive]);

  useEffect(() => {
    if (videoRef.current) videoRef.current.playbackRate = playbackRate;
    if (mixAudioRef.current) mixAudioRef.current.playbackRate = playbackRate;
    if (collaboration && videoRef.current && Date.now() >= remoteTransportUntilRef.current) {
      void invoke("publish_collaboration_event", {
        kind: "transport",
        payload: {
          position: videoRef.current.currentTime,
          playing: !videoRef.current.paused,
          playbackRate,
        },
      }).catch(() => undefined);
    }
  }, [collaboration?.code, collaboration?.mode, playbackRate]);

  const persistPlaybackPosition = useCallback(async (position = currentTimeRef.current) => {
    const currentDocument = documentRef.current;
    if (!IS_TAURI || !currentDocument) return;
    const safePosition = Number.isFinite(position) ? Math.max(0, position) : 0;
    await invoke("save_playback_position", {
      videoPath: currentDocument.videoPath,
      positionSeconds: safePosition,
    });
    updateRecentPlayback(currentDocument.videoPath, safePosition, videoRef.current?.duration);
  }, [updateRecentPlayback]);

  useEffect(() => {
    if (!IS_TAURI) return;
    const timer = window.setInterval(() => void persistPlaybackPosition().catch(() => undefined), 10_000);
    return () => window.clearInterval(timer);
  }, [persistPlaybackPosition]);

  useEffect(() => {
    if (!IS_TAURI) return;
    let unlisten: (() => void) | undefined;
    const appWindow = getCurrentWindow();
    void appWindow.onCloseRequested(async (event) => {
      if (closingRef.current) return;
      event.preventDefault();
      closingRef.current = true;
      try {
        await Promise.all([
          persistPlaybackPosition(),
          persistApiKey(config.provider, config.apiKey),
          invoke("stop_collaboration").catch(() => undefined),
        ]);
      } finally {
        await appWindow.destroy();
      }
    }).then((stop) => { unlisten = stop; });
    return () => unlisten?.();
  }, [config.apiKey, config.provider, persistApiKey, persistPlaybackPosition]);

  const stopMix = useCallback(() => {
    const audio = mixAudioRef.current;
    if (audio) {
      audio.pause();
      audio.removeAttribute("src");
      audio.load();
    }
    setMixState("off");
  }, []);

  const startMixAt = useCallback((position = currentTimeRef.current) => {
    const audio = mixAudioRef.current;
    if (!audio || !mediaRegistration || !mixerActive || !selectedAudioTracks.length) return;
    const params = new URLSearchParams({
      tracks: selectedAudioTracks.map((track) => track.streamIndex).join(","),
      volumes: selectedAudioTracks.map((track) => trackLevels[track.streamIndex] ?? 1).join(","),
      start: Math.max(0, position).toFixed(3),
    });
    mixAnchorRef.current = Math.max(0, position);
    setMixState("loading");
    audio.src = `${mediaRegistration.mixBaseUrl}?${params.toString()}`;
    audio.volume = volume;
    audio.muted = isMuted;
    audio.playbackRate = playbackRate;
    audio.load();
    if (videoRef.current && !videoRef.current.paused) {
      void audio.play().catch(() => undefined);
    }
  }, [isMuted, mediaRegistration, mixerActive, playbackRate, selectedAudioTracks, trackLevels, volume]);

  useEffect(() => {
    if (!mixerActive) {
      stopMix();
      return;
    }
    if (!selectedAudioTracks.length) {
      setMixerActive(false);
      stopMix();
      return;
    }
    const timer = window.setTimeout(() => startMixAt(), 220);
    return () => window.clearTimeout(timer);
  }, [mixerActive, selectedAudioTracks, startMixAt, stopMix, trackLevels]);

  const loadVideoPath = useCallback(async (path: string) => {
    if (exportPhase === "running") {
      showNotice("Stop the current clip export before opening another video.", "error");
      return;
    }
    if (!IS_TAURI) {
      showNotice("Open the Tauri desktop app to choose local files.", "error");
      return;
    }
    setOpening(true);
    try {
      if (collaboration) {
        await invoke("stop_collaboration").catch(() => undefined);
        setCollaboration(null);
        setCollaborationPhase("idle");
      }
      await persistPlaybackPosition().catch(() => undefined);
      const next = await invoke<SidecarDocument>("load_video", { videoPath: path });
      const registration = await invoke<MediaRegistration>("register_media_source", { videoPath: next.videoPath });
      applyDocument(next);
      recordRecentProject(next);
      setMediaRegistration(registration);
      setVideoUrl(registration.url);
      setTrackLevels(registration.audioTracks[0] ? { [registration.audioTracks[0].streamIndex]: 1 } : {});
      setMixerActive(false);
      setMixerOpen(false);
      setWaveformOpen(false);
      setWaveformSelection(null);
      setEditingId(null);
      setSubtitleTimingOverride(null);
      setAnalysisOpen(false);
      setExportOpen(false);
      setExportPhase("idle");
      setExportCursor(0);
      setExportTotal(0);
      setExportOutputDirectory("");
      setExportDetail("Choose a folder and export every completed mark.");
      stopMix();
      resumePositionRef.current = next.playbackPosition;
      setDuration(0);
      setCurrentTime(next.playbackPosition);
      currentTimeRef.current = next.playbackPosition;
      setIsPlaying(false);
      setRawMode(false);
      setSettingsOpen(false);
      setAnalysisStart(next.playbackPosition);
      setAnalysisPhase("idle");
      setAnalysisCursor(0);
      setAnalysisTotal(0);
      setAnalysisDetail("Ready to build a private local timeline.");
      window.setTimeout(() => void checkOllama(), 200);
    } catch (error) {
      showNotice(errorMessage(error), "error");
    } finally {
      setOpening(false);
    }
  }, [applyDocument, checkOllama, collaboration, exportPhase, persistPlaybackPosition, recordRecentProject, showNotice, stopMix]);

  const openVideo = useCallback(async () => {
    if (exportPhase === "running") {
      showNotice("Stop the current clip export before opening another video.", "error");
      return;
    }
    if (!IS_TAURI) {
      showNotice("Open the Tauri desktop app to choose local files.", "error");
      return;
    }
    setOpening(true);
    try {
      const path = await invoke<string | null>("pick_video");
      if (path) await loadVideoPath(path);
    } catch (error) {
      showNotice(errorMessage(error), "error");
    } finally {
      setOpening(false);
    }
  }, [exportPhase, loadVideoPath, showNotice]);

  const startSharing = useCallback(async () => {
    if (!document || !IS_TAURI) {
      showNotice("Open a local video before creating a session.", "error");
      return;
    }
    setCollaborationPhase("hosting");
    localStorage.setItem("framenote:display-name", displayName.trim() || "Host");
    try {
      const session = await invoke<CollaborationSession>("host_collaboration", {
        videoPath: document.videoPath,
        displayName,
      });
      lastSharedMarkdownRef.current = document.markdown;
      collaborationPollFailuresRef.current = 0;
      setCollaboration(session);
      setCollaborationPhase("connected");
      showNotice(`Session ${session.code} is live on this local network.`);
    } catch (error) {
      setCollaborationPhase("idle");
      showNotice(errorMessage(error), "error");
    }
  }, [displayName, document, showNotice]);

  const joinSharing = useCallback(async () => {
    if (!IS_TAURI) return;
    const normalizedCode = joinCode.replace(/\D/g, "").slice(0, 6);
    if (normalizedCode.length !== 6) {
      showNotice("Enter the six-digit session code.", "error");
      return;
    }
    setCollaborationPhase("joining");
    localStorage.setItem("framenote:display-name", displayName.trim() || "Guest");
    try {
      await persistPlaybackPosition().catch(() => undefined);
      const result = await invoke<JoinCollaborationResult>("join_collaboration", {
        code: normalizedCode,
        displayName,
      });
      stopMix();
      applyDocument(result.document);
      setMediaRegistration(result.mediaRegistration);
      setVideoUrl(result.mediaRegistration.url);
      setTrackLevels(result.mediaRegistration.audioTracks[0]
        ? { [result.mediaRegistration.audioTracks[0].streamIndex]: 1 }
        : {});
      setMixerActive(false);
      setMixerOpen(false);
      setWaveformOpen(false);
      setWaveformSelection(null);
      setWaveform(null);
      setWaveformPhase("idle");
      setEditingId(null);
      setSubtitleTimingOverride(null);
      setAnalysisOpen(false);
      setExportOpen(false);
      setSettingsOpen(false);
      setRawMode(false);
      setDuration(0);
      setPlaybackRate(result.transport.playbackRate);
      setCurrentTime(result.transport.position);
      currentTimeRef.current = result.transport.position;
      resumePositionRef.current = result.transport.position;
      initialRemotePlayingRef.current = result.transport.playing;
      setIsPlaying(false);
      lastSharedMarkdownRef.current = result.document.markdown;
      collaborationPollFailuresRef.current = 0;
      setCollaboration(result.session);
      setCollaborationPhase("connected");
      showNotice(`Joined ${result.document.videoName} · playback and notes are live.`);
    } catch (error) {
      setCollaborationPhase("idle");
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, displayName, joinCode, persistPlaybackPosition, showNotice, stopMix]);

  const stopSharing = useCallback(async () => {
    const wasGuest = collaboration?.mode === "guest";
    try {
      if (IS_TAURI) await invoke("stop_collaboration");
    } finally {
      setCollaboration(null);
      setCollaborationPhase("idle");
      setCollaborationOpen(false);
      collaborationPollFailuresRef.current = 0;
      if (wasGuest) {
        stopMix();
        setDocument(null);
        setVideoUrl(null);
        setMediaRegistration(null);
        setIsPlaying(false);
        setCurrentTime(0);
        currentTimeRef.current = 0;
      }
    }
  }, [collaboration?.mode, stopMix]);

  const copySessionCode = useCallback(async () => {
    if (!collaboration) return;
    try {
      await navigator.clipboard.writeText(collaboration.code);
      showNotice("Session code copied.");
    } catch {
      showNotice(`Session code: ${collaboration.code}`);
    }
  }, [collaboration, showNotice]);

  const publishTransport = useCallback((transport?: Partial<CollaborationTransport>) => {
    const video = videoRef.current;
    if (!collaboration || !video || Date.now() < remoteTransportUntilRef.current) return;
    const payload: CollaborationTransport = {
      position: Math.max(0, transport?.position ?? video.currentTime),
      playing: transport?.playing ?? !video.paused,
      playbackRate: transport?.playbackRate ?? video.playbackRate,
      emittedAt: Date.now(),
    };
    void invoke("publish_collaboration_event", { kind: "transport", payload })
      .catch(() => setCollaborationPhase("reconnecting"));
  }, [collaboration]);

  useEffect(() => {
    if (!collaboration || !document || !IS_TAURI) return;
    if (document.markdown === lastSharedMarkdownRef.current) return;
    const markdown = document.markdown;
    lastSharedMarkdownRef.current = markdown;
    const timer = window.setTimeout(() => {
      void invoke("publish_collaboration_event", {
        kind: "document",
        payload: { markdown },
      }).catch((error) => {
        if (lastSharedMarkdownRef.current === markdown) lastSharedMarkdownRef.current = "";
        setCollaborationPhase("reconnecting");
        showNotice(errorMessage(error), "error");
      });
    }, 120);
    return () => window.clearTimeout(timer);
  }, [collaboration, document, showNotice]);

  useEffect(() => {
    if (!collaboration || !IS_TAURI) return;
    let cancelled = false;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const result = await invoke<CollaborationPollResult>("poll_collaboration");
        if (cancelled) return;
        collaborationPollFailuresRef.current = 0;
        setCollaborationPhase("connected");
        setCollaboration((current) => current ? {
          ...current,
          participantCount: result.participantCount,
          participants: result.participants,
        } : current);
        for (const event of result.events) {
          if (event.senderId === collaboration.clientId) continue;
          if (event.kind === "document") {
            const markdown = typeof event.payload.markdown === "string" ? event.payload.markdown : null;
            const current = documentRef.current;
            if (markdown !== null && current && markdown !== current.markdown) {
              lastSharedMarkdownRef.current = markdown;
              applyDocument({ ...current, markdown });
            }
            continue;
          }
          if (event.kind === "transport") {
            const rawPosition = Number(event.payload.position);
            const playbackRate = Number(event.payload.playbackRate);
            const playing = event.payload.playing === true;
            const emittedAt = Number(event.payload.emittedAt);
            const video = videoRef.current;
            if (!video || !Number.isFinite(rawPosition) || !Number.isFinite(playbackRate)) continue;
            const transitSeconds = playing && Number.isFinite(emittedAt)
              ? Math.max(0, Math.min(5, (Date.now() - emittedAt) / 1_000))
              : 0;
            const position = rawPosition + transitSeconds * playbackRate;
            remoteTransportUntilRef.current = Date.now() + 1_200;
            setPlaybackRate(playbackRate);
            video.playbackRate = playbackRate;
            if (Math.abs(video.currentTime - position) > 0.18) {
              video.currentTime = Math.max(0, Math.min(video.duration || Number.POSITIVE_INFINITY, position));
              currentTimeRef.current = video.currentTime;
              setCurrentTime(video.currentTime);
            }
            if (playing && video.paused) {
              void video.play().catch(() => showNotice("Click the player once to allow synchronized playback.", "error"));
            } else if (!playing && !video.paused) {
              video.pause();
            }
          }
        }
      } catch (error) {
        if (cancelled) return;
        collaborationPollFailuresRef.current += 1;
        setCollaborationPhase("reconnecting");
        if (collaborationPollFailuresRef.current === 3) showNotice(errorMessage(error), "error");
      } finally {
        if (!cancelled) timer = window.setTimeout(poll, 280);
      }
    };
    void poll();
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, [applyDocument, collaboration?.clientId, collaboration?.mode, collaboration?.code, showNotice]);

  useEffect(() => {
    if (collaboration?.mode !== "host" || !isPlaying) return;
    const timer = window.setInterval(() => publishTransport({ playing: true }), 2_000);
    return () => window.clearInterval(timer);
  }, [collaboration?.mode, isPlaying, publishTransport]);

  const reloadMarkdown = useCallback(async (quiet = false) => {
    if (!IS_TAURI || !document || rawDirty) return;
    try {
      const next = await invoke<SidecarDocument>("read_sidecar", { videoPath: document.videoPath });
      applyDocument(next);
      if (!quiet) showNotice("Markdown reloaded from disk.");
    } catch (error) {
      if (!quiet) showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, document, rawDirty, showNotice]);

  useEffect(() => {
    const onFocus = () => void reloadMarkdown(true);
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [reloadMarkdown]);

  const togglePlayback = useCallback(() => {
    const video = videoRef.current;
    if (!video) return;
    if (video.paused) void video.play();
    else video.pause();
  }, []);

  const toggleFullscreen = useCallback(async () => {
    if (IS_TAURI) {
      const appWindow = getCurrentWindow();
      await appWindow.setFullscreen(!(await appWindow.isFullscreen()));
      return;
    }
    if (window.document.fullscreenElement) await window.document.exitFullscreen();
    else await playerRef.current?.requestFullscreen();
  }, []);

  const seekTo = useCallback((seconds: number) => {
    const next = Math.max(0, Math.min(duration || Number.POSITIVE_INFINITY, seconds));
    if (videoRef.current) videoRef.current.currentTime = next;
    currentTimeRef.current = next;
    setCurrentTime(next);
  }, [duration]);

  const stepFrame = useCallback((direction: -1 | 1) => {
    const video = videoRef.current;
    if (video && !video.paused) video.pause();
    const frameRate = mediaRegistration?.frameRate && mediaRegistration.frameRate > 0
      ? mediaRegistration.frameRate
      : 30;
    const time = Math.max(0, Math.min(duration || Number.POSITIVE_INFINITY, currentTimeRef.current + direction / frameRate));
    seekTo(time);
    setFrameSeek({ time, scale: 0.008, lift: 190 });
    if (frameSeekTimerRef.current) window.clearTimeout(frameSeekTimerRef.current);
    frameSeekTimerRef.current = window.setTimeout(() => setFrameSeek(null), 1200);
  }, [duration, mediaRegistration?.frameRate, seekTo]);

  const beginPrecisionSeek = useCallback((event: React.PointerEvent<HTMLInputElement>) => {
    if (!duration) return;
    event.preventDefault();
    if (frameSeekTimerRef.current) window.clearTimeout(frameSeekTimerRef.current);
    setFrameSeek(null);
    const bounds = event.currentTarget.getBoundingClientRect();
    const startTime = Math.max(0, Math.min(duration, ((event.clientX - bounds.left) / bounds.width) * duration));
    event.currentTarget.setPointerCapture(event.pointerId);
    precisionSeekRef.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      startTime,
      width: bounds.width,
    };
    seekTo(startTime);
    setPrecisionSeek({ time: startTime, scale: 1, lift: 0 });
  }, [duration, seekTo]);

  const movePrecisionSeek = useCallback((event: React.PointerEvent<HTMLInputElement>) => {
    const anchor = precisionSeekRef.current;
    if (!anchor || anchor.pointerId !== event.pointerId || !duration) return;
    event.preventDefault();
    const lift = Math.max(0, anchor.startY - event.clientY);
    const scale = precisionSeekScale(lift);
    const time = Math.max(0, Math.min(
      duration,
      anchor.startTime + ((event.clientX - anchor.startX) / anchor.width) * duration * scale,
    ));
    seekTo(time);
    setPrecisionSeek({ time, scale, lift });
  }, [duration, seekTo]);

  const endPrecisionSeek = useCallback((event: React.PointerEvent<HTMLInputElement>) => {
    if (precisionSeekRef.current?.pointerId !== event.pointerId) return;
    precisionSeekRef.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    setPrecisionSeek(null);
  }, []);

  const handleLoadedMetadata = useCallback((video: HTMLVideoElement) => {
    setDuration(video.duration);
    const saved = resumePositionRef.current;
    const resumeAt = saved > 0 && saved < video.duration - 2 ? saved : 0;
    if (resumeAt > 0) {
      video.currentTime = resumeAt;
      currentTimeRef.current = resumeAt;
      setCurrentTime(resumeAt);
      showNotice(`Resumed at ${formatTime(resumeAt, true)}.`);
    }
    if (documentRef.current) {
      updateRecentPlayback(documentRef.current.videoPath, resumeAt, video.duration);
    }
    resumePositionRef.current = 0;
    if (initialRemotePlayingRef.current) {
      initialRemotePlayingRef.current = false;
      remoteTransportUntilRef.current = Date.now() + 1_200;
      void video.play().catch(() => showNotice("Click the player once to allow synchronized playback.", "error"));
    }
  }, [showNotice, updateRecentPlayback]);

  const handleTimeUpdate = useCallback((video: HTMLVideoElement) => {
    const time = video.currentTime;
    currentTimeRef.current = time;
    setCurrentTime(time);
    const audio = mixAudioRef.current;
    if (mixerActive && mixState === "ready" && audio && !audio.paused) {
      const mixedTime = mixAnchorRef.current + audio.currentTime;
      if (Math.abs(mixedTime - time) > 1.25) startMixAt(time);
    }
  }, [mixState, mixerActive, startMixAt]);

  const handleVideoPlay = useCallback((video: HTMLVideoElement) => {
    setIsPlaying(true);
    if (mixerActive) startMixAt(video.currentTime);
    publishTransport({ position: video.currentTime, playing: true });
  }, [mixerActive, publishTransport, startMixAt]);

  const handleVideoPause = useCallback((video: HTMLVideoElement) => {
    setIsPlaying(false);
    mixAudioRef.current?.pause();
    void persistPlaybackPosition(video.currentTime).catch(() => undefined);
    publishTransport({ position: video.currentTime, playing: false });
  }, [persistPlaybackPosition, publishTransport]);

  const handleVideoEnded = useCallback(() => {
    setIsPlaying(false);
    stopMix();
    currentTimeRef.current = 0;
    void persistPlaybackPosition(0).catch(() => undefined);
  }, [persistPlaybackPosition, stopMix]);

  const handleSeeked = useCallback((video: HTMLVideoElement) => {
    currentTimeRef.current = video.currentTime;
    if (mixerActive && !video.paused) startMixAt(video.currentTime);
    publishTransport({ position: video.currentTime, playing: !video.paused });
  }, [mixerActive, publishTransport, startMixAt]);

  const addBookmark = useCallback(async () => {
    if (!document || !IS_TAURI) {
      if (IS_DEMO) showNotice("Demo mode is read-only. Run the Tauri app to create a mark.");
      return;
    }
    try {
      const result = await invoke<AddBookmarkResult>("add_bookmark", {
        videoPath: document.videoPath,
        timestampSeconds: videoRef.current?.currentTime ?? currentTime,
      });
      applyDocument(result.document);
      setFilter("all");
      setRawMode(false);
      setEditingId(result.entryId);
      showNotice(`Mark started at ${formatTime(videoRef.current?.currentTime ?? currentTime, true)}.`);
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, currentTime, document, showNotice]);

  const endBookmark = useCallback(async () => {
    if (!document || !IS_TAURI) {
      if (IS_DEMO) showNotice("Demo mode is read-only. Run the Tauri app to end a mark.");
      return;
    }
    const timestamp = videoRef.current?.currentTime ?? currentTime;
    try {
      const result = await invoke<AddBookmarkResult>("end_bookmark", {
        videoPath: document.videoPath,
        timestampSeconds: timestamp,
      });
      applyDocument(result.document);
      setEditingId(null);
      setFilter("all");
      showNotice(`Mark ended at ${formatTime(timestamp, true)}.`);
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, currentTime, document, showNotice]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.matches("input, textarea, select, [contenteditable='true']")) return;
      const key = event.key.toLowerCase();
      if (key === "n" && document) {
        event.preventDefault();
        void addBookmark();
      } else if (key === "m" && document) {
        event.preventDefault();
        void endBookmark();
      } else if (event.key === " ") {
        event.preventDefault();
        togglePlayback();
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        seekTo((videoRef.current?.currentTime ?? currentTime) - 10);
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        seekTo((videoRef.current?.currentTime ?? currentTime) + 10);
      } else if (event.code === "Comma" || event.key === ",") {
        event.preventDefault();
        stepFrame(-1);
      } else if (event.code === "Period" || event.key === ".") {
        event.preventDefault();
        stepFrame(1);
      } else if (key === "f") {
        void toggleFullscreen();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [addBookmark, currentTime, document, endBookmark, seekTo, stepFrame, toggleFullscreen, togglePlayback]);

  const updateEntry = useCallback(async (entry: TimelineEntry, text: string) => {
    if (!document || !IS_TAURI || !entry.editable || text.trim() === entry.text) {
      setEditingId(null);
      return;
    }
    try {
      const next = await invoke<SidecarDocument>("update_entry", {
        videoPath: document.videoPath,
        entryId: entry.id,
        text,
      });
      applyDocument(next);
      setEditingId(null);
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, document, showNotice]);

  const persistSubtitle = useCallback(async (entry: TimelineEntry, draft: SubtitleDraft): Promise<boolean> => {
    if (!document || !IS_TAURI || entry.type !== "subtitle" || !entry.editable) return false;
    if (!Number.isFinite(draft.start) || !Number.isFinite(draft.end) || draft.start < 0 || draft.end <= draft.start) {
      showNotice("Subtitle end time must be after its start time.", "error");
      return false;
    }
    if (!draft.text.trim()) {
      showNotice("Subtitle text cannot be empty.", "error");
      return false;
    }
    try {
      const next = await invoke<SidecarDocument>("update_subtitle", {
        videoPath: document.videoPath,
        entryId: entry.id,
        startSeconds: draft.start,
        endSeconds: draft.end,
        text: draft.text,
        speaker: draft.speaker,
        language: draft.language,
      });
      applyDocument(next);
      return true;
    } catch (error) {
      showNotice(errorMessage(error), "error");
      return false;
    }
  }, [applyDocument, document, showNotice]);

  const createSubtitleAt = useCallback(async (requestedStart: number): Promise<string | null> => {
    if (!document || !IS_TAURI) {
      if (IS_DEMO) showNotice("Demo mode is read-only. Run the Tauri app to add subtitles.");
      return null;
    }
    const start = Math.max(0, Number.isFinite(requestedStart) ? requestedStart : 0);
    const end = duration > start ? Math.min(duration, start + 3) : start + 3;
    try {
      const result = await invoke<AddBookmarkResult>("add_subtitle", {
        videoPath: document.videoPath,
        startSeconds: start,
        endSeconds: Math.max(start + 0.1, end),
      });
      applyDocument(result.document);
      setRawMode(false);
      setFilter("subtitle");
      setEditingId(result.entryId);
      setSubtitleTimingOverride({ id: result.entryId, start, end: Math.max(start + 0.1, end) });
      seekTo(start);
      return result.entryId;
    } catch (error) {
      showNotice(errorMessage(error), "error");
      return null;
    }
  }, [applyDocument, document, duration, seekTo, showNotice]);

  const addSubtitleAtPlayhead = useCallback(async () => {
    const playhead = videoRef.current?.currentTime ?? currentTimeRef.current;
    const id = await createSubtitleAt(playhead);
    if (id) showNotice(`Subtitle added at ${formatTime(playhead, true)}.`);
  }, [createSubtitleAt, showNotice]);

  const createSubtitleFromWaveform = useCallback(async () => {
    const selection = waveformSelection;
    if (!selection || selection.end - selection.start < 0.01) return;
    if (!document || !IS_TAURI) {
      if (IS_DEMO) showNotice("Demo mode is read-only. Run the Tauri app to add subtitles.");
      return;
    }
    try {
      const result = await invoke<AddBookmarkResult>("add_subtitle", {
        videoPath: document.videoPath,
        startSeconds: selection.start,
        endSeconds: selection.end,
      });
      applyDocument(result.document);
      setRawMode(false);
      setFilter("subtitle");
      setEditingId(result.entryId);
      setSubtitleTimingOverride({ id: result.entryId, start: selection.start, end: selection.end });
      setWaveformSelection(null);
      seekTo(selection.start);
      showNotice(`Subtitle range added · ${(selection.end - selection.start).toFixed(2)} seconds.`);
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, document, seekTo, showNotice, waveformSelection]);

  const createMarkFromWaveform = useCallback(async () => {
    const selection = waveformSelection;
    if (!selection || selection.end - selection.start < 0.01) return;
    if (!document || !IS_TAURI) {
      if (IS_DEMO) showNotice("Demo mode is read-only. Run the Tauri app to create a mark.");
      return;
    }
    try {
      const result = await invoke<AddBookmarkResult>("add_bookmark_range", {
        videoPath: document.videoPath,
        startSeconds: selection.start,
        endSeconds: selection.end,
      });
      applyDocument(result.document);
      setRawMode(false);
      setFilter("all");
      setEditingId(result.entryId);
      setWaveformSelection(null);
      seekTo(selection.start);
      showNotice(`Mark range added · ${(selection.end - selection.start).toFixed(2)} seconds.`);
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, document, seekTo, showNotice, waveformSelection]);

  const saveSubtitle = useCallback(async (entry: TimelineEntry, draft: SubtitleDraft) => {
    if (await persistSubtitle(entry, draft)) {
      setEditingId(null);
      setSubtitleTimingOverride(null);
    }
  }, [persistSubtitle]);

  const advanceSubtitle = useCallback(async (entry: TimelineEntry, draft: SubtitleDraft) => {
    if (subtitleTransitionRef.current) return;
    subtitleTransitionRef.current = true;
    try {
      if (!await persistSubtitle(entry, draft)) return;
      const currentIndex = subtitleEntries.findIndex((candidate) => candidate.id === entry.id);
      const next = currentIndex >= 0 ? subtitleEntries[currentIndex + 1] : undefined;
      if (next) {
        setFilter("subtitle");
        setEditingId(next.id);
        setSubtitleTimingOverride({ id: next.id, start: next.start, end: next.end ?? next.start + 3 });
        seekTo(next.start);
        return;
      }
      const playhead = videoRef.current?.currentTime ?? currentTimeRef.current;
      await createSubtitleAt(nextSubtitleStart(playhead, draft.start, draft.end));
    } finally {
      subtitleTransitionRef.current = false;
    }
  }, [createSubtitleAt, persistSubtitle, seekTo, subtitleEntries]);

  const deleteEntry = useCallback(async (entry: TimelineEntry) => {
    if (!document || !IS_TAURI || !entry.editable) return;
    try {
      const next = await invoke<SidecarDocument>("delete_entry", {
        videoPath: document.videoPath,
        entryId: entry.id,
      });
      applyDocument(next);
      showNotice("Timeline entry removed.");
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, document, showNotice]);

  const saveRawMarkdown = useCallback(async () => {
    if (!document || !IS_TAURI) return;
    try {
      const next = await invoke<SidecarDocument>("save_markdown", {
        videoPath: document.videoPath,
        markdown: rawDraft,
      });
      applyDocument(next);
      showNotice("Markdown saved.");
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [applyDocument, document, rawDraft, showNotice]);

  const runAnalysis = useCallback(async () => {
    if (!document || !duration || !IS_TAURI || analysisPhase === "running") return;
    const status = await checkOllama();
    if (!status?.available || !status.modelAvailable) {
      setAnalysisOpen(false);
      setSettingsOpen(true);
      showNotice(status?.message ?? "Ollama is unavailable.", "error");
      return;
    }

    const chunkIsComplete = (chunk: { start: number; end: number }) => isChunkCovered(entries, chunk.start, chunk.end)
      && (config.provider !== "gemini" || isChunkTranscriptCovered(entries, chunk.start, chunk.end));
    const pending = plannedChunks.filter((chunk) => !chunkIsComplete(chunk));
    const completedInPlan = plannedChunks.length - pending.length;
    setAnalysisTotal(plannedChunks.length);
    setAnalysisCursor(completedInPlan);
    if (!pending.length) {
      setAnalysisPhase("complete");
      setAnalysisDetail("This selected range is already saved in the Markdown timeline.");
      return;
    }

    const jobId = crypto.randomUUID();
    activeJobRef.current = jobId;
    cancelRequestedRef.current = false;
    setAnalysisPhase("running");
    setAnalysisDetail(`Preparing ${pending.length} remaining ${pending.length === 1 ? "chunk" : "chunks"}…`);

    try {
      await invoke("begin_analysis", { jobId });
      let finished = completedInPlan;
      for (const chunk of pending) {
        if (cancelRequestedRef.current) throw new Error("ANALYSIS_CANCELLED");
        setAnalysisDetail(`Reading ${formatTime(chunk.start, true)}–${formatTime(chunk.end, true)} · playback stays available`);
        const result = await invoke<AnalysisChunkResult>("analyze_chunk", {
          request: {
            jobId,
            videoPath: document.videoPath,
            startSeconds: chunk.start,
            endSeconds: chunk.end,
            ollamaUrl: config.ollamaUrl,
            model: config.model,
            provider: config.provider,
            apiKey: config.apiKey || null,
            whisperModelPath: config.whisperModelPath || null,
            frameCount: config.frameCount,
          },
        });
        if (cancelRequestedRef.current) throw new Error("ANALYSIS_CANCELLED");
        const next = await invoke<SidecarDocument>("append_analysis_result", {
          videoPath: document.videoPath,
          startSeconds: chunk.start,
          endSeconds: chunk.end,
          summary: result.summary,
          transcriptCues: result.transcriptCues,
          transcriptComplete: result.transcriptComplete,
        });
        applyDocument(next);
        finished += 1;
        setAnalysisCursor(finished);
        setAnalysisDetail(`${result.frameCount} frames · ${result.transcriptSource} · saved to Markdown`);
      }
      setAnalysisPhase("complete");
      setAnalysisDetail("Selected range complete. Every summary is saved beside the video.");
      showNotice("Selected video range analyzed.");
    } catch (error) {
      const message = errorMessage(error);
      if (message.includes("ANALYSIS_CANCELLED")) {
        setAnalysisPhase("paused");
        setAnalysisDetail("Analysis paused. Completed chunks are saved and ready to resume.");
      } else {
        setAnalysisPhase("error");
        setAnalysisDetail(message);
        showNotice(message, "error");
      }
    } finally {
      await invoke("finish_analysis", { jobId }).catch(() => undefined);
      activeJobRef.current = null;
    }
  }, [analysisPhase, applyDocument, checkOllama, config, document, duration, entries, plannedChunks, showNotice]);

  const cancelAnalysis = useCallback(async () => {
    cancelRequestedRef.current = true;
    setAnalysisDetail("Stopping after the current safe boundary…");
    if (activeJobRef.current && IS_TAURI) {
      await invoke("cancel_analysis", { jobId: activeJobRef.current }).catch(() => undefined);
    }
  }, []);

  const chooseExportDestination = useCallback(async () => {
    if (!IS_TAURI) {
      showNotice("Open the Tauri desktop app to choose an export folder.", "error");
      return;
    }
    try {
      const folder = await invoke<string | null>("pick_export_directory");
      if (folder) {
        setExportDestination(folder);
        setExportPhase("idle");
        setExportDetail("Ready to export completed marks.");
      }
    } catch (error) {
      showNotice(errorMessage(error), "error");
    }
  }, [showNotice]);

  const cancelExport = useCallback(async () => {
    exportCancelRequestedRef.current = true;
    setExportDetail("Stopping the current FFmpeg export…");
    if (activeExportJobRef.current && IS_TAURI) {
      await invoke("cancel_export", { jobId: activeExportJobRef.current }).catch(() => undefined);
    }
  }, []);

  const runExport = useCallback(async () => {
    if (!document || !IS_TAURI || exportPhase === "running") return;
    if (!completedMarks.length) {
      showNotice("End at least one mark before exporting.", "error");
      return;
    }
    if (!exportDestination) {
      showNotice("Choose where to save the exported clips.", "error");
      return;
    }
    if (exportAudioMode === "selected" && !selectedAudioTracks.length) {
      showNotice("Select at least one audio track in the mixer, or export all tracks.", "error");
      return;
    }
    const jobId = crypto.randomUUID();
    activeExportJobRef.current = jobId;
    exportCancelRequestedRef.current = false;
    setExportPhase("running");
    setExportCursor(0);
    setExportTotal(completedMarks.length);
    setExportDetail("Preparing a new rough-cut folder…");
    try {
      const outputDirectory = await invoke<string>("prepare_export_directory", {
        videoPath: document.videoPath,
        parentDirectory: exportDestination,
      });
      setExportOutputDirectory(outputDirectory);
      await invoke("begin_export", { jobId });
      const clips: ExportManifestClip[] = [];
      for (let index = 0; index < completedMarks.length; index += 1) {
        if (exportCancelRequestedRef.current) throw new Error("ANALYSIS_CANCELLED");
        const mark = completedMarks[index];
        setExportDetail(`Exporting ${index + 1} of ${completedMarks.length} · ${mark.text}`);
        const result = await invoke<ExportClipResult>("export_mark_clip", {
          request: {
            jobId,
            videoPath: document.videoPath,
            outputDirectory,
            clipIndex: index,
            startSeconds: mark.start,
            endSeconds: mark.end!,
            label: mark.text,
            preset: exportPreset,
            audioStreamIndexes: exportAudioMode === "selected"
              ? selectedAudioTracks.map((track) => track.streamIndex)
              : null,
          },
        });
        clips.push({
          fileName: result.fileName,
          subtitleFileName: result.subtitleFileName,
          startSeconds: mark.start,
          endSeconds: mark.end!,
          label: mark.text,
        });
        setExportCursor(index + 1);
      }
      const audioDescription = exportAudioMode === "all"
        ? `All ${mediaRegistration?.audioTracks.length ?? 0} embedded tracks, kept separate`
        : `${selectedAudioTracks.map((track) => track.label).join(", ")}, kept separate`;
      await invoke<string>("write_export_manifest", {
        videoPath: document.videoPath,
        outputDirectory,
        preset: exportPreset,
        audioDescription,
        clips,
      });
      setExportPhase("complete");
      setExportDetail(`${clips.length} clips, SRT subtitles, and Resolve manifests exported.`);
      showNotice(`Rough cut exported to ${outputDirectory}.`);
    } catch (error) {
      const message = errorMessage(error);
      if (message.includes("ANALYSIS_CANCELLED")) {
        setExportPhase("cancelled");
        setExportDetail("Export stopped. Completed clips remain in the export folder.");
      } else {
        setExportPhase("error");
        setExportDetail(message);
        showNotice(message, "error");
      }
    } finally {
      await invoke("finish_export", { jobId }).catch(() => undefined);
      activeExportJobRef.current = null;
    }
  }, [completedMarks, document, exportAudioMode, exportDestination, exportPhase, exportPreset, mediaRegistration?.audioTracks.length, selectedAudioTracks, showNotice]);

  const toggleExportMenu = useCallback(() => {
    const opening = !exportOpen;
    if (opening) {
      setAnalysisOpen(false);
      setMixerOpen(false);
      setSettingsOpen(false);
      setWaveformOpen(false);
      setWaveformSelection(null);
    }
    setExportOpen(opening);
  }, [exportOpen]);

  const toggleWaveform = useCallback(() => {
    const opening = !waveformOpen;
    if (opening) {
      setAnalysisOpen(false);
      setMixerOpen(false);
      setExportOpen(false);
    } else {
      setWaveformSelection(null);
    }
    setWaveformOpen(opening);
  }, [waveformOpen]);

  const useCurrentAnalysisStart = useCallback(() => {
    const start = videoRef.current?.currentTime ?? currentTimeRef.current;
    setAnalysisStart(Math.max(0, Math.min(duration || start, start)));
    setAnalysisCursor(0);
    setAnalysisTotal(0);
    setAnalysisPhase("idle");
    setAnalysisDetail("Ready to analyze from the current frame.");
  }, [duration]);

  const toggleAnalysisMenu = useCallback(() => {
    const openingMenu = !analysisOpen;
    if (openingMenu) {
      setMixerOpen(false);
      setSettingsOpen(false);
      setExportOpen(false);
      setWaveformOpen(false);
      setWaveformSelection(null);
      if (!matchesAnalysisResumeState(analysisPhase)) {
        const start = videoRef.current?.currentTime ?? currentTimeRef.current;
        setAnalysisStart(Math.max(0, Math.min(duration || start, start)));
        setAnalysisCursor(0);
        setAnalysisTotal(0);
        setAnalysisPhase("idle");
        setAnalysisDetail("Ready to analyze from the current frame.");
      }
    }
    setAnalysisOpen(openingMenu);
  }, [analysisOpen, analysisPhase, duration]);

  const activeEntryId = useMemo(() => {
    const active = entries
      .filter((entry) => entry.start <= currentTime + 0.4)
      .filter((entry) => entry.source === "embedded-chapter"
        ? Math.abs(currentTime - entry.start) <= 0.4
        : entry.end === undefined || currentTime < entry.end)
      .at(-1);
    return active?.id;
  }, [currentTime, entries]);
  const seekLens = precisionSeek ?? frameSeek;
  const panelCollapseBlocked = !timelineCollapsed && (rawDirty || editingId !== null);

  if (!document) {
    return (
      <main className="empty-shell">
        <header className="app-header empty-header">
          <Brand />
          <div className="empty-header-actions">
            <div className="privacy-note"><span className="status-dot" /> Local files stay local</div>
            <button className="session-button" onClick={() => setCollaborationOpen(true)}><Users size={14} /> Join session</button>
          </div>
        </header>
        <section className={`empty-state ${recentProjects.length ? "with-recents" : ""}`}>
          <div className="empty-intro">
            <div className="empty-art" aria-hidden="true">
              <div className="empty-frame">
                <div className="empty-play"><Play size={24} fill="currentColor" /></div>
                <div className="empty-progress"><i /><b /></div>
              </div>
              <div className="empty-note n-one"><span>01:24</span><i /></div>
              <div className="empty-note n-two"><Sparkles size={14} /><i /></div>
            </div>
            <p className="eyebrow">VIDEO, MEET MARKDOWN</p>
            <h1>Watch closely.<br />Keep what matters.</h1>
            <p className="empty-copy">Open a local video to create a timestamped Markdown timeline beside it. No upload, no duplicate media.</p>
            <button className="primary large" onClick={() => void openVideo()} disabled={opening}>
              {opening ? <RefreshCw className="spin" size={18} /> : <FolderOpen size={18} />}
              {opening ? "Opening…" : "Open a video"}
            </button>
            <button className="empty-join" onClick={() => setCollaborationOpen(true)}><Share2 size={14} /> Join a peer with a code</button>
            <div className="shortcut-hint"><kbd>N</kbd> starts a mark <kbd>M</kbd> ends it</div>
          </div>
          {recentProjects.length > 0 && (
            <section className="recent-projects" aria-labelledby="recent-projects-title">
              <div className="recent-projects-head">
                <div><p className="eyebrow">CONTINUE</p><h2 id="recent-projects-title">Recent projects</h2></div>
                <span>{recentProjects.length} local</span>
              </div>
              <div className="recent-project-list">
                {recentProjects.map((project) => (
                  <article className="recent-project" key={project.videoPath}>
                    <button
                      className="recent-project-open"
                      disabled={opening}
                      aria-label={`Open ${project.videoName}`}
                      onClick={() => void loadVideoPath(project.videoPath)}
                    >
                      <span className="recent-project-icon"><Film size={15} /></span>
                      <span className="recent-project-copy">
                        <strong>{project.videoName}</strong>
                        <small title={project.videoPath}>{project.videoPath}</small>
                        <span>
                          <time>{recentOpenedLabel(project.lastOpenedAt)}</time>
                          <i />
                          <b>{project.duration
                            ? `${formatTime(project.playbackPosition || 0, true)} / ${formatTime(project.duration, true)}`
                            : `${formatTime(project.playbackPosition || 0, true)} saved`}</b>
                        </span>
                      </span>
                      <Play size={12} fill="currentColor" />
                    </button>
                    <button
                      className="recent-project-remove"
                      aria-label={`Remove ${project.videoName} from recent projects`}
                      title="Remove from recents"
                      onClick={() => updateRecentProjects((current) => current.filter((candidate) => candidate.videoPath !== project.videoPath))}
                    ><X size={12} /></button>
                  </article>
                ))}
              </div>
              <p className="recent-projects-note"><span className="status-dot" /> Paths and playback positions stay on this Mac.</p>
            </section>
          )}
        </section>
        <footer className="empty-footer"><span>Sidecars use the video filename</span><span>recording.mp4 <b>→</b> recording.md</span></footer>
        {collaborationOpen && (
          <CollaborationDialog
            session={collaboration}
            phase={collaborationPhase}
            canHost={false}
            code={joinCode}
            displayName={displayName}
            onCode={setJoinCode}
            onDisplayName={setDisplayName}
            onHost={() => void startSharing()}
            onJoin={() => void joinSharing()}
            onCopy={() => void copySessionCode()}
            onStop={() => void stopSharing()}
            onClose={() => setCollaborationOpen(false)}
          />
        )}
        {notice && <Notice notice={notice} onClose={() => setNotice(null)} />}
      </main>
    );
  }

  return (
    <main className="app-shell">
      <header className="app-header">
        <Brand />
        <div className="file-identity">
          <strong>{document.videoName}</strong>
          <span title={document.sidecarPath}>{collaboration ? <Users size={12} /> : <Check size={12} />} {collaboration ? `${collaboration.mode === "host" ? "Hosting" : "Joined"} · ${collaboration.participantCount} live` : `${document.sidecarPath.split(/[\\/]/).at(-1)} saved`}</span>
        </div>
        <div className="header-actions">
          <button className="quiet-button" onClick={() => void openVideo()}><FolderOpen size={16} /> Open</button>
          <button className={`session-button ${collaboration ? "live" : ""} ${collaborationPhase === "reconnecting" ? "reconnecting" : ""}`} onClick={() => setCollaborationOpen(true)} aria-label={collaboration ? `Shared session ${collaboration.code}, ${collaboration.participantCount} participants` : "Share this project"}>
            {collaborationPhase === "reconnecting" ? <WifiOff size={14} /> : collaboration ? <Wifi size={14} /> : <Share2 size={14} />}
            <span>{collaboration ? `${collaboration.participantCount} live` : "Share"}</span>
          </button>
          <div className="header-export">
            <button
              className={`export-button header-export-button ${exportOpen ? "selected" : ""} ${exportPhase === "running" ? "running" : ""}`}
              aria-expanded={exportOpen}
              aria-label={`Export ${completedMarks.length} completed marks`}
              title={collaboration?.mode === "guest" ? "Export is available on the host computer" : "Export completed marks"}
              disabled={collaboration?.mode === "guest"}
              onClick={toggleExportMenu}
            >
              {exportPhase === "running" ? <RefreshCw className="spin" size={14} /> : <Scissors size={15} />}
              <span>{exportPhase === "running" ? `${exportCursor}/${exportTotal}` : "Export"}</span>
            </button>
            {exportOpen && (
              <>
                <button className="export-popover-scrim" aria-label="Close export menu" onClick={() => setExportOpen(false)} />
                <ExportMenu
                  completedMarks={completedMarks.length}
                  audioTracks={mediaRegistration?.audioTracks ?? []}
                  selectedAudioTracks={selectedAudioTracks}
                  destination={exportDestination}
                  preset={exportPreset}
                  audioMode={exportAudioMode}
                  phase={exportPhase}
                  completed={exportCursor}
                  total={exportTotal}
                  detail={exportDetail}
                  outputDirectory={exportOutputDirectory}
                  onDestination={() => void chooseExportDestination()}
                  onPreset={setExportPreset}
                  onAudioMode={setExportAudioMode}
                  onStart={() => void runExport()}
                  onCancel={() => void cancelExport()}
                  onClose={() => setExportOpen(false)}
                />
              </>
            )}
          </div>
          <button
            className={`icon-button panel-toggle ${timelineCollapsed ? "selected" : ""}`}
            aria-label={timelineCollapsed ? "Show timeline panel" : "Hide timeline panel"}
            aria-expanded={!timelineCollapsed}
            aria-controls="timeline-panel"
            title={timelineCollapsed ? "Show timeline" : panelCollapseBlocked ? "Finish the current timeline edit before hiding the panel" : "Hide timeline"}
            disabled={panelCollapseBlocked}
            onClick={() => setTimelineCollapsed((value) => !value)}
          >
            {timelineCollapsed ? <PanelRightOpen size={18} /> : <PanelRightClose size={18} />}
          </button>
          <button className={`icon-button ${settingsOpen ? "selected" : ""}`} aria-label="Analysis settings" title="Analysis settings" onClick={() => { setTimelineCollapsed(false); setAnalysisOpen(false); setExportOpen(false); setWaveformOpen(false); setWaveformSelection(null); setSettingsOpen((value) => !value); }}><Settings2 size={18} /></button>
        </div>
      </header>

      <div className={`workspace ${timelineCollapsed ? "timeline-collapsed" : ""}`}>
        <section className="player-column">
          <div className={`player-stage ${waveformOpen ? "waveform-open" : ""}`} ref={playerRef}>
            {IS_DEMO && !videoUrl ? (
              <div className="demo-frame" aria-label="Video preview placeholder">
                <div className="demo-light" />
                <div className="demo-subject" />
                <div className="demo-caption">LOCAL STORYTELLING · ROUGH CUT 03</div>
              </div>
            ) : (
              <video
                key={videoUrl}
                ref={videoRef}
                src={videoUrl ?? undefined}
                onClick={togglePlayback}
                onLoadedMetadata={(event) => handleLoadedMetadata(event.currentTarget)}
                onDurationChange={(event) => setDuration(event.currentTarget.duration)}
                onTimeUpdate={(event) => handleTimeUpdate(event.currentTarget)}
                onPlay={(event) => handleVideoPlay(event.currentTarget)}
                onPause={(event) => handleVideoPause(event.currentTarget)}
                onSeeking={() => mixAudioRef.current?.pause()}
                onSeeked={(event) => handleSeeked(event.currentTarget)}
                onEnded={handleVideoEnded}
                onError={() => showNotice("Playback could not decode this video container or codec.", "error")}
              />
            )}
            <audio
              ref={mixAudioRef}
              hidden
              onCanPlay={() => {
                setMixState("ready");
                if (videoRef.current && !videoRef.current.paused) {
                  void mixAudioRef.current?.play().catch(() => undefined);
                }
              }}
              onError={() => {
                if (mixerActive) {
                  setMixState("error");
                  showNotice("The audio mix could not start. Confirm FFmpeg is installed.", "error");
                }
              }}
            />
            <div className="stage-topline">
              <span>{collaboration ? `LIVE ${collaboration.mode === "host" ? "HOST" : "PEER"} · ${collaboration.code}` : "LOCAL PLAYBACK"}</span>
              <span>{document.videoName.split(".").at(-1)?.toUpperCase()} · {duration ? formatTime(duration, true) : "Loading…"}</span>
            </div>
            {activeSubtitles.length > 0 && (
              <div className="subtitle-overlay" role="status" aria-live="polite">
                {activeSubtitles.map((entry) => (
                  <div key={entry.id}>
                    {entry.speaker && entry.speaker.toLowerCase() !== "unknown" && <b>{entry.speaker}</b>}
                    <span>{entry.text}</span>
                  </div>
                ))}
              </div>
            )}
            {!isPlaying && !IS_DEMO && <button className="center-play" aria-label="Play" onClick={togglePlayback}><Play size={26} fill="currentColor" /></button>}
            {mixerOpen && mediaRegistration && (
              <AudioMixer
                tracks={mediaRegistration.audioTracks}
                levels={trackLevels}
                active={mixerActive}
                state={mixState}
                onToggleTrack={(streamIndex) => setTrackLevels((current) => {
                  const next = { ...current };
                  if (next[streamIndex] !== undefined) delete next[streamIndex];
                  else next[streamIndex] = 1;
                  return next;
                })}
                onLevel={(streamIndex, level) => setTrackLevels((current) => ({ ...current, [streamIndex]: level }))}
                onAll={() => setTrackLevels(
                  selectedAudioTracks.length === mediaRegistration.audioTracks.length
                    ? {}
                    : Object.fromEntries(mediaRegistration.audioTracks.map((track) => [track.streamIndex, 1])),
                )}
                onActivate={() => setMixerActive((value) => !value)}
                onClose={() => setMixerOpen(false)}
              />
            )}
            {analysisOpen && (
              <AnalysisMenu
                start={analysisStart}
                currentTime={currentTime}
                end={plannedChunks.at(-1)?.end ?? analysisStart}
                plannedCount={plannedChunks.length}
                config={config}
                setConfig={setConfig}
                phase={analysisPhase}
                completed={analysisCursor}
                total={analysisTotal || plannedChunks.length}
                progress={progress}
                detail={analysisDetail}
                onUseCurrent={useCurrentAnalysisStart}
                onStart={() => void runAnalysis()}
                onStop={() => void cancelAnalysis()}
                onSettings={() => { setAnalysisOpen(false); setSettingsOpen(true); }}
                onClose={() => setAnalysisOpen(false)}
              />
            )}
            {waveformOpen && (
              <WaveformWorkbench
                data={waveform}
                phase={waveformPhase}
                time={currentTime}
                playing={isPlaying}
                videoRef={videoRef}
                duration={duration}
                frameRate={mediaRegistration?.frameRate ?? 30}
                zoom={waveformZoom}
                selection={waveformSelection}
                subtitles={subtitleEntries}
                editingSubtitle={editingSubtitle}
                onZoom={(value) => { setWaveformSelection(null); setWaveformZoom(value); }}
                onSelection={setWaveformSelection}
                onSeek={seekTo}
                onAdjustSubtitle={(start, end) => {
                  if (editingSubtitle) setSubtitleTimingOverride({ id: editingSubtitle.id, start, end });
                }}
                onCreateSubtitle={() => void createSubtitleFromWaveform()}
                onCreateMark={() => void createMarkFromWaveform()}
                onClose={() => { setWaveformOpen(false); setWaveformSelection(null); }}
              />
            )}
            <div className="player-controls">
              <div className="seek-wrap">
                <div className="analysis-ranges">
                  {entries.filter((entry) => entry.type === "ai" && entry.end !== undefined).map((entry) => (
                    <button
                      key={entry.id}
                      type="button"
                      aria-label={`AI summary at ${formatTime(entry.start, true)}: ${entry.text}`}
                      style={{
                        left: `${duration ? (entry.start / duration) * 100 : 0}%`,
                        width: `${duration ? (((entry.end ?? entry.start) - entry.start) / duration) * 100 : 0}%`,
                      }}
                      onClick={() => seekTo(entry.start)}
                    >
                      <i />
                      <span><b>{formatTime(entry.start, true)}–{formatTime(entry.end ?? entry.start, true)}</b>{entry.text}</span>
                    </button>
                  ))}
                </div>
                <input
                  className="seek-range"
                  aria-label="Video position"
                  type="range"
                  min="0"
                  max={duration || 100}
                  step="0.001"
                  value={Math.min(currentTime, duration || 100)}
                  style={{ "--progress": `${duration ? (currentTime / duration) * 100 : 0}%` } as React.CSSProperties}
                  onChange={(event) => seekTo(Number(event.currentTarget.value))}
                  onPointerDown={beginPrecisionSeek}
                  onPointerMove={movePrecisionSeek}
                  onPointerUp={endPrecisionSeek}
                  onPointerCancel={endPrecisionSeek}
                />
                <div className="timeline-markers" aria-hidden="true">
                  {entries.filter((entry) => entry.type === "bookmark").map((entry) => (
                    <i
                      key={entry.id}
                      className={entry.source === "embedded-chapter" ? "point" : entry.end !== undefined ? "range" : "open"}
                      style={{
                        left: `${duration ? (entry.start / duration) * 100 : 0}%`,
                        width: entry.end !== undefined ? `${duration ? ((entry.end - entry.start) / duration) * 100 : 0}%` : undefined,
                      }}
                    />
                  ))}
                </div>
                {seekLens && !waveformOpen && (
                  <WaveformLens
                    data={waveform}
                    phase={waveformPhase}
                    time={seekLens.time}
                    duration={duration}
                    scale={seekLens.scale}
                    frameStep={Boolean(frameSeek && !precisionSeek)}
                    frameRate={mediaRegistration?.frameRate ?? 30}
                    style={{
                      left: `clamp(154px, ${duration ? (seekLens.time / duration) * 100 : 50}%, calc(100% - 154px))`,
                      bottom: `${20 + Math.min(34, seekLens.lift * 0.16)}px`,
                    }}
                  />
                )}
              </div>
              <div className="control-row">
                <div className="control-group">
                  <button className="control-icon" aria-label="Rewind 10 seconds" onClick={() => seekTo(currentTime - 10)}><RotateCcw size={18} /><small>10</small></button>
                  <button className="play-button" aria-label={isPlaying ? "Pause" : "Play"} onClick={togglePlayback}>{isPlaying ? <Pause size={19} fill="currentColor" /> : <Play size={19} fill="currentColor" />}</button>
                  <button className="control-icon" aria-label="Forward 10 seconds" onClick={() => seekTo(currentTime + 10)}><RotateCw size={18} /><small>10</small></button>
                  <span className="time-readout">{formatTime(currentTime, true)} <b>/</b> {formatTime(duration, true)}</span>
                </div>
                <div className="control-group right">
                  <button
                    className={`analyze-button ${analysisPhase === "running" ? "running" : analysisOpen ? "selected" : ""}`}
                    aria-expanded={analysisOpen}
                    aria-label={analysisPhase === "running" ? `Analysis ${analysisCursor} of ${analysisTotal}` : "Analyze from current frame"}
                    title={analysisPhase === "running" ? `Analyzing ${analysisCursor} of ${analysisTotal}` : "Analyze from current frame"}
                    disabled={collaboration?.mode === "guest"}
                    onClick={toggleAnalysisMenu}
                  >
                    {analysisPhase === "running" ? <RefreshCw className="spin" size={14} /> : <Sparkles size={14} />}
                    {analysisPhase === "running" && <i style={{ width: `${progress * 100}%` }} />}
                  </button>
                  <button
                    className={`mark-button contextual-mark ${openMark ? "open" : ""}`}
                    aria-label={openMark ? "End current mark" : "Start a new mark"}
                    aria-pressed={Boolean(openMark)}
                    title={openMark ? `End mark started at ${formatTime(openMark.start, true)} (M)` : "Start a new mark (N)"}
                    onClick={() => void (openMark ? endBookmark() : addBookmark())}
                  >
                    {openMark ? <Square size={10} fill="currentColor" /> : <Plus size={16} />}
                  </button>
                  <button
                    className={`control-icon waveform-trigger ${waveformOpen ? "selected" : ""}`}
                    aria-label={waveformOpen ? "Hide waveform editor" : "Show waveform editor"}
                    title={collaboration?.mode === "guest" ? "Waveform extraction runs on the host computer" : waveformOpen ? "Hide waveform editor" : "Show waveform editor"}
                    disabled={collaboration?.mode === "guest"}
                    onClick={toggleWaveform}
                  >
                    <AudioWaveform size={18} />
                  </button>
                  {subtitleEntries.length > 0 && (
                    <button
                      className={`control-icon caption-toggle ${captionsVisible ? "selected" : ""}`}
                      aria-label={captionsVisible ? "Hide subtitles" : "Show subtitles"}
                      title={`${captionsVisible ? "Hide" : "Show"} ${subtitleEntries.length} subtitle cues`}
                      onClick={() => setCaptionsVisible((value) => !value)}
                    >
                      <Captions size={18} />
                    </button>
                  )}
                  {mediaRegistration && mediaRegistration.audioTracks.length > 0 && (
                    <button
                      className={`control-icon mixer-trigger ${mixerActive ? "selected" : ""}`}
                      aria-label="Audio mixer"
                      title={`${mediaRegistration.audioTracks.length} audio ${mediaRegistration.audioTracks.length === 1 ? "track" : "tracks"}`}
                      onClick={() => { setExportOpen(false); setAnalysisOpen(false); setWaveformOpen(false); setWaveformSelection(null); setMixerOpen((value) => !value); }}
                    >
                      <AudioLines size={18} />
                      {mediaRegistration.audioTracks.length > 1 && <b>{mediaRegistration.audioTracks.length}</b>}
                    </button>
                  )}
                  <button className="control-icon" aria-label={isMuted ? "Unmute" : "Mute"} onClick={() => setIsMuted((value) => !value)}>{isMuted ? <VolumeX size={18} /> : <Volume2 size={18} />}</button>
                  <input
                    className="volume-slider"
                    aria-label="Volume"
                    type="range"
                    min="0"
                    max="1"
                    step="0.01"
                    value={volume}
                    onChange={(event) => setVolume(Number(event.currentTarget.value))}
                  />
                  <select className="speed-select" aria-label="Playback speed" value={playbackRate} onChange={(event) => setPlaybackRate(Number(event.target.value))}>
                    {[0.5, 0.75, 1, 1.25, 1.5, 2].map((rate) => <option key={rate} value={rate}>{rate}×</option>)}
                  </select>
                  <button className="control-icon" aria-label="Fullscreen" onClick={() => void toggleFullscreen()}><Maximize2 size={18} /></button>
                </div>
              </div>
            </div>
          </div>
        </section>

        <aside id="timeline-panel" className="timeline-panel" aria-hidden={timelineCollapsed}>
          {settingsOpen ? (
            <AnalysisSettings
              config={config}
              setConfig={setConfig}
              status={ollamaStatus}
              checking={checkingOllama}
              credentialPhase={credentialPhase}
              onCheck={() => void checkOllama()}
              onSaveKey={() => void persistApiKey(config.provider, config.apiKey)}
              onClose={() => setSettingsOpen(false)}
            />
          ) : (
            <>
              <div className="panel-head">
                <div><p className="eyebrow">SIDECAR TIMELINE</p><h2>Notes & context</h2></div>
                <div className="panel-tools">
                  <button className="icon-button" aria-label="Hide timeline panel" title={panelCollapseBlocked ? "Finish the current edit before hiding the panel" : "Hide timeline"} disabled={panelCollapseBlocked} onClick={() => setTimelineCollapsed(true)}><PanelRightClose size={16} /></button>
                  <button className="icon-button" aria-label="Reload Markdown" title="Reload Markdown" disabled={rawDirty} onClick={() => void reloadMarkdown()}><RefreshCw size={16} /></button>
                  <button className={`icon-button ${rawMode ? "selected" : ""}`} aria-label="Edit Markdown source" title="Edit Markdown source" onClick={() => setRawMode((value) => !value)}>{rawMode ? <FileText size={16} /> : <Code2 size={16} />}</button>
                </div>
              </div>

              {rawMode ? (
                <div className="markdown-editor">
                  <div className="markdown-file"><FileText size={15} /><span>{document.sidecarPath}</span></div>
                  <textarea
                    aria-label="Markdown source"
                    spellCheck
                    value={rawDraft}
                    onChange={(event) => { setRawDraft(event.target.value); setRawDirty(event.target.value !== document.markdown); }}
                  />
                  <div className="editor-actions">
                    <span>{rawDirty ? "Unsaved changes" : "Canonical source on disk"}</span>
                    <button className="primary compact" disabled={!rawDirty || !IS_TAURI} onClick={() => void saveRawMarkdown()}><Save size={14} /> Save Markdown</button>
                  </div>
                </div>
              ) : (
                <>
                  <div className="timeline-filters">
                    <div className="timeline-tabs" role="tablist" aria-label="Timeline filters">
                      {(["all", "bookmark", "ai", "subtitle"] as const).map((value) => (
                        <button key={value} role="tab" aria-selected={filter === value} className={filter === value ? "active" : ""} onClick={() => setFilter(value)}>
                          {value === "all" ? "All" : value === "bookmark" ? "My marks" : value === "ai" ? "AI timeline" : "Subtitles"}
                          <span>{value === "all" ? entries.length : entries.filter((entry) => entry.type === value).length}</span>
                        </button>
                      ))}
                    </div>
                    <button className="subtitle-add" aria-label="Add subtitle at playhead" title="Add subtitle at the current video time" onClick={() => void addSubtitleAtPlayhead()}>
                      <Plus size={12} /> Subtitle
                    </button>
                  </div>
                  <div className="timeline-list">
                    {filteredEntries.length ? filteredEntries.map((entry) => (
                      <TimelineRow
                        key={entry.id}
                        entry={entry}
                        active={entry.id === activeEntryId}
                        editing={entry.id === editingId}
                        onEdit={() => {
                          if (!entry.editable) return;
                          setEditingId(entry.id);
                          if (entry.type === "subtitle") {
                            setFilter("subtitle");
                            setWaveformOpen(true);
                            setWaveformSelection(null);
                            setAnalysisOpen(false);
                            setMixerOpen(false);
                            setExportOpen(false);
                            setSubtitleTimingOverride({ id: entry.id, start: entry.start, end: entry.end ?? entry.start + 3 });
                            seekTo(entry.start);
                          }
                        }}
                        onSeek={() => seekTo(entry.start)}
                        onSaveText={(text) => void updateEntry(entry, text)}
                        onSaveSubtitle={(draft) => void saveSubtitle(entry, draft)}
                        onAdvanceSubtitle={(draft) => void advanceSubtitle(entry, draft)}
                        subtitleTiming={subtitleTimingOverride?.id === entry.id ? subtitleTimingOverride : undefined}
                        onSubtitleTimingDraft={(start, end) => setSubtitleTimingOverride({ id: entry.id, start, end })}
                        onCancel={() => { setEditingId(null); setSubtitleTimingOverride(null); }}
                        onDelete={() => void deleteEntry(entry)}
                      />
                    )) : (
                      <div className="timeline-empty">
                        <Clock3 size={22} />
                        <strong>{filter === "ai" ? "No AI context yet" : filter === "subtitle" ? "No subtitles yet" : "No marks here yet"}</strong>
                        <p>{filter === "ai" ? "Analyze the video to add concise chunk summaries." : filter === "subtitle" ? "Gemini audio analysis adds verbatim timestamped cues here." : "Press N to start a mark, then M at its ending point."}</p>
                      </div>
                    )}
                  </div>
                  <div className="panel-footer"><FileText size={13} /><span title={document.sidecarPath}>{document.sidecarPath}</span></div>
                </>
              )}
            </>
          )}
        </aside>
      </div>
      {collaborationOpen && (
        <CollaborationDialog
          session={collaboration}
          phase={collaborationPhase}
          canHost={true}
          code={joinCode}
          displayName={displayName}
          onCode={setJoinCode}
          onDisplayName={setDisplayName}
          onHost={() => void startSharing()}
          onJoin={() => void joinSharing()}
          onCopy={() => void copySessionCode()}
          onStop={() => void stopSharing()}
          onClose={() => setCollaborationOpen(false)}
        />
      )}
      {notice && <Notice notice={notice} onClose={() => setNotice(null)} />}
    </main>
  );
}

function WaveformWorkbench({
  data,
  phase,
  time,
  playing,
  videoRef,
  duration,
  frameRate,
  zoom,
  selection,
  subtitles,
  editingSubtitle,
  onZoom,
  onSelection,
  onSeek,
  onAdjustSubtitle,
  onCreateSubtitle,
  onCreateMark,
  onClose,
}: {
  data: WaveformData | null;
  phase: "idle" | "loading" | "ready" | "error";
  time: number;
  playing: boolean;
  videoRef: React.RefObject<HTMLVideoElement | null>;
  duration: number;
  frameRate: number;
  zoom: number;
  selection: WaveformSelection | null;
  subtitles: TimelineEntry[];
  editingSubtitle: SubtitleTimingOverride | null;
  onZoom: (seconds: number) => void;
  onSelection: (selection: WaveformSelection | null) => void;
  onSeek: (seconds: number) => void;
  onAdjustSubtitle: (start: number, end: number) => void;
  onCreateSubtitle: () => void;
  onCreateMark: () => void;
  onClose: () => void;
}) {
  const dragRef = useRef<{
    mode: "select" | "subtitle";
    pointerId: number;
    startX: number;
    startTime: number;
    viewStart: number;
    viewEnd: number;
    edge?: "start" | "end";
    subtitleStart?: number;
    subtitleEnd?: number;
  } | null>(null);
  const [smoothTime, setSmoothTime] = useState(time);
  const [subtitleAdjustment, setSubtitleAdjustment] = useState<SubtitleTimingOverride | null>(null);
  useEffect(() => {
    if (!playing) setSmoothTime(time);
  }, [playing, time]);
  useEffect(() => {
    if (!playing) return;
    const video = videoRef.current;
    if (!video) return;
    let stopped = false;
    let videoFrame = 0;
    let animationFrame = 0;
    if (typeof video.requestVideoFrameCallback === "function") {
      const update = (_now: number, metadata: VideoFrameCallbackMetadata) => {
        if (stopped) return;
        setSmoothTime(metadata.mediaTime);
        videoFrame = video.requestVideoFrameCallback(update);
      };
      videoFrame = video.requestVideoFrameCallback(update);
    } else {
      const update = () => {
        if (stopped) return;
        setSmoothTime(video.currentTime);
        animationFrame = window.requestAnimationFrame(update);
      };
      animationFrame = window.requestAnimationFrame(update);
    }
    return () => {
      stopped = true;
      if (videoFrame && typeof video.cancelVideoFrameCallback === "function") video.cancelVideoFrameCallback(videoFrame);
      if (animationFrame) window.cancelAnimationFrame(animationFrame);
    };
  }, [playing, videoRef]);
  useEffect(() => {
    setSubtitleAdjustment(null);
  }, [editingSubtitle?.id]);
  const activeEditingRange = subtitleAdjustment ?? editingSubtitle;
  const minZoom = Math.max(0.25, 2 / Math.max(1, frameRate));
  const maxZoom = Math.max(minZoom, Math.min(180, duration || 180));
  const safeZoom = Math.max(minZoom, Math.min(maxZoom, Number.isFinite(zoom) ? zoom : 8));
  const view = useMemo(() => {
    if (selection) return { start: selection.viewStart, end: selection.viewEnd };
    const editingDuration = activeEditingRange ? activeEditingRange.end - activeEditingRange.start : 0;
    const windowSeconds = Math.max(safeZoom, editingDuration * 1.2);
    const center = activeEditingRange
      ? (activeEditingRange.start + activeEditingRange.end) / 2
      : smoothTime;
    let start = center - windowSeconds / 2;
    let end = center + windowSeconds / 2;
    if (start < 0) {
      end -= start;
      start = 0;
    }
    if (end > duration) {
      start = Math.max(0, start - (end - duration));
      end = duration;
    }
    return { start, end: Math.max(start + minZoom, end) };
  }, [activeEditingRange, duration, minZoom, safeZoom, selection, smoothTime]);
  const path = useMemo(() => {
    if (!data?.peaks.length || view.end <= view.start) return "";
    const from = Math.max(0, Math.floor(view.start * data.samplesPerSecond));
    const to = Math.min(data.peaks.length, Math.ceil(view.end * data.samplesPerSecond));
    const bucketCount = 360;
    const points: string[] = [];
    for (let bucket = 0; bucket < bucketCount; bucket += 1) {
      const sampleStart = from + Math.floor(((to - from) * bucket) / bucketCount);
      const sampleEnd = Math.max(sampleStart + 1, from + Math.ceil(((to - from) * (bucket + 1)) / bucketCount));
      let peak = 0;
      for (let sample = sampleStart; sample < Math.min(to, sampleEnd); sample += 1) {
        peak = Math.max(peak, data.peaks[sample] ?? 0);
      }
      const x = 2 + (bucket / (bucketCount - 1)) * 996;
      const height = Math.max(1, peak * 32);
      points.push(`M${x.toFixed(2)} ${(35 - height).toFixed(2)}V${(35 + height).toFixed(2)}`);
    }
    return points.join("");
  }, [data, view.end, view.start]);
  const windowDuration = Math.max(0.001, view.end - view.start);
  const selectedDuration = selection ? selection.end - selection.start : 0;
  const selectionLeft = selection ? ((selection.start - view.start) / windowDuration) * 100 : 0;
  const selectionWidth = selection ? (selectedDuration / windowDuration) * 100 : 0;
  const playheadLeft = ((smoothTime - view.start) / windowDuration) * 100;
  const sliderValue = maxZoom === minZoom
    ? 0
    : (Math.log(safeZoom / minZoom) / Math.log(maxZoom / minZoom)) * 100;
  const timeAtPointer = (clientX: number, bounds: DOMRect, rangeStart: number, rangeEnd: number) => (
    rangeStart + Math.max(0, Math.min(1, (clientX - bounds.left) / bounds.width)) * (rangeEnd - rangeStart)
  );

  return (
    <section className="waveform-workbench" aria-label="Waveform range editor">
      <div className="waveform-workbench-head">
        <div className="waveform-workbench-title">
          <AudioWaveform size={15} />
          <div><strong>Audio waveform</strong><span>{formatTimecode(smoothTime)}</span></div>
        </div>
        <div className="waveform-zoom">
          <button aria-label="Zoom into waveform" title="Zoom in" onClick={() => onZoom(Math.max(minZoom, safeZoom / 1.6))}><ZoomIn size={13} /></button>
          <input
            type="range"
            aria-label="Waveform zoom"
            min="0"
            max="100"
            step="1"
            value={sliderValue}
            onChange={(event) => {
              const ratio = Number(event.currentTarget.value) / 100;
              onZoom(minZoom * (maxZoom / minZoom) ** ratio);
            }}
          />
          <button aria-label="Zoom out of waveform" title="Zoom out" onClick={() => onZoom(Math.min(maxZoom, safeZoom * 1.6))}><ZoomOut size={13} /></button>
          <span>{safeZoom < 10 ? safeZoom.toFixed(1) : safeZoom.toFixed(0)}s</span>
        </div>
        <button className="icon-button waveform-close" aria-label="Close waveform editor" onClick={onClose}><X size={15} /></button>
      </div>
      <div
        className={`waveform-workbench-plot ${phase}`}
        role="slider"
        aria-label="Click to seek or drag to select a time range"
        aria-valuemin={view.start}
        aria-valuemax={view.end}
        aria-valuenow={smoothTime}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          event.preventDefault();
          const bounds = event.currentTarget.getBoundingClientRect();
          const startTime = timeAtPointer(event.clientX, bounds, view.start, view.end);
          event.currentTarget.setPointerCapture(event.pointerId);
          const edgeElement = (event.target as Element).closest<HTMLElement>("[data-subtitle-edge]");
          const edge = edgeElement?.dataset.subtitleEdge as "start" | "end" | undefined;
          if (edge && activeEditingRange) {
            dragRef.current = {
              mode: "subtitle",
              pointerId: event.pointerId,
              startX: event.clientX,
              startTime,
              viewStart: view.start,
              viewEnd: view.end,
              edge,
              subtitleStart: activeEditingRange.start,
              subtitleEnd: activeEditingRange.end,
            };
            setSubtitleAdjustment(activeEditingRange);
            return;
          }
          dragRef.current = {
            mode: "select",
            pointerId: event.pointerId,
            startX: event.clientX,
            startTime,
            viewStart: view.start,
            viewEnd: view.end,
          };
          onSelection({ start: startTime, end: startTime, viewStart: view.start, viewEnd: view.end });
        }}
        onPointerMove={(event) => {
          const drag = dragRef.current;
          if (!drag || drag.pointerId !== event.pointerId) return;
          event.preventDefault();
          const bounds = event.currentTarget.getBoundingClientRect();
          const pointerTime = timeAtPointer(event.clientX, bounds, drag.viewStart, drag.viewEnd);
          if (drag.mode === "subtitle") {
            const minimumDuration = 1 / Math.max(1, frameRate);
            const start = drag.edge === "start"
              ? Math.min(pointerTime, (drag.subtitleEnd ?? pointerTime) - minimumDuration)
              : (drag.subtitleStart ?? pointerTime);
            const end = drag.edge === "end"
              ? Math.max(pointerTime, (drag.subtitleStart ?? pointerTime) + minimumDuration)
              : (drag.subtitleEnd ?? pointerTime);
            setSubtitleAdjustment({
              id: activeEditingRange?.id ?? "",
              start: Math.max(0, start),
              end: Math.min(duration, end),
            });
            return;
          }
          onSelection({
            start: Math.min(drag.startTime, pointerTime),
            end: Math.max(drag.startTime, pointerTime),
            viewStart: drag.viewStart,
            viewEnd: drag.viewEnd,
          });
        }}
        onPointerUp={(event) => {
          const drag = dragRef.current;
          if (!drag || drag.pointerId !== event.pointerId) return;
          const bounds = event.currentTarget.getBoundingClientRect();
          const pointerTime = timeAtPointer(event.clientX, bounds, drag.viewStart, drag.viewEnd);
          if (drag.mode === "subtitle") {
            const minimumDuration = 1 / Math.max(1, frameRate);
            const start = drag.edge === "start"
              ? Math.max(0, Math.min(pointerTime, (drag.subtitleEnd ?? pointerTime) - minimumDuration))
              : (drag.subtitleStart ?? pointerTime);
            const end = drag.edge === "end"
              ? Math.min(duration, Math.max(pointerTime, (drag.subtitleStart ?? pointerTime) + minimumDuration))
              : (drag.subtitleEnd ?? pointerTime);
            dragRef.current = null;
            if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
            setSubtitleAdjustment(null);
            onAdjustSubtitle(start, end);
            return;
          }
          const wasClick = Math.abs(event.clientX - drag.startX) < 3;
          dragRef.current = null;
          if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
          if (wasClick) {
            onSelection(null);
            onSeek(pointerTime);
          } else {
            onSelection({
              start: Math.min(drag.startTime, pointerTime),
              end: Math.max(drag.startTime, pointerTime),
              viewStart: drag.viewStart,
              viewEnd: drag.viewEnd,
            });
          }
        }}
        onPointerCancel={(event) => {
          if (dragRef.current?.pointerId !== event.pointerId) return;
          dragRef.current = null;
          setSubtitleAdjustment(null);
          onSelection(null);
        }}
      >
        {path ? (
          <svg viewBox="0 0 1000 70" preserveAspectRatio="none" aria-hidden="true">
            <path className="waveform-baseline" d="M0 35H1000" />
            <path className="waveform-peaks" d={path} />
          </svg>
        ) : (
          <div className="waveform-pending">
            <AudioWaveform size={16} />
            <span>{phase === "error" ? "Waveform unavailable" : "Preparing audio waveform…"}</span>
          </div>
        )}
        {subtitles.map((subtitle) => {
          const range = activeEditingRange?.id === subtitle.id
            ? activeEditingRange
            : { id: subtitle.id, start: subtitle.start, end: subtitle.end ?? subtitle.start };
          const clippedStart = Math.max(view.start, range.start);
          const clippedEnd = Math.min(view.end, range.end);
          if (clippedEnd <= clippedStart) return null;
          const isEditing = activeEditingRange?.id === subtitle.id;
          return (
            <div
              key={subtitle.id}
              className={`waveform-subtitle-range ${isEditing ? "editing" : ""}`}
              title={`${formatTimecode(range.start)}–${formatTimecode(range.end)} · ${subtitle.text}`}
              style={{
                left: `${((clippedStart - view.start) / windowDuration) * 100}%`,
                width: `${((clippedEnd - clippedStart) / windowDuration) * 100}%`,
              }}
            >
              {isEditing && <><i data-subtitle-edge="start" /><i data-subtitle-edge="end" /></>}
            </div>
          );
        })}
        {playheadLeft >= 0 && playheadLeft <= 100 && <i className="waveform-current" style={{ left: `${playheadLeft}%` }} />}
        {selection && selectedDuration > 0 && (
          <div className="waveform-selection" style={{ left: `${selectionLeft}%`, width: `${selectionWidth}%` }}>
            <i /><i />
          </div>
        )}
      </div>
      <div className="waveform-workbench-foot">
        <time>{formatTimecode(view.start)}</time>
        {selection && selectedDuration >= 0.01 ? (
          <div className="waveform-selection-actions">
            <span>{formatTimecode(selection.start)} <b>→</b> {formatTimecode(selection.end)} <em>{selectedDuration.toFixed(3)}s</em></span>
            <button onClick={onCreateSubtitle}><Captions size={12} /> Subtitle</button>
            <button onClick={onCreateMark}><Plus size={12} /> Mark</button>
          </div>
        ) : (
          <span className="waveform-hint">Click to seek · drag to select a range</span>
        )}
        <time>{formatTimecode(view.end)}</time>
      </div>
    </section>
  );
}

function WaveformLens({
  data,
  phase,
  time,
  duration,
  scale,
  frameStep,
  frameRate,
  style,
}: {
  data: WaveformData | null;
  phase: "idle" | "loading" | "ready" | "error";
  time: number;
  duration: number;
  scale: number;
  frameStep: boolean;
  frameRate: number;
  style: React.CSSProperties;
}) {
  const detail = useMemo(() => {
    const windowSeconds = frameStep ? 1.2 : 1.2 + 6.8 * Math.max(0, Math.min(1, scale)) ** 0.7;
    let start = time - windowSeconds / 2;
    let end = time + windowSeconds / 2;
    if (start < 0) {
      end -= start;
      start = 0;
    }
    if (end > duration) {
      start = Math.max(0, start - (end - duration));
      end = duration;
    }
    if (!data?.peaks.length) return { path: "", start, end, windowSeconds };

    const from = Math.max(0, Math.floor(start * data.samplesPerSecond));
    const to = Math.min(data.peaks.length, Math.ceil(end * data.samplesPerSecond));
    const bucketCount = 140;
    const points: string[] = [];
    for (let bucket = 0; bucket < bucketCount; bucket += 1) {
      const sampleStart = from + Math.floor(((to - from) * bucket) / bucketCount);
      const sampleEnd = Math.max(sampleStart + 1, from + Math.ceil(((to - from) * (bucket + 1)) / bucketCount));
      let peak = 0;
      for (let sample = sampleStart; sample < Math.min(to, sampleEnd); sample += 1) {
        peak = Math.max(peak, data.peaks[sample] ?? 0);
      }
      const x = 2 + (bucket / (bucketCount - 1)) * 276;
      const height = Math.max(1, peak * 23);
      points.push(`M${x.toFixed(2)} ${(29 - height).toFixed(2)}V${(29 + height).toFixed(2)}`);
    }
    return { path: points.join(""), start, end, windowSeconds };
  }, [data, duration, frameStep, scale, time]);

  const sensitivity = scale >= 0.995 ? "Normal scrub" : `${scale.toFixed(scale < 0.1 ? 3 : 2)}× fine`;
  return (
    <div className="waveform-lens" style={style} role="status" aria-label={`Seeking at ${formatTimecode(time)}`}>
      <div className="waveform-lens-head">
        <span><AudioLines size={11} /> Audio detail</span>
        <strong>{formatTimecode(time)}</strong>
        <em>{frameStep ? `Frame · ${frameRate.toFixed(2)} fps` : sensitivity}</em>
      </div>
      <div className={`waveform-plot ${phase}`}>
        {detail.path ? (
          <svg viewBox="0 0 280 58" preserveAspectRatio="none" aria-hidden="true">
            <path className="waveform-baseline" d="M0 29H280" />
            <path className="waveform-peaks" d={detail.path} />
            <path className="waveform-playhead" d="M140 0V58" />
          </svg>
        ) : (
          <div className="waveform-pending">
            <AudioLines size={15} />
            <span>{phase === "error" ? "Waveform unavailable" : "Preparing audio waveform…"}</span>
          </div>
        )}
      </div>
      <div className="waveform-lens-foot">
        <span>{formatTimecode(detail.start)}</span>
        <b>{detail.windowSeconds.toFixed(2)}s window</b>
        <span>{formatTimecode(detail.end)}</span>
      </div>
    </div>
  );
}

function Brand() {
  return <div className="brand"><span className="brand-mark"><Play size={12} fill="currentColor" /></span><strong>FrameNote</strong></div>;
}

function Notice({ notice, onClose }: { notice: { tone: "info" | "error"; text: string }; onClose: () => void }) {
  return (
    <div className={`notice ${notice.tone}`} role={notice.tone === "error" ? "alert" : "status"}>
      {notice.tone === "error" ? <CircleAlert size={17} /> : <Check size={17} />}
      <span>{notice.text}</span>
      <button aria-label="Dismiss" onClick={onClose}><X size={15} /></button>
    </div>
  );
}

function CollaborationDialog({
  session,
  phase,
  canHost,
  code,
  displayName,
  onCode,
  onDisplayName,
  onHost,
  onJoin,
  onCopy,
  onStop,
  onClose,
}: {
  session: CollaborationSession | null;
  phase: "idle" | "hosting" | "joining" | "connected" | "reconnecting";
  canHost: boolean;
  code: string;
  displayName: string;
  onCode: (code: string) => void;
  onDisplayName: (name: string) => void;
  onHost: () => void;
  onJoin: () => void;
  onCopy: () => void;
  onStop: () => void;
  onClose: () => void;
}) {
  const busy = phase === "hosting" || phase === "joining";
  return (
    <div className="session-layer" role="presentation">
      <button className="session-scrim" aria-label="Close sharing dialog" onClick={onClose} />
      <section className="session-dialog" role="dialog" aria-modal="true" aria-labelledby="session-title">
        <header>
          <div>
            <p className="eyebrow">PEER SESSION</p>
            <h2 id="session-title">{session ? "Watching together" : "Share without uploading"}</h2>
          </div>
          <button className="icon-button" aria-label="Close sharing dialog" onClick={onClose}><X size={16} /></button>
        </header>
        {session ? (
          <div className="session-connected">
            <div className={`session-network-state ${phase}`}>
              {phase === "reconnecting" ? <WifiOff size={17} /> : <Wifi size={17} />}
              <div><strong>{phase === "reconnecting" ? "Reconnecting on local network" : session.mode === "host" ? "Session is discoverable" : "Connected directly to host"}</strong><span>{session.videoName}</span></div>
            </div>
            <button className="session-code" onClick={onCopy} aria-label={`Copy session code ${session.code}`}>
              <span>Six-digit code</span>
              <strong>{session.code.slice(0, 3)}&nbsp;{session.code.slice(3)}</strong>
              <Copy size={15} />
            </button>
            <div className="session-presence">
              <div><Users size={15} /><strong>{session.participantCount} watching</strong></div>
              <ul>{session.participants.map((name, index) => <li key={`${name}-${index}`}><i />{name}</li>)}</ul>
            </div>
            <p className="session-footnote">Playback, seeking, marks, subtitles, and Markdown changes synchronize directly between FrameNote peers. The host keeps the canonical sidecar and serves the original video read-only.</p>
            <button className="secondary full session-stop" onClick={onStop}>{session.mode === "host" ? "End session" : "Leave session"}</button>
          </div>
        ) : (
          <div className="session-setup">
            <label className="session-name">
              <span>Your name</span>
              <input value={displayName} maxLength={48} autoComplete="name" onChange={(event) => onDisplayName(event.target.value)} placeholder="Editor" />
            </label>
            {canHost && (
              <div className="session-path host-path">
                <div><Share2 size={17} /><span><strong>Create session</strong><small>Share this video and its timeline from this computer.</small></span></div>
                <button className="primary compact" disabled={busy} onClick={onHost}>{phase === "hosting" ? <RefreshCw className="spin" size={14} /> : <Wifi size={14} />}{phase === "hosting" ? "Starting…" : "Create"}</button>
              </div>
            )}
            <div className="session-divider"><span>JOIN WITH CODE</span></div>
            <div className="session-path join-path">
              <label>
                <span>Session code</span>
                <input
                  className="session-code-input"
                  inputMode="numeric"
                  pattern="[0-9]*"
                  maxLength={6}
                  autoFocus={!canHost}
                  value={code}
                  onChange={(event) => onCode(event.target.value.replace(/\D/g, "").slice(0, 6))}
                  onKeyDown={(event) => { if (event.key === "Enter" && code.length === 6) onJoin(); }}
                  placeholder="000000"
                  aria-label="Six-digit session code"
                />
              </label>
              <button className="secondary" disabled={busy || code.length !== 6} onClick={onJoin}>{phase === "joining" ? <RefreshCw className="spin" size={14} /> : <Users size={14} />}{phase === "joining" ? "Finding…" : "Join"}</button>
            </div>
            <p className="session-footnote">Both computers must be on the same local network. The code finds the host through peer discovery; no account, cloud upload, or central FrameNote server is used.</p>
          </div>
        )}
      </section>
    </div>
  );
}

function ExportMenu({
  completedMarks,
  audioTracks,
  selectedAudioTracks,
  destination,
  preset,
  audioMode,
  phase,
  completed,
  total,
  detail,
  outputDirectory,
  onDestination,
  onPreset,
  onAudioMode,
  onStart,
  onCancel,
  onClose,
}: {
  completedMarks: number;
  audioTracks: AudioTrackInfo[];
  selectedAudioTracks: AudioTrackInfo[];
  destination: string;
  preset: ExportPreset;
  audioMode: "all" | "selected";
  phase: ExportPhase;
  completed: number;
  total: number;
  detail: string;
  outputDirectory: string;
  onDestination: () => void;
  onPreset: (preset: ExportPreset) => void;
  onAudioMode: (mode: "all" | "selected") => void;
  onStart: () => void;
  onCancel: () => void;
  onClose: () => void;
}) {
  const running = phase === "running";
  const presetDescription = preset === "resolve"
    ? "ProRes 422 + 24-bit PCM · edit-friendly, larger files"
    : preset === "mp4"
      ? "H.264 + AAC · compact and frame-accurate"
      : "Original codecs · fastest, cut accuracy follows source keyframes";
  const destinationName = destination.split(/[\\/]/).filter(Boolean).at(-1);
  const audioCount = audioMode === "all" ? audioTracks.length : selectedAudioTracks.length;
  const disabled = !completedMarks || !destination || (audioMode === "selected" && !selectedAudioTracks.length);

  return (
    <div className="export-menu" role="dialog" aria-label="Export marked clips" onClick={(event) => event.stopPropagation()}>
      <div className="export-menu-head">
        <div><p className="eyebrow">ROUGH CUT</p><h3>Export marked clips</h3></div>
        <button className="icon-button" aria-label="Close export menu" onClick={onClose}><X size={16} /></button>
      </div>
      <div className="export-summary">
        <Scissors size={16} />
        <div><span>Completed marks</span><strong>{completedMarks} {completedMarks === 1 ? "clip" : "clips"}</strong></div>
        <div><span>Subtitles</span><strong>SRT + CSV</strong></div>
      </div>
      <div className="export-fields">
        <label className="export-destination">
          <span>Save inside</span>
          <button type="button" onClick={onDestination} disabled={running}>
            <FolderOpen size={14} />
            <b title={destination}>{destinationName ?? "Choose folder…"}</b>
          </button>
          <small>A new, non-overwriting FrameNote folder is created here.</small>
        </label>
        <label>
          <span>Media format</span>
          <select value={preset} disabled={running} onChange={(event) => onPreset(event.target.value as ExportPreset)}>
            <option value="resolve">MOV · ProRes 422 (Resolve)</option>
            <option value="mp4">MP4 · H.264</option>
            <option value="source">MKV · Source codecs</option>
          </select>
          <small>{presetDescription}</small>
        </label>
        <label>
          <span>Audio streams</span>
          <select value={audioMode} disabled={running} onChange={(event) => onAudioMode(event.target.value as "all" | "selected")}>
            <option value="all">All embedded tracks · separate</option>
            <option value="selected">Mixer-selected tracks · separate</option>
          </select>
          <small>{audioCount ? `${audioCount} ${audioCount === 1 ? "track" : "tracks"} exported without mixing.` : "No audio tracks are available for this choice."}</small>
        </label>
      </div>
      <div className={`export-status ${phase}`}>
        <div><span>{running ? `Exporting ${Math.min(total, completed + 1)} of ${total}` : phase === "complete" ? "Export complete" : phase === "cancelled" ? "Export stopped" : "Ready"}</span><em>{completedMarks ? `${completedMarks} marked ranges` : "Close a mark with M first"}</em></div>
        <p title={outputDirectory || detail}>{phase === "complete" && outputDirectory ? outputDirectory : detail}</p>
        {(running || completed > 0) && <div className="export-progress"><i style={{ width: `${total ? (completed / total) * 100 : 0}%` }} /></div>}
      </div>
      <div className="export-menu-actions">
        <span>Speaker-labelled, clip-relative subtitles</span>
        {running ? (
          <button className="secondary danger" onClick={onCancel}><Square size={10} fill="currentColor" /> Stop</button>
        ) : (
          <button className="primary compact" disabled={disabled} onClick={onStart}><Download size={14} /> {phase === "complete" ? "Export again" : "Export clips"}</button>
        )}
      </div>
    </div>
  );
}

function AnalysisMenu({
  start,
  currentTime,
  end,
  plannedCount,
  config,
  setConfig,
  phase,
  completed,
  total,
  progress,
  detail,
  onUseCurrent,
  onStart,
  onStop,
  onSettings,
  onClose,
}: {
  start: number;
  currentTime: number;
  end: number;
  plannedCount: number;
  config: AnalysisConfig;
  setConfig: React.Dispatch<React.SetStateAction<AnalysisConfig>>;
  phase: AnalysisPhase;
  completed: number;
  total: number;
  progress: number;
  detail: string;
  onUseCurrent: () => void;
  onStart: () => void;
  onStop: () => void;
  onSettings: () => void;
  onClose: () => void;
}) {
  const running = phase === "running";
  const provider = config.provider === "gemini" ? "Gemini audio + vision + transcript" : config.provider === "cloud" ? "Ollama Cloud · summary only" : "Local Ollama · summary only";
  const actionLabel = phase === "paused" ? "Resume range" : phase === "error" ? "Retry range" : "Analyze range";

  return (
    <div className="analysis-menu" role="dialog" aria-label="Analyze video range" onClick={(event) => event.stopPropagation()}>
      <div className="analysis-menu-head">
        <div><p className="eyebrow">AI TIMELINE</p><h3>Analyze from this frame</h3></div>
        <button className="icon-button" aria-label="Close analysis menu" onClick={onClose}><X size={16} /></button>
      </div>

      <div className="analysis-range-summary">
        <Clock3 size={15} />
        <div><span>Selected range</span><strong>{formatTime(start, true)} <i>→</i> {formatTime(end, true)}</strong></div>
        <button onClick={onUseCurrent} disabled={running}>Use {formatTime(currentTime, true)}</button>
      </div>

      <div className="analysis-tuning">
        <label>
          <span>Seconds per chunk</span>
          <select
            value={config.chunkSeconds}
            disabled={running}
            onChange={(event) => setConfig((value) => ({ ...value, chunkSeconds: Number(event.target.value) }))}
          >
            {[15, 30, 45, 60, 90, 120, 180, 300].map((seconds) => <option key={seconds} value={seconds}>{seconds}s</option>)}
          </select>
        </label>
        <label>
          <span>Number of chunks</span>
          <input
            type="number"
            min="1"
            max="50"
            step="1"
            value={config.chunkCount}
            disabled={running}
            onChange={(event) => setConfig((value) => ({ ...value, chunkCount: Math.max(1, Math.min(50, Number(event.target.value) || 1)) }))}
          />
        </label>
        <label>
          <span>Frames per chunk</span>
          <input
            type="number"
            min="2"
            max="8"
            step="1"
            value={config.frameCount}
            disabled={running}
            onChange={(event) => setConfig((value) => ({ ...value, frameCount: Math.max(2, Math.min(8, Number(event.target.value) || 2)) }))}
          />
        </label>
      </div>

      <div className={`analysis-menu-status ${phase}`}>
        <div>
          <span>{running ? `Analyzing ${Math.min(total, completed + 1)} of ${total}` : phase === "complete" ? "Range complete" : phase === "paused" ? `${completed} of ${total} saved` : `${plannedCount} ${plannedCount === 1 ? "chunk" : "chunks"} selected`}</span>
          <em>{provider} · current frame + {Math.max(1, config.frameCount - 1)} samples</em>
        </div>
        <p>{detail}</p>
        {(running || completed > 0) && <div className="analysis-menu-progress"><i style={{ width: `${Math.min(100, progress * 100)}%` }} /></div>}
      </div>

      <div className="analysis-menu-actions">
        <button className="quiet-button" onClick={onSettings}><Settings2 size={14} /> Provider</button>
        {running ? (
          <button className="secondary danger" onClick={onStop}><Square size={11} fill="currentColor" /> Stop</button>
        ) : (
          <button className="primary compact" disabled={!plannedCount || !IS_TAURI} onClick={onStart}><Sparkles size={14} /> {actionLabel}</button>
        )}
      </div>
    </div>
  );
}

function AudioMixer({
  tracks,
  levels,
  active,
  state,
  onToggleTrack,
  onLevel,
  onAll,
  onActivate,
  onClose,
}: {
  tracks: AudioTrackInfo[];
  levels: Record<number, number>;
  active: boolean;
  state: "off" | "loading" | "ready" | "error";
  onToggleTrack: (streamIndex: number) => void;
  onLevel: (streamIndex: number, level: number) => void;
  onAll: () => void;
  onActivate: () => void;
  onClose: () => void;
}) {
  const selectedCount = tracks.filter((track) => levels[track.streamIndex] !== undefined).length;
  const stateLabel = active
    ? state === "loading"
      ? "Building mix…"
      : state === "ready"
        ? "Live mix"
        : state === "error"
          ? "Mix unavailable"
          : "Starting…"
    : "Video audio";

  return (
    <div className="audio-mixer" role="dialog" aria-label="Audio mixer" onClick={(event) => event.stopPropagation()}>
      <div className="audio-mixer-head">
        <div>
          <p className="eyebrow">EMBEDDED AUDIO</p>
          <h3>Audio mixer</h3>
        </div>
        <div className={`mix-state ${active ? state : "off"}`}><i />{stateLabel}</div>
        <button className="icon-button" aria-label="Close audio mixer" onClick={onClose}><X size={16} /></button>
      </div>
      <div className="audio-track-list">
        {tracks.map((track, index) => {
          const level = levels[track.streamIndex];
          const selected = level !== undefined;
          const meta = [track.language?.toUpperCase(), track.codec.toUpperCase(), track.channels ? `${track.channels}ch` : null]
            .filter(Boolean)
            .join(" · ");
          return (
            <div className={`audio-track ${selected ? "selected" : ""}`} key={track.streamIndex}>
              <button className="track-toggle" aria-pressed={selected} onClick={() => onToggleTrack(track.streamIndex)}>
                <span>{selected && <Check size={11} />}</span>
                <b>{index + 1}</b>
                <div><strong>{track.label}</strong><small>{meta || "Audio track"}</small></div>
              </button>
              <label title={`${Math.round((level ?? 1) * 100)}%`}>
                <Volume2 size={13} />
                <input
                  type="range"
                  min="0"
                  max="2"
                  step="0.05"
                  disabled={!selected}
                  value={level ?? 1}
                  aria-label={`${track.label} level`}
                  onChange={(event) => onLevel(track.streamIndex, Number(event.currentTarget.value))}
                />
                <em>{Math.round((level ?? 1) * 100)}</em>
              </label>
            </div>
          );
        })}
      </div>
      <div className="audio-mixer-actions">
        <button className="secondary" onClick={onAll}>{selectedCount === tracks.length ? "Clear" : "Select all"}</button>
        <span>{selectedCount} of {tracks.length} selected</span>
        <button className={active ? "secondary" : "primary compact"} disabled={!selectedCount} onClick={onActivate}>
          {active ? <><Volume2 size={14} /> Use video audio</> : <><AudioLines size={14} /> Play selected mix</>}
        </button>
      </div>
    </div>
  );
}

function TimelineRow({
  entry,
  active,
  editing,
  onEdit,
  onSeek,
  onSaveText,
  onSaveSubtitle,
  onAdvanceSubtitle,
  subtitleTiming,
  onSubtitleTimingDraft,
  onCancel,
  onDelete,
}: {
  entry: TimelineEntry;
  active: boolean;
  editing: boolean;
  onEdit: () => void;
  onSeek: () => void;
  onSaveText: (text: string) => void;
  onSaveSubtitle: (draft: SubtitleDraft) => void;
  onAdvanceSubtitle: (draft: SubtitleDraft) => void;
  subtitleTiming?: SubtitleTimingOverride;
  onSubtitleTimingDraft: (start: number, end: number) => void;
  onCancel: () => void;
  onDelete: () => void;
}) {
  const rowRef = useRef<HTMLElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const languageRef = useRef<HTMLInputElement>(null);
  const [draft, setDraft] = useState(entry.text);
  const [startDraft, setStartDraft] = useState(formatTimecode(entry.start));
  const [endDraft, setEndDraft] = useState(formatTimecode(entry.end ?? entry.start + 3));
  const [speakerDraft, setSpeakerDraft] = useState(entry.speaker ?? "Unknown");
  const [languageDraft, setLanguageDraft] = useState(entry.language ?? "unknown");

  useEffect(() => {
    setDraft(entry.text);
    setStartDraft(formatTimecode(entry.start));
    setEndDraft(formatTimecode(entry.end ?? entry.start + 3));
    setSpeakerDraft(entry.speaker ?? "Unknown");
    setLanguageDraft(entry.language ?? "unknown");
  }, [entry.end, entry.language, entry.speaker, entry.start, entry.text]);
  useEffect(() => {
    if (!editing || entry.type !== "subtitle" || subtitleTiming?.id !== entry.id) return;
    setStartDraft(formatTimecode(subtitleTiming.start));
    setEndDraft(formatTimecode(subtitleTiming.end));
  }, [editing, entry.id, entry.type, subtitleTiming]);
  useEffect(() => {
    if (editing) {
      rowRef.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const subtitleDraft = (): SubtitleDraft => ({
    start: timestampToSeconds(startDraft) ?? Number.NaN,
    end: timestampToSeconds(endDraft) ?? Number.NaN,
    text: draft,
    speaker: speakerDraft,
    language: languageDraft,
  });
  const cancelEditing = () => {
    setDraft(entry.text);
    setStartDraft(formatTimecode(entry.start));
    setEndDraft(formatTimecode(entry.end ?? entry.start + 3));
    setSpeakerDraft(entry.speaker ?? "Unknown");
    setLanguageDraft(entry.language ?? "unknown");
    onCancel();
  };

  const entryLabel = entry.type === "ai"
    ? "AI context"
    : entry.type === "subtitle"
      ? ["Subtitle", entry.speaker, entry.language?.toUpperCase()].filter(Boolean).join(" · ")
      : entry.source === "embedded-chapter" ? "Embedded marker" : "My mark";

  return (
    <article
      ref={rowRef}
      className={`timeline-row ${entry.type} ${active ? "active" : ""} ${editing ? "editing" : ""}`}
      onClick={onSeek}
      onDoubleClick={(event) => {
        if (entry.type !== "subtitle" || !entry.editable || editing) return;
        event.stopPropagation();
        onEdit();
      }}
    >
      <button className="timestamp" onClick={(event) => { event.stopPropagation(); onSeek(); }}>
        {formatTime(entry.start, true)}{entry.end !== undefined && <><i>–</i>{formatTime(entry.end, true)}</>}
      </button>
      <div className="entry-rail"><i>{entry.type === "ai" ? <Sparkles size={11} /> : entry.type === "subtitle" ? <Captions size={11} /> : null}</i><span /></div>
      <div className="entry-content">
        <div className="entry-label">{entryLabel}</div>
        {editing && entry.type === "subtitle" ? (
          <div
            className="subtitle-editor"
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => {
              if (event.key === "Tab" && !event.shiftKey && event.target === languageRef.current) {
                event.preventDefault();
                onAdvanceSubtitle(subtitleDraft());
              } else if (event.key === "Escape") {
                event.preventDefault();
                cancelEditing();
              } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
                event.preventDefault();
                onSaveSubtitle(subtitleDraft());
              }
            }}
          >
            <div className="subtitle-time-fields">
              <label>
                <span>Start</span>
                <input
                  ref={inputRef}
                  aria-label="Subtitle start time"
                  value={startDraft}
                  onChange={(event) => {
                    const value = event.target.value;
                    setStartDraft(value);
                    const start = timestampToSeconds(value);
                    const end = timestampToSeconds(endDraft);
                    if (start !== null && end !== null && end > start) onSubtitleTimingDraft(start, end);
                  }}
                />
              </label>
              <i>→</i>
              <label>
                <span>End</span>
                <input
                  aria-label="Subtitle end time"
                  value={endDraft}
                  onChange={(event) => {
                    const value = event.target.value;
                    setEndDraft(value);
                    const start = timestampToSeconds(startDraft);
                    const end = timestampToSeconds(value);
                    if (start !== null && end !== null && end > start) onSubtitleTimingDraft(start, end);
                  }}
                />
              </label>
            </div>
            <label className="subtitle-text-field">
              <span>Text</span>
              <input
                aria-label="Subtitle text"
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
              />
            </label>
            <div className="subtitle-meta-fields">
              <label>
                <span>Speaker</span>
                <input aria-label="Subtitle speaker" value={speakerDraft} onChange={(event) => setSpeakerDraft(event.target.value)} />
              </label>
              <label>
                <span>Language</span>
                <input ref={languageRef} aria-label="Subtitle language" value={languageDraft} onChange={(event) => setLanguageDraft(event.target.value)} />
              </label>
            </div>
            <div className="subtitle-editor-actions">
              <span><kbd>Tab</kbd> from Language saves & next</span>
              <button type="button" onClick={cancelEditing}>Cancel</button>
              <button type="button" className="save-subtitle" onClick={() => onSaveSubtitle(subtitleDraft())}>Save</button>
            </div>
          </div>
        ) : editing ? (
          <input
            ref={inputRef}
            value={draft}
            onClick={(event) => event.stopPropagation()}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={() => onSaveText(draft)}
            onKeyDown={(event) => {
              if (event.key === "Enter") event.currentTarget.blur();
              if (event.key === "Escape") { setDraft(entry.text); event.currentTarget.blur(); }
            }}
          />
        ) : (
          <p onDoubleClick={(event) => { if (entry.type === "subtitle") return; event.stopPropagation(); onEdit(); }}>{entry.text}</p>
        )}
        {entry.editable && !editing && (
          <div className="entry-actions">
            <button className="edit-entry" aria-label="Edit entry" title="Edit" onClick={(event) => { event.stopPropagation(); onEdit(); }}><Pencil size={12} /></button>
            <button aria-label="Delete entry" onClick={(event) => { event.stopPropagation(); onDelete(); }}><Trash2 size={13} /></button>
          </div>
        )}
      </div>
    </article>
  );
}

function AnalysisSettings({
  config,
  setConfig,
  status,
  checking,
  credentialPhase,
  onCheck,
  onSaveKey,
  onClose,
}: {
  config: AnalysisConfig;
  setConfig: React.Dispatch<React.SetStateAction<AnalysisConfig>>;
  status: OllamaStatus | null;
  checking: boolean;
  credentialPhase: CredentialPhase;
  onCheck: () => void;
  onSaveKey: () => void;
  onClose: () => void;
}) {
  const setProvider = (provider: AnalysisConfig["provider"]) => {
    setConfig((value) => value.provider === provider ? value : ({
        ...value,
        provider,
        ollamaUrl: provider === "cloud" ? OLLAMA_CLOUD_URL : provider === "gemini" ? "https://generativelanguage.googleapis.com/v1beta" : "http://127.0.0.1:11434",
        model: provider === "cloud" ? "gemma4:31b" : provider === "gemini" ? "gemini-3.6-flash" : "gemma3:4b",
        apiKey: "",
      }));
  };

  return (
    <div className="settings-panel">
      <div className="panel-head">
        <div><p className="eyebrow">AI ANALYSIS</p><h2>Analysis settings</h2></div>
        <button className="icon-button" aria-label="Close settings" onClick={onClose}><X size={17} /></button>
      </div>
      <div className="settings-body">
        <div className="provider-switch" role="group" aria-label="Ollama provider">
          <button className={config.provider === "local" ? "selected" : ""} onClick={() => setProvider("local")}>
            <HardDrive size={15} /><span><b>Local</b><small>On this Mac</small></span>
          </button>
          <button className={config.provider === "cloud" ? "selected" : ""} onClick={() => setProvider("cloud")}>
            <Cloud size={15} /><span><b>Cloud</b><small>ollama.com</small></span>
          </button>
          <button className={config.provider === "gemini" ? "selected" : ""} onClick={() => setProvider("gemini")}>
            <Sparkles size={15} /><span><b>Audio + vision</b><small>Gemini</small></span>
          </button>
        </div>

        <div className={`service-state ${status?.available && status.modelAvailable ? "ready" : status ? "offline" : ""}`}>
          <span>{checking ? <RefreshCw className="spin" size={16} /> : status?.available && status.modelAvailable ? <Check size={16} /> : <BrainCircuit size={16} />}</span>
          <div><strong>{checking ? "Checking provider…" : status?.available ? `${config.provider === "gemini" ? "Gemini" : "Ollama"} connected` : config.provider === "gemini" ? "Gemini multimodal" : config.provider === "cloud" ? "Ollama Cloud" : "Local Ollama"}</strong><p>{checking ? `Testing ${config.provider === "gemini" ? "the Gemini API" : config.provider === "cloud" ? "ollama.com" : "the local endpoint"}` : status?.message ?? (config.provider === "gemini" ? "Sends each chunk's real audio and frames together." : config.provider === "cloud" ? "Add a Cloud API key, then test the connection." : "The safe loopback endpoint is used by default.")}</p></div>
        </div>

        <label>
          <span>API URL <em>{config.provider === "local" ? "Configurable" : "Managed"}</em></span>
          <input readOnly={config.provider !== "local"} value={config.ollamaUrl} onChange={(event) => setConfig((value) => ({ ...value, ollamaUrl: event.target.value }))} placeholder="http://127.0.0.1:11434" />
          <small>{config.provider === "gemini" ? "Native audio + vision requests use Google's HTTPS API." : config.provider === "cloud" ? "Direct requests use Ollama's HTTPS API." : "Loopback stays on this computer. You can also enter another Ollama host."}</small>
        </label>
        {config.provider !== "local" && (
          <label>
            <span>{config.provider === "gemini" ? "Gemini" : "Ollama Cloud"} API key <em aria-live="polite">{credentialPhase === "loading" ? "Loading…" : credentialPhase === "saving" ? "Saving…" : credentialPhase === "error" ? "Not saved" : config.apiKey ? "Saved securely" : "No key saved"}</em></span>
            <div className="secret-input"><KeyRound size={14} /><input type="password" autoComplete="off" disabled={credentialPhase === "loading"} value={config.apiKey} onChange={(event) => setConfig((value) => ({ ...value, apiKey: event.target.value }))} onBlur={onSaveKey} placeholder={`Paste your ${config.provider === "gemini" ? "Gemini" : "Ollama"} API key`} /></div>
            <small>Stored in your operating system's secure credential store, never in the Markdown sidecar or browser settings. Clear the field to remove it.</small>
          </label>
        )}
        <label>
          <span>{config.provider === "gemini" ? "Audio + vision model" : "Vision model"}</span>
          <input list="ollama-models" value={config.model} onChange={(event) => setConfig((value) => ({ ...value, model: event.target.value }))} placeholder={config.provider === "gemini" ? "gemini-3.6-flash" : config.provider === "cloud" ? "gemma4:31b" : "gemma3:4b"} />
          <datalist id="ollama-models">{status?.models.map((model) => <option key={model} value={model} />)}</datalist>
          <small>{config.provider === "gemini" ? "Gemini receives AAC audio and frames once, then returns both the timeline summary and timestamped verbatim transcript." : config.provider === "cloud" ? "Choose a direct API model. Local aliases ending in -cloud or :cloud are accepted and mapped automatically." : "Choose an installed model that accepts images. Signed-in local Ollama can also run cloud models."}</small>
        </label>
        <label>
          <span>Whisper model path <em>Optional</em></span>
          <input value={config.whisperModelPath} onChange={(event) => setConfig((value) => ({ ...value, whisperModelPath: event.target.value }))} placeholder="/path/to/ggml-base.en.bin" />
          <small>If `whisper-cli` and this local model exist, audio is transcribed per chunk. Companion .srt/.vtt files are used first.</small>
        </label>
        <button className="secondary full" onClick={onCheck} disabled={checking}><RefreshCw className={checking ? "spin" : ""} size={15} /> Test connection</button>

        <div className="privacy-box">
          <strong>What analysis reads</strong>
          <p>{config.provider === "gemini" ? `The chunk's extracted AAC audio and ${config.frameCount} chronological frames are sent to Gemini over HTTPS. Summary and subtitle cues are saved to the human-editable Markdown sidecar.` : `${config.frameCount} frames, plus adjacent subtitles or an optional local Whisper transcript. ${config.provider === "cloud" ? "Those samples are sent to Ollama Cloud over HTTPS." : "With local Ollama, they stay on this computer."}`} The original video is never modified.</p>
        </div>
      </div>
    </div>
  );
}

export default App;
