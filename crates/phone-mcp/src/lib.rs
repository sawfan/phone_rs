//! Standalone MCP server for sound synthesis.
//!
//! Tools are named `phone.<operation>`. [`requests`] holds the request types
//! and [`tools::render_tones`] the blocking-safe operation, which hosts
//! reuse under their own names.

pub mod requests;
pub mod tools;

use rmcp::{
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    model::{Implementation, ServerCapabilities, ServerConfig},
    tool_handler,
};

#[derive(Clone)]
pub struct PhoneMcpServer {
    tool_router: ToolRouter<Self>,
}

impl PhoneMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: tools::tool_router(),
        }
    }

    pub fn tool_names() -> Vec<String> {
        tools::tool_router()
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }
}

impl Default for PhoneMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PhoneMcpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhoneMcpServer").finish_non_exhaustive()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for PhoneMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("phone-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Use the phone tools to render notes (frequencies with start times and durations) to WAV files with preset or customized sounds, and to list the presets.",
            )
    }
}

#[cfg(test)]
mod tests {
    use rmcp::handler::server::wrapper::Parameters;

    use super::*;
    use crate::requests::RenderTonesRequest;

    fn request(json: serde_json::Value) -> RenderTonesRequest {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn lists_its_tools() {
        assert_eq!(
            PhoneMcpServer::tool_names().len(),
            2,
            "{:?}",
            PhoneMcpServer::tool_names()
        );
        assert!(PhoneMcpServer::tool_names().contains(&"phone.render_tones".to_string()));
    }

    #[tokio::test]
    async fn renders_new_files_only() {
        let server = PhoneMcpServer::new();
        let dir = std::env::temp_dir().join(format!("phone-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("chord.wav").to_string_lossy().into_owned();
        let chord = serde_json::json!({
            "tones": [
                { "hz": 261.63, "duration": 1.0 },
                { "hz": 329.63, "duration": 1.0 },
                { "hz": 392.0, "start": 0.5, "duration": 0.5 }
            ],
            "out": out,
            "preset": "pad",
            "cutoff_hz": 900.0,
            "sample_rate": 16000
        });
        let written = server
            .render_tones(Parameters(request(chord.clone())))
            .await
            .unwrap()
            .0;
        // One second plus the pad preset's 1.2 s release.
        assert!((written.seconds - 2.2).abs() < 1e-3);
        assert_eq!(written.sample_rate, 16_000);
        let again = server.render_tones(Parameters(request(chord))).await;
        assert!(again.err().unwrap().contains("already exists"));

        let bad = serde_json::json!({
            "tones": [{ "hz": 440.0, "duration": 1.0 }],
            "out": dir.join("bad.wav").to_string_lossy(),
            "waveform": "noise"
        });
        let error = server.render_tones(Parameters(request(bad))).await;
        assert!(error.err().unwrap().contains("unknown waveform"));
        assert!(!dir.join("bad.wav").exists());
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(server.presets().await.0.presets.len(), 6);
    }
}
