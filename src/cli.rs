use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(clap::Subcommand, Debug)]
pub enum Commands {
    /// Packs a directory into a .arc file
    Pack {
        /// Input directory path
        #[arg(required = true)]
        input: PathBuf,

        /// Output archive file path (optional)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Enable LZSS compression
        #[arg(short, long, default_value_t = false)]
        compress: bool,
    },
    /// Unpacks a .arc file into a directory
    Unpack {
        /// Input archive file path
        #[arg(required = true)]
        input: PathBuf,

        /// Output directory path (optional)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}
