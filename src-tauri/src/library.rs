// Michael Burgus — https://github.com/NeuralDrifter
// Shared library location and one-time migration from the old per-copy folders

use serde::Serialize;
use std::path::Path;
use walkdir::WalkDir;

/// Folder under the user's Documents that both editions share.
pub const LIBRARY_DIR_NAME: &str = "StandardIssue Library";

#[derive(Debug, Default, Serialize)]
pub struct MigrationReport {
    pub moved: usize,
    pub duplicates: usize,
    pub conflicts: Vec<String>,
    pub failures: Vec<String>,
}

impl MigrationReport {
    pub fn is_empty(&self) -> bool {
        self.moved == 0 && self.duplicates == 0 && self.conflicts.is_empty() && self.failures.is_empty()
    }
}

/// Move every file under `src` to the same relative path under `dst`.
/// Same-size files already at `dst` are treated as duplicates and removed from `src`;
/// different-size files are left in place and reported as conflicts.
pub fn migrate_dir(src: &Path, dst: &Path, report: &mut MigrationReport) {
    if !src.is_dir() {
        return;
    }

    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let from = entry.path();
        let Ok(rel) = from.strip_prefix(src) else { continue };
        let to = dst.join(rel);
        let label = rel.to_string_lossy().to_string();

        if let Ok(existing) = std::fs::metadata(&to) {
            let src_len = entry.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
            if existing.len() == src_len {
                match std::fs::remove_file(from) {
                    Ok(()) => report.duplicates += 1,
                    Err(e) => report.failures.push(format!("{label}: {e}")),
                }
            } else {
                report.conflicts.push(label);
            }
            continue;
        }

        match move_file(from, &to) {
            Ok(()) => report.moved += 1,
            Err(e) => report.failures.push(format!("{label}: {e}")),
        }
    }

    // Remove directories emptied by the move, deepest first. Non-empty ones fail and stay.
    for entry in WalkDir::new(src)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() {
            let _ = std::fs::remove_dir(entry.path());
        }
    }
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Cross-volume: copy, verify, then delete the source
    let copied = std::fs::copy(from, to)?;
    if copied != std::fs::metadata(from)?.len() {
        let _ = std::fs::remove_file(to);
        return Err(std::io::Error::other("size mismatch after copy"));
    }
    std::fs::remove_file(from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, data: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    #[test]
    fn moves_new_files_preserving_relative_paths() {
        let t = tempfile::tempdir().unwrap();
        let (src, dst) = (t.path().join("src"), t.path().join("dst"));
        write(&src.join("Fiction/Joyce/Dubliners.epub"), "book");

        let mut r = MigrationReport::default();
        migrate_dir(&src, &dst, &mut r);

        assert_eq!(r.moved, 1);
        assert_eq!(fs::read_to_string(dst.join("Fiction/Joyce/Dubliners.epub")).unwrap(), "book");
        assert!(!src.join("Fiction/Joyce/Dubliners.epub").exists());
    }

    #[test]
    fn deletes_same_size_duplicate_from_source() {
        let t = tempfile::tempdir().unwrap();
        let (src, dst) = (t.path().join("src"), t.path().join("dst"));
        write(&src.join("a.jpg"), "1234");
        write(&dst.join("a.jpg"), "abcd");

        let mut r = MigrationReport::default();
        migrate_dir(&src, &dst, &mut r);

        assert_eq!(r.duplicates, 1);
        assert!(!src.join("a.jpg").exists());
        assert_eq!(fs::read_to_string(dst.join("a.jpg")).unwrap(), "abcd");
    }

    #[test]
    fn leaves_size_conflict_in_place() {
        let t = tempfile::tempdir().unwrap();
        let (src, dst) = (t.path().join("src"), t.path().join("dst"));
        write(&src.join("a.epub"), "longer data");
        write(&dst.join("a.epub"), "short");

        let mut r = MigrationReport::default();
        migrate_dir(&src, &dst, &mut r);

        assert_eq!(r.conflicts, vec!["a.epub".to_string()]);
        assert!(src.join("a.epub").exists());
        assert_eq!(fs::read_to_string(dst.join("a.epub")).unwrap(), "short");
    }

    #[test]
    fn noop_when_source_missing() {
        let t = tempfile::tempdir().unwrap();
        let mut r = MigrationReport::default();
        migrate_dir(&t.path().join("nope"), &t.path().join("dst"), &mut r);
        assert!(r.is_empty());
        assert!(!t.path().join("dst").exists());
    }

    #[test]
    fn removes_empty_source_dirs() {
        let t = tempfile::tempdir().unwrap();
        let (src, dst) = (t.path().join("src"), t.path().join("dst"));
        write(&src.join("Poetry/Keats/odes.epub"), "x");

        let mut r = MigrationReport::default();
        migrate_dir(&src, &dst, &mut r);

        assert!(!src.exists());
    }
}
