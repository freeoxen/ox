//! ox-gateway binary entry point. The broker assembly + axum serve loop
//! lands in Task 4.8; this is a placeholder so the crate builds.

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("ox-gateway starting (skeleton; routes pending)");
    // Broker assembly + axum serve come in Task 4.8.
    Ok(())
}
