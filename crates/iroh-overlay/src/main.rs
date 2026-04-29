use anyhow::Result;
use clap::Parser;
use rusternetes_iroh_overlay::{IrohOverlayConfig, IrohOverlayRuntime};
use rusternetes_storage::StorageBackend;
use std::sync::Arc;
use tokio::signal;
use tracing::info;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Node name to advertise under.
    #[arg(long)]
    node_name: String,

    /// Storage backend URL (same as Rusternetes).
    #[arg(long)]
    storage_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let storage = Arc::new(StorageBackend::new(&args.storage_url).await?);
    let config = IrohOverlayConfig::new(args.node_name);
    let runtime = IrohOverlayRuntime::start(Arc::clone(&storage), config).await?;

    info!(addr = ?runtime.local_endpoint_addr(), "iroh overlay running");
    signal::ctrl_c().await.ok();
    Ok(())
}
