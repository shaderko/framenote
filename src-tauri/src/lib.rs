use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tungstenite::Message;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command as StdCommand, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Manager, State};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use tokio::{process::Command, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const CANCELLED: &str = "ANALYSIS_CANCELLED";
const BOOKMARK_HEADING: &str = "## Bookmarks";
const AI_HEADING: &str = "## AI timeline";
const SUBTITLE_HEADING: &str = "## Subtitles";
const WAVEFORM_CACHE_VERSION: &str = "waveform-v1-rate100-all-tracks-normalized";
const WAVEFORM_CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const KEYRING_SERVICE: &str = "com.framenote.desktop.ai";
const EMBEDDED_CHAPTER_IMPORT_VERSION: &str = "embedded-chapters-v1";
const EMBEDDED_CHAPTER_MARKER: &str = "<!-- framenote:embedded-chapters fingerprint=";
const COLLABORATION_SERVICE_TYPE: &str = "_framenote._tcp.local.";
const COLLABORATION_EVENT_LIMIT: usize = 1024;
const COLLABORATION_PEER_TTL: Duration = Duration::from_secs(12);

struct AppState {
    jobs: Mutex<HashMap<String, CancellationToken>>,
    media: MediaServer,
    collaboration: CollaborationService,
}

#[derive(Clone)]
struct MediaServer {
    base_url: String,
    files: Arc<RwLock<HashMap<String, MediaSource>>>,
}

impl AppState {
    fn new(media: MediaServer, collaboration: CollaborationService) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            media,
            collaboration,
        }
    }
}

#[derive(Clone)]
enum MediaSource {
    Local(PathBuf),
    Remote(RemoteMediaSource),
}

#[derive(Clone)]
struct RemoteMediaSource {
    media_url: String,
    mix_url: String,
    content_type: String,
}

#[derive(Clone)]
struct CollaborationService {
    mdns: ServiceDaemon,
    port: u16,
    hosted: Arc<RwLock<Option<HostedSession>>>,
    joined: Arc<Mutex<Option<JoinedSession>>>,
    host_cursor: Arc<Mutex<u64>>,
    client_id: String,
    /// When hosting via the internet relay, stores the connection state.
    relay: Arc<RwLock<Option<RelayState>>>,
}

/// Tracks an active internet-relay session.
struct RelayState {
    #[allow(dead_code)]
    url: String,
    /// The relay listener thread checks this flag. Set to true to wind down.
    disconnect: Arc<AtomicBool>,
}

unsafe impl Send for RelayState {}
unsafe impl Sync for RelayState {}

#[derive(Clone)]
struct HostedSession {
    code: String,
    token: String,
    service_fullname: String,
    video_path: PathBuf,
    sidecar_path: PathBuf,
    video_name: String,
    audio_tracks: Vec<AudioTrackInfo>,
    frame_rate: Option<f64>,
    host_name: String,
    runtime: Arc<Mutex<HostedSessionRuntime>>,
}

struct HostedSessionRuntime {
    sequence: u64,
    document_revision: u64,
    markdown: String,
    transport: CollaborationTransport,
    events: VecDeque<CollaborationEvent>,
    peers: HashMap<String, PeerPresence>,
}

struct PeerPresence {
    name: String,
    last_seen: Instant,
}

