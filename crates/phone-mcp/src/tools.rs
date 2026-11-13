use phone_core::dto::{self, Catalog, Existing, RenderedFile};
use phone_tts::{BackendInfo, Tts};
use rmcp::{Json, handler::server::wrapper::Parameters, tool, tool_router};

use crate::PhoneMcpServer;
use crate::requests::{DEFAULT_SAMPLE_RATE, DEFAULT_VOLUME, RenderTonesRequest, SayRequest};

pub(crate) fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<PhoneMcpServer> {
    PhoneMcpServer::tool_router()
}

/// Renders the request and writes a new WAV file, on a blocking thread
/// (rendering many long notes takes a while). Existing files are never
/// replaced.
pub async fn render_tones(request: RenderTonesRequest) -> Result<RenderedFile, String> {
    tokio::task::spawn_blocking(move || {
        let sound = request.sound()?;
        let pcm = phone_core::tone::render(
            &request.tones(),
            &sound,
            request.volume.unwrap_or(DEFAULT_VOLUME),
            request.sample_rate.unwrap_or(DEFAULT_SAMPLE_RATE),
        )?;
        dto::write_wav(&pcm, &request.out, Existing::Refuse)
    })
    .await
    .map_err(|error| format!("rendering stopped unexpectedly: {error}"))?
}

/// Says the request's text with the backend `tts` builds and writes a new
/// WAV file, on a blocking thread (the backend blocks while it speaks).
/// Existing files are never replaced; the path is checked before the
/// backend is asked.
pub async fn say(
    tts: impl FnOnce() -> Result<Tts, String> + Send + 'static,
    request: SayRequest,
) -> Result<RenderedFile, String> {
    tokio::task::spawn_blocking(move || {
        if std::path::Path::new(&request.out).exists() {
            return Err(format!(
                "{} already exists; choose a new file name",
                request.out
            ));
        }
        let pcm = tts()?.speak_to_pcm(&request.text, &request.options())?;
        dto::write_wav(&pcm, &request.out, Existing::Refuse)
    })
    .await
    .map_err(|error| format!("speaking stopped unexpectedly: {error}"))?
}

/// The backend `tts` builds, resolved on a blocking thread (it may read
/// configuration).
pub async fn tts_backend(
    tts: impl FnOnce() -> Result<Tts, String> + Send + 'static,
) -> Result<BackendInfo, String> {
    tokio::task::spawn_blocking(move || tts().map(|tts| tts.info()))
        .await
        .map_err(|error| {
            format!("reading the speech configuration stopped unexpectedly: {error}")
        })?
}

/// The backend from `PHONE_TTS_*` variables.
fn from_env() -> Result<Tts, String> {
    Tts::from_env().map_err(String::from)
}

#[tool_router(router = tool_router)]
impl PhoneMcpServer {
    #[tool(
        name = "phone.render_tones",
        description = "Render notes to a new WAV file on the server's machine: each note is a frequency in hertz with a start and a duration in seconds (overlapping notes make chords), played on a preset sound (pure, soft, organ, pluck, pad, bright) optionally changed (waveform, envelope, low-pass cutoff). Returns the file, its size, length, and sample rate."
    )]
    pub async fn render_tones(
        &self,
        Parameters(request): Parameters<RenderTonesRequest>,
    ) -> Result<Json<RenderedFile>, String> {
        render_tones(request).await.map(Json)
    }

    #[tool(
        name = "phone.presets",
        description = "The preset sounds (waveform, envelope, cutoff) and the waveforms that phone.render_tones accepts."
    )]
    pub async fn presets(&self) -> Json<Catalog> {
        Json(dto::catalog())
    }

    #[tool(
        name = "phone.say",
        description = "Say text with the configured text-to-speech backend and write it to a new WAV file on the server's machine (the path must end in .wav and must not exist yet). Optionally give a voice the backend knows and a speed (0.25 to 4). Returns the file, its size, length, and sample rate."
    )]
    pub async fn say(
        &self,
        Parameters(request): Parameters<SayRequest>,
    ) -> Result<Json<RenderedFile>, String> {
        say(from_env, request).await.map(Json)
    }

    #[tool(
        name = "phone.tts_backend",
        description = "The configured text-to-speech backend: its kind, model, default voice, and endpoint."
    )]
    pub async fn tts_backend(&self) -> Result<Json<BackendInfo>, String> {
        tts_backend(from_env).await.map(Json)
    }
}
