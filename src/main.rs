use anyhow::Result;
use clap::{Parser, Subcommand};
use lzss::{Lzss, SliceReader, SliceWriter};
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::{self, Read, Seek as _, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use walkdir::WalkDir;

// 定义LZSS类型
type SilkyLzss = Lzss<10, 4, 0x20, { 1 << 10 }, { 2 << 10 }>;

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Pack files into a Silky archive
    Pack {
        /// Input directory to pack
        input: PathBuf,
        /// Output archive file (optional)
        #[arg(short, long)]
        output_dir: Option<PathBuf>,
    },
    /// Unpack a Silky archive
    Unpack {
        /// Input archive file to unpack
        input: PathBuf,
        /// Output directory (optional)
        #[arg(short, long)]
        output_dir: Option<PathBuf>,
    },
}

// 文件头结构
#[derive(Debug)]
struct FileEntry {
    name: String,
    compressed_size: u32,
    original_size: u32,
    offset: u32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Pack { input, output_dir } => {
            let output = output_dir.unwrap_or_else(|| {
                let mut path = input.clone();
                path.set_extension("arc");
                path
            });
            pack_archive(&input, &output)?;
        }
        Commands::Unpack { input, output_dir } => {
            let output = output_dir.unwrap_or_else(|| {
                let stem = input.file_stem().unwrap_or_default();
                PathBuf::from(stem)
            });
            unpack_archive(&input, &output)?;
        }
    }

    Ok(())
}

fn pack_archive(input_dir: &Path, output_file: &Path) -> Result<()> {
    // 收集所有文件
    let mut files = Vec::new();
    for entry in WalkDir::new(input_dir) {
        let entry = entry?;
        if entry.file_type().is_file() {
            files.push(entry.path().to_owned());
        }
    }

    // 创建临时缓冲区存储压缩数据
    let mut compressed_data = Vec::new();
    let mut file_entries = Vec::new();
    let mut current_offset = 0u32;

    // 并行压缩所有文件
    let compressed_results: Vec<_> = files
        .par_iter()
        .map(|path| {
            let content = fs::read(path)?;
            let compressed = compress_data(&content)?;
            let rel_path = path.strip_prefix(input_dir)?.to_string_lossy().into_owned();

            Ok((rel_path, content.len(), compressed))
        })
        .collect::<Result<_>>()?;

    // 计算文件头大小
    let mut header_size = 4; // 文件数量的4字节
    for (name, _, _) in &compressed_results {
        header_size += 1 + name.len() + 12; // 名字长度(1) + 名字 + 压缩大小(4) + 原始大小(4) + 偏移量(4)
    }

    current_offset = header_size as u32;

    // 构建文件条目
    for (name, original_size, compressed) in compressed_results {
        let compressed_size = compressed.len() as u32;
        let entry = FileEntry {
            name,
            compressed_size,
            original_size: original_size as u32,
            offset: current_offset,
        };
        file_entries.push(entry);
        compressed_data.extend(compressed);
        current_offset += compressed_size;
    }

    // 写入文件
    let mut output = File::create(output_file)?;

    // 写入文件数量
    output.write_all(&(file_entries.len() as u32).to_le_bytes())?;

    // 写入文件头
    for entry in &file_entries {
        output.write_all(&(entry.name.len() as u8).to_le_bytes())?;
        output.write_all(entry.name.as_bytes())?;
        output.write_all(&entry.compressed_size.to_be_bytes())?;
        output.write_all(&entry.original_size.to_be_bytes())?;
        output.write_all(&entry.offset.to_be_bytes())?;
    }

    // 写入压缩数据
    output.write_all(&compressed_data)?;

    Ok(())
}

fn unpack_archive(input_file: &Path, output_dir: &Path) -> Result<()> {
    let file = File::open(input_file)?;
    let file = Mutex::new(file);

    // 读取文件数量
    let mut count_buf = [0u8; 4];
    file.lock().unwrap().read_exact(&mut count_buf)?;
    let file_count = u32::from_le_bytes(count_buf);

    // 读取文件头
    let mut entries = Vec::new();
    {
        let mut file = file.lock().unwrap();
        for _ in 0..file_count {
            let mut name_len = [0u8; 1];
            file.read_exact(&mut name_len)?;
            let name_len = name_len[0] as usize;

            let mut name = vec![0u8; name_len];
            file.read_exact(&mut name)?;
            let name = String::from_utf8(name)?;

            let mut size_buf = [0u8; 4];

            file.read_exact(&mut size_buf)?;
            let compressed_size = u32::from_be_bytes(size_buf);

            file.read_exact(&mut size_buf)?;
            let original_size = u32::from_be_bytes(size_buf);

            file.read_exact(&mut size_buf)?;
            let offset = u32::from_be_bytes(size_buf);

            entries.push(FileEntry {
                name,
                compressed_size,
                original_size,
                offset,
            });
        }
    }

    // 并行解压文件
    entries.par_iter().try_for_each(|entry| {
        let mut compressed = vec![0u8; entry.compressed_size as usize];
        {
            let mut file = file.lock().unwrap();
            file.seek(io::SeekFrom::Start(entry.offset as u64))?;
            file.read_exact(&mut compressed)?;
        }

        let decompressed = if entry.compressed_size != entry.original_size {
            decompress_data(&compressed)?
        } else {
            compressed
        };

        let output_path = output_dir.join(&entry.name);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output_path, decompressed)?;

        anyhow::Ok(())
    })?;

    Ok(())
}

fn compress_data(data: &[u8]) -> Result<Vec<u8>> {
    let mut output = vec![0u8; data.len() * 2]; // 预分配足够的空间
    let written = SilkyLzss::compress_stack(SliceReader::new(data), SliceWriter::new(&mut output))?;
    output.truncate(written);
    Ok(output)
}

fn decompress_data(data: &[u8]) -> Result<Vec<u8>> {
    let mut output = vec![0u8; data.len() * 4]; // 预分配足够的空间
    let written =
        SilkyLzss::decompress_stack(SliceReader::new(data), SliceWriter::new(&mut output))?;
    output.truncate(written);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_pack_arc() {
        let temp_dir = tempdir().unwrap();
        let input_dir = temp_dir.path().join("input");
        let output_file = temp_dir.path().join("output.arc");
        fs::create_dir(&input_dir).unwrap();
        fs::write(input_dir.join("test.txt"), "test").unwrap();
        pack_archive(&input_dir, &output_file).unwrap();
        assert!(output_file.exists());
    }

    #[test]
    fn test_unpack_arc() {
        let temp_dir = tempdir().unwrap();
        let input_file = temp_dir.path().join("input.arc");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir(&output_dir).unwrap();
        fs::write(&input_file, include_bytes!("../test_assets/test.arc")).unwrap();
        unpack_archive(&input_file, &output_dir).unwrap();
        assert!(output_dir.join("KT_A0000.OGG").exists());
    }
}
