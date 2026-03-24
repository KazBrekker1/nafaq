mod connection;
mod ipc;
mod messages;
mod node;
mod protocol;

use clap::Parser;

#[derive(Parser)]
#[command(name = "nafaq-sidecar")]
struct Cli {
    /// WebSocket port for IPC with Electrobun
    #[arg(short, long, default_value = "9320")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("nafaq_sidecar=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    tracing::info!("nafaq-sidecar starting on port {}", cli.port);

    // TODO: wire up endpoint + ws server
    Ok(())
}
