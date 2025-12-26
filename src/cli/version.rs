use miette::Result;

/// This is the version of the multi CLI, pulled from Cargo.toml.
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Print the CLI version to stdout.
pub struct Version;

impl Version {
    pub fn new() -> Self {
        Self
    }

    /// Print the version and exit.
    pub fn dispatch(self) -> Result<()> {
        println!("{}", CLI_VERSION);
        Ok(())
    }
}
