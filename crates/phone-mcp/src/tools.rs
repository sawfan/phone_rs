use phone_core::dto::{self, Catalog, Existing, RenderedFile};
use rmcp::{Json, handler::server::wrapper::Parameters, tool, tool_router};

use crate::PhoneMcpServer;
use crate::requests::{DEFAULT_SAMPLE_RATE, DEFAULT_VOLUME, RenderTonesRequest};

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
}