#[derive(Clone)]
struct JoinedSession {
    code: String,
    token: String,
    host_base_url: String,
    video_name: String,
    shadow_sidecar_path: PathBuf,
    peer_id: String,
    display_name: String,
    cursor: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollaborationTransport {
    position: f64,
    playing: bool,
    playback_rate: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollaborationEvent {
    sequence: u64,
    sender_id: String,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollaborationSessionInfo {
    mode: String,
    code: String,
    participant_count: usize,
    video_name: String,
    display_name: String,
    client_id: String,
    participants: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinCollaborationResult {
    document: SidecarDocument,
    media_registration: MediaRegistration,
    session: CollaborationSessionInfo,
    transport: CollaborationTransport,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CollaborationPollResult {
    events: Vec<CollaborationEvent>,
    participant_count: usize,
    participants: Vec<String>,
    connected: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkJoinRequest {
    code: String,
    peer_id: String,
    display_name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkEventRequest {
    peer_id: String,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NetworkJoinResponse {
    token: String,
    video_name: String,
    markdown: String,
    playback_position: f64,
    audio_tracks: Vec<AudioTrackInfo>,
    frame_rate: Option<f64>,
    transport: CollaborationTransport,
    sequence: u64,
    host_name: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SidecarDocument {
    video_path: String,
    video_name: String,
    sidecar_path: String,
    markdown: String,
    playback_position: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AddBookmarkResult {
    document: SidecarDocument,
    entry_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OllamaStatus {
    available: bool,
    model_available: bool,
    message: String,
    models: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisChunkResult {
    summary: String,
    frame_count: usize,
    transcript_source: String,
    transcript_cues: Vec<TranscriptCue>,
    transcript_complete: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptCue {
    start_seconds: f64,
    end_seconds: f64,
    text: String,
    speaker: String,
    language: String,
}

#[derive(Deserialize)]
struct GeminiPayload {
    summary: String,
    transcript: Vec<GeminiTranscriptCue>,
}

#[derive(Deserialize)]
struct GeminiTranscriptCue {
    start: f64,
    end: f64,
    text: String,
    speaker: String,
    language: String,
}

#[derive(Debug)]
struct GeminiAnalysis {
    summary: String,
    transcript_cues: Vec<TranscriptCue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisChunkRequest {
    job_id: String,
    video_path: String,
    start_seconds: f64,
    end_seconds: f64,
    provider: String,
    ollama_url: String,
    model: String,
    api_key: Option<String>,
    whisper_model_path: Option<String>,
    frame_count: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportClipRequest {
    job_id: String,
    video_path: String,
    output_directory: String,
    clip_index: usize,
    start_seconds: f64,
    end_seconds: f64,
    label: String,
    preset: String,
    audio_stream_indexes: Option<Vec<u32>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportClipResult {
    file_name: String,
    subtitle_file_name: String,
    video_path: String,
    subtitle_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportManifestClip {
    file_name: String,
    subtitle_file_name: String,
    start_seconds: f64,
    end_seconds: f64,
    label: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioTrackInfo {
    stream_index: u32,
    label: String,
    language: Option<String>,
    codec: String,
    channels: Option<u32>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaRegistration {
    url: String,
    mix_base_url: String,
    audio_tracks: Vec<AudioTrackInfo>,
    frame_rate: Option<f64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WaveformData {
    samples_per_second: f64,
    peaks: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
struct EmbeddedChapter {
    source_index: usize,
    start_seconds: f64,
    title: String,
}

struct FfmpegStream {
    child: Child,
    stdout: ChildStdout,
}

impl Read for FfmpegStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buffer)
    }
}

impl Drop for FfmpegStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn validate_video_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_file() {
        return Err("The selected video no longer exists.".into());
    }
    Ok(path)
}

fn api_key_account(provider: &str) -> Result<&'static str, String> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "cloud" => Ok("ollama-cloud"),
        "gemini" => Ok("gemini"),
        _ => Err("API keys can only be stored for Ollama Cloud or Gemini.".into()),
    }
}

fn load_api_key_from_keyring(provider: &str) -> Result<Option<String>, String> {
    let account = api_key_account(provider)?;
    let entry = keyring::Entry::new(KEYRING_SERVICE, account)
        .map_err(|error| format!("Could not open the secure credential store: {error}"))?;
    match entry.get_password() {
        Ok(key) if key.trim().is_empty() => Ok(None),
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("Could not load the API key securely: {error}")),
    }
}

fn save_api_key_to_keyring(provider: &str, api_key: &str) -> Result<(), String> {
    let account = api_key_account(provider)?;
    let entry = keyring::Entry::new(KEYRING_SERVICE, account)
        .map_err(|error| format!("Could not open the secure credential store: {error}"))?;
    if api_key.trim().is_empty() {
        return match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(format!("Could not remove the saved API key: {error}")),
        };
    }
    entry
        .set_password(api_key.trim())
        .map_err(|error| format!("Could not save the API key securely: {error}"))
}

#[tauri::command]
async fn load_api_key(provider: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || load_api_key_from_keyring(&provider))
        .await
        .map_err(|error| format!("Could not access the secure credential store: {error}"))?
}

#[tauri::command]
async fn save_api_key(provider: String, api_key: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || save_api_key_to_keyring(&provider, &api_key))
        .await
        .map_err(|error| format!("Could not access the secure credential store: {error}"))?
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("static HTTP header")
}

fn media_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mpeg" | "mpg" => "video/mpeg",
        _ => "application/octet-stream",
    }
}

fn requested_range(request: &Request, size: u64) -> Option<(u64, u64)> {
    let value = request
        .headers()
        .iter()
        .find(|candidate| candidate.field.equiv("Range"))?
        .value
        .as_str()
        .trim()
        .strip_prefix("bytes=")?
        .split(',')
        .next()?;
    let (start, end) = value.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?.min(size);
        return Some((size.saturating_sub(suffix), size.saturating_sub(1)));
    }
    let start = start.parse::<u64>().ok()?;
    let end = if end.is_empty() {
        size.saturating_sub(1)
    } else {
        end.parse::<u64>().ok()?.min(size.saturating_sub(1))
    };
    (start < size && start <= end).then_some((start, end))
}

fn respond_local_media(request: Request, path: &Path) {
    if !matches!(request.method(), Method::Get | Method::Head) {
        let _ = request.respond(Response::empty(StatusCode(405)));
        return;
    }
    let Ok(mut file) = File::open(path) else {
        let _ = request.respond(Response::empty(StatusCode(404)));
        return;
    };
    let Ok(metadata) = file.metadata() else {
        let _ = request.respond(Response::empty(StatusCode(500)));
        return;
    };
    let size = metadata.len();
    if size == 0 {
        let _ = request.respond(Response::empty(StatusCode(416)));
        return;
    }
    let range = requested_range(&request, size);
    let (start, end, status) = match range {
        Some((start, end)) => (start, end, StatusCode(206)),
        None => (0, size - 1, StatusCode(200)),
    };
    let length = end - start + 1;
    let mut headers = vec![
        header("Accept-Ranges", "bytes"),
        header("Content-Type", media_content_type(path)),
        header("Cache-Control", "no-store"),
        header("Access-Control-Allow-Origin", "*"),
    ];
    if status == StatusCode(206) {
        headers.push(header(
            "Content-Range",
            &format!("bytes {start}-{end}/{size}"),
        ));
    }
    if request.method() == &Method::Head {
        let _ = request.respond(Response::new(
            status,
            headers,
            std::io::empty(),
            Some(length as usize),
            None,
        ));
        return;
    }
    if file.seek(SeekFrom::Start(start)).is_err() {
        let _ = request.respond(Response::empty(StatusCode(500)));
        return;
    }
    let _ = request.respond(Response::new(
        status,
        headers,
        file.take(length),
        Some(length as usize),
        None,
    ));
}

fn respond_remote_proxy(request: Request, url: &str, fallback_content_type: &str) {
    if !matches!(request.method(), Method::Get | Method::Head) {
        let _ = request.respond(Response::empty(StatusCode(405)));
        return;
    }
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            let _ = request.respond(Response::empty(StatusCode(503)));
            return;
        }
    };
    let mut outbound = if request.method() == &Method::Head {
        client.head(url)
    } else {
        client.get(url)
    };
    if let Some(range) = request
        .headers()
        .iter()
        .find(|candidate| candidate.field.equiv("Range"))
    {
        outbound = outbound.header(reqwest::header::RANGE, range.value.as_str());
    }
    let response = match outbound.send() {
        Ok(response) => response,
        Err(_) => {
            let _ = request.respond(
                Response::from_string("The sharing peer is unavailable.").with_status_code(503),
            );
            return;
        }
    };
    let status = StatusCode(response.status().as_u16());
    let length = response
        .content_length()
        .and_then(|value| usize::try_from(value).ok());
    let mut headers = vec![
        header("Accept-Ranges", "bytes"),
        header("Cache-Control", "no-store"),
        header("Access-Control-Allow-Origin", "*"),
        header(
            "Content-Type",
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or(fallback_content_type),
        ),
    ];
    if let Some(value) = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
    {
        headers.push(header("Content-Range", value));
    }
    let _ = request.respond(Response::new(status, headers, response, length, None));
}

fn respond_media(request: Request, files: &RwLock<HashMap<String, MediaSource>>) {
    let token = request
        .url()
        .split('?')
        .next()
        .unwrap_or_default()
        .strip_prefix("/media/")
        .unwrap_or_default();
    let source = files
        .read()
        .ok()
        .and_then(|files| files.get(token).cloned());
    let Some(source) = source else {
        let _ = request.respond(Response::empty(StatusCode(404)));
        return;
    };
    match source {
        MediaSource::Local(path) => respond_local_media(request, &path),
        MediaSource::Remote(remote) => {
            respond_remote_proxy(request, &remote.media_url, &remote.content_type)
        }
    }
}

fn respond_audio_mix_for_path(request: Request, path: &Path) {
    if !matches!(request.method(), Method::Get | Method::Head) {
        let _ = request.respond(Response::empty(StatusCode(405)));
        return;
    }
    let Ok(parsed) = url::Url::parse(&format!("http://localhost{}", request.url())) else {
        let _ = request.respond(Response::empty(StatusCode(400)));
        return;
    };
    let query = parsed.query_pairs().collect::<HashMap<_, _>>();
    let tracks = query
        .get("tracks")
        .map(|value| {
            value
                .split(',')
                .filter_map(|value| value.parse::<u32>().ok())
                .take(16)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if tracks.is_empty() {
        let _ = request.respond(Response::empty(StatusCode(400)));
        return;
    }
    let mut volumes = query
        .get("volumes")
        .map(|value| {
            value
                .split(',')
                .filter_map(|value| value.parse::<f32>().ok())
                .map(|value| value.clamp(0.0, 2.0))
                .take(tracks.len())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    volumes.resize(tracks.len(), 1.0);
    let start = query
        .get("start")
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or_default()
        .max(0.0);
    let headers = vec![
        header("Content-Type", "audio/aac"),
        header("Cache-Control", "no-store"),
        header("Access-Control-Allow-Origin", "*"),
    ];
    if request.method() == &Method::Head {
        let _ = request.respond(Response::new(
            StatusCode(200),
            headers,
            std::io::empty(),
            Some(0),
            None,
        ));
        return;
    }
    let Some(ffmpeg) = find_executable("ffmpeg") else {
        let _ = request.respond(Response::from_string("FFmpeg is required").with_status_code(503));
        return;
    };

    let mut filters = tracks
        .iter()
        .zip(volumes.iter())
        .enumerate()
        .map(|(index, (track, volume))| format!("[0:{track}]volume={volume:.3}[a{index}]"))
        .collect::<Vec<_>>();
    if tracks.len() == 1 {
        filters.push("[a0]anull[mix]".into());
    } else {
        let inputs = (0..tracks.len())
            .map(|index| format!("[a{index}]"))
            .collect::<String>();
        filters.push(format!(
            "{inputs}amix=inputs={}:duration=longest:normalize=1:dropout_transition=0[mix]",
            tracks.len()
        ));
    }
    let args = [
        vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-ss".into(),
            format!("{start:.3}"),
            "-i".into(),
            path.to_string_lossy().into_owned(),
            "-filter_complex".into(),
            filters.join(";"),
            "-map".into(),
            "[mix]".into(),
        ],
        vec![
            "-vn".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "192k".into(),
            "-f".into(),
            "adts".into(),
            "-flush_packets".into(),
            "1".into(),
            "pipe:1".into(),
        ],
    ]
    .concat();
    let child = StdCommand::new(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        let _ =
            request.respond(Response::from_string("Could not start FFmpeg").with_status_code(500));
        return;
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = request.respond(Response::empty(StatusCode(500)));
        return;
    };
    let reader = FfmpegStream { child, stdout };
    let _ = request.respond(Response::new(StatusCode(200), headers, reader, None, None));
}

fn respond_audio_mix(request: Request, files: &RwLock<HashMap<String, MediaSource>>) {
    let Ok(parsed) = url::Url::parse(&format!("http://localhost{}", request.url())) else {
        let _ = request.respond(Response::empty(StatusCode(400)));
        return;
    };
    let token = parsed.path().strip_prefix("/mix/").unwrap_or_default();
    let source = files
        .read()
        .ok()
        .and_then(|files| files.get(token).cloned());
    let Some(source) = source else {
        let _ = request.respond(Response::empty(StatusCode(404)));
        return;
    };
    match source {
        MediaSource::Local(path) => respond_audio_mix_for_path(request, &path),
        MediaSource::Remote(remote) => {
            let url = parsed
                .query()
                .map(|query| format!("{}?{query}", remote.mix_url))
                .unwrap_or(remote.mix_url);
            respond_remote_proxy(request, &url, "audio/aac");
        }
    }
}

fn respond_request(request: Request, files: &RwLock<HashMap<String, MediaSource>>) {
    if request.url().starts_with("/media/") {
        respond_media(request, files);
    } else if request.url().starts_with("/mix/") {
        respond_audio_mix(request, files);
    } else {
        let _ = request.respond(Response::empty(StatusCode(404)));
    }
}

fn start_media_server() -> Result<MediaServer, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("Could not start the private media server: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("Could not read the private media server address: {error}"))?
        .port();
    let server = Server::from_listener(listener, None)
        .map_err(|error| format!("Could not start the private media server: {error}"))?;
    let files = Arc::new(RwLock::new(HashMap::new()));
    let server_files = Arc::clone(&files);
    thread::Builder::new()
        .name("framenote-media".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                let request_files = Arc::clone(&server_files);
                thread::spawn(move || respond_request(request, &request_files));
            }
        })
        .map_err(|error| format!("Could not start the private media thread: {error}"))?;
    Ok(MediaServer {
        base_url: format!("http://127.0.0.1:{port}"),
        files,
    })
}

fn respond_json<T: Serialize>(request: Request, status: u16, value: &T) {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    let response = Response::from_data(body)
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
        .with_header(header("Access-Control-Allow-Origin", "*"));
    let _ = request.respond(response);
}

fn read_request_json<T: for<'de> Deserialize<'de>>(request: &mut Request) -> Result<T, String> {
    let mut body = Vec::new();
    request
        .as_reader()
        .take(8 * 1024 * 1024)
        .read_to_end(&mut body)
        .map_err(|error| format!("Could not read peer request: {error}"))?;
    serde_json::from_slice(&body).map_err(|error| format!("Invalid peer request: {error}"))
}

fn prune_peers(runtime: &mut HostedSessionRuntime) {
    runtime
        .peers
        .retain(|_, peer| peer.last_seen.elapsed() <= COLLABORATION_PEER_TTL);
}

fn participant_count(runtime: &mut HostedSessionRuntime) -> usize {
    prune_peers(runtime);
    1 + runtime.peers.len()
}

fn participant_names(runtime: &mut HostedSessionRuntime, host_name: &str) -> Vec<String> {
    prune_peers(runtime);
    let mut names = vec![host_name.to_string()];
    names.extend(runtime.peers.values().map(|peer| peer.name.clone()));
    names.sort();
    names
}

fn publish_host_event(
    session: &HostedSession,
    sender_id: String,
    kind: String,
    payload: serde_json::Value,
) -> Result<Option<CollaborationEvent>, String> {
    if !matches!(kind.as_str(), "transport" | "document") {
        return Err("Unsupported collaboration event.".into());
    }
    let mut runtime = session
        .runtime
        .lock()
        .map_err(|_| "The collaboration session is unavailable.".to_string())?;
    if kind == "transport" {
        let transport = serde_json::from_value::<CollaborationTransport>(payload.clone())
            .map_err(|_| "The synchronized playback state is invalid.".to_string())?;
        if !transport.position.is_finite()
            || transport.position < 0.0
            || !transport.playback_rate.is_finite()
            || !(0.25..=4.0).contains(&transport.playback_rate)
        {
            return Err("The synchronized playback state is invalid.".into());
        }
        runtime.transport = transport;
    } else {
        let markdown = payload["markdown"]
            .as_str()
            .ok_or_else(|| "The shared Markdown update is invalid.".to_string())?;
        if markdown.len() > 8 * 1024 * 1024 {
            return Err("The shared Markdown update is too large.".into());
        }
        if markdown == runtime.markdown {
            return Ok(None);
        }
        fs::write(&session.sidecar_path, markdown).map_err(|error| {
            format!(
                "Could not save the shared Markdown to {}: {error}",
                session.sidecar_path.display()
            )
        })?;
        runtime.markdown = markdown.to_string();
        runtime.document_revision += 1;
    }

    runtime.sequence += 1;
    let event = CollaborationEvent {
        sequence: runtime.sequence,
        sender_id,
        kind,
        payload,
    };
    runtime.events.push_back(event.clone());
    while runtime.events.len() > COLLABORATION_EVENT_LIMIT {
        runtime.events.pop_front();
    }
    Ok(Some(event))
}

fn collaboration_session_for_token(
    hosted: &RwLock<Option<HostedSession>>,
    token: &str,
) -> Option<HostedSession> {
    hosted.read().ok().and_then(|session| {
        session
            .as_ref()
            .filter(|session| session.token == token)
            .cloned()
    })
}

fn respond_collaboration_request(mut request: Request, hosted: &RwLock<Option<HostedSession>>) {
    let Ok(parsed) = url::Url::parse(&format!("http://localhost{}", request.url())) else {
        let _ = request.respond(Response::empty(StatusCode(400)));
        return;
    };
    let path = parsed.path().to_string();
    if path == "/join" {
        if request.method() != &Method::Post {
            let _ = request.respond(Response::empty(StatusCode(405)));
            return;
        }
        let join = match read_request_json::<NetworkJoinRequest>(&mut request) {
            Ok(join) => join,
            Err(error) => {
                respond_json(request, 400, &serde_json::json!({ "error": error }));
                return;
            }
        };
        let session = hosted.read().ok().and_then(|session| session.clone());
        let Some(session) = session.filter(|session| session.code == join.code) else {
            respond_json(
                request,
                403,
                &serde_json::json!({ "error": "Session code not found." }),
            );
            return;
        };
        let mut runtime = match session.runtime.lock() {
            Ok(runtime) => runtime,
            Err(_) => {
                respond_json(
                    request,
                    503,
                    &serde_json::json!({ "error": "Session unavailable." }),
                );
                return;
            }
        };
        runtime.peers.insert(
            join.peer_id,
            PeerPresence {
                name: sanitize_metadata(&join.display_name, "Guest"),
                last_seen: Instant::now(),
            },
        );
        let response = NetworkJoinResponse {
            token: session.token.clone(),
            video_name: session.video_name.clone(),
            markdown: runtime.markdown.clone(),
            playback_position: runtime.transport.position,
            audio_tracks: session.audio_tracks.clone(),
            frame_rate: session.frame_rate,
            transport: runtime.transport.clone(),
            sequence: runtime.sequence,
            host_name: session.host_name.clone(),
        };
        drop(runtime);
        respond_json(request, 200, &response);
        return;
    }

    let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if segments.len() != 3 || segments[0] != "session" {
        let _ = request.respond(Response::empty(StatusCode(404)));
        return;
    }
    let Some(session) = collaboration_session_for_token(hosted, segments[1]) else {
        let _ = request.respond(Response::empty(StatusCode(404)));
        return;
    };
    match segments[2] {
        "media" => respond_local_media(request, &session.video_path),
        "mix" => respond_audio_mix_for_path(request, &session.video_path),
        "events" if request.method() == &Method::Get => {
            let query = parsed.query_pairs().collect::<HashMap<_, _>>();
            let after = query
                .get("after")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_default();
            let peer_id = query.get("peerId").map(|value| value.to_string());
            let mut runtime = match session.runtime.lock() {
                Ok(runtime) => runtime,
                Err(_) => {
                    respond_json(
                        request,
                        503,
                        &serde_json::json!({ "error": "Session unavailable." }),
                    );
                    return;
                }
            };
            if let Some(peer_id) = peer_id {
                if let Some(peer) = runtime.peers.get_mut(&peer_id) {
                    peer.last_seen = Instant::now();
                }
            }
            let events = runtime
                .events
                .iter()
                .filter(|event| event.sequence > after)
                .cloned()
                .collect::<Vec<_>>();
            let count = participant_count(&mut runtime);
            let participants = participant_names(&mut runtime, &session.host_name);
            drop(runtime);
            respond_json(
                request,
                200,
                &CollaborationPollResult {
                    events,
                    participant_count: count,
                    participants,
                    connected: true,
                },
            );
        }
        "event" if request.method() == &Method::Post => {
            let event = match read_request_json::<NetworkEventRequest>(&mut request) {
                Ok(event) => event,
                Err(error) => {
                    respond_json(request, 400, &serde_json::json!({ "error": error }));
                    return;
                }
            };
            if let Ok(mut runtime) = session.runtime.lock() {
                if let Some(peer) = runtime.peers.get_mut(&event.peer_id) {
                    peer.last_seen = Instant::now();
                }
            }
            match publish_host_event(&session, event.peer_id, event.kind, event.payload) {
                Ok(_) => respond_json(request, 200, &serde_json::json!({ "accepted": true })),
                Err(error) => respond_json(request, 400, &serde_json::json!({ "error": error })),
            }
        }
        "leave" if request.method() == &Method::Post => {
            let body = read_request_json::<serde_json::Value>(&mut request).unwrap_or_default();
            if let Some(peer_id) = body["peerId"].as_str() {
                if let Ok(mut runtime) = session.runtime.lock() {
                    runtime.peers.remove(peer_id);
                }
            }
            respond_json(request, 200, &serde_json::json!({ "left": true }));
        }
        _ => {
            let _ = request.respond(Response::empty(StatusCode(404)));
        }
    }
}

fn start_collaboration_service() -> Result<CollaborationService, String> {
    let listener = TcpListener::bind(("0.0.0.0", 0))
        .map_err(|error| format!("Could not start peer sharing: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("Could not read the peer sharing address: {error}"))?
        .port();
    let server = Server::from_listener(listener, None)
        .map_err(|error| format!("Could not start peer sharing: {error}"))?;
    let hosted = Arc::new(RwLock::new(None));
    let server_hosted = Arc::clone(&hosted);
    thread::Builder::new()
        .name("framenote-peer".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                let request_hosted = Arc::clone(&server_hosted);
                thread::spawn(move || respond_collaboration_request(request, &request_hosted));
            }
        })
        .map_err(|error| format!("Could not start the peer sharing thread: {error}"))?;
    let mdns = ServiceDaemon::new()
        .map_err(|error| format!("Could not start local session discovery: {error}"))?;
    Ok(CollaborationService {
        mdns,
        port,
        hosted,
        joined: Arc::new(Mutex::new(None)),
        host_cursor: Arc::new(Mutex::new(0)),
        client_id: Uuid::new_v4().to_string(),
        relay: Arc::new(RwLock::new(None)),
    })
}

fn sidecar_path(video_path: &Path) -> Result<PathBuf, String> {
    let extension = video_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" | "mpeg" | "mpg"
    ) {
        return Err("Choose a supported local video file.".into());
    }
    Ok(video_path.with_extension("md"))
}

fn initial_markdown(video_path: &Path) -> String {
    let name = video_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Video");
    format!("# {name}\n\n<!-- framenote:v1 -->\n<!-- framenote:position seconds=0.000 -->\n\n{BOOKMARK_HEADING}\n\n{AI_HEADING}\n\n{SUBTITLE_HEADING}\n")
}

fn playback_position(markdown: &str) -> f64 {
    markdown
        .lines()
        .find_map(|line| {
            let marker = line
                .trim()
                .strip_prefix("<!-- framenote:position seconds=")?;
            marker.strip_suffix(" -->")?.parse::<f64>().ok()
        })
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or_default()
}

fn with_playback_position(markdown: &str, seconds: f64) -> String {
    let marker = format!(
        "<!-- framenote:position seconds={:.3} -->",
        seconds.max(0.0)
    );
    let mut found = false;
    let mut lines = markdown
        .lines()
        .map(|line| {
            if line.trim().starts_with("<!-- framenote:position seconds=") {
                found = true;
                marker.clone()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>();
    if !found {
        let insert_at = lines
            .iter()
            .position(|line| line.trim() == "<!-- framenote:v1 -->")
            .map(|index| index + 1)
            .unwrap_or_else(|| {
                lines
                    .iter()
                    .position(|line| line.starts_with('#'))
                    .map(|index| index + 1)
                    .unwrap_or(0)
            });
        lines.insert(insert_at, marker);
    }
    format!("{}\n", lines.join("\n"))
}

fn read_or_create_markdown(video_path: &Path) -> Result<(PathBuf, String), String> {
    let path = sidecar_path(video_path)?;
    if !path.exists() {
        fs::write(&path, initial_markdown(video_path))
            .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
    }
    let markdown = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    Ok((path, markdown))
}

fn write_markdown(video_path: &Path, markdown: &str) -> Result<SidecarDocument, String> {
    let sidecar = sidecar_path(video_path)?;
    let normalized = if markdown.ends_with('\n') {
        markdown.to_string()
    } else {
        format!("{markdown}\n")
    };
    fs::write(&sidecar, &normalized)
        .map_err(|error| format!("Could not save {}: {error}", sidecar.display()))?;
    document(video_path, sidecar, normalized)
}

fn document(
    video_path: &Path,
    sidecar: PathBuf,
    markdown: String,
) -> Result<SidecarDocument, String> {
    Ok(SidecarDocument {
        video_path: video_path.to_string_lossy().into_owned(),
        video_name: video_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Video")
            .to_string(),
        sidecar_path: sidecar.to_string_lossy().into_owned(),
        playback_position: playback_position(&markdown),
        markdown,
    })
}

fn append_to_section(markdown: &str, heading: &str, line: &str) -> String {
    let mut lines: Vec<String> = markdown.lines().map(str::to_string).collect();

    if let Some(heading_index) = lines
        .iter()
        .position(|candidate| candidate.trim().eq_ignore_ascii_case(heading))
    {
        let next_heading = lines
            .iter()
            .enumerate()
            .skip(heading_index + 1)
            .find(|(_, candidate)| candidate.trim_start().starts_with("## "))
            .map(|(index, _)| index)
            .unwrap_or(lines.len());

        let mut insert_at = next_heading;
        while insert_at > heading_index + 1 && lines[insert_at - 1].trim().is_empty() {
            insert_at -= 1;
        }
        if insert_at == heading_index + 1 {
            lines.insert(insert_at, String::new());
            insert_at += 1;
        }
        lines.insert(insert_at, line.to_string());
    } else {
        if !lines.is_empty() && !lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.push(heading.to_string());
        lines.push(String::new());
        lines.push(line.to_string());
    }

    format!("{}\n", lines.join("\n"))
}

fn stable_fnv1a(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

fn embedded_chapter_fingerprint(video: &Path) -> Result<String, String> {
    let metadata = fs::metadata(video)
        .map_err(|error| format!("Could not inspect the video chapters: {error}"))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    let identity = format!(
        "{}\0{}\0{}\0{}",
        EMBEDDED_CHAPTER_IMPORT_VERSION,
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos()
    );
    Ok(format!("{:016x}", stable_fnv1a(&identity)))
}

fn parse_embedded_chapters(json: &serde_json::Value) -> Vec<EmbeddedChapter> {
    let mut chapters = json["chapters"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(source_index, chapter)| {
            let start_seconds = chapter["start_time"]
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())
                .or_else(|| chapter["start_time"].as_f64())?;
            if !start_seconds.is_finite() || start_seconds < 0.0 {
                return None;
            }
            let fallback = format!("Embedded marker {}", source_index + 1);
            let title = chapter["tags"]["title"]
                .as_str()
                .map(sanitize_entry_text)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(fallback);
            Some(EmbeddedChapter {
                source_index,
                start_seconds,
                title,
            })
        })
        .collect::<Vec<_>>();
    chapters.sort_by(|left, right| left.start_seconds.total_cmp(&right.start_seconds));
    chapters
}

fn probe_embedded_chapters(video: &Path) -> Result<Vec<EmbeddedChapter>, String> {
    let ffprobe = find_executable("ffprobe")
        .ok_or_else(|| "FFprobe is required to import embedded chapter markers.".to_string())?;
    let video_arg = video.to_string_lossy().into_owned();
    let output = StdCommand::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_chapters",
            "-show_entries",
            "chapter=start_time:chapter_tags=title",
            "-of",
            "json",
            &video_arg,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not inspect embedded chapter markers: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            "FFprobe could not inspect embedded chapter markers.".into()
        } else {
            format!("FFprobe could not inspect embedded chapter markers: {detail}")
        });
    }
    let json = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .map_err(|error| format!("FFprobe returned unreadable chapter metadata: {error}"))?;
    Ok(parse_embedded_chapters(&json))
}

fn bookmark_start_exists(markdown: &str, start_seconds: f64) -> bool {
    let mut in_bookmarks = false;
    markdown.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_bookmarks = trimmed.eq_ignore_ascii_case(BOOKMARK_HEADING);
            return false;
        }
        if !in_bookmarks {
            return false;
        }
        let start = marker_number(line, "start").or_else(|| {
            let bracket = line.split_once('[')?.1.split_once(']')?.0;
            let timestamp = bracket.split(['–', '—']).next()?.trim();
            parse_subtitle_timestamp(timestamp)
        });
        start.is_some_and(|value| (value - start_seconds).abs() <= 0.005)
    })
}

fn with_embedded_chapter_marker(markdown: &str, fingerprint: &str) -> String {
    let marker = format!("{EMBEDDED_CHAPTER_MARKER}{fingerprint} -->");
    let mut found = false;
    let mut lines = markdown
        .lines()
        .map(|line| {
            if line.trim().starts_with(EMBEDDED_CHAPTER_MARKER) {
                found = true;
                marker.clone()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>();
    if !found {
        let insert_at = lines
            .iter()
            .position(|line| line.trim().starts_with("<!-- framenote:position seconds="))
            .map(|index| index + 1)
            .or_else(|| {
                lines
                    .iter()
                    .position(|line| line.trim() == "<!-- framenote:v1 -->")
                    .map(|index| index + 1)
            })
            .unwrap_or(0);
        lines.insert(insert_at, marker);
    }
    format!("{}\n", lines.join("\n"))
}

fn merge_embedded_chapters(
    markdown: &str,
    chapters: &[EmbeddedChapter],
    fingerprint: &str,
) -> Option<String> {
    if markdown
        .lines()
        .any(|line| line.trim().starts_with(EMBEDDED_CHAPTER_MARKER))
    {
        return None;
    }

    let mut updated = markdown.to_string();
    for chapter in chapters {
        if bookmark_start_exists(&updated, chapter.start_seconds) {
            continue;
        }
        let milliseconds = (chapter.start_seconds * 1000.0).round() as u64;
        let id = format!("embedded-{}-{milliseconds}", chapter.source_index + 1);
        let line = format!(
            "- [{}] {} <!-- framenote:bookmark:{id} start={:.3} source=embedded-chapter -->",
            format_precise_timestamp(chapter.start_seconds),
            chapter.title,
            chapter.start_seconds
        );
        updated = append_to_section(&updated, BOOKMARK_HEADING, &line);
    }
    Some(with_embedded_chapter_marker(&updated, fingerprint))
}

fn sanitize_entry_text(value: &str) -> String {
    let flattened = value
        .replace(['\r', '\n'], " ")
        .replace("<!--", "")
        .replace("-->", "");
    let compact = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "Untitled note".into()
    } else {
        compact.chars().take(700).collect()
    }
}

fn sanitize_metadata(value: &str, fallback: &str) -> String {
    let clean = value
        .replace(['\r', '\n', '"'], "")
        .replace("<!--", "")
        .replace("-->", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if clean.is_empty() {
        fallback.into()
    } else {
        clean.chars().take(48).collect()
    }
}

fn marker_number(line: &str, name: &str) -> Option<f64> {
    let value = line.split_once(&format!("{name}="))?.1;
    let token = value
        .chars()
        .take_while(|character| character.is_ascii_digit() || matches!(character, '.' | '-'))
        .collect::<String>();
    token.parse().ok()
}

fn ai_range_matches(line: &str, start: f64, end: f64) -> bool {
    line.contains("framenote:ai:")
        && marker_number(line, "start").is_some_and(|value| (value - start).abs() <= 1.0)
        && marker_number(line, "end").is_some_and(|value| (value - end).abs() <= 1.0)
}

fn format_timestamp(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_precise_timestamp(seconds: f64) -> String {
    let total = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = total / 3_600_000;
    let minutes = (total % 3_600_000) / 60_000;
    let seconds = (total % 60_000) / 1000;
    let milliseconds = total % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{milliseconds:03}")
}

#[tauri::command]
async fn pick_video() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Open a video")
            .add_filter(
                "Video",
                &["mp4", "m4v", "mov", "webm", "mkv", "avi", "mpeg", "mpg"],
            )
            .pick_file()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| format!("The file picker failed: {error}"))
}

#[tauri::command]
async fn pick_export_directory() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("Choose FrameNote export location")
            .pick_folder()
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| format!("The folder picker failed: {error}"))
}

fn six_digit_session_code() -> String {
    let bytes = *Uuid::new_v4().as_bytes();
    let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % 1_000_000;
    format!("{value:06}")
}

fn hosted_session_info(
    service: &CollaborationService,
    session: &HostedSession,
) -> CollaborationSessionInfo {
    let participant_count = session
        .runtime
        .lock()
        .map(|mut runtime| participant_count(&mut runtime))
        .unwrap_or(1);
    CollaborationSessionInfo {
        mode: "host".into(),
        code: session.code.clone(),
        participant_count,
        video_name: session.video_name.clone(),
        display_name: session.host_name.clone(),
        client_id: service.client_id.clone(),
        participants: session
            .runtime
            .lock()
            .map(|mut runtime| participant_names(&mut runtime, &session.host_name))
            .unwrap_or_else(|_| vec![session.host_name.clone()]),
    }
}

#[tauri::command]
fn host_collaboration(
    state: State<'_, AppState>,
    video_path: String,
    display_name: String,
) -> Result<CollaborationSessionInfo, String> {
    if state
        .collaboration
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .is_some()
    {
        return Err("Leave the current shared session before hosting another one.".into());
    }
    if state
        .collaboration
        .hosted
        .read()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .is_some()
    {
        return Err("This project is already being shared.".into());
    }
    let video = validate_video_path(&video_path)?;
    let (sidecar, markdown) = read_or_create_markdown(&video)?;
    let video_name = video
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Shared video")
        .to_string();
    let code = six_digit_session_code();
    let token = Uuid::new_v4().to_string();
    let instance_name = format!("FrameNote {code} {}", &token[..6]);
    let host_name = format!("framenote-{}.local.", &token[..8]);
    let mut properties = HashMap::new();
    properties.insert("code".to_string(), code.clone());
    properties.insert("version".to_string(), "1".to_string());
    let service_info = ServiceInfo::new(
        COLLABORATION_SERVICE_TYPE,
        &instance_name,
        &host_name,
        "",
        state.collaboration.port,
        properties,
    )
    .map_err(|error| format!("Could not publish the local session: {error}"))?
    .enable_addr_auto();
    let service_fullname = service_info.get_fullname().to_string();
    let host_name_label = sanitize_metadata(&display_name, "Host");
    let session = HostedSession {
        code: code.clone(),
        token,
        service_fullname,
        video_path: video,
        sidecar_path: sidecar,
        video_name,
        audio_tracks: probe_audio_tracks(Path::new(&video_path)),
        frame_rate: probe_frame_rate(Path::new(&video_path)),
        host_name: host_name_label,
        runtime: Arc::new(Mutex::new(HostedSessionRuntime {
            sequence: 0,
            document_revision: 0,
            markdown: markdown.clone(),
            transport: CollaborationTransport {
                position: playback_position(&markdown),
                playing: false,
                playback_rate: 1.0,
            },
            events: VecDeque::new(),
            peers: HashMap::new(),
        })),
    };
    *state
        .collaboration
        .hosted
        .write()
        .map_err(|_| "The collaboration state is unavailable.".to_string())? =
        Some(session.clone());
    *state
        .collaboration
        .host_cursor
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())? = 0;
    if let Err(error) = state.collaboration.mdns.register(service_info) {
        if let Ok(mut hosted) = state.collaboration.hosted.write() {
            *hosted = None;
        }
        return Err(format!("Could not advertise the local session: {error}"));
    }
    Ok(hosted_session_info(&state.collaboration, &session))
}

#[tauri::command]
async fn host_relay_session(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
    relay_url: String,
    video_path: String,
    display_name: String,
) -> Result<CollaborationSessionInfo, String> {
    let relay_url = relay_url.trim().trim_end_matches('/').to_string();
    if relay_url.is_empty() {
        return Err("Enter the relay server address.".into());
    }
    {
        let joined = state
            .collaboration
            .joined
            .lock()
            .map_err(|_| "The collaboration state is unavailable.".to_string())?;
        if joined.is_some() {
            return Err("Leave the current shared session before hosting another one.".into());
        }
    }
    {
        let hosted = state
            .collaboration
            .hosted
            .read()
            .map_err(|_| "The collaboration state is unavailable.".to_string())?;
        if hosted.is_some() {
            return Err("This project is already being shared.".into());
        }
    }
    {
        let relay_active = state
            .collaboration
            .relay
            .read()
            .map_err(|_| "The collaboration state is unavailable.".to_string())?;
        if relay_active.is_some() {
            return Err("An internet session is already active.".into());
        }
    }

    let video = validate_video_path(&video_path)?;
    let (sidecar, markdown) = read_or_create_markdown(&video)?;
    let video_name = video
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Shared video")
        .to_string();
    let code = six_digit_session_code();
    let token = Uuid::new_v4().to_string();
    let host_name_label = sanitize_metadata(&display_name, "Host");

    let session = HostedSession {
        code: code.clone(),
        token: token.clone(),
        service_fullname: String::new(),
        video_path: video,
        sidecar_path: sidecar,
        video_name: video_name.clone(),
        audio_tracks: probe_audio_tracks(Path::new(&video_path)),
        frame_rate: probe_frame_rate(Path::new(&video_path)),
        host_name: host_name_label.clone(),
        runtime: Arc::new(Mutex::new(HostedSessionRuntime {
            sequence: 0,
            document_revision: 0,
            markdown: markdown.clone(),
            transport: CollaborationTransport {
                position: playback_position(&markdown),
                playing: false,
                playback_rate: 1.0,
            },
            events: VecDeque::new(),
            peers: HashMap::new(),
        })),
    };

    // Connect to relay and register — run on blocking thread
    let ws_url = format!("wss://{relay_url}/ws");
    let register_code = code.clone();
    let register_token = token.clone();
    let register_host = host_name_label.clone();
    let register_video = video_name.clone();

    let ws = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        let (mut ws, _) = tungstenite::connect(&ws_url)
            .map_err(|e| format!("Could not connect to relay ({ws_url}): {e}"))?;

        let register = serde_json::json!({
            "type": "register",
            "code": register_code,
            "token": register_token,
            "hostName": register_host,
            "videoName": register_video,
        });
        ws.send(Message::Text(register.to_string().into()))
            .map_err(|e| format!("Could not register with relay: {e}"))?;

        match ws.read().map_err(|e| format!("Relay error: {e}"))? {
            Message::Text(text) => {
                let data: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|_| "Relay sent invalid response.".to_string())?;
                if data["type"] != "registered" {
                    let msg = data["message"]
                        .as_str()
                        .unwrap_or("relay rejected registration");
                    return Err(msg.to_string());
                }
            }
            _ => return Err("Relay sent unexpected response.".into()),
        }

        Ok(ws)
    })
    .await
    .map_err(|e| format!("The relay connection failed: {e}"))??;

    // Store the hosted session
    *state
        .collaboration
        .hosted
        .write()
        .map_err(|_| "The collaboration state is unavailable.".to_string())? =
        Some(session.clone());
    *state
        .collaboration
        .host_cursor
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())? = 0;

    // Spawn relay listener thread
    let disconnect = Arc::new(AtomicBool::new(false));
    let disconnect_clone = Arc::clone(&disconnect);
    let coll = state.collaboration.clone();
    let relay_url_store = relay_url.clone();
    let port = state.collaboration.port;

    thread::Builder::new()
        .name("framenote-relay".into())
        .spawn(move || {
            let mut ws = ws; // move ws into the thread
            loop {
                if disconnect_clone.load(Ordering::Relaxed) {
                    let _ = ws.send(Message::Close(None));
                    break;
                }

                let msg = match ws.read() {
                    Ok(msg) => msg,
                    Err(_) => break,
                };

                match msg {
                    Message::Text(text) => {
                        let data: serde_json::Value = match serde_json::from_str(&text) {
                            Ok(d) => d,
                            Err(_) => continue,
                        };

                        match data["type"].as_str() {
                            Some("http-request") => {
                                let Some(id) = data["id"].as_str().map(|s| s.to_string()) else {
                                    continue;
                                };
                                let method = data["method"].as_str().unwrap_or("GET").to_string();
                                let path = data["path"].as_str().unwrap_or("/").to_string();
                                let body_b64 = data["body"].as_str().map(|s| s.to_string());

                                let local_url = format!("http://127.0.0.1:{port}{path}");

                                let client = reqwest::blocking::Client::builder()
                                    .timeout(Duration::from_secs(30))
                                    .build()
                                    .expect("reqwest client");

                                let mut req = client.request(
                                    method.parse().unwrap_or(reqwest::Method::GET),
                                    &local_url,
                                );

                                if let Some(headers) = data["headers"].as_object() {
                                    for (k, v) in headers {
                                        if let Some(v) = v.as_str() {
                                            let lower = k.to_lowercase();
                                            if lower != "host"
                                                && lower != "connection"
                                                && lower != "upgrade"
                                            {
                                                req = req.header(k.as_str(), v);
                                            }
                                        }
                                    }
                                }

                                let response = if let Some(b64) = &body_b64 {
                                    if let Ok(bytes) = BASE64.decode(b64) {
                                        req.body(bytes).send()
                                    } else {
                                        req.send()
                                    }
                                } else {
                                    req.send()
                                };

                                match response {
                                    Ok(resp) => {
                                        let status = resp.status().as_u16() as u64;
                                        let resp_headers = resp
                                            .headers()
                                            .iter()
                                            .map(|(k, v)| {
                                                (
                                                    k.to_string(),
                                                    serde_json::Value::String(
                                                        v.to_str().unwrap_or("").to_string(),
                                                    ),
                                                )
                                            })
                                            .collect::<serde_json::Map<_, _>>();
                                        let resp_body = resp.bytes().unwrap_or_default();

                                        // For small bodies (< 256 KiB), send inline base64 to
                                        // keep things simple. For large bodies (video data),
                                        // send the body as a binary WebSocket frame to avoid
                                        // base64 overhead and WebSocket message size limits.
                                        if resp_body.len() > 256 * 1024 {
                                            // Send JSON metadata first, then raw binary frame
                                            let meta = serde_json::json!({
                                                "type": "http-response",
                                                "id": id,
                                                "status": status,
                                                "headers": resp_headers,
                                                "bodyLength": resp_body.len(),
                                            });
                                            if ws
                                                .send(Message::Text(meta.to_string().into()))
                                                .is_err()
                                            {
                                                break;
                                            }
                                            if ws
                                                .send(Message::Binary(resp_body.into()))
                                                .is_err()
                                            {
                                                break;
                                            }
                                        } else {
                                            let body_b64 = BASE64.encode(&resp_body);
                                            let response_msg = serde_json::json!({
                                                "type": "http-response",
                                                "id": id,
                                                "status": status,
                                                "headers": resp_headers,
                                                "body": body_b64,
                                            });
                                            if ws
                                                .send(Message::Text(
                                                    response_msg.to_string().into(),
                                                ))
                                                .is_err()
                                            {
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let error_msg = serde_json::json!({
                                            "type": "http-response",
                                            "id": id,
                                            "status": 502,
                                            "headers": {},
                                            "body": BASE64.encode(format!("Proxy error: {e}")),
                                        });
                                        let _ = ws.send(Message::Text(error_msg.to_string().into()));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Message::Ping(data) => {
                        let _ = ws.send(Message::Pong(data));
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }

            // Cleanup
            if let Ok(mut relay_state) = coll.relay.write() {
                *relay_state = None;
            }
            if let Ok(mut hosted) = coll.hosted.write() {
                *hosted = None;
            }
        })
        .map_err(|e| format!("Could not start the relay listener: {e}"))?;

    // Store relay state
    *state
        .collaboration
        .relay
        .write()
        .map_err(|_| "The collaboration state is unavailable.".to_string())? =
        Some(RelayState {
            url: relay_url_store,
            disconnect,
        });

    Ok(hosted_session_info(&state.collaboration, &session))
}

fn join_discovered_session(
    service: &CollaborationService,
    code: &str,
    display_name: &str,
) -> Result<(NetworkJoinResponse, String), String> {
    let receiver = service
        .mdns
        .browse(COLLABORATION_SERVICE_TYPE)
        .map_err(|error| format!("Could not search for local sessions: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(8);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| format!("Could not prepare peer connection: {error}"))?;
    let mut last_error = None;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = receiver.recv_timeout(remaining.min(Duration::from_millis(750)));
        let Ok(ServiceEvent::ServiceResolved(info)) = event else {
            continue;
        };
        if info.get_property_val_str("code") != Some(code) {
            continue;
        }
        for address in info.get_addresses_v4() {
            let base_url = format!("http://{}:{}", address, info.get_port());
            let response = client
                .post(format!("{base_url}/join"))
                .json(&serde_json::json!({
                    "code": code,
                    "peerId": service.client_id,
                    "displayName": display_name,
                }))
                .send();
            match response {
                Ok(response) if response.status().is_success() => {
                    let joined = response.json::<NetworkJoinResponse>().map_err(|error| {
                        format!("The sharing peer returned invalid session data: {error}")
                    })?;
                    let _ = service.mdns.stop_browse(COLLABORATION_SERVICE_TYPE);
                    return Ok((joined, base_url));
                }
                Ok(response) => {
                    last_error = Some(format!(
                        "The sharing peer rejected the connection ({})",
                        response.status()
                    ));
                }
                Err(error) => {
                    last_error = Some(format!("Could not connect to the sharing peer: {error}"))
                }
            }
        }
    }
    let _ = service.mdns.stop_browse(COLLABORATION_SERVICE_TYPE);
    Err(last_error.unwrap_or_else(|| {
        "No FrameNote session with that code was found on this local network.".into()
    }))
}

#[tauri::command]
async fn join_collaboration(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    code: String,
    display_name: String,
) -> Result<JoinCollaborationResult, String> {
    let code = code.trim().to_string();
    if code.len() != 6 || !code.chars().all(|character| character.is_ascii_digit()) {
        return Err("Enter the six-digit session code.".into());
    }
    if state
        .collaboration
        .hosted
        .read()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .is_some()
    {
        return Err("Stop hosting before joining another session.".into());
    }
    if state
        .collaboration
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .is_some()
    {
        return Err("Leave the current session before joining another one.".into());
    }
    let service = state.collaboration.clone();
    let display_name = sanitize_metadata(&display_name, "Guest");
    let join_name = display_name.clone();
    let discovery_code = code.clone();
    let (network, host_base_url) = tauri::async_runtime::spawn_blocking(move || {
        join_discovered_session(&service, &discovery_code, &join_name)
    })
    .await
    .map_err(|error| format!("The local session search stopped unexpectedly: {error}"))??;

    let cache_root = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("Could not prepare the shared project cache: {error}"))?
        .join("collaboration")
        .join(&network.token[..12]);
    fs::create_dir_all(&cache_root)
        .map_err(|error| format!("Could not prepare the shared project cache: {error}"))?;
    let shadow_video_path = cache_root.join(safe_file_component(&network.video_name, 120));
    if !shadow_video_path.exists() {
        fs::write(&shadow_video_path, [])
            .map_err(|error| format!("Could not prepare the shared project cache: {error}"))?;
    }
    let shadow_sidecar_path = shadow_video_path.with_extension("md");
    fs::write(&shadow_sidecar_path, &network.markdown)
        .map_err(|error| format!("Could not cache the shared Markdown: {error}"))?;
    let media_token = Uuid::new_v4().to_string();
    state
        .media
        .files
        .write()
        .map_err(|_| "The private media server is unavailable.".to_string())?
        .insert(
            media_token.clone(),
            MediaSource::Remote(RemoteMediaSource {
                media_url: format!("{host_base_url}/session/{}/media", network.token),
                mix_url: format!("{host_base_url}/session/{}/mix", network.token),
                content_type: media_content_type(Path::new(&network.video_name)).into(),
            }),
        );
    let joined = JoinedSession {
        code: code.clone(),
        token: network.token.clone(),
        host_base_url,
        video_name: network.video_name.clone(),
        shadow_sidecar_path: shadow_sidecar_path.clone(),
        peer_id: state.collaboration.client_id.clone(),
        display_name: display_name.clone(),
        cursor: network.sequence,
    };
    *state
        .collaboration
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())? = Some(joined);
    let initial_transport = network.transport.clone();
    let document = SidecarDocument {
        video_path: shadow_video_path.to_string_lossy().into_owned(),
        video_name: network.video_name.clone(),
        sidecar_path: shadow_sidecar_path.to_string_lossy().into_owned(),
        markdown: network.markdown,
        playback_position: network.transport.position,
    };
    Ok(JoinCollaborationResult {
        document,
        media_registration: MediaRegistration {
            url: format!("{}/media/{media_token}", state.media.base_url),
            mix_base_url: format!("{}/mix/{media_token}", state.media.base_url),
            audio_tracks: network.audio_tracks,
            frame_rate: network.frame_rate,
        },
        session: CollaborationSessionInfo {
            mode: "guest".into(),
            code,
            participant_count: 2,
            video_name: network.video_name,
            display_name: display_name.clone(),
            client_id: state.collaboration.client_id.clone(),
            participants: vec![network.host_name, display_name],
        },
        transport: initial_transport,
    })
}

#[tauri::command]
async fn join_relay_session(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    relay_url: String,
    code: String,
    display_name: String,
) -> Result<JoinCollaborationResult, String> {
    let code = code.trim().to_string();
    if code.len() != 6 || !code.chars().all(|character| character.is_ascii_digit()) {
        return Err("Enter the six-digit session code.".into());
    }
    let relay_url = relay_url.trim().trim_end_matches('/').to_string();
    if relay_url.is_empty() {
        return Err("Enter the relay server address.".into());
    }
    {
        let hosted = state
            .collaboration
            .hosted
            .read()
            .map_err(|_| "The collaboration state is unavailable.".to_string())?;
        if hosted.is_some() {
            return Err("Stop hosting before joining another session.".into());
        }
    }
    {
        let joined = state
            .collaboration
            .joined
            .lock()
            .map_err(|_| "The collaboration state is unavailable.".to_string())?;
        if joined.is_some() {
            return Err("Leave the current session before joining another one.".into());
        }
    }

    let http_base = format!("https://{relay_url}");
    let join_url = format!("{http_base}/join");
    let display_name = sanitize_metadata(&display_name, "Guest");
    let peer_id = state.collaboration.client_id.clone();

    // Clone values before moving into the blocking closure
    let join_code = code.clone();
    let join_peer_id = peer_id.clone();
    let join_display_name = display_name.clone();
    let join_http_base = http_base.clone();

    let network: NetworkJoinResponse = tauri::async_runtime::spawn_blocking(move || {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| format!("Could not prepare relay connection: {e}"))?;

        let response = client
            .post(&join_url)
            .json(&serde_json::json!({
                "code": join_code,
                "peerId": join_peer_id,
                "displayName": join_display_name,
            }))
            .send()
            .map_err(|e| {
                format!("Could not reach the relay server ({join_http_base}): {e}")
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(format!("The relay returned {status}: {body}"));
        }

        response
            .json::<NetworkJoinResponse>()
            .map_err(|e| format!("Invalid session data from relay: {e}"))
    })
    .await
    .map_err(|e| format!("The join request failed: {e}"))??;

    // Set up cached files (identical to join_collaboration)
    let cache_root = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("Could not prepare the shared project cache: {error}"))?
        .join("collaboration")
        .join(&network.token[..12]);
    fs::create_dir_all(&cache_root)
        .map_err(|error| format!("Could not prepare the shared project cache: {error}"))?;
    let shadow_video_path = cache_root.join(safe_file_component(&network.video_name, 120));
    if !shadow_video_path.exists() {
        fs::write(&shadow_video_path, [])
            .map_err(|error| format!("Could not prepare the shared project cache: {error}"))?;
    }
    let shadow_sidecar_path = shadow_video_path.with_extension("md");
    fs::write(&shadow_sidecar_path, &network.markdown)
        .map_err(|error| format!("Could not cache the shared Markdown: {error}"))?;
    let media_token = Uuid::new_v4().to_string();
    state
        .media
        .files
        .write()
        .map_err(|_| "The private media server is unavailable.".to_string())?
        .insert(
            media_token.clone(),
            MediaSource::Remote(RemoteMediaSource {
                media_url: format!("{http_base}/session/{}/media", network.token),
                mix_url: format!("{http_base}/session/{}/mix", network.token),
                content_type: media_content_type(Path::new(&network.video_name)).into(),
            }),
        );
    let joined = JoinedSession {
        code: code.clone(),
        token: network.token.clone(),
        host_base_url: http_base,
        video_name: network.video_name.clone(),
        shadow_sidecar_path: shadow_sidecar_path.clone(),
        peer_id: state.collaboration.client_id.clone(),
        display_name: display_name.clone(),
        cursor: network.sequence,
    };
    *state
        .collaboration
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())? = Some(joined);
    let initial_transport = network.transport.clone();
    let document = SidecarDocument {
        video_path: shadow_video_path.to_string_lossy().into_owned(),
        video_name: network.video_name.clone(),
        sidecar_path: shadow_sidecar_path.to_string_lossy().into_owned(),
        markdown: network.markdown,
        playback_position: network.transport.position,
    };
    Ok(JoinCollaborationResult {
        document,
        media_registration: MediaRegistration {
            url: format!("{}/media/{media_token}", state.media.base_url),
            mix_base_url: format!("{}/mix/{media_token}", state.media.base_url),
            audio_tracks: network.audio_tracks,
            frame_rate: network.frame_rate,
        },
        session: CollaborationSessionInfo {
            mode: "guest".into(),
            code,
            participant_count: 2,
            video_name: network.video_name,
            display_name: display_name.clone(),
            client_id: state.collaboration.client_id.clone(),
            participants: vec![network.host_name, display_name],
        },
        transport: initial_transport,
    })
}

fn poll_collaboration_service(
    service: &CollaborationService,
) -> Result<CollaborationPollResult, String> {
    if let Some(session) = service
        .hosted
        .read()
        .ok()
        .and_then(|session| session.clone())
    {
        let mut cursor = service
            .host_cursor
            .lock()
            .map_err(|_| "The collaboration cursor is unavailable.".to_string())?;
        let mut runtime = session
            .runtime
            .lock()
            .map_err(|_| "The collaboration session is unavailable.".to_string())?;
        let events = runtime
            .events
            .iter()
            .filter(|event| event.sequence > *cursor)
            .cloned()
            .collect::<Vec<_>>();
        if let Some(event) = events.last() {
            *cursor = event.sequence;
        }
        let count = participant_count(&mut runtime);
        let participants = participant_names(&mut runtime, &session.host_name);
        return Ok(CollaborationPollResult {
            events,
            participant_count: count,
            participants,
            connected: true,
        });
    }
    let joined = service
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .clone()
        .ok_or_else(|| "No shared session is active.".to_string())?;
    let url = format!(
        "{}/session/{}/events?after={}&peerId={}",
        joined.host_base_url, joined.token, joined.cursor, joined.peer_id
    );
    let response = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|error| format!("Could not prepare the peer connection: {error}"))?
        .get(url)
        .send()
        .map_err(|_| "The sharing peer is unavailable on the local network.".to_string())?;
    if !response.status().is_success() {
        return Err("The sharing peer ended this session.".into());
    }
    let result = response
        .json::<CollaborationPollResult>()
        .map_err(|error| format!("The sharing peer returned invalid updates: {error}"))?;
    if let Some(event) = result.events.last() {
        if let Ok(mut current) = service.joined.lock() {
            if let Some(current) = current
                .as_mut()
                .filter(|current| current.token == joined.token)
            {
                current.cursor = event.sequence;
            }
        }
    }
    for event in &result.events {
        if event.kind == "document" {
            if let Some(markdown) = event.payload["markdown"].as_str() {
                let _ = fs::write(&joined.shadow_sidecar_path, markdown);
            }
        }
    }
    Ok(result)
}

#[tauri::command]
async fn poll_collaboration(state: State<'_, AppState>) -> Result<CollaborationPollResult, String> {
    let service = state.collaboration.clone();
    tauri::async_runtime::spawn_blocking(move || poll_collaboration_service(&service))
        .await
        .map_err(|error| format!("The peer update task stopped unexpectedly: {error}"))?
}

fn publish_collaboration_event_service(
    service: &CollaborationService,
    kind: String,
    payload: serde_json::Value,
) -> Result<(), String> {
    if let Some(session) = service
        .hosted
        .read()
        .ok()
        .and_then(|session| session.clone())
    {
        publish_host_event(&session, service.client_id.clone(), kind, payload)?;
        return Ok(());
    }
    let joined = service
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .clone()
        .ok_or_else(|| "No shared session is active.".to_string())?;
    let response = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|error| format!("Could not prepare the peer connection: {error}"))?
        .post(format!(
            "{}/session/{}/event",
            joined.host_base_url, joined.token
        ))
        .json(&NetworkEventRequest {
            peer_id: joined.peer_id,
            kind,
            payload,
        })
        .send()
        .map_err(|_| "The sharing peer is unavailable on the local network.".to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err("The sharing peer rejected this update.".into())
    }
}

#[tauri::command]
async fn publish_collaboration_event(
    state: State<'_, AppState>,
    kind: String,
    payload: serde_json::Value,
) -> Result<(), String> {
    let service = state.collaboration.clone();
    tauri::async_runtime::spawn_blocking(move || {
        publish_collaboration_event_service(&service, kind, payload)
    })
    .await
    .map_err(|error| format!("The peer update task stopped unexpectedly: {error}"))?
}

#[tauri::command]
fn collaboration_status(
    state: State<'_, AppState>,
) -> Result<Option<CollaborationSessionInfo>, String> {
    if let Some(session) = state
        .collaboration
        .hosted
        .read()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .clone()
    {
        return Ok(Some(hosted_session_info(&state.collaboration, &session)));
    }
    let joined = state
        .collaboration
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .clone();
    Ok(joined.map(|joined| {
        let participants = vec![joined.display_name.clone()];
        CollaborationSessionInfo {
            mode: "guest".into(),
            code: joined.code,
            participant_count: 1,
            video_name: joined.video_name,
            display_name: joined.display_name,
            client_id: state.collaboration.client_id.clone(),
            participants,
        }
    }))
}

#[tauri::command]
fn stop_collaboration(state: State<'_, AppState>) -> Result<(), String> {
    // Disconnect relay if active (signals the relay listener thread to stop)
    if let Some(relay) = state
        .collaboration
        .relay
        .write()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .take()
    {
        relay.disconnect.store(true, Ordering::Relaxed);
    }
    if let Some(session) = state
        .collaboration
        .hosted
        .write()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .take()
    {
        let _ = state
            .collaboration
            .mdns
            .unregister(&session.service_fullname);
    }
    if let Some(joined) = state
        .collaboration
        .joined
        .lock()
        .map_err(|_| "The collaboration state is unavailable.".to_string())?
        .take()
    {
        let _ = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .and_then(|client| {
                client
                    .post(format!(
                        "{}/session/{}/leave",
                        joined.host_base_url, joined.token
                    ))
                    .json(&serde_json::json!({ "peerId": joined.peer_id }))
                    .send()
            });
    }
    Ok(())
}

#[tauri::command]
fn prepare_export_directory(
    video_path: String,
    parent_directory: String,
) -> Result<String, String> {
    let video = validate_video_path(&video_path)?;
    let parent = PathBuf::from(parent_directory);
    if !parent.is_dir() {
        return Err("Choose an existing export folder.".into());
    }
    let source = video
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Video");
    let base = format!("FrameNote - {}", safe_file_component(source, 70));
    let mut output = parent.join(&base);
    let mut suffix = 2;
    while output.exists() {
        output = parent.join(format!("{base} {suffix}"));
        suffix += 1;
    }
    fs::create_dir(&output)
        .map_err(|error| format!("Could not create {}: {error}", output.display()))?;
    Ok(output.to_string_lossy().into_owned())
}

#[tauri::command]
fn register_media_source(
    state: State<'_, AppState>,
    video_path: String,
) -> Result<MediaRegistration, String> {
    let video = validate_video_path(&video_path)?;
    sidecar_path(&video)?;
    let audio_tracks = probe_audio_tracks(&video);
    let frame_rate = probe_frame_rate(&video);
    let token = Uuid::new_v4().to_string();
    state
        .media
        .files
        .write()
        .map_err(|_| "The private media server is unavailable")?
        .insert(token.clone(), MediaSource::Local(video));
    Ok(MediaRegistration {
        url: format!("{}/media/{token}", state.media.base_url),
        mix_base_url: format!("{}/mix/{token}", state.media.base_url),
        audio_tracks,
        frame_rate,
    })
}

fn probe_frame_rate(video: &Path) -> Option<f64> {
    let ffprobe = find_executable("ffprobe")?;
    let video_arg = video.to_string_lossy().into_owned();
    let output = StdCommand::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=avg_frame_rate,r_frame_rate",
            "-of",
            "json",
            &video_arg,
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let json = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()?;
    let stream = json["streams"].as_array()?.first()?;
    ["avg_frame_rate", "r_frame_rate"]
        .into_iter()
        .filter_map(|field| parse_frame_rate(stream[field].as_str()?))
        .find(|rate| rate.is_finite() && *rate > 0.0)
}

fn parse_frame_rate(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.parse::<f64>().ok()?;
    let denominator = denominator.parse::<f64>().ok()?;
    (denominator != 0.0).then_some(numerator / denominator)
}

fn probe_audio_tracks(video: &Path) -> Vec<AudioTrackInfo> {
    let Some(ffprobe) = find_executable("ffprobe") else {
        return vec![];
    };
    let video_arg = video.to_string_lossy().into_owned();
    let output = StdCommand::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index,codec_name,channels:stream_tags=title,language,handler_name",
            "-of",
            "json",
            &video_arg,
        ])
        .stdin(Stdio::null())
        .output();
    let Ok(output) = output else {
        return vec![];
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return vec![];
    };
    json["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(position, stream)| {
            let stream_index = stream["index"].as_u64()? as u32;
            let tags = &stream["tags"];
            let title = tags["title"]
                .as_str()
                .or_else(|| tags["handler_name"].as_str())
                .filter(|value| !value.eq_ignore_ascii_case("soundhandler"));
            let language = tags["language"].as_str().map(str::to_string);
            let fallback = language
                .as_deref()
                .map(|language| format!("Track {} · {}", position + 1, language.to_uppercase()))
                .unwrap_or_else(|| format!("Track {}", position + 1));
            Some(AudioTrackInfo {
                stream_index,
                label: title.unwrap_or(&fallback).to_string(),
                language,
                codec: stream["codec_name"].as_str().unwrap_or("audio").to_string(),
                channels: stream["channels"].as_u64().map(|value| value as u32),
            })
        })
        .collect()
}

