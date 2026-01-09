use clap::ValueEnum;

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum EnableColors {
    Always,
    Never,
    #[default]
    Auto,
}
