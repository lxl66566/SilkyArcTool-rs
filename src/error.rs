use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ArcError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("File not found: {0}")]
    NotFound(PathBuf),
    #[error("Invalid .arc format: {0}")]
    InvalidFormat(String),
    #[error("Failed to decode filename (CP932): {0:?}")]
    NameDecodeError(Vec<u8>),
    #[error("Failed to encode filename (CP932): {0}")]
    NameEncodeError(String),
    #[error("LZSS compression error: {0:?}")]
    LzssCompressError(String),
    #[error("LZSS decompression error: {0:?}")]
    LzssDecompressError(String),
    #[error("Walkdir error: {0}")]
    WalkdirError(#[from] walkdir::Error),
    #[error("Path strip prefix error: {0}")]
    StripPrefixError(#[from] std::path::StripPrefixError),
    #[error("Cannot get filename from path: {0:?}")]
    NoFilename(PathBuf),
    #[error("Output path is not specified and cannot be derived from input: {0:?}")]
    CannotDeriveOutputPath(PathBuf),
}