fn waveform_cache_key(video: &Path) -> Result<String, String> {
    let metadata = fs::metadata(video)
        .map_err(|error| format!("Could not inspect the video for waveform caching: {error}"))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .unwrap_or_default();
    let canonical = video.canonicalize().unwrap_or_else(|_| video.to_path_buf());
    let identity = format!(
        "{}\0{}\0{}\0{}\0{}",
        WAVEFORM_CACHE_VERSION,
        canonical.to_string_lossy(),
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos()
    );

    // Stable FNV-1a keeps cache filenames short without adding a crypto dependency.
    let hash = stable_fnv1a(&identity);
    Ok(format!("{hash:016x}.json"))
}

fn waveform_cache_is_fresh(modified: SystemTime, now: SystemTime) -> bool {
    now.duration_since(modified)
        .map(|age| age <= WAVEFORM_CACHE_TTL)
        .unwrap_or(true)
}

fn prune_waveform_cache(cache_dir: &Path) {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let is_fresh = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(|modified| waveform_cache_is_fresh(modified, now))
            .unwrap_or(false);
        if !is_fresh {
            let _ = fs::remove_file(path);
        }
    }
}

fn read_waveform_cache(path: &Path) -> Option<WaveformData> {
    let metadata = fs::metadata(path).ok()?;
    if !waveform_cache_is_fresh(metadata.modified().ok()?, SystemTime::now()) {
        let _ = fs::remove_file(path);
        return None;
    }
    let data = serde_json::from_slice::<WaveformData>(&fs::read(path).ok()?).ok()?;
    if !data.samples_per_second.is_finite()
        || data.samples_per_second <= 0.0
        || data.peaks.is_empty()
        || data.peaks.iter().any(|peak| !peak.is_finite())
    {
        let _ = fs::remove_file(path);
        return None;
    }
    Some(data)
}

