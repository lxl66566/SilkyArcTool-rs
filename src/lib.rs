pub mod cli;
pub mod error;

use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf}, // Mutex needed for parallel writing to the same archive potentially
};

use byteorder::{BigEndian, LittleEndian, ReadBytesExt, WriteBytesExt}; /* For endianness
                                                                         * control */
use encoding_rs::SHIFT_JIS; // CP932 encoding
use log::{debug, error, info, warn};
use lzss::{Lzss, SliceReader, SliceWriter};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::error::ArcError; // To easily walk directories for packing

// --- .arc File Format ---
// Global Header (4 bytes):
//   - metadata_end_offset (u32, Little Endian): Offset marking the end of
//     metadata / start of file data.
//
// Metadata Section (from offset 4 to metadata_end_offset):
//   Repeated File Entry structures:
//     - name_length (u8): Length of the encrypted_name.
//     - encrypted_name (Vec<u8>): Filename encrypted with a specific algorithm
//       (CP932 + byte shift).
//     - compressed_size (u32, Big Endian): Size of the file data block in the
//       archive.
//     - original_size (u32, Big Endian): Original size of the file before
//       compression.
//     - file_data_offset (u32, Big Endian): Absolute offset of this file's data
//       block from the start of the archive.
//
// File Data Section (from metadata_end_offset to EOF):
//   Concatenated file data blocks, potentially LZSS compressed if
// compressed_size != original_size. --- End of Format ---

// Define the specific LZSS parameters based on the Python code analysis
// N=4096 (Buffer Size) => EI=12 (since 1 << 12 = 4096)
// F=18 (Max Match Length)
// Threshold=2 (Min Match Length)
// For the lzss crate: F = (1 << EJ) + Threshold => 18 = (1 << EJ) + 2 => 16 = 1
// << EJ => EJ=4 Padding byte C = 0x00 (from python default)

type SilkyLzss = Lzss<12, 4, 0x00, { 1 << 12 }, { 2 << 12 }>;

#[allow(dead_code)]
#[derive(Debug)]
struct FileEntry {
    encrypted_name: Vec<u8>,
    name: String, // Decrypted name
    compressed_size: u32,
    original_size: u32,
    offset: u32,
}

// --- Name Encryption/Decryption ---
// (Ported from Python's decrypt_name)
pub fn decrypt_name(encrypted: &[u8]) -> Result<String, ArcError> {
    let mut tester = Vec::with_capacity(encrypted.len());
    for (k, &byte) in encrypted.iter().rev().enumerate() {
        // k starts at 0, Python's k started at 1
        tester.push(byte.wrapping_add((k + 1) as u8));
    }
    tester.reverse(); // Because we pushed in reverse order
    let (cow, _encoding_used, had_errors) = SHIFT_JIS.decode(&tester);
    if had_errors {
        Err(ArcError::NameDecodeError(encrypted.to_vec()))
    } else {
        Ok(cow.into_owned())
    }
}

// (Ported from Python's encrypt_name)
pub fn encrypt_name(name: &str) -> Result<Vec<u8>, ArcError> {
    let (encoded_bytes, _encoding_used, had_errors) = SHIFT_JIS.encode(name);
    if had_errors {
        return Err(ArcError::NameEncodeError(name.to_string()));
    }

    let mut tester = Vec::with_capacity(encoded_bytes.len());
    for (k, &byte) in encoded_bytes.iter().rev().enumerate() {
        // k starts at 0, Python's k started at 1
        tester.push(byte.wrapping_sub((k + 1) as u8));
    }
    tester.reverse(); // Because we pushed in reverse order
    Ok(tester)
}

