use std::fs;

use silky_arc_tool::{handle_pack, handle_unpack};
use tempfile::tempdir;

#[test]
fn test_unpack() {
    let temp_dir = tempdir().unwrap();
    handle_unpack("./test_assets/test.arc", temp_dir.path()).unwrap();
    assert!(temp_dir.path().join("test.txt").exists());
    assert!(temp_dir.path().join("KT_A0000.OGG").exists());
}

#[test]
fn test_pack() {
    let temp_dir = tempdir().unwrap();
    let output_path = temp_dir.path().join("test.arc");
    let input_dir = temp_dir.path().join("test");
    fs::create_dir_all(&input_dir).unwrap();
    fs::write(input_dir.join("test.txt"), "test").unwrap();
    handle_pack(&input_dir, &output_path, false).unwrap();
    assert!(&output_path.exists());
    fs::remove_file(&output_path).unwrap();
    handle_pack(&input_dir, &output_path, true).unwrap();
    assert!(output_path.exists());
}