fn write_waveform_cache(path: &Path, data: &WaveformData) {
    let Some(cache_dir) = path.parent() else {
        return;
    };
    if fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    let Ok(encoded) = serde_json::to_vec(data) else {
        return;
    };
    let temporary = cache_dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("waveform"),
        std::process::id()
    ));
    if fs::write(&temporary, encoded).is_ok() && fs::rename(&temporary, path).is_err() {
        let _ = fs::remove_file(&temporary);
    }
}

#[tauri::command]
async fn extract_waveform(
    app: tauri::AppHandle,
    video_path: String,
) -> Result<WaveformData, String> {
    const DECODE_RATE: usize = 400;
    const PEAK_RATE: usize = 100;

    let video = validate_video_path(&video_path)?;
    let cache_path = app.path().app_cache_dir().ok().and_then(|directory| {
        let cache_dir = directory.join("waveforms");
        if fs::create_dir_all(&cache_dir).is_err() {
            return None;
        }
        prune_waveform_cache(&cache_dir);
        waveform_cache_key(&video)
            .ok()
            .map(|key| cache_dir.join(key))
    });
    if let Some(data) = cache_path.as_deref().and_then(read_waveform_cache) {
        return Ok(data);
    }

    let ffmpeg = find_executable("ffmpeg")
        .ok_or_else(|| "FFmpeg is required to prepare the audio waveform.".to_string())?;
    let tracks = probe_audio_tracks(&video);
    if tracks.is_empty() {
        return Err("No audio track is available for the waveform.".into());
    }

    let mut filters = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            format!(
                "[0:{}]aresample={DECODE_RATE},aformat=sample_fmts=flt:channel_layouts=mono[a{index}]",
                track.stream_index
            )
        })
        .collect::<Vec<_>>();
    if tracks.len() == 1 {
        filters.push("[a0]anull[wave]".into());
    } else {
        let inputs = (0..tracks.len())
            .map(|index| format!("[a{index}]"))
            .collect::<String>();
        filters.push(format!(
            "{inputs}amix=inputs={}:duration=longest:normalize=1:dropout_transition=0[wave]",
            tracks.len()
        ));
    }

    let video_arg = video.to_string_lossy().into_owned();
    let filter_graph = filters.join(";");
    let decode_rate = DECODE_RATE.to_string();
    let output = Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            &video_arg,
            "-filter_complex",
            &filter_graph,
            "-map",
            "[wave]",
            "-vn",
            "-ac",
            "1",
            "-ar",
            &decode_rate,
            "-c:a",
            "pcm_f32le",
            "-f",
            "f32le",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("Could not prepare the audio waveform: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" ");
        return Err(format!("Could not prepare the audio waveform: {detail}"));
    }

    let raw = output
        .stdout
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).abs())
        .collect::<Vec<_>>();
    let samples_per_peak = DECODE_RATE / PEAK_RATE;
    let mut peaks = raw
        .chunks(samples_per_peak)
        .map(|samples| samples.iter().copied().fold(0.0_f32, f32::max))
        .collect::<Vec<_>>();
    if peaks.is_empty() {
        return Err("The selected video has no decodable audio waveform.".into());
    }

    let mut distribution = peaks.clone();
    distribution.sort_by(f32::total_cmp);
    let reference =
        distribution[((distribution.len() - 1) as f64 * 0.98).round() as usize].max(0.025);
    for peak in &mut peaks {
        *peak = (*peak / reference).min(1.0);
    }
    let data = WaveformData {
        samples_per_second: PEAK_RATE as f64,
        peaks,
    };
    if let Some(path) = cache_path.as_deref() {
        write_waveform_cache(path, &data);
    }
    Ok(data)
}