// --- Unpack Logic ---
pub fn handle_unpack(
    input_path: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
) -> Result<(), ArcError> {
    let input_path = input_path.as_ref();
    let output_dir = output_dir.as_ref();

    info!("Starting unpack of: {:?}", input_path);
    info!("Output directory: {:?}", output_dir);

    if !input_path.exists() {
        return Err(ArcError::NotFound(input_path.to_path_buf()));
    }
    fs::create_dir_all(output_dir)?; // Create output dir if needed

    let input_file = File::open(input_path)?;
    let mut reader = BufReader::new(input_file);

    // 1. Read global header
    let metadata_end_offset = reader.read_u32::<LittleEndian>()?;
    debug!("Metadata ends at offset: {}", metadata_end_offset);

    // 2. Read metadata entries
    let mut file_entries: Vec<FileEntry> = Vec::new();
    while reader.stream_position()? < metadata_end_offset as u64 {
        let name_len = reader.read_u8()?;
        let mut encrypted_name_buf = vec![0u8; name_len as usize];
        reader.read_exact(&mut encrypted_name_buf)?;

        let compressed_size = reader.read_u32::<BigEndian>()?;
        let original_size = reader.read_u32::<BigEndian>()?;
        let offset = reader.read_u32::<BigEndian>()?;

        let name = decrypt_name(&encrypted_name_buf)?;
        //println!("  Found entry: Name='{}', CompSize={}, OrigSize={}, Offset={}",
        // name, compressed_size, original_size, offset);

        file_entries.push(FileEntry {
            encrypted_name: encrypted_name_buf, // Keep for potential packing later if needed
            name,
            compressed_size,
            original_size,
            offset,
        });
    }
    info!("Read {} file entries from metadata.", file_entries.len());

    // 3. Extract files (using Rayon for parallelism)
    // We need to be careful with file handles for parallel seeking/reading.
    // Cloning the reader or opening new handles per thread is necessary.
    // Using pread might be more efficient if the OS supports it well, but less
    // portable. Let's reopen the file for each parallel task to ensure thread
    // safety.
    let arc_path_clone = input_path.to_path_buf(); // Clone for parallel use

    file_entries
        .par_iter()
        .map(|entry| -> Result<(), ArcError> {
            let output_file_path = output_dir.join(&entry.name);

            // Ensure parent directory exists for the output file
            if let Some(parent) = output_file_path.parent() {
                fs::create_dir_all(parent)?;
            }

            // Open a *new* handle to the archive for this thread/task
            let file = File::open(&arc_path_clone)?;
            let mut task_reader = BufReader::new(file);

            // Seek and read the (potentially compressed) data
            task_reader.seek(SeekFrom::Start(entry.offset as u64))?;
            let mut compressed_data = vec![0u8; entry.compressed_size as usize];
            task_reader.read_exact(&mut compressed_data)?;

            let final_data = if entry.compressed_size != entry.original_size {
                // Decompress using LZSS
                let mut decompressed_data = vec![0u8; entry.original_size as usize * 4]; // 4 times buffer size of the original data
                let result = SilkyLzss::decompress_stack(
                    SliceReader::new(&compressed_data),
                    SliceWriter::new(&mut decompressed_data),
                )
                .map_err(|e| ArcError::LzssDecompressError(e.to_string()))?;
                decompressed_data.truncate(result);
                decompressed_data
            } else {
                // Data is not compressed
                compressed_data
            };

            // Write the final data to the output file
            let mut output_file = File::create(&output_file_path)?;
            output_file.write_all(&final_data)?;

            info!("Unpacked: {}", entry.name);
            Ok(())
        })
        .collect::<Result<Vec<_>, _>>()?; // Collect results and propagate first error

    info!("=== Unpack finished ===");
    Ok(())
}

// --- Pack Logic ---

// Intermediate structure for packing
#[derive(Debug)]
struct PackFileInfo {
    relative_path: PathBuf,
    full_path: PathBuf,
    encrypted_name: Vec<u8>,
    original_size: u32,
    // These are determined after processing
    compressed_data: Option<Vec<u8>>, // Holds compressed or original data
    compressed_size: u32,
    offset: u32, // Placeholder
}

