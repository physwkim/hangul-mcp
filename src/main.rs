use hangul_mcp::server::HangulMcp;
use rmcp::transport::stdio;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = HangulMcp::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