#[tauri::command]
fn load_video(video_path: String) -> Result<SidecarDocument, String> {
    let video = validate_video_path(&video_path)?;
    let (sidecar, markdown) = read_or_create_markdown(&video)?;
    let supports_embedded_chapters = video
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "mp4" | "m4v" | "mov"));
    let fingerprint = supports_embedded_chapters
        .then(|| embedded_chapter_fingerprint(&video).ok())
        .flatten();
    let imported = fingerprint.as_deref().and_then(|fingerprint| {
        if markdown
            .lines()
            .any(|line| line.trim().starts_with(EMBEDDED_CHAPTER_MARKER))
        {
            return None;
        }
        probe_embedded_chapters(&video)
            .ok()
            .and_then(|chapters| merge_embedded_chapters(&markdown, &chapters, fingerprint))
    });
    if let Some(updated) = imported {
        fs::write(&sidecar, &updated).map_err(|error| {
            format!(
                "Could not import embedded markers into {}: {error}",
                sidecar.display()
            )
        })?;
        return document(&video, sidecar, updated);
    }
    document(&video, sidecar, markdown)
}

#[tauri::command]
fn read_sidecar(video_path: String) -> Result<SidecarDocument, String> {
    load_video(video_path)
}

#[tauri::command]
fn save_markdown(video_path: String, markdown: String) -> Result<SidecarDocument, String> {
    let video = validate_video_path(&video_path)?;
    write_markdown(&video, &markdown)
}

#[tauri::command]
fn save_playback_position(video_path: String, position_seconds: f64) -> Result<(), String> {
    let video = validate_video_path(&video_path)?;
    let (sidecar, markdown) = read_or_create_markdown(&video)?;
    let updated = with_playback_position(&markdown, position_seconds);
    fs::write(&sidecar, updated).map_err(|error| {
        format!(
            "Could not save playback position to {}: {error}",
            sidecar.display()
        )
    })
}

#[tauri::command]
fn add_bookmark(video_path: String, timestamp_seconds: f64) -> Result<AddBookmarkResult, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let id = Uuid::new_v4().to_string();
    let start = if timestamp_seconds.is_finite() {
        timestamp_seconds.max(0.0)
    } else {
        0.0
    };
    let line = format!(
        "- [{}] New mark <!-- framenote:bookmark:{id} start={start:.3} -->",
        format_precise_timestamp(start)
    );
    let updated = append_to_section(&markdown, BOOKMARK_HEADING, &line);
    Ok(AddBookmarkResult {
        document: write_markdown(&video, &updated)?,
        entry_id: id,
    })
}

#[tauri::command]
fn add_bookmark_range(
    video_path: String,
    start_seconds: f64,
    end_seconds: f64,
) -> Result<AddBookmarkResult, String> {
    let video = validate_video_path(&video_path)?;
    if !start_seconds.is_finite() || !end_seconds.is_finite() {
        return Err("The selected mark range is not valid.".into());
    }
    let start = start_seconds.max(0.0);
    if end_seconds <= start {
        return Err("Drag across the waveform to select a mark range.".into());
    }
    let end = end_seconds;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let id = Uuid::new_v4().to_string();
    let line = format!(
        "- [{}–{}] New mark <!-- framenote:bookmark:{id} start={start:.3} end={end:.3} -->",
        format_precise_timestamp(start),
        format_precise_timestamp(end)
    );
    let updated = append_to_section(&markdown, BOOKMARK_HEADING, &line);
    Ok(AddBookmarkResult {
        document: write_markdown(&video, &updated)?,
        entry_id: id,
    })
}

fn open_bookmark_start(line: &str) -> Option<f64> {
    if !line.contains("framenote:bookmark:")
        || line.contains("source=embedded-chapter")
        || marker_number(line, "end").is_some()
    {
        return None;
    }
    let bracket = line.split_once('[')?.1.split_once(']')?.0;
    if bracket.contains(['–', '—']) {
        return None;
    }
    marker_number(line, "start").or_else(|| parse_subtitle_timestamp(bracket.trim()))
}

#[tauri::command]
fn end_bookmark(video_path: String, timestamp_seconds: f64) -> Result<AddBookmarkResult, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let end = if timestamp_seconds.is_finite() {
        timestamp_seconds.max(0.0)
    } else {
        return Err("The current playback time is not valid.".into());
    };
    let mut lines = markdown.lines().map(str::to_string).collect::<Vec<_>>();
    let Some((index, start)) = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| open_bookmark_start(line).map(|start| (index, start)))
        .filter(|(_, start)| *start <= end)
        .max_by(|(_, left), (_, right)| left.total_cmp(right))
    else {
        return Err("No open mark starts before the current playback time.".into());
    };
    if end <= start {
        return Err("Move past the mark start before ending it.".into());
    }
    let line = &lines[index];
    let marker = "framenote:bookmark:";
    let id = line
        .split_once(marker)
        .map(|(_, value)| {
            value
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '-')
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "The open mark has an invalid identifier.".to_string())?;
    let close = line
        .find(']')
        .ok_or_else(|| "The open mark has an invalid timestamp.".to_string())?;
    let comment = line.find("<!--").unwrap_or(line.len());
    let text = sanitize_entry_text(line[close + 1..comment].trim());
    lines[index] = format!(
        "- [{}–{}] {text} <!-- framenote:bookmark:{id} start={start:.3} end={end:.3} -->",
        format_precise_timestamp(start),
        format_precise_timestamp(end)
    );
    Ok(AddBookmarkResult {
        document: write_markdown(&video, &format!("{}\n", lines.join("\n")))?,
        entry_id: id,
    })
}

fn subtitle_timing(start_seconds: f64, end_seconds: f64) -> Result<(f64, f64), String> {
    if !start_seconds.is_finite() || !end_seconds.is_finite() {
        return Err("Enter valid subtitle start and end times.".into());
    }
    let start = start_seconds.max(0.0);
    if end_seconds <= start {
        return Err("Subtitle end time must be after its start time.".into());
    }
    Ok((start, end_seconds))
}

fn subtitle_line(
    id: &str,
    start_seconds: f64,
    end_seconds: f64,
    text: &str,
    speaker: &str,
    language: &str,
) -> Result<String, String> {
    let (start, end) = subtitle_timing(start_seconds, end_seconds)?;
    Ok(format!(
        "- [{}–{}] {} <!-- framenote:subtitle:{id} start={start:.3} end={end:.3} speaker=\"{}\" language=\"{}\" -->",
        format_precise_timestamp(start),
        format_precise_timestamp(end),
        sanitize_entry_text(text),
        sanitize_metadata(speaker, "Unknown"),
        sanitize_metadata(language, "unknown")
    ))
}

#[tauri::command]
fn add_subtitle(
    video_path: String,
    start_seconds: f64,
    end_seconds: f64,
) -> Result<AddBookmarkResult, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let id = Uuid::new_v4().to_string();
    let line = subtitle_line(
        &id,
        start_seconds,
        end_seconds,
        "New subtitle",
        "Unknown",
        "unknown",
    )?;
    let updated = append_to_section(&markdown, SUBTITLE_HEADING, &line);
    Ok(AddBookmarkResult {
        document: write_markdown(&video, &updated)?,
        entry_id: id,
    })
}

#[tauri::command]
fn update_subtitle(
    video_path: String,
    entry_id: String,
    start_seconds: f64,
    end_seconds: f64,
    text: String,
    speaker: String,
    language: String,
) -> Result<SidecarDocument, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let marker = format!("framenote:subtitle:{entry_id}");
    let replacement = subtitle_line(
        &entry_id,
        start_seconds,
        end_seconds,
        &text,
        &speaker,
        &language,
    )?;
    let mut found = false;
    let lines = markdown
        .lines()
        .map(|line| {
            if line.contains(&marker) {
                found = true;
                replacement.clone()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>();
    if !found {
        return Err(
            "That subtitle was changed outside FrameNote. Reload the Markdown and try again."
                .into(),
        );
    }
    write_markdown(&video, &format!("{}\n", lines.join("\n")))
}

#[tauri::command]
fn update_entry(
    video_path: String,
    entry_id: String,
    text: String,
) -> Result<SidecarDocument, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let marker = format!("framenote:bookmark:{entry_id}");
    let ai_marker = format!("framenote:ai:{entry_id}");
    let subtitle_marker = format!("framenote:subtitle:{entry_id}");
    let mut found = false;
    let cleaned = sanitize_entry_text(&text);
    let lines = markdown
        .lines()
        .map(|line| {
            if line.contains(&marker)
                || line.contains(&ai_marker)
                || line.contains(&subtitle_marker)
            {
                found = true;
                if let (Some(close), Some(comment)) = (line.find(']'), line.find("<!--")) {
                    return format!("{} {} {}", &line[..=close], cleaned, line[comment..].trim());
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>();
    if !found {
        return Err(
            "That entry was changed outside FrameNote. Reload the Markdown and try again.".into(),
        );
    }
    write_markdown(&video, &format!("{}\n", lines.join("\n")))
}

#[tauri::command]
fn delete_entry(video_path: String, entry_id: String) -> Result<SidecarDocument, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let bookmark_marker = format!("framenote:bookmark:{entry_id}");
    let ai_marker = format!("framenote:ai:{entry_id}");
    let subtitle_marker = format!("framenote:subtitle:{entry_id}");
    let updated = markdown
        .lines()
        .filter(|line| {
            !line.contains(&bookmark_marker)
                && !line.contains(&ai_marker)
                && !line.contains(&subtitle_marker)
        })
        .collect::<Vec<_>>()
        .join("\n");
    write_markdown(&video, &updated)
}

#[tauri::command]
fn append_ai_entry(
    video_path: String,
    start_seconds: f64,
    end_seconds: f64,
    summary: String,
) -> Result<SidecarDocument, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let range_start = if start_seconds.is_finite() {
        start_seconds.max(0.0)
    } else {
        0.0
    };
    let range_end = if end_seconds.is_finite() {
        end_seconds.max(range_start)
    } else {
        range_start
    };
    let id = Uuid::new_v4().to_string();
    let line = format!(
        "- [{}–{}] {} <!-- framenote:ai:{id} start={:.3} end={:.3} -->",
        format_timestamp(start_seconds),
        format_timestamp(end_seconds),
        sanitize_entry_text(&summary),
        range_start,
        range_end
    );
    let updated = append_to_section(&markdown, AI_HEADING, &line);
    write_markdown(&video, &updated)
}

#[tauri::command]
fn append_analysis_result(
    video_path: String,
    start_seconds: f64,
    end_seconds: f64,
    summary: String,
    transcript_cues: Vec<TranscriptCue>,
    transcript_complete: bool,
) -> Result<SidecarDocument, String> {
    let video = validate_video_path(&video_path)?;
    let (_, markdown) = read_or_create_markdown(&video)?;
    let range_start = if start_seconds.is_finite() {
        start_seconds.max(0.0)
    } else {
        0.0
    };
    let range_end = if end_seconds.is_finite() {
        end_seconds.max(range_start)
    } else {
        range_start
    };
    let mut lines = markdown.lines().map(str::to_string).collect::<Vec<_>>();
    let existing_ai = lines
        .iter()
        .position(|line| ai_range_matches(line, range_start, range_end));
    let mut updated = if let Some(index) = existing_ai {
        if transcript_complete && !lines[index].contains("transcript=complete") {
            lines[index] = lines[index].replacen("-->", "transcript=complete -->", 1);
        }
        format!("{}\n", lines.join("\n"))
    } else {
        let id = Uuid::new_v4().to_string();
        let transcript_marker = if transcript_complete {
            " transcript=complete"
        } else {
            ""
        };
        let ai_line = format!(
            "- [{}–{}] {} <!-- framenote:ai:{id} start={:.3} end={:.3}{transcript_marker} -->",
            format_timestamp(range_start),
            format_timestamp(range_end),
            sanitize_entry_text(&summary),
            range_start,
            range_end
        );
        append_to_section(&markdown, AI_HEADING, &ai_line)
    };
    for cue in transcript_cues.into_iter().take(500) {
        if !cue.start_seconds.is_finite() || !cue.end_seconds.is_finite() {
            continue;
        }
        let cue_start = cue.start_seconds.clamp(range_start, range_end);
        let cue_end = cue.end_seconds.clamp(cue_start, range_end);
        let text = sanitize_entry_text(&cue.text);
        if cue_end <= cue_start || text == "Untitled note" {
            continue;
        }
        let cue_id = Uuid::new_v4().to_string();
        let speaker = sanitize_metadata(&cue.speaker, "Speaker");
        let language = sanitize_metadata(&cue.language, "unknown");
        let line = format!(
            "- [{}–{}] {} <!-- framenote:subtitle:{cue_id} start={cue_start:.3} end={cue_end:.3} speaker=\"{speaker}\" language=\"{language}\" -->",
            format_timestamp(cue_start),
            format_timestamp(cue_end),
            text
        );
        updated = append_to_section(&updated, SUBTITLE_HEADING, &line);
    }
    write_markdown(&video, &updated)
}

fn normalize_ollama_url(value: &str) -> Result<url::Url, String> {
    let mut url = url::Url::parse(value.trim())
        .map_err(|_| "Enter a valid Ollama URL, for example http://127.0.0.1:11434".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Ollama must use an http:// or https:// URL.".into());
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn ollama_endpoint(base: &url::Url, route: &str) -> Result<url::Url, String> {
    let path = base.path().trim_end_matches('/');
    let relative = if path.ends_with("/api") {
        route.to_string()
    } else {
        format!("api/{route}")
    };
    base.join(&relative)
        .map_err(|error| format!("Invalid Ollama URL: {error}"))
}

fn model_for_endpoint(base: &url::Url, model: &str) -> String {
    let requested = model.trim();
    if base
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case("ollama.com"))
    {
        requested
            .strip_suffix("-cloud")
            .or_else(|| requested.strip_suffix(":cloud"))
            .unwrap_or(requested)
            .to_string()
    } else {
        requested.to_string()
    }
}

#[tauri::command]
async fn check_ollama(
    ollama_url: String,
    model: String,
    api_key: Option<String>,
) -> Result<OllamaStatus, String> {
    let base = normalize_ollama_url(&ollama_url)?;
    let endpoint = ollama_endpoint(&base, "tags")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client.get(endpoint);
    if let Some(key) = api_key.as_deref().filter(|value| !value.trim().is_empty()) {
        request = request.bearer_auth(key.trim());
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => {
            return Ok(OllamaStatus {
                available: false,
                model_available: false,
                message: "Ollama is offline. Playback and notes still work normally.".into(),
                models: vec![],
            })
        }
    };
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(OllamaStatus {
            available: false,
            model_available: false,
            message: "Ollama rejected the API key. Create or replace it in ollama.com settings."
                .into(),
            models: vec![],
        });
    }
    if !response.status().is_success() {
        return Ok(OllamaStatus {
            available: false,
            model_available: false,
            message: format!("Ollama returned HTTP {}.", response.status()),
            models: vec![],
        });
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Ollama returned an unreadable response: {error}"))?;
    let mut models = json["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["name"].as_str().or_else(|| item["model"].as_str()))
        .map(str::to_string)
        .collect::<Vec<_>>();
    models.sort();
    let requested = model.trim();
    let api_model = model_for_endpoint(&base, requested);
    let comparable_model = api_model.trim_end_matches(":latest");
    let model_available = models
        .iter()
        .any(|candidate| candidate.trim_end_matches(":latest") == comparable_model);
    Ok(OllamaStatus {
        available: true,
        model_available,
        message: if model_available {
            if requested == api_model {
                format!("Connected · {requested} is ready")
            } else {
                format!("Connected · {requested} maps to {api_model} for direct Cloud")
            }
        } else {
            format!("Connected, but {api_model} is not available at this endpoint")
        },
        models,
    })
}

fn gemini_model_name(value: &str) -> Result<&str, String> {
    let model = value.trim().strip_prefix("models/").unwrap_or(value.trim());
    if model.is_empty()
        || !model.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        return Err("Enter a valid Gemini model name, for example gemini-3.6-flash.".into());
    }
    Ok(model)
}

#[tauri::command]
async fn check_gemini(model: String, api_key: String) -> Result<OllamaStatus, String> {
    if api_key.trim().is_empty() {
        return Ok(OllamaStatus {
            available: false,
            model_available: false,
            message: "Enter a Gemini API key to connect.".into(),
            models: vec![],
        });
    }
    let model = gemini_model_name(&model)?;
    let endpoint = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}");
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|error| error.to_string())?
        .get(endpoint)
        .header("x-goog-api-key", api_key.trim())
        .send()
        .await;
    let Ok(response) = response else {
        return Ok(OllamaStatus {
            available: false,
            model_available: false,
            message: "Could not reach the Gemini API. Playback and notes still work normally."
                .into(),
            models: vec![],
        });
    };
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&detail)
            .ok()
            .and_then(|body| body["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("Gemini returned HTTP {status}."));
        return Ok(OllamaStatus {
            available: status.is_server_error(),
            model_available: false,
            message,
            models: vec![],
        });
    }
    Ok(OllamaStatus {
        available: true,
        model_available: true,
        message: format!("Connected · {model} accepts native audio and vision"),
        models: vec![model.to_string()],
    })
}

#[tauri::command]
fn begin_analysis(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let mut jobs = state
        .jobs
        .lock()
        .map_err(|_| "Analysis state is unavailable")?;
    jobs.insert(job_id, CancellationToken::new());
    Ok(())
}

#[tauri::command]
fn cancel_analysis(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let jobs = state
        .jobs
        .lock()
        .map_err(|_| "Analysis state is unavailable")?;
    if let Some(token) = jobs.get(&job_id) {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
fn finish_analysis(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let mut jobs = state
        .jobs
        .lock()
        .map_err(|_| "Analysis state is unavailable")?;
    jobs.remove(&job_id);
    Ok(())
}

#[tauri::command]
fn begin_export(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let mut jobs = state
        .jobs
        .lock()
        .map_err(|_| "Export state is unavailable")?;
    jobs.insert(job_id, CancellationToken::new());
    Ok(())
}

#[tauri::command]
fn cancel_export(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let jobs = state
        .jobs
        .lock()
        .map_err(|_| "Export state is unavailable")?;
    if let Some(token) = jobs.get(&job_id) {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
fn finish_export(state: State<'_, AppState>, job_id: String) -> Result<(), String> {
    let mut jobs = state
        .jobs
        .lock()
        .map_err(|_| "Export state is unavailable")?;
    jobs.remove(&job_id);
    Ok(())
}

fn safe_file_component(value: &str, limit: usize) -> String {
    let cleaned = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = cleaned.trim_matches(['.', ' ']);
    let result = if trimmed.is_empty() { "mark" } else { trimmed };
    result.chars().take(limit).collect()
}

fn srt_timestamp(seconds: f64) -> String {
    let total = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = total / 3_600_000;
    let minutes = (total % 3_600_000) / 60_000;
    let seconds = (total % 60_000) / 1000;
    let milliseconds = total % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02},{milliseconds:03}")
}

fn marker_text(line: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let value = line.split_once(&marker)?.1.split_once('"')?.0;
    Some(value.to_string())
}

fn subtitle_cues_from_markdown(markdown: &str) -> Vec<TranscriptCue> {
    let mut cues = markdown
        .lines()
        .filter(|line| line.contains("framenote:subtitle:"))
        .filter_map(|line| {
            let start_seconds = marker_number(line, "start")?;
            let end_seconds = marker_number(line, "end")?;
            let close = line.find(']')?;
            let comment = line.find("<!--").unwrap_or(line.len());
            (end_seconds > start_seconds && comment > close).then(|| TranscriptCue {
                start_seconds,
                end_seconds,
                text: sanitize_entry_text(line[close + 1..comment].trim()),
                speaker: marker_text(line, "speaker").unwrap_or_else(|| "Unknown".into()),
                language: marker_text(line, "language").unwrap_or_else(|| "unknown".into()),
            })
        })
        .collect::<Vec<_>>();
    cues.sort_by(|left, right| left.start_seconds.total_cmp(&right.start_seconds));
    cues
}

fn clip_srt(cues: &[TranscriptCue], start: f64, end: f64) -> String {
    cues.iter()
        .filter(|cue| cue.start_seconds < end && cue.end_seconds > start)
        .enumerate()
        .map(|(index, cue)| {
            let relative_start = cue.start_seconds.max(start) - start;
            let relative_end = cue.end_seconds.min(end) - start;
            let speaker = cue.speaker.trim();
            let text = if speaker.is_empty() || speaker.eq_ignore_ascii_case("unknown") {
                cue.text.clone()
            } else {
                format!("{speaker}: {}", cue.text)
            };
            format!(
                "{}\n{} --> {}\n{}\n\n",
                index + 1,
                srt_timestamp(relative_start),
                srt_timestamp(relative_end),
                text
            )
        })
        .collect()
}

fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn export_extension(preset: &str) -> Result<&'static str, String> {
    match preset {
        "resolve" => Ok("mov"),
        "mp4" => Ok("mp4"),
        "source" => Ok("mkv"),
        _ => Err("Choose a supported export format.".into()),
    }
}

#[tauri::command]
async fn export_mark_clip(
    state: State<'_, AppState>,
    request: ExportClipRequest,
) -> Result<ExportClipResult, String> {
    let token = {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| "Export state is unavailable")?;
        jobs.get(&request.job_id)
            .cloned()
            .ok_or_else(|| "The export job is no longer active.".to_string())?
    };
    if token.is_cancelled() {
        return Err(CANCELLED.into());
    }
    let video = validate_video_path(&request.video_path)?;
    let output_directory = PathBuf::from(&request.output_directory);
    if !output_directory.is_dir() {
        return Err("The export folder is no longer available.".into());
    }
    if !request.start_seconds.is_finite()
        || !request.end_seconds.is_finite()
        || request.end_seconds <= request.start_seconds
    {
        return Err("This mark does not have a valid start and end time.".into());
    }
    let ffmpeg = find_executable("ffmpeg").ok_or_else(|| {
        "FFmpeg was not found. Install FFmpeg before exporting clips.".to_string()
    })?;
    let extension = export_extension(&request.preset)?;
    let source = video
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("video");
    let stem = format!(
        "{}_{:03}_{}",
        safe_file_component(source, 48),
        request.clip_index + 1,
        safe_file_component(&request.label, 56)
    );
    let file_name = format!("{stem}.{extension}");
    let subtitle_file_name = format!("{stem}.srt");
    let output_path = output_directory.join(&file_name);
    let subtitle_path = output_directory.join(&subtitle_file_name);
    let (_, markdown) = read_or_create_markdown(&video)?;
    let cues = subtitle_cues_from_markdown(&markdown);
    fs::write(
        &subtitle_path,
        clip_srt(&cues, request.start_seconds, request.end_seconds),
    )
    .map_err(|error| format!("Could not write {}: {error}", subtitle_path.display()))?;

    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{:.3}", request.start_seconds.max(0.0)),
        "-i".into(),
        video.to_string_lossy().into_owned(),
        "-t".into(),
        format!("{:.3}", request.end_seconds - request.start_seconds),
        "-map".into(),
        "0:v:0".into(),
    ];
    if let Some(indexes) = request
        .audio_stream_indexes
        .as_ref()
        .filter(|indexes| !indexes.is_empty())
    {
        for index in indexes {
            args.extend(["-map".into(), format!("0:{index}?")]);
        }
    } else {
        args.extend(["-map".into(), "0:a?".into()]);
    }
    args.extend([
        "-map_metadata".into(),
        "0".into(),
        "-map_chapters".into(),
        "-1".into(),
        "-sn".into(),
        "-dn".into(),
    ]);
    match request.preset.as_str() {
        "resolve" => args.extend([
            "-c:v".into(),
            "prores_ks".into(),
            "-profile:v".into(),
            "2".into(),
            "-pix_fmt".into(),
            "yuv422p10le".into(),
            "-c:a".into(),
            "pcm_s24le".into(),
        ]),
        "mp4" => args.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "medium".into(),
            "-crf".into(),
            "18".into(),
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            "192k".into(),
            "-movflags".into(),
            "+faststart".into(),
        ]),
        "source" => args.extend([
            "-c".into(),
            "copy".into(),
            "-avoid_negative_ts".into(),
            "make_zero".into(),
        ]),
        _ => return Err("Choose a supported export format.".into()),
    }
    args.extend(["-y".into(), output_path.to_string_lossy().into_owned()]);
    let output = run_process(&ffmpeg, &args, &token).await;
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            let _ = fs::remove_file(&output_path);
            let _ = fs::remove_file(&subtitle_path);
            return Err(error);
        }
    };
    if !output.status.success() {
        let _ = fs::remove_file(&output_path);
        let _ = fs::remove_file(&subtitle_path);
        let detail = String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(" ");
        return Err(format!(
            "FFmpeg could not export mark {}: {detail}",
            request.clip_index + 1
        ));
    }
    Ok(ExportClipResult {
        file_name,
        subtitle_file_name,
        video_path: output_path.to_string_lossy().into_owned(),
        subtitle_path: subtitle_path.to_string_lossy().into_owned(),
    })
}

