use clap::ValueEnum;

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum EnableColors {
    Always,
    Never,
    Auto,
}

impl Default for EnableColors {
    fn default() -> Self {
        Self::Auto
    }
}
