mod cli;
mod logging;
mod server;

use clap::Parser;

use cli::Cli;
use server::run_server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    run_server().await
}