#[tauri::command]
fn write_export_manifest(
    video_path: String,
    output_directory: String,
    preset: String,
    audio_description: String,
    clips: Vec<ExportManifestClip>,
) -> Result<String, String> {
    let video = validate_video_path(&video_path)?;
    let output = PathBuf::from(output_directory);
    if !output.is_dir() {
        return Err("The export folder is no longer available.".into());
    }
    let mut manifest = String::from("clip,file,subtitles,source_start,source_end,duration,label\n");
    for (index, clip) in clips.iter().enumerate() {
        manifest.push_str(&format!(
            "{},{},{},{:.3},{:.3},{:.3},{}\n",
            index + 1,
            csv_field(&clip.file_name),
            csv_field(&clip.subtitle_file_name),
            clip.start_seconds,
            clip.end_seconds,
            (clip.end_seconds - clip.start_seconds).max(0.0),
            csv_field(&clip.label)
        ));
    }
    fs::write(output.join("framenote_manifest.csv"), manifest)
        .map_err(|error| format!("Could not write export manifest: {error}"))?;

    let (_, markdown) = read_or_create_markdown(&video)?;
    let cues = subtitle_cues_from_markdown(&markdown);
    let mut transcript = String::from(
        "clip,file,relative_start,relative_end,source_start,source_end,speaker,language,text\n",
    );
    for (index, clip) in clips.iter().enumerate() {
        for cue in cues.iter().filter(|cue| {
            cue.start_seconds < clip.end_seconds && cue.end_seconds > clip.start_seconds
        }) {
            transcript.push_str(&format!(
                "{},{},{:.3},{:.3},{:.3},{:.3},{},{},{}\n",
                index + 1,
                csv_field(&clip.file_name),
                cue.start_seconds.max(clip.start_seconds) - clip.start_seconds,
                cue.end_seconds.min(clip.end_seconds) - clip.start_seconds,
                cue.start_seconds.max(clip.start_seconds),
                cue.end_seconds.min(clip.end_seconds),
                csv_field(&cue.speaker),
                csv_field(&cue.language),
                csv_field(&cue.text)
            ));
        }
    }
    fs::write(output.join("framenote_subtitles.csv"), transcript)
        .map_err(|error| format!("Could not write subtitle manifest: {error}"))?;
    let source_name = video
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("video");
    let readme = format!(
        "FrameNote rough-cut export\n\nSource: {source_name}\nPreset: {preset}\nAudio: {audio_description}\n\nEach completed mark is exported as a separate media clip. Every matching .srt file uses clip-relative timestamps and prefixes distinguishable speakers. Import the media files into DaVinci Resolve's Media Pool, then import the matching SRT files as subtitles. framenote_manifest.csv retains source in/out points; framenote_subtitles.csv retains source and relative timestamps, speaker labels, language codes, and verbatim text.\n"
    );
    fs::write(output.join("README.txt"), readme)
        .map_err(|error| format!("Could not write export instructions: {error}"))?;
    Ok(output.to_string_lossy().into_owned())
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let mut candidates = vec![
        PathBuf::from(format!("/opt/homebrew/bin/{name}")),
        PathBuf::from(format!("/usr/local/bin/{name}")),
        PathBuf::from(format!("/usr/bin/{name}")),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|folder| folder.join(name)));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

async fn run_process(
    executable: &Path,
    args: &[String],
    token: &CancellationToken,
) -> Result<std::process::Output, String> {
    if token.is_cancelled() {
        return Err(CANCELLED.into());
    }
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("Could not run {}: {error}", executable.display()))?;
    tokio::select! {
        _ = token.cancelled() => Err(CANCELLED.into()),
        output = child.wait_with_output() => output.map_err(|error| format!("{} failed: {error}", executable.display())),
    }
}

fn parse_subtitle_timestamp(value: &str) -> Option<f64> {
    let cleaned = value.trim().replace(',', ".");
    let pieces = cleaned.split(':').collect::<Vec<_>>();
    if pieces.len() != 3 {
        return None;
    }
    Some(
        pieces[0].parse::<f64>().ok()? * 3600.0
            + pieces[1].parse::<f64>().ok()? * 60.0
            + pieces[2].parse::<f64>().ok()?,
    )
}

fn companion_transcript(video: &Path, start: f64, end: f64) -> Option<String> {
    ["srt", "vtt"].iter().find_map(|extension| {
        let path = video.with_extension(extension);
        let source = fs::read_to_string(&path).ok()?;
        let normalized = source.replace("\r\n", "\n");
        let mut excerpts = vec![];
        for block in normalized.split("\n\n") {
            let lines = block.lines().collect::<Vec<_>>();
            let Some(time_index) = lines.iter().position(|line| line.contains("-->")) else {
                continue;
            };
            let times = lines[time_index].split("-->").collect::<Vec<_>>();
            if times.len() != 2 {
                continue;
            }
            let cue_start = parse_subtitle_timestamp(times[0]);
            let cue_end = parse_subtitle_timestamp(times[1].split_whitespace().next().unwrap_or(""));
            if matches!((cue_start, cue_end), (Some(cue_start), Some(cue_end)) if cue_end >= start && cue_start <= end) {
                let text = lines[time_index + 1..].join(" ");
                if !text.trim().is_empty() {
                    excerpts.push(text);
                }
            }
        }
        (!excerpts.is_empty()).then(|| excerpts.join(" "))
    })
}

async fn extract_frames(
    ffmpeg: &Path,
    video: &Path,
    start: f64,
    end: f64,
    frame_count: usize,
    folder: &Path,
    token: &CancellationToken,
) -> Vec<String> {
    let duration = (end - start).max(1.0);
    let frame_count = frame_count.clamp(2, 8);
    let mut images = vec![];
    for index in 0..frame_count {
        if token.is_cancelled() {
            break;
        }
        let output_path = folder.join(format!("frame-{index}.jpg"));
        let ratio = index as f64 / (frame_count - 1) as f64 * 0.92;
        let at = start + duration * ratio;
        let args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-ss".into(),
            format!("{at:.3}"),
            "-i".into(),
            video.to_string_lossy().into_owned(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            "scale='min(1024,iw)':-2".into(),
            "-q:v".into(),
            "4".into(),
            "-y".into(),
            output_path.to_string_lossy().into_owned(),
        ];
        if run_process(ffmpeg, &args, token)
            .await
            .is_ok_and(|result| result.status.success())
        {
            if let Ok(bytes) = fs::read(output_path) {
                images.push(BASE64.encode(bytes));
            }
        }
    }
    images
}

async fn extract_audio(
    ffmpeg: &Path,
    video: &Path,
    start: f64,
    end: f64,
    folder: &Path,
    token: &CancellationToken,
) -> Option<String> {
    let tracks = probe_audio_tracks(video);
    if tracks.is_empty() {
        return None;
    }
    let output_path = folder.join("chunk-audio.aac");
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{start:.3}"),
        "-t".into(),
        format!("{:.3}", (end - start).max(1.0)),
        "-i".into(),
        video.to_string_lossy().into_owned(),
    ];
    if tracks.len() == 1 {
        args.extend(["-map".into(), format!("0:{}", tracks[0].stream_index)]);
    } else {
        let inputs = tracks
            .iter()
            .map(|track| format!("[0:{}]", track.stream_index))
            .collect::<String>();
        args.extend([
            "-filter_complex".into(),
            format!(
                "{inputs}amix=inputs={}:duration=longest:normalize=1:dropout_transition=0[chunk_audio]",
                tracks.len()
            ),
            "-map".into(),
            "[chunk_audio]".into(),
        ]);
    }
    args.extend([
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "16000".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "64k".into(),
        "-f".into(),
        "adts".into(),
        "-y".into(),
        output_path.to_string_lossy().into_owned(),
    ]);
    let output = run_process(ffmpeg, &args, token).await.ok()?;
    if !output.status.success() {
        return None;
    }
    fs::read(output_path).ok().map(|bytes| BASE64.encode(bytes))
}

fn gemini_generation_config() -> serde_json::Value {
    serde_json::json!({
        "maxOutputTokens": 8192,
        "thinkingConfig": { "thinkingLevel": "minimal" },
        "responseFormat": {
            "text": {
                "mimeType": "APPLICATION_JSON",
                "schema": {
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "A concrete English timeline summary of the chunk."
                        },
                        "transcript": {
                            "type": "array",
                            "description": "Verbatim spoken-language subtitle cues. Empty when no intelligible speech is audible.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "start": { "type": "number", "minimum": 0, "description": "Cue start in seconds relative to the beginning of this audio chunk." },
                                    "end": { "type": "number", "minimum": 0, "description": "Cue end in seconds relative to the beginning of this audio chunk." },
                                    "text": { "type": "string", "description": "Exact words as spoken, kept in the original language without translation or cleanup." },
                                    "speaker": { "type": "string", "description": "Stable speaker label such as Speaker 1, or Unknown when indistinguishable." },
                                    "language": { "type": "string", "description": "BCP-47 language code such as sk or en; use mixed for a code-switched cue." }
                                },
                                "required": ["start", "end", "text", "speaker", "language"]
                            }
                        }
                    },
                    "required": ["summary", "transcript"]
                }
            }
        }
    })
}

fn gemini_error_message(detail: &str) -> String {
    let Ok(body) = serde_json::from_str::<serde_json::Value>(detail) else {
        return detail.chars().take(1200).collect();
    };
    let error = &body["error"];
    let mut messages = error["message"]
        .as_str()
        .map(str::to_string)
        .into_iter()
        .collect::<Vec<_>>();
    for violation in error["details"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|detail| detail["fieldViolations"].as_array().into_iter().flatten())
    {
        let field = violation["field"].as_str().unwrap_or("request");
        let description = violation["description"]
            .as_str()
            .or_else(|| violation["reason"].as_str())
            .unwrap_or("invalid value");
        messages.push(format!("{field}: {description}"));
    }
    if messages.is_empty() {
        detail.chars().take(1200).collect()
    } else {
        messages.join(" · ")
    }
}

