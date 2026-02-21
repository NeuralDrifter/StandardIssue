// Michael Burgus — https://github.com/NeuralDrifter
// Epub builder — shallow git clone + zip src/ into valid epub

use crate::books::{epub_path, repo_name, Book, BookStore};
use crate::enrich;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

/// Enrich a single book's metadata if it's missing categories.
async fn ensure_enriched(book: &mut Book, client: &reqwest::Client) {
    if !book.categories.is_empty() {
        return;
    }
    let rn = repo_name(book);
    if rn.is_empty() {
        return;
    }
    if let Ok(meta) = enrich::fetch_and_parse_opf(client, &rn).await {
        enrich::apply_metadata(book, &meta);
    }
}

const ORG: &str = "standardebooks";

pub struct DownloadResult {
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
}

pub async fn download_all(
    store: &BookStore,
    client: &reqwest::Client,
    epubs_dir: &Path,
    concurrency: usize,
    progress: impl Fn(usize, usize, String, String) + Send + Sync + 'static,
    cancel: Arc<AtomicBool>,
) -> DownloadResult {
    std::fs::create_dir_all(epubs_dir).ok();

    let mut books = store.all();

    // Enrich any books missing categories so they get proper folder paths
    let mut enriched_any = false;
    for book in &mut books {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        if book.categories.is_empty() && !repo_name(book).is_empty() {
            ensure_enriched(book, client).await;
            enriched_any = true;
        }
    }
    if enriched_any {
        store.set_books(books.clone());
        let _ = store.save();
    }

    struct Job {
        book: Book,
    }

    let mut to_download = Vec::new();
    let mut skipped_count = 0usize;

    for b in &books {
        let rn = repo_name(b);
        if rn.is_empty() {
            continue;
        }
        let dest = epub_path(epubs_dir, b);
        if !dest.as_os_str().is_empty() && dest.exists() {
            skipped_count += 1;
            continue;
        }
        // Also check flat path for backward compat
        let flat = epubs_dir.join(format!("{rn}.epub"));
        if flat.exists() {
            skipped_count += 1;
            continue;
        }
        to_download.push(Job { book: b.clone() });
    }

    let total = to_download.len();
    let succeeded = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    let sem = Arc::new(Semaphore::new(concurrency));
    let progress = Arc::new(progress);

    let mut handles = Vec::new();

    for (i, job) in to_download.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }

        let permit = sem.clone().acquire_owned().await.unwrap();
        let cancel = cancel.clone();
        let succeeded = succeeded.clone();
        let failed = failed.clone();
        let progress = progress.clone();
        let book = job.book.clone();
        let epubs_dir = epubs_dir.to_path_buf();

        let handle = tokio::spawn(async move {
            let _permit = permit;

            if cancel.load(Ordering::SeqCst) {
                return;
            }

            let rn = repo_name(&book);
            let dest = epub_path(&epubs_dir, &book);
            let title = book.title.clone();

            match build_epub_from_repo(&rn, &dest).await {
                Ok(()) => {
                    succeeded.fetch_add(1, Ordering::Relaxed);
                    progress(i + 1, total, title, "downloaded".into());
                }
                Err(e) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    progress(i + 1, total, title, format!("error: {e}"));
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    DownloadResult {
        succeeded: succeeded.load(Ordering::Relaxed),
        skipped: skipped_count,
        failed: failed.load(Ordering::Relaxed),
    }
}

/// Build a single epub by repo name. Enriches metadata first if missing.
pub async fn download_single_book(
    store: &BookStore,
    client: &reqwest::Client,
    repo: &str,
    epubs_dir: &Path,
) -> Result<PathBuf, String> {
    let mut books = store.all();
    let idx = books.iter().position(|b| repo_name(b) == repo);

    let dest = if let Some(i) = idx {
        // Enrich if missing categories
        if books[i].categories.is_empty() {
            ensure_enriched(&mut books[i], client).await;
            store.set_books(books.clone());
            let _ = store.save();
        }
        epub_path(epubs_dir, &books[i])
    } else {
        epubs_dir.join("Uncategorized").join(format!("{repo}.epub"))
    };

    if dest.exists() {
        return Ok(dest);
    }

    build_epub_from_repo(repo, &dest).await?;
    Ok(dest)
}

async fn build_epub_from_repo(repo: &str, dest_path: &Path) -> Result<(), String> {
    // Ensure parent directory exists
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let tmp_dir = tempfile::tempdir().map_err(|e| format!("temp dir: {e}"))?;
    let clone_dir = tmp_dir.path().join(repo);
    let repo_url = format!("https://github.com/{ORG}/{repo}.git");

    // Shallow clone
    let output = tokio::process::Command::new("git")
        .args(["clone", "--depth", "1", "--quiet", &repo_url])
        .arg(&clone_dir)
        .output()
        .await
        .map_err(|e| format!("git clone spawn: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("clone failed: {stderr}"));
    }

    let src_dir = clone_dir.join("src");
    if !src_dir.exists() {
        return Err(format!("no src/ directory in {repo}"));
    }

    let mimetype_path = src_dir.join("mimetype");
    if !mimetype_path.exists() {
        return Err(format!("no mimetype file in {repo}/src/"));
    }

    // Zip in a blocking task to avoid blocking the async runtime
    let dest = dest_path.to_path_buf();
    tokio::task::spawn_blocking(move || zip_epub(&dest, &src_dir, &mimetype_path))
        .await
        .map_err(|e| format!("zip task panic: {e}"))?
}

fn zip_epub(epub_path: &Path, src_dir: &Path, mimetype_path: &Path) -> Result<(), String> {
    let file = std::fs::File::create(epub_path).map_err(|e| format!("create epub: {e}"))?;
    let mut zw = zip::ZipWriter::new(file);

    // mimetype must be first, uncompressed (EPUB spec)
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zw.start_file("mimetype", options)
        .map_err(|e| format!("zip mimetype: {e}"))?;
    let mime_data = std::fs::read(mimetype_path).map_err(|e| format!("read mimetype: {e}"))?;
    zw.write_all(&mime_data)
        .map_err(|e| format!("write mimetype: {e}"))?;

    // Collect and sort all other files
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in WalkDir::new(src_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() && path != mimetype_path {
            files.push(path.to_path_buf());
        }
    }
    files.sort();

    let options =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for path in &files {
        let rel = path
            .strip_prefix(src_dir)
            .map_err(|e| format!("strip prefix: {e}"))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");

        zw.start_file(&rel_str, options)
            .map_err(|e| format!("zip {rel_str}: {e}"))?;
        let data = std::fs::read(path).map_err(|e| format!("read {rel_str}: {e}"))?;
        zw.write_all(&data)
            .map_err(|e| format!("write {rel_str}: {e}"))?;
    }

    zw.finish().map_err(|e| format!("zip finish: {e}"))?;
    Ok(())
}
