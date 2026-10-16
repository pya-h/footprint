use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub fn copy_and_hash(src: &Path, dst_dir: &Path) -> Result<(PathBuf, String)> {
    let dst = dst_dir.join(src.file_name().context("source has no file name")?);
    fs::copy(src, &dst)?;
    let hash = sha256_file(&dst)?;
    Ok((dst, hash))
}

pub fn copy_db_set(src: &Path, dst_dir: &Path) -> Result<(PathBuf, String)> {
    let (dst, hash) = copy_and_hash(src, dst_dir)?;
    for ext in ["-wal", "-journal", "-shm"] {
        let side = src.with_extension("");
        let side = PathBuf::from(format!("{}{}", side.display(), ext));
        if side.exists() {
            let _ = copy_and_hash(&side, dst_dir)?;
        }
    }
    Ok((dst, hash))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}