async fn analyze_with_gemini(
    model: &str,
    api_key: &str,
    prompt: &str,
    images: &[String],
    audio: Option<&str>,
    chunk_duration: f64,
    token: &CancellationToken,
) -> Result<GeminiAnalysis, String> {
    if api_key.trim().is_empty() {
        return Err("Enter a Gemini API key before starting native audio analysis.".into());
    }
    let model = gemini_model_name(model)?;
    let endpoint =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    let mut parts = Vec::new();
    if let Some(audio) = audio {
        parts.push(serde_json::json!({
            "inline_data": { "mime_type": "audio/aac", "data": audio }
        }));
    }
    parts.extend(images.iter().map(|image| {
        serde_json::json!({
            "inline_data": { "mime_type": "image/jpeg", "data": image }
        })
    }));
    parts.push(serde_json::json!({ "text": prompt }));
    let request = reqwest::Client::builder()
        .timeout(Duration::from_secs(240))
        .build()
        .map_err(|error| error.to_string())?
        .post(endpoint)
        .header("x-goog-api-key", api_key.trim())
        .json(&serde_json::json!({
            "contents": [{ "role": "user", "parts": parts }],
            "generationConfig": gemini_generation_config()
        }));
    let response = tokio::select! {
        _ = token.cancelled() => return Err(CANCELLED.into()),
        result = timeout(Duration::from_secs(245), request.send()) => {
            match result {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => return Err("Could not reach Gemini. Check the network and API key, then resume.".into()),
                Err(_) => return Err("Gemini took too long to answer. Saved chunks remain ready to resume.".into()),
            }
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        let message = gemini_error_message(&detail);
        return Err(format!("Gemini returned HTTP {status}: {message}"));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("Gemini returned an unreadable response: {error}"))?;
    gemini_analysis(&body, chunk_duration)
}

fn gemini_analysis(
    body: &serde_json::Value,
    chunk_duration: f64,
) -> Result<GeminiAnalysis, String> {
    let finish_reason = body["candidates"][0]["finishReason"]
        .as_str()
        .unwrap_or("UNKNOWN");
    if finish_reason == "MAX_TOKENS" {
        return Err(
            "Gemini used its output budget before completing the summary and transcript. Resume to retry this chunk."
                .into(),
        );
    }
    if !matches!(finish_reason, "STOP" | "UNKNOWN") {
        return Err(format!(
            "Gemini did not complete this analysis ({finish_reason}). Resume to retry the chunk."
        ));
    }
    let text = body["candidates"][0]["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| !part["thought"].as_bool().unwrap_or(false))
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if text.trim().is_empty() {
        return Err("Gemini returned no analysis for this chunk.".into());
    }
    let payload: GeminiPayload = serde_json::from_str(text.trim())
        .map_err(|error| format!("Gemini returned invalid transcript JSON: {error}"))?;
    let duration = chunk_duration.max(1.0);
    let transcript_cues = payload
        .transcript
        .into_iter()
        .take(500)
        .filter_map(|cue| {
            if !cue.start.is_finite() || !cue.end.is_finite() {
                return None;
            }
            let start = cue.start.clamp(0.0, duration);
            let end = cue.end.clamp(start, duration);
            let text = sanitize_entry_text(&cue.text);
            (end > start && text != "Untitled note").then_some(TranscriptCue {
                start_seconds: start,
                end_seconds: end,
                text,
                speaker: sanitize_metadata(&cue.speaker, "Unknown"),
                language: sanitize_metadata(&cue.language, "unknown"),
            })
        })
        .collect();
    Ok(GeminiAnalysis {
        summary: sanitize_entry_text(
            payload
                .summary
                .trim()
                .trim_start_matches(['-', '*', '#', ' ']),
        ),
        transcript_cues,
    })
}

async fn whisper_transcript(
    ffmpeg: &Path,
    whisper_model: &Path,
    video: &Path,
    start: f64,
    end: f64,
    folder: &Path,
    token: &CancellationToken,
) -> Option<String> {
    if !whisper_model.is_file() {
        return None;
    }
    let whisper = find_executable("whisper-cli")?;
    let audio = folder.join("audio.wav");
    let ffmpeg_args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{start:.3}"),
        "-t".into(),
        format!("{:.3}", (end - start).max(1.0)),
        "-i".into(),
        video.to_string_lossy().into_owned(),
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "16000".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        "-y".into(),
        audio.to_string_lossy().into_owned(),
    ];
    let output = run_process(ffmpeg, &ffmpeg_args, token).await.ok()?;
    if !output.status.success() {
        return None;
    }
    let whisper_args = vec![
        "-m".into(),
        whisper_model.to_string_lossy().into_owned(),
        "-f".into(),
        audio.to_string_lossy().into_owned(),
        "-l".into(),
        "auto".into(),
        "-nt".into(),
        "-np".into(),
        "-t".into(),
        "4".into(),
    ];
    let output = run_process(&whisper, &whisper_args, token).await.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

#[tauri::command]
async fn analyze_chunk(
    state: State<'_, AppState>,
    request: AnalysisChunkRequest,
) -> Result<AnalysisChunkResult, String> {
    let AnalysisChunkRequest {
        job_id,
        video_path,
        start_seconds,
        end_seconds,
        provider,
        ollama_url,
        model,
        api_key,
        whisper_model_path,
        frame_count,
    } = request;
    let token = {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| "Analysis state is unavailable")?;
        jobs.get(&job_id)
            .cloned()
            .ok_or_else(|| "The analysis job is no longer active.".to_string())?
    };
    if token.is_cancelled() {
        return Err(CANCELLED.into());
    }
    if model.trim().is_empty() {
        return Err("Choose an Ollama model before starting analysis.".into());
    }
    let video = validate_video_path(&video_path)?;
    let ffmpeg = find_executable("ffmpeg").ok_or_else(|| {
        "FFmpeg was not found. Install FFmpeg to enable frame and audio sampling; playback and notes still work."
            .to_string()
    })?;
    let folder = tempfile::tempdir()
        .map_err(|error| format!("Could not create analysis workspace: {error}"))?;
    let end = end_seconds.max(start_seconds + 1.0);

    let images = extract_frames(
        &ffmpeg,
        &video,
        start_seconds.max(0.0),
        end,
        frame_count.unwrap_or(4),
        folder.path(),
        &token,
    )
    .await;
    if token.is_cancelled() {
        return Err(CANCELLED.into());
    }

    if provider.eq_ignore_ascii_case("gemini") {
        let audio = extract_audio(
            &ffmpeg,
            &video,
            start_seconds.max(0.0),
            end,
            folder.path(),
            &token,
        )
        .await;
        if images.is_empty() && audio.is_none() {
            return Err(
                "FFmpeg could not extract audio or representative frames from this chunk.".into(),
            );
        }
        let prompt = format!(
            "Analyze the actual audio and chronological frames together. The first image is the exact frame at the selected chunk start; the remaining images sample what follows. Produce both fields required by the response schema.\n\nSUMMARY FIELD:\nCreate a dense, searchable English timeline note focused on what specifically changed. Prioritize the concrete topic, question, decision, instruction, joke, disagreement, or outcome in speech; then exact actions, people/player names, game/app/screen, selected option, setting, amount, item, result, and visible text. Use 1–3 compact sentences and 35–75 words with 2–4 facts in chronological order. Never use filler such as ‘the video shows,’ ‘a user navigates,’ ‘gameplay continues,’ ‘players chat/talk,’ or ‘in this chunk.’\n\nTRANSCRIPT FIELD:\nTranscribe every intelligible spoken word verbatim in the original language, including Slovak/English code-switching. Do not translate, paraphrase, grammar-correct, censor, or omit audible filler words, repetitions, and false starts. Split speech into readable cues of roughly 1–8 seconds and normally no more than 16 words. Use decimal start/end seconds relative to the beginning of the supplied audio, not the original video. Timestamps must follow the audio closely, stay chronological, and may overlap for simultaneous speakers. Keep stable speaker labels only when voices are distinguishable; otherwise use Unknown. Use language codes such as sk, en, or mixed. Do not put music, sound effects, descriptions, or uncertain invented words in transcript text. Return an empty transcript array when no intelligible speech is audible.\n\nIf any evidence is unclear, remain conservative rather than inventing it.\n\nVideo: {}\nChunk: {} to {}",
            video.file_name().and_then(|value| value.to_str()).unwrap_or("video"),
            format_timestamp(start_seconds),
            format_timestamp(end)
        );
        let analysis = analyze_with_gemini(
            &model,
            api_key.as_deref().unwrap_or_default(),
            &prompt,
            &images,
            audio.as_deref(),
            (end - start_seconds).max(1.0),
            &token,
        )
        .await?;
        let transcript_cues = if audio.is_some() {
            analysis
                .transcript_cues
                .into_iter()
                .map(|cue| TranscriptCue {
                    start_seconds: start_seconds.max(0.0) + cue.start_seconds,
                    end_seconds: start_seconds.max(0.0) + cue.end_seconds,
                    ..cue
                })
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        let cue_count = transcript_cues.len();
        return Ok(AnalysisChunkResult {
            summary: analysis.summary,
            frame_count: images.len(),
            transcript_source: if audio.is_some() {
                format!("Native AAC audio + vision · Gemini · {cue_count} subtitle cues")
            } else {
                "Vision only · no audio stream".into()
            },
            transcript_cues,
            transcript_complete: true,
        });
    }

    let mut transcript_source = "Visual frames only".to_string();
    let transcript = if let Some(text) = companion_transcript(&video, start_seconds, end) {
        transcript_source = "Companion subtitles".into();
        Some(text)
    } else if let Some(model_path) = whisper_model_path.filter(|value| !value.trim().is_empty()) {
        let text = whisper_transcript(
            &ffmpeg,
            Path::new(&model_path),
            &video,
            start_seconds,
            end,
            folder.path(),
            &token,
        )
        .await;
        if text.is_some() {
            transcript_source = "Local Whisper transcript".into();
        }
        text
    } else {
        None
    };

    if images.is_empty() && transcript.is_none() {
        return Err("FFmpeg could not extract representative frames from this chunk.".into());
    }

    let transcript_for_prompt = transcript
        .as_deref()
        .unwrap_or("No transcript was available for this chunk.")
        .chars()
        .take(8000)
        .collect::<String>();
    let prompt = format!(
        "Create a dense, searchable timeline note using only the chronological frames and transcript supplied for this private video chunk. The first image is the exact frame at the selected chunk start; the remaining images sample what follows. Focus on what specifically changed.\n\nPriority order:\n1. Speech: state the concrete topic, question, decision, instruction, joke, disagreement, or outcome. Do not merely say that people talk or name the language.\n2. Actions and state changes: name the person/player when known, the exact game/app/screen, selected option, setting, amount, item, result, or visible on-screen text.\n3. Sounds only when they explain an event; omit routine music and generic gameplay sounds.\n\nUse 1–3 compact sentences and 35–75 words when the evidence supports it. Include 2–4 specific facts in chronological order. Never use filler such as ‘the video shows,’ ‘a user navigates,’ ‘gameplay continues,’ ‘players chat/talk,’ or ‘in this chunk.’ Do not invent unclear transcript content or visual events; explicitly mark only the uncertain detail. Return only the note—no timestamp, heading, bullet, preamble, or Markdown.\n\nVideo: {}\nChunk: {} to {}\nTranscript (may be unavailable): {}",
        video.file_name().and_then(|value| value.to_str()).unwrap_or("video"),
        format_timestamp(start_seconds),
        format_timestamp(end),
        transcript_for_prompt
    );

    let base = normalize_ollama_url(&ollama_url)?;
    let endpoint = ollama_endpoint(&base, "generate")?;
    let api_model = model_for_endpoint(&base, &model);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client.post(endpoint).json(&serde_json::json!({
        "model": api_model,
        "prompt": prompt,
        "images": images,
        "stream": false,
        "options": { "temperature": 0.15 }
    }));
    if let Some(key) = api_key.as_deref().filter(|value| !value.trim().is_empty()) {
        request = request.bearer_auth(key.trim());
    }
    let response = tokio::select! {
        _ = token.cancelled() => return Err(CANCELLED.into()),
        result = timeout(Duration::from_secs(185), request.send()) => {
            match result {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => return Err(format!("Could not reach Ollama at {}. Check the endpoint, network access, and API key, then resume.", ollama_url.trim())),
                Err(_) => return Err("Ollama took too long to answer. The completed timeline remains saved; you can resume later.".into()),
            }
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(format!(
            "Ollama returned HTTP {status}: {}",
            detail.chars().take(240).collect::<String>()
        ));
    }
    let body: serde_json::Value = tokio::select! {
        _ = token.cancelled() => return Err(CANCELLED.into()),
        result = response.json() => result.map_err(|error| format!("Ollama returned an unreadable response: {error}"))?,
    };
    let raw_summary = body["response"]
        .as_str()
        .ok_or_else(|| "Ollama returned no summary.".to_string())?;
    let summary = sanitize_entry_text(raw_summary.trim().trim_start_matches(['-', '*', '#', ' ']));
    Ok(AnalysisChunkResult {
        summary,
        frame_count: images.len(),
        transcript_source,
        transcript_cues: vec![],
        transcript_complete: false,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let media = start_media_server().expect("could not start the private media server");
    let collaboration =
        start_collaboration_service().expect("could not start local peer collaboration");
    tauri::Builder::default()
        .manage(AppState::new(media, collaboration))
        .invoke_handler(tauri::generate_handler![
            pick_video,
            pick_export_directory,
            host_collaboration,
            join_collaboration,
            poll_collaboration,
            publish_collaboration_event,
            collaboration_status,
            stop_collaboration,
            host_relay_session,
            join_relay_session,
            prepare_export_directory,
            register_media_source,
            extract_waveform,
            load_api_key,
            save_api_key,
            load_video,
            read_sidecar,
            save_markdown,
            save_playback_position,
            add_bookmark,
            add_bookmark_range,
            end_bookmark,
            add_subtitle,
            update_entry,
            update_subtitle,
            delete_entry,
            append_ai_entry,
            append_analysis_result,
            check_ollama,
            check_gemini,
            begin_analysis,
            cancel_analysis,
            finish_analysis,
            begin_export,
            cancel_export,
            finish_export,
            export_mark_clip,
            write_export_manifest,
            analyze_chunk,
        ])
        .run(tauri::generate_context!())
        .expect("error while running FrameNote");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpStream;

    fn collaboration_fixture(folder: &tempfile::TempDir, code: &str) -> HostedSession {
        let video = folder.path().join("shared.mp4");
        let sidecar = folder.path().join("shared.md");
        fs::write(&video, b"0123456789abcdef").expect("shared video fixture");
        let markdown = initial_markdown(&video);
        fs::write(&sidecar, &markdown).expect("shared sidecar fixture");
        HostedSession {
            code: code.into(),
            token: Uuid::new_v4().to_string(),
            service_fullname: format!("FrameNote test {code}.{COLLABORATION_SERVICE_TYPE}"),
            video_path: video,
            sidecar_path: sidecar,
            video_name: "shared.mp4".into(),
            audio_tracks: Vec::new(),
            frame_rate: Some(30.0),
            host_name: "Host".into(),
            runtime: Arc::new(Mutex::new(HostedSessionRuntime {
                sequence: 0,
                document_revision: 0,
                markdown,
                transport: CollaborationTransport {
                    position: 3.5,
                    playing: false,
                    playback_rate: 1.0,
                },
                events: VecDeque::new(),
                peers: HashMap::new(),
            })),
        }
    }

    #[test]
    fn formats_long_timestamps() {
        assert_eq!(format_timestamp(3661.2), "01:01:01");
    }

    #[test]
    fn waveform_cache_key_tracks_source_changes() {
        let folder = tempfile::tempdir().expect("temp folder");
        let video = folder.path().join("recording.mp4");
        fs::write(&video, b"first").expect("video fixture");
        let first = waveform_cache_key(&video).expect("first cache key");
        fs::write(&video, b"longer replacement").expect("changed video fixture");
        let second = waveform_cache_key(&video).expect("second cache key");
        assert_ne!(first, second);
    }

    #[test]
    fn waveform_cache_round_trips_and_rejects_invalid_data() {
        let folder = tempfile::tempdir().expect("temp folder");
        let cache = folder.path().join("waveform.json");
        let data = WaveformData {
            samples_per_second: 100.0,
            peaks: vec![0.1, 0.5, 1.0],
        };
        write_waveform_cache(&cache, &data);
        let loaded = read_waveform_cache(&cache).expect("cached waveform");
        assert_eq!(loaded.samples_per_second, 100.0);
        assert_eq!(loaded.peaks, vec![0.1, 0.5, 1.0]);

        fs::write(&cache, br#"{"samplesPerSecond":0,"peaks":[]}"#).expect("invalid cache fixture");
        assert!(read_waveform_cache(&cache).is_none());
        assert!(!cache.exists());
    }

    #[test]
    fn waveform_cache_expires_after_seven_days() {
        let now = UNIX_EPOCH + Duration::from_secs(10 * 24 * 60 * 60);
        assert!(waveform_cache_is_fresh(
            now - Duration::from_secs(6 * 24 * 60 * 60),
            now
        ));
        assert!(!waveform_cache_is_fresh(
            now - Duration::from_secs(8 * 24 * 60 * 60),
            now
        ));
    }

    #[test]
    fn api_keys_use_separate_provider_accounts() {
        assert_eq!(
            api_key_account("cloud").expect("cloud account"),
            "ollama-cloud"
        );
        assert_eq!(
            api_key_account(" GEMINI ").expect("Gemini account"),
            "gemini"
        );
        assert!(api_key_account("local").is_err());
        assert!(api_key_account("unknown").is_err());
    }

    #[test]
    fn embedded_chapters_import_once_and_respect_deleted_marks() {
        let markdown = initial_markdown(Path::new("recording.mp4"));
        let chapters = vec![
            EmbeddedChapter {
                source_index: 0,
                start_seconds: 12.25,
                title: "Unnamed 1".into(),
            },
            EmbeddedChapter {
                source_index: 1,
                start_seconds: 48.5,
                title: "Boss fight".into(),
            },
        ];
        let imported = merge_embedded_chapters(&markdown, &chapters, "source-one")
            .expect("first chapter import");
        assert!(imported.contains("[00:00:12.250] Unnamed 1"));
        assert!(imported.contains("[00:00:48.500] Boss fight"));
        assert!(imported.contains("source=embedded-chapter"));
        assert!(imported.contains("framenote:embedded-chapters fingerprint=source-one"));
        assert!(merge_embedded_chapters(&imported, &chapters, "source-one").is_none());

        let after_delete = imported
            .lines()
            .filter(|line| !line.contains("embedded-1-12250"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(merge_embedded_chapters(&after_delete, &chapters, "source-one").is_none());
        assert!(merge_embedded_chapters(&after_delete, &chapters, "changed-source").is_none());
        assert!(open_bookmark_start(
            "- [00:00:12.250] Unnamed 1 <!-- framenote:bookmark:embedded-1-12250 start=12.250 source=embedded-chapter -->"
        )
        .is_none());
    }

    #[test]
    fn parses_ffprobe_chapter_titles_and_times() {
        let json = serde_json::json!({
            "chapters": [
                { "start_time": "48.500000", "tags": { "title": "Boss fight" } },
                { "start_time": "12.250000", "tags": { "title": "Unnamed 1" } },
                { "start_time": "invalid", "tags": { "title": "Broken" } }
            ]
        });
        assert_eq!(
            parse_embedded_chapters(&json),
            vec![
                EmbeddedChapter {
                    source_index: 1,
                    start_seconds: 12.25,
                    title: "Unnamed 1".into(),
                },
                EmbeddedChapter {
                    source_index: 0,
                    start_seconds: 48.5,
                    title: "Boss fight".into(),
                },
            ]
        );
    }

    #[test]
    fn ffprobe_reads_mp4_chapter_markers() {
        let Some(ffmpeg) = find_executable("ffmpeg") else {
            return;
        };
        if find_executable("ffprobe").is_none() {
            return;
        }
        let folder = tempfile::tempdir().expect("temp folder");
        let metadata = folder.path().join("chapters.ffmeta");
        let video = folder.path().join("hybrid-marker-fixture.mp4");
        fs::write(
            &metadata,
            ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=750\ntitle=Opening\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=750\nEND=3000\ntitle=OBS marker\n",
        )
        .expect("chapter metadata fixture");
        let status = StdCommand::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:r=1:d=3",
                "-i",
            ])
            .arg(&metadata)
            .args(["-map", "0:v:0", "-map_chapters", "1", "-c:v", "mpeg4", "-y"])
            .arg(&video)
            .status()
            .expect("create MP4 chapter fixture");
        assert!(status.success());

        let chapters = probe_embedded_chapters(&video).expect("probe MP4 chapters");
        assert_eq!(chapters.len(), 2);
        assert!(
            (chapters[1].start_seconds - 0.75).abs() < 0.001,
            "unexpected chapter data: {chapters:?}"
        );
        assert_eq!(chapters[1].title, "OBS marker");
    }

    #[test]
    fn exports_clip_relative_srt_with_speaker_labels() {
        let cues = vec![
            TranscriptCue {
                start_seconds: 11.25,
                end_seconds: 13.5,
                text: "Ahoj všetci.".into(),
                speaker: "Filip".into(),
                language: "sk".into(),
            },
            TranscriptCue {
                start_seconds: 14.0,
                end_seconds: 16.0,
                text: "Let's start.".into(),
                speaker: "Unknown".into(),
                language: "en".into(),
            },
        ];
        let srt = clip_srt(&cues, 10.0, 15.0);
        assert!(srt.contains("00:00:01,250 --> 00:00:03,500\nFilip: Ahoj všetci."));
        assert!(srt.contains("00:00:04,000 --> 00:00:05,000\nLet's start."));
        assert_eq!(safe_file_component("A/B: rough * cut", 40), "A B rough cut");
    }

    #[test]
    fn appends_inside_the_requested_section() {
        let source = "# clip\n\n## Bookmarks\n\n## AI timeline\n";
        let result = append_to_section(source, BOOKMARK_HEADING, "- [00:00:03] Note");
        assert!(result.contains("## Bookmarks\n\n- [00:00:03] Note\n\n## AI timeline"));
    }

    #[test]
    fn preserves_unrecognized_markdown() {
        let source = "# clip\n\nA free-form paragraph.\n";
        let result = append_to_section(source, AI_HEADING, "- [00:00:00–00:01:00] Opening");
        assert!(result.starts_with(source));
        assert!(result.contains("## AI timeline"));
    }

    #[test]
    fn playback_position_round_trips_without_changing_human_content() {
        let source = "# clip.mp4\n\n<!-- framenote:v1 -->\n\nA human paragraph.\n";
        let first = with_playback_position(source, 83.25);
        assert_eq!(playback_position(&first), 83.25);
        assert!(first.contains("A human paragraph."));
        assert_eq!(first.matches("framenote:position").count(), 1);

        let second = with_playback_position(&first, 912.5);
        assert_eq!(playback_position(&second), 912.5);
        assert_eq!(second.matches("framenote:position").count(), 1);
    }

    #[test]
    fn direct_cloud_model_aliases_map_without_changing_local_names() {
        let cloud = normalize_ollama_url("https://ollama.com/api").expect("cloud URL");
        let local = normalize_ollama_url("http://127.0.0.1:11434").expect("local URL");

        assert_eq!(model_for_endpoint(&cloud, "gemma4:31b-cloud"), "gemma4:31b");
        assert_eq!(model_for_endpoint(&cloud, "glm-4.7:cloud"), "glm-4.7");
        assert_eq!(
            model_for_endpoint(&local, "gemma4:31b-cloud"),
            "gemma4:31b-cloud"
        );
    }

    #[test]
    fn validates_gemini_model_names() {
        assert_eq!(
            gemini_model_name("models/gemini-3.6-flash").expect("model name"),
            "gemini-3.6-flash"
        );
        assert!(gemini_model_name("gemini-3.6-flash?key=secret").is_err());
    }

    #[test]
    fn uses_generate_content_schema_wire_types() {
        let config = gemini_generation_config();
        let text_format = &config["responseFormat"]["text"];
        assert_eq!(text_format["mimeType"], "APPLICATION_JSON");
        assert_eq!(text_format["schema"]["type"], "object");
        assert_eq!(
            text_format["schema"]["properties"]["transcript"]["items"]["properties"]["start"]
                ["type"],
            "number"
        );
        assert!(config.get("responseMimeType").is_none());
        assert!(config.get("responseSchema").is_none());
    }

    #[test]
    fn includes_gemini_field_violations_in_errors() {
        let detail = serde_json::json!({
            "error": {
                "message": "Request contains an invalid argument.",
                "details": [{
                    "fieldViolations": [{
                        "field": "generation_config.response_format.text.mime_type",
                        "description": "Invalid enum value"
                    }]
                }]
            }
        })
        .to_string();
        let message = gemini_error_message(&detail);
        assert!(message.contains("generation_config.response_format.text.mime_type"));
        assert!(message.contains("Invalid enum value"));
    }

    #[test]
    fn parses_structured_gemini_summary_and_transcript() {
        let truncated = serde_json::json!({
            "candidates": [{
                "finishReason": "MAX_TOKENS",
                "content": { "parts": [{ "text": "While players chat in Slovak and" }] }
            }]
        });
        assert!(gemini_analysis(&truncated, 60.0)
            .expect_err("truncated response")
            .contains("output budget"));

        let payload = serde_json::json!({
            "summary": "The host asks the group to join the lobby.",
            "transcript": [{
                "start": 2.25,
                "end": 5.5,
                "text": "Poďte už do lobby.",
                "speaker": "Speaker 1",
                "language": "sk"
            }]
        });
        let complete = serde_json::json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": { "parts": [{ "text": payload.to_string() }] }
            }]
        });
        let analysis = gemini_analysis(&complete, 60.0).expect("complete response");
        assert_eq!(
            analysis.summary,
            "The host asks the group to join the lobby."
        );
        assert_eq!(analysis.transcript_cues.len(), 1);
        assert_eq!(analysis.transcript_cues[0].start_seconds, 2.25);
        assert_eq!(analysis.transcript_cues[0].text, "Poďte už do lobby.");
    }

    #[tokio::test]
    async fn analysis_audio_combines_embedded_tracks() {
        let (Some(ffmpeg), Some(_ffprobe)) =
            (find_executable("ffmpeg"), find_executable("ffprobe"))
        else {
            return;
        };
        let folder = tempfile::tempdir().expect("temp folder");
        let fixture = folder.path().join("two-analysis-tracks.m4a");
        let status = StdCommand::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:duration=1",
                "-map",
                "0:a",
                "-map",
                "1:a",
                "-c:a",
                "aac",
                "-y",
            ])
            .arg(&fixture)
            .status()
            .expect("create audio fixture");
        assert!(status.success());

        let encoded = extract_audio(
            &ffmpeg,
            &fixture,
            0.0,
            1.0,
            folder.path(),
            &CancellationToken::new(),
        )
        .await
        .expect("mixed AAC audio");
        assert!(BASE64.decode(encoded).expect("base64 audio").len() > 1_000);
    }

    #[test]
    fn private_audio_mix_streams_progressive_aac() {
        let Some(ffmpeg) = find_executable("ffmpeg") else {
            return;
        };
        let folder = tempfile::tempdir().expect("temp folder");
        let fixture = folder.path().join("two-tracks.m4a");
        let status = StdCommand::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:duration=1",
                "-map",
                "0:a",
                "-map",
                "1:a",
                "-c:a",
                "aac",
                "-y",
            ])
            .arg(&fixture)
            .status()
            .expect("create audio fixture");
        assert!(status.success());

        let server = start_media_server().expect("media server");
        server
            .files
            .write()
            .expect("media registry")
            .insert("mix-test".into(), MediaSource::Local(fixture));
        let address = server
            .base_url
            .strip_prefix("http://")
            .expect("loopback URL");
        let mut stream = TcpStream::connect(address).expect("connect to media server");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        stream
            .write_all(
                b"GET /mix/mix-test?tracks=0,1&volumes=1,0.5&start=0 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .expect("request audio mix");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read audio mix");

        assert!(response.starts_with(b"HTTP/1.1 200"));
        assert!(response
            .windows(b"Content-Type: audio/aac".len())
            .any(|window| window == b"Content-Type: audio/aac"));
        assert!(response
            .windows(2)
            .any(|window| window[0] == 0xff && window[1] & 0xf6 == 0xf0));
    }

    #[test]
    fn peer_protocol_joins_streams_ranges_and_syncs_markdown_and_transport() {
        let folder = tempfile::tempdir().expect("temp folder");
        let service = start_collaboration_service().expect("peer service");
        let session = collaboration_fixture(&folder, "420731");
        *service.hosted.write().expect("hosted session") = Some(session.clone());
        let base_url = format!("http://127.0.0.1:{}", service.port);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("peer client");

        let joined = client
            .post(format!("{base_url}/join"))
            .json(&serde_json::json!({
                "code": "420731",
                "peerId": "guest-one",
                "displayName": "Editor One"
            }))
            .send()
            .expect("join peer")
            .error_for_status()
            .expect("accepted join")
            .json::<NetworkJoinResponse>()
            .expect("join response");
        assert_eq!(joined.video_name, "shared.mp4");
        assert_eq!(joined.playback_position, 3.5);

        let media = client
            .get(format!("{base_url}/session/{}/media", session.token))
            .header("Range", "bytes=4-8")
            .send()
            .expect("peer media range");
        assert_eq!(media.status(), reqwest::StatusCode::PARTIAL_CONTENT);
        assert_eq!(media.bytes().expect("peer media body").as_ref(), b"45678");

        client
            .post(format!("{base_url}/session/{}/event", session.token))
            .json(&serde_json::json!({
                "peerId": "guest-one",
                "kind": "transport",
                "payload": { "position": 8.25, "playing": true, "playbackRate": 1.25 }
            }))
            .send()
            .expect("publish transport")
            .error_for_status()
            .expect("accepted transport");

        let shared_markdown = format!(
            "{}\n- [00:00:08.250] Shared mark <!-- framenote:bookmark:shared start=8.250 -->\n",
            joined.markdown.trim_end()
        );
        client
            .post(format!("{base_url}/session/{}/event", session.token))
            .json(&serde_json::json!({
                "peerId": "guest-one",
                "kind": "document",
                "payload": { "markdown": shared_markdown }
            }))
            .send()
            .expect("publish markdown")
            .error_for_status()
            .expect("accepted markdown");

        let poll = client
            .get(format!(
                "{base_url}/session/{}/events?after=0&peerId=guest-one",
                session.token
            ))
            .send()
            .expect("poll peer")
            .error_for_status()
            .expect("accepted poll")
            .json::<CollaborationPollResult>()
            .expect("poll response");
        assert_eq!(poll.participant_count, 2);
        assert_eq!(poll.events.len(), 2);
        assert_eq!(poll.events[0].kind, "transport");
        assert_eq!(poll.events[1].kind, "document");
        assert!(fs::read_to_string(&session.sidecar_path)
            .expect("canonical sidecar")
            .contains("Shared mark"));
        let runtime = session.runtime.lock().expect("session runtime");
        assert_eq!(runtime.transport.position, 8.25);
        assert!(runtime.transport.playing);
        drop(runtime);
        let _ = service.mdns.shutdown();
    }

    #[test]
    fn six_digit_mdns_discovery_connects_two_app_instances() {
        let folder = tempfile::tempdir().expect("temp folder");
        let host = start_collaboration_service().expect("host peer service");
        let guest = start_collaboration_service().expect("guest peer service");
        let code = six_digit_session_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|character| character.is_ascii_digit()));
        let session = collaboration_fixture(&folder, &code);
        *host.hosted.write().expect("hosted session") = Some(session.clone());

        let instance_name = format!("FrameNote test {}", Uuid::new_v4());
        let host_name = format!("framenote-test-{}.local.", &session.token[..8]);
        let mut properties = HashMap::new();
        properties.insert("code".to_string(), code.clone());
        properties.insert("version".to_string(), "1".to_string());
        let service_info = ServiceInfo::new(
            COLLABORATION_SERVICE_TYPE,
            &instance_name,
            &host_name,
            "",
            host.port,
            properties,
        )
        .expect("mDNS service info")
        .enable_addr_auto();
        let fullname = service_info.get_fullname().to_string();
        host.mdns.register(service_info).expect("advertise session");

        let (joined, _) = join_discovered_session(&guest, &code, "Second editor")
            .expect("discover and join host");
        assert_eq!(joined.token, session.token);
        assert_eq!(joined.host_name, "Host");
        assert_eq!(
            session
                .runtime
                .lock()
                .expect("session runtime")
                .peers
                .get("Second editor")
                .map(|peer| peer.name.as_str()),
            None,
            "peer IDs are generated by each app, not display names"
        );
        assert_eq!(
            session
                .runtime
                .lock()
                .expect("session runtime")
                .peers
                .values()
                .next()
                .map(|peer| peer.name.as_str()),
            Some("Second editor")
        );
        let _ = host.mdns.unregister(&fullname);
        let _ = host.mdns.shutdown();
        let _ = guest.mdns.shutdown();
    }

    #[test]
    fn reads_overlapping_subtitle_cues() {
        assert_eq!(parse_subtitle_timestamp("01:02:03,500"), Some(3723.5));
    }

    #[test]
    fn sidecar_workflow_never_changes_the_video() {
        let folder = tempfile::tempdir().expect("temp folder");
        let video = folder.path().join("recording.final.mp4");
        let original = b"not-a-real-video-but-untouched";
        fs::write(&video, original).expect("video fixture");
        let video_path = video.to_string_lossy().into_owned();

        let loaded = load_video(video_path.clone()).expect("load video");
        assert!(loaded.sidecar_path.ends_with("recording.final.md"));
        assert!(Path::new(&loaded.sidecar_path).is_file());

        let added = add_bookmark(video_path.clone(), 83.0).expect("bookmark");
        let edited = update_entry(
            video_path.clone(),
            added.entry_id.clone(),
            "A human-editable observation".into(),
        )
        .expect("edit bookmark");
        assert!(edited
            .markdown
            .contains("[00:01:23.000] A human-editable observation"));
        let ended = end_bookmark(video_path.clone(), 91.5).expect("end bookmark");
        assert_eq!(ended.entry_id, added.entry_id);
        assert!(ended
            .document
            .markdown
            .contains("[00:01:23.000–00:01:31.500] A human-editable observation"));

        let ranged = add_bookmark_range(video_path.clone(), 101.25, 104.875)
            .expect("waveform range bookmark");
        assert!(ranged
            .document
            .markdown
            .contains("[00:01:41.250–00:01:44.875] New mark"));

        let subtitle = add_subtitle(video_path.clone(), 83.125, 86.75).expect("subtitle");
        let edited_subtitle = update_subtitle(
            video_path.clone(),
            subtitle.entry_id,
            83.25,
            87.5,
            "Presný ručný prepis".into(),
            "Filip".into(),
            "sk".into(),
        )
        .expect("edit subtitle timing");
        assert!(edited_subtitle
            .markdown
            .contains("[00:01:23.250–00:01:27.500] Presný ručný prepis"));
        assert!(edited_subtitle
            .markdown
            .contains("start=83.250 end=87.500 speaker=\"Filip\" language=\"sk\""));

        let analyzed = append_ai_entry(
            video_path.clone(),
            60.0,
            120.0,
            "A concise generated summary".into(),
        )
        .expect("append AI entry");
        assert!(analyzed
            .markdown
            .contains("[00:01:00–00:02:00] A concise generated summary"));

        let migrated = append_analysis_result(
            video_path.clone(),
            60.0,
            120.0,
            "This replacement must not overwrite the existing summary.".into(),
            vec![TranscriptCue {
                start_seconds: 62.0,
                end_seconds: 64.0,
                text: "Legacy range now has captions.".into(),
                speaker: "Speaker 1".into(),
                language: "en".into(),
            }],
            true,
        )
        .expect("migrate legacy analysis");
        assert!(migrated.markdown.contains("A concise generated summary"));
        assert!(!migrated
            .markdown
            .contains("This replacement must not overwrite"));
        assert!(migrated.markdown.contains("Legacy range now has captions."));

        let transcribed = append_analysis_result(
            video_path.clone(),
            120.0,
            180.0,
            "The speaker invites the group into the lobby.".into(),
            vec![TranscriptCue {
                start_seconds: 122.25,
                end_seconds: 125.5,
                text: "Poďte už do lobby.".into(),
                speaker: "Speaker 1".into(),
                language: "sk".into(),
            }],
            true,
        )
        .expect("append transcript");
        assert!(transcribed.markdown.contains("## Subtitles"));
        assert!(transcribed.markdown.contains("Poďte už do lobby."));
        assert!(transcribed
            .markdown
            .contains("speaker=\"Speaker 1\" language=\"sk\""));
        assert!(transcribed.markdown.contains("transcript=complete"));
        assert_eq!(fs::read(video).expect("read original"), original);
    }

    #[test]
    fn private_media_server_supports_byte_ranges() {
        use std::{io::Write, net::TcpStream};

        let folder = tempfile::tempdir().expect("temp folder");
        let video = folder.path().join("range.mp4");
        fs::write(&video, b"0123456789").expect("media fixture");
        let media = start_media_server().expect("media server");
        media
            .files
            .write()
            .expect("media map")
            .insert("test-token".into(), MediaSource::Local(video));
        let address = media.base_url.trim_start_matches("http://");
        let mut stream = TcpStream::connect(address).expect("connect media server");
        stream
            .write_all(
                b"GET /media/test-token HTTP/1.1\r\nHost: localhost\r\nRange: bytes=2-5\r\nConnection: close\r\n\r\n",
            )
            .expect("write request");
        let mut response = vec![];
        stream.read_to_end(&mut response).expect("read response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with("HTTP/1.1 206"));
        assert!(response.contains("Content-Range: bytes 2-5/10"));
        assert!(response.ends_with("2345"));
    }
}
