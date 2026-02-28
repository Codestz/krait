use std::path::{Path, PathBuf};

use anyhow::Context;

/// Hash a file's contents with BLAKE3, returning hex-encoded digest.
///
/// # Errors
/// Returns an error if the file can't be read.
pub fn hash_file(path: &Path) -> anyhow::Result<String> {
    let data =
        std::fs::read(path).with_context(|| format!("failed to read: {}", path.display()))?;
    Ok(blake3::hash(&data).to_hex().to_string())
}

/// Hash multiple files in parallel using rayon.
///
/// Returns a vec of (path, hash) pairs. Files that can't be read are skipped.
#[must_use]
pub fn hash_files_parallel(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    use rayon::prelude::*;
    paths
        .par_iter()
        .filter_map(|p| hash_file(p).ok().map(|h| (p.clone(), h)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();

        let h1 = hash_file(&file).unwrap();
        let h2 = hash_file(&file).unwrap();
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }

    #[test]
    fn hash_changes_on_modify() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");

        std::fs::write(&file, "version 1").unwrap();
        let h1 = hash_file(&file).unwrap();

        std::fs::write(&file, "version 2").unwrap();
        let h2 = hash_file(&file).unwrap();

        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_parallel_matches_sequential() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..5)
            .map(|i| {
                let p = dir.path().join(format!("file{i}.txt"));
                std::fs::write(&p, format!("content {i}")).unwrap();
                p
            })
            .collect();

        let parallel = hash_files_parallel(&files);
        let sequential: Vec<(PathBuf, String)> = files
            .iter()
            .map(|p| (p.clone(), hash_file(p).unwrap()))
            .collect();

        assert_eq!(parallel.len(), sequential.len());
        for (p, s) in parallel.iter().zip(sequential.iter()) {
            assert_eq!(p.0, s.0);
            assert_eq!(p.1, s.1);
        }
    }

    #[test]
    fn hash_missing_file_returns_error() {
        let result = hash_file(Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn hash_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.txt");
        std::fs::write(&file, "").unwrap();

        let hash = hash_file(&file).unwrap();
        assert!(!hash.is_empty());
    }
}
