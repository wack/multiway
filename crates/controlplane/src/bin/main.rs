use clap::{CommandFactory, Parser};
use multiway::cli::Cli;

fn main() -> miette::Result<()> {
    let cli = Cli::parse();
    dispatch_command(cli)
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
