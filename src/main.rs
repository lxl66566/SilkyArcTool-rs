pub mod cli;
pub mod error;

use std::path::PathBuf;

use clap::Parser as _;
use cli::{Cli, Commands};
use path_absolutize::Absolutize;
use silky_arc_tool::{error::ArcError, handle_pack, handle_unpack};
use tap::Tap;

fn main() -> Result<(), ArcError> {
    _ = pretty_env_logger::formatted_builder()
        .filter_level(log::LevelFilter::Info)
        .format_timestamp_secs()
        .parse_default_env()
        .try_init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Pack {
            input,
            output,
            compress,
        } => {
            let output_path = output.unwrap_or_else(|| {
                // Default output: input + .arc in the same directory
                PathBuf::from(
                    input
                        .absolutize()
                        .expect("cannot absolutize input path")
                        .as_os_str()
                        .to_owned()
                        .tap_mut(|x| x.push(".arc")),
                )
            });
            // Check if derived path is same as input dir path, which is invalid
            if output_path == input {
                return Err(ArcError::CannotDeriveOutputPath(input));
            }
            handle_pack(&input, &output_path, compress)?;
        }
        Commands::Unpack { input, output } => {
            let output_dir = output.unwrap_or_else(|| {
                // Default output: input filename (no ext) in the same directory
                let mut derived = input.with_extension("");
                // If removing extension resulted in empty filename (e.g. ".arc"), use base name
                if derived.file_name().is_none() || derived.file_name().unwrap().is_empty() {
                    derived = input
                        .file_name()
                        .map(|name| input.with_file_name(name))
                        .unwrap_or_else(|| PathBuf::from("output_dir")); // Fallback
                }
                // Avoid unpacking into archive itself if names clash after removing ext
                if derived == input {
                    derived.set_file_name(format!(
                        "{}_unpacked",
                        derived.file_name().unwrap_or_default().to_string_lossy()
                    ));
                }
                derived
            });
            // Prevent unpacking directly into the archive file itself
            if output_dir == input {
                return Err(ArcError::CannotDeriveOutputPath(input));
            }

            handle_unpack(&input, &output_dir)?;
        }
    }

    Ok(())
}
