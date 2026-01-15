use clap::{CommandFactory, Parser};
use multiway::cli::{Cli, LogFormat};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> miette::Result<()> {
    let cli = Cli::parse();

    // Initialize logging before dispatching commands
    init_logging(&cli)?;

    dispatch_command(cli)
}

fn init_logging(cli: &Cli) -> miette::Result<()> {
    // Build the filter from the log level, or fall back to RUST_LOG env var
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cli.log_level.to_string()));

    match cli.log_format {
        LogFormat::JSON => {
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
        LogFormat::Text => {
            tracing_subscriber::registry()
                .with(filter)
                .with(tracing_subscriber::fmt::layer())
                .init();
        }
    }

    Ok(())
}

fn empty_command() -> miette::Result<()> {
    Cli::command()
        .print_long_help()
        .expect("unable to print help message");
    Ok(())
}

fn dispatch_command(cli: Cli) -> miette::Result<()> {
    match &cli.cmd {
        None => empty_command(),
        Some(cmd) => cmd.clone().dispatch(),
    }
}
