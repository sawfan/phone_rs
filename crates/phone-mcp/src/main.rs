use std::process::ExitCode;

use phone_mcp::PhoneMcpServer;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [command] if command == "serve" => {}
        [command] if command == "summary" => {
            let names = PhoneMcpServer::tool_names();
            println!("phone-mcp: {} tools: {}", names.len(), names.join(", "));
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("usage: phone-mcp [serve|summary]");
            return ExitCode::FAILURE;
        }
    }
    let result = async {
        PhoneMcpServer::new()
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|error| format!("failed to start the phone MCP server: {error}"))?
            .waiting()
            .await
            .map(|_| ())
            .map_err(|error| format!("the phone MCP server stopped with an error: {error}"))
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
