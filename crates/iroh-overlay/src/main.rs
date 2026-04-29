use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Example flag
    #[arg(long, default_value_t = false)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    println!("iroh-overlay stub");
    Ok(())
}