pub fn handle_pack(
    input_dir: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
    compress: bool,
) -> Result<(), ArcError> {
    let input_dir = input_dir.as_ref();
    let output_path = output_path.as_ref();

    info!("Starting pack of directory: {:?}", input_dir);
    info!("Output archive: {:?}", output_path);
    info!("Compression enabled: {}", compress);

    if !input_dir.is_dir() {
        return Err(ArcError::NotFound(input_dir.to_path_buf()));
    }

    // 1. Collect all files recursively and prepare initial metadata
    let mut files_to_pack: Vec<PackFileInfo> = Vec::new();
    for entry_result in WalkDir::new(input_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry_result.path();
        if path.is_file() {
            let relative_path = path.strip_prefix(input_dir)?.to_path_buf();
            // Convert path separators to ensure consistency if needed (e.g., always '\')
            // Silky engine likely expects backslashes. Let's try converting.
            let name_str_for_encrypt = relative_path.to_string_lossy().replace("/", "\\");

            let encrypted_name = encrypt_name(&name_str_for_encrypt)?;
            let metadata = fs::metadata(path)?;

            files_to_pack.push(PackFileInfo {
                relative_path, // Keep original relative path for clarity
                full_path: path.to_path_buf(),
                encrypted_name,
                original_size: metadata.len() as u32,
                compressed_data: None, // Will be filled next
                compressed_size: 0,    // Placeholder
                offset: 0,             // Placeholder
            });
        }
    }

    if files_to_pack.is_empty() {
        info!("Input directory is empty. Creating an empty archive.");
        // Create an empty archive file (header only)
        let mut writer = BufWriter::new(File::create(output_path)?);
        writer.write_u32::<LittleEndian>(4)?; // metadata_end_offset = 4 (no entries)
        writer.flush()?; // Ensure buffer is written
        return Ok(());
    }

    // 2. Read file data and compress in parallel (if enabled)
    let _processed_files = files_to_pack
        .par_iter_mut() // Use par_iter_mut to modify items in place
        .map(|file_info| -> Result<(), ArcError> {
            let file_data = fs::read(&file_info.full_path)?;
            assert_eq!(file_data.len() as u32, file_info.original_size); // Sanity check

            if compress && file_info.original_size > 0 {
                // Don't try to compress empty files
                let mut compressed_output: Vec<u8> = vec![0; file_info.original_size as usize * 2]; // Start with double of original size capacity
                let compress_result = SilkyLzss::compress_stack(
                    SliceReader::new(&file_data),
                    SliceWriter::new(&mut compressed_output),
                );

                match compress_result {
                    Ok(compressed_len) => {
                        // Only use compressed data if it's actually smaller
                        if (compressed_len as u32) < file_info.original_size {
                            compressed_output.truncate(compressed_len);
                            file_info.compressed_data = Some(compressed_output);
                            file_info.compressed_size = compressed_len as u32;
                            info!(
                                "Compressed: {:?} ({} -> {} bytes)",
                                file_info.relative_path,
                                file_info.original_size,
                                file_info.compressed_size
                            );
                        } else {
                            // Compression didn't help, store original data
                            file_info.compressed_data = Some(file_data);
                            file_info.compressed_size = file_info.original_size;
                            info!(
                                "Storing uncompressed (LZSS ineffective): {:?}",
                                file_info.relative_path
                            );
                        }
                    }
                    Err(e) => {
                        // Handle compression error, e.g., log it and store uncompressed
                        warn!(
                            "LZSS compression failed for {:?}: {:?}. Storing uncompressed.",
                            file_info.relative_path, e
                        );
                        file_info.compressed_data = Some(file_data);
                        file_info.compressed_size = file_info.original_size;
                        // Optionally return an error: return
                        // Err(ArcError::LzssCompressError(e));
                    }
                }
            } else {
                // Store original data if compression is disabled or file is empty
                file_info.compressed_data = Some(file_data);
                file_info.compressed_size = file_info.original_size;
                if compress {
                    // Only print this message if compression was attempted but file was empty
                    info!(
                        "Storing uncompressed (empty file): {:?}",
                        file_info.relative_path
                    );
                } else {
                    info!("Storing uncompressed: {:?}", file_info.relative_path);
                }
            }
            Ok(())
        })
        .collect::<Result<Vec<_>, ArcError>>()?; // Collect results and propagate errors

    // 3. Calculate metadata size and file offsets (Sequentially)
    let mut current_offset = 4u32; // Start with global header size
    for file_info in files_to_pack.iter_mut() {
        // Now iterate mutably on the original vector
        current_offset += 1 // name_length
                        + file_info.encrypted_name.len() as u32
                        + 4 // compressed_size
                        + 4 // original_size
                        + 4; // offset
    }
    let metadata_block_size = current_offset - 4;
    debug!("Calculated metadata_block_size: {metadata_block_size}");

    // Assign final offsets
    for file_info in files_to_pack.iter_mut() {
        file_info.offset = current_offset;
        // Ensure compressed_data is Some (should be unless there was an error before)
        let data_len = file_info.compressed_data.as_ref().map_or(0, |d| d.len());
        // Sanity check: data length should match calculated compressed_size
        if data_len as u32 != file_info.compressed_size {
            error!(
                "Internal inconsistency for {:?}: stored data length {} != calculated compressed_size {}",
                file_info.relative_path, data_len, file_info.compressed_size
            );
            // Potentially return an error here
        }
        current_offset += file_info.compressed_size;
        //println!("  Assigning offset {} to {:?}", file_info.offset,
        // file_info.relative_path);
    }

    // 4. Write the archive file (Sequentially)
    let output_file = File::create(output_path)?;
    let mut writer = BufWriter::new(output_file);

    // Write global header
    writer.write_u32::<LittleEndian>(metadata_block_size)?;

    // Write metadata entries
    for file_info in &files_to_pack {
        // Iterate immutably now
        writer.write_u8(file_info.encrypted_name.len() as u8)?;
        writer.write_all(&file_info.encrypted_name)?;
        writer.write_u32::<BigEndian>(file_info.compressed_size)?;
        writer.write_u32::<BigEndian>(file_info.original_size)?;
        writer.write_u32::<BigEndian>(file_info.offset)?;
    }
    info!("Metadata written.");

    // Write file data blocks
    for file_info in files_to_pack {
        // Consume the vector or iterate again
        if let Some(data) = file_info.compressed_data {
            // Sanity check seek position (optional but good)
            let current_pos = writer.stream_position()?;
            if current_pos != file_info.offset as u64 {
                error!(
                    "Mismatch writing file data for {:?}. Expected offset {}, current position {}",
                    file_info.relative_path, file_info.offset, current_pos
                );
                // Attempt to seek to the correct position
                writer.seek(SeekFrom::Start(file_info.offset as u64))?;
            }
            writer.write_all(&data)?;
            info!("Wrote data for: {:?}", file_info.relative_path);
        } else {
            // This shouldn't happen if processing was successful
            return Err(ArcError::InvalidFormat(format!(
                "Missing processed data for {:?}",
                file_info.relative_path
            )));
        }
    }
    info!("File data written.");

    writer.flush()?; // Ensure all buffered data is written to the file
    info!("=== Pack finished ===");
    Ok(())
}
