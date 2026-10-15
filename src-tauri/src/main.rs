// Michael Burgus — https://github.com/NeuralDrifter
// Pure Tauri v2 Rust backend for Standard Ebooks Manager

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ebook_manager::books::{Book, BookStore};
use ebook_manager::catalog;
use ebook_manager::covers;
use ebook_manager::download;
use ebook_manager::enrich;
use ebook_manager::instance::{self, Acquire, HolderInfo, InstanceLock};
use ebook_manager::library::{self, MigrationReport};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Which edition this build is. Shown to the other edition while this one holds the lock.
const EDITION: &str = "Simple";
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

pub struct AppState {
    pub store: BookStore,
    pub operation: Mutex<Option<String>>,
    pub cancel: Arc<AtomicBool>,
    pub client: reqwest::Client,
    /// True once this process holds the instance lock and has loaded its data.
    ready: AtomicBool,
    lock: Mutex<Option<InstanceLock>>,
    data_dir: PathBuf,
    project_dir: PathBuf,
}

impl AppState {
    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn ensure_ready(&self) -> Result<(), String> {
        if self.is_ready() {
            Ok(())
        } else {
            Err("not ready: another copy of StandardIssue is running".to_string())
        }
    }

    fn start_op(&self, name: &str) -> bool {
        if !self.is_ready() {
            return false;
        }
        let mut op = self.operation.lock().unwrap();
        if op.is_some() {
            return false;
        }
        self.cancel.store(false, Ordering::SeqCst);
        *op = Some(name.to_string());
        true
    }

    fn finish_op(&self) {
        *self.operation.lock().unwrap() = None;
    }

    fn current_op(&self) -> Option<String> {
        self.operation.lock().unwrap().clone()
    }
}

// --- Serializable response types ---

#[derive(Serialize)]
struct BooksResponse {
    books: Vec<BookWithStatus>,
    total: usize,
    page: usize,
    per_page: usize,
    pages: usize,
}

#[derive(Serialize)]
struct BookWithStatus {
    #[serde(flatten)]
    book: Book,
    downloaded: bool,
}

#[derive(Serialize)]
struct StatsResponse {
    total: usize,
    downloaded: usize,
    operation: Option<String>,
}

#[derive(Serialize)]
struct SearchResponse {
    books: Vec<BookWithStatus>,
    total: usize,
}

// --- Simple commands ---

#[tauri::command]
fn get_books(state: tauri::State<'_, Arc<AppState>>, page: usize, per_page: usize) -> BooksResponse {
    let per_page = if per_page == 0 { 50 } else { per_page };
    let page = if page == 0 { 1 } else { page };

    let books = state.store.all();
    let total = books.len();
    let pages = total.div_ceil(per_page);

    let start = (page - 1) * per_page;
    if start >= total {
        return BooksResponse {
            books: vec![],
            total,
            page,
            per_page,
            pages,
        };
    }
    let end = (start + per_page).min(total);

    let result: Vec<BookWithStatus> = books[start..end]
        .iter()
        .map(|b| BookWithStatus {
            downloaded: state.store.is_downloaded(b),
            book: b.clone(),
        })
        .collect();

    BooksResponse {
        books: result,
        total,
        page,
        per_page,
        pages,
    }
}

#[tauri::command]
fn search_books(state: tauri::State<'_, Arc<AppState>>, query: String) -> SearchResponse {
    if query.is_empty() {
        return SearchResponse {
            books: vec![],
            total: 0,
        };
    }
    let results = state.store.search(&query);
    let total = results.len();
    let books: Vec<BookWithStatus> = results
        .iter()
        .map(|b| BookWithStatus {
            downloaded: state.store.is_downloaded(b),
            book: b.clone(),
        })
        .collect();
    SearchResponse { books, total }
}

#[tauri::command]
fn get_stats(state: tauri::State<'_, Arc<AppState>>) -> StatsResponse {
    StatsResponse {
        total: state.store.count(),
        downloaded: state.store.count_downloaded(),
        operation: state.current_op(),
    }
}

#[tauri::command]
fn cancel_operation(state: tauri::State<'_, Arc<AppState>>) -> serde_json::Value {
    let op = state.current_op();
    if op.is_none() {
        return serde_json::json!({"status": "nothing running"});
    }
    state.cancel.store(true, Ordering::SeqCst);
    serde_json::json!({"status": "cancelling", "operation": op})
}

// --- Long-running operation commands ---

#[tauri::command]
async fn sync_catalog(handle: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    state.ensure_ready()?;
    if !state.start_op("sync") {
        return Err(format!("operation {:?} already running", state.current_op()));
    }

    let state = Arc::clone(&state);
    let handle2 = handle.clone();

    let result = tokio::task::spawn(async move {
        let progress = |msg: String| {
            let _ = handle2.emit("op-progress", serde_json::json!({"message": msg}));
        };

        let r = catalog::sync_catalog(&state.store, &state.client, progress).await;
        state.finish_op();
        r
    })
    .await
    .map_err(|e| format!("task panic: {e}"))?;

    match result {
        Ok(summary) => {
            let _ = handle.emit("op-done", &summary);
            Ok(summary)
        }
        Err(e) => {
            let _ = handle.emit("op-error", serde_json::json!({"error": e}));
            Err(e)
        }
    }
}

#[tauri::command]
async fn enrich_all(handle: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    state.ensure_ready()?;
    if !state.start_op("enrich") {
        return Err(format!("operation {:?} already running", state.current_op()));
    }

    let state = Arc::clone(&state);
    let handle2 = handle.clone();

    let result = tokio::task::spawn(async move {
        let cancel = Arc::clone(&state.cancel);
        let progress = {
            let h = handle2.clone();
            move |current: usize, total: usize, title: String, status: String| {
                let _ = h.emit(
                    "op-progress",
                    serde_json::json!({
                        "current": current,
                        "total": total,
                        "title": title,
                        "status": status,
                    }),
                );
            }
        };

        let r = enrich::enrich_all(&state.store, &state.client, 10, progress, cancel).await;
        state.finish_op();
        r
    })
    .await
    .map_err(|e| format!("task panic: {e}"))?;

    let summary = serde_json::json!({
        "enriched": result.enriched,
        "skipped": result.skipped,
        "errors": result.errors,
    });
    let _ = handle.emit("op-done", &summary);
    Ok(summary)
}

#[tauri::command]
async fn fetch_covers(handle: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    state.ensure_ready()?;
    if !state.start_op("covers") {
        return Err(format!("operation {:?} already running", state.current_op()));
    }

    let state = Arc::clone(&state);
    let handle2 = handle.clone();

    let result = tokio::task::spawn(async move {
        let cancel = Arc::clone(&state.cancel);
        let covers_dir = state.store.covers_dir.clone();
        let progress = {
            let h = handle2.clone();
            move |current: usize, total: usize, title: String, status: String| {
                // Throttle: every 5th or last
                if current.is_multiple_of(5) || current == total {
                    let _ = h.emit(
                        "op-progress",
                        serde_json::json!({
                            "current": current,
                            "total": total,
                            "title": title,
                            "status": status,
                        }),
                    );
                }
            }
        };

        let r =
            covers::fetch_all_covers(&state.store, &state.client, &covers_dir, 10, progress, cancel)
                .await;
        state.finish_op();
        r
    })
    .await
    .map_err(|e| format!("task panic: {e}"))?;

    let summary = serde_json::json!({
        "fetched": result.fetched,
        "cached": result.cached,
        "errors": result.errors,
    });
    let _ = handle.emit("op-done", &summary);
    Ok(summary)
}

#[tauri::command]
async fn download_all(handle: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    state.ensure_ready()?;
    if !state.start_op("download") {
        return Err(format!("operation {:?} already running", state.current_op()));
    }

    let state = Arc::clone(&state);
    let handle2 = handle.clone();

    let result = tokio::task::spawn(async move {
        let cancel = Arc::clone(&state.cancel);
        let epubs_dir = state.store.epubs_dir.clone();
        let progress = {
            let h = handle2.clone();
            move |current: usize, total: usize, title: String, status: String| {
                let _ = h.emit(
                    "op-progress",
                    serde_json::json!({
                        "current": current,
                        "total": total,
                        "title": title,
                        "status": status,
                    }),
                );
            }
        };

        let r = download::download_all(&state.store, &state.client, &epubs_dir, 3, progress, cancel).await;
        state.store.refresh_download_status();
        state.finish_op();
        r
    })
    .await
    .map_err(|e| format!("task panic: {e}"))?;

    let summary = serde_json::json!({
        "succeeded": result.succeeded,
        "skipped": result.skipped,
        "failed": result.failed,
    });
    let _ = handle.emit("op-done", &summary);
    Ok(summary)
}

#[tauri::command]
async fn download_single(state: tauri::State<'_, Arc<AppState>>, repo: String) -> Result<serde_json::Value, String> {
    state.ensure_ready()?;
    let epubs_dir = state.store.epubs_dir.clone();

    let path = download::download_single_book(&state.store, &state.client, &repo, &epubs_dir).await?;
    state.store.refresh_download_status();

    Ok(serde_json::json!({"path": path.to_string_lossy(), "status": "ok"}))
}

#[tauri::command]
fn get_cover_path(state: tauri::State<'_, Arc<AppState>>, repo_name: String) -> String {
    let cover_path = state.store.covers_dir.join(format!("{repo_name}.jpg"));
    cover_path.to_string_lossy().to_string()
}

/// Locate a book's epub: organized path first, then the flat `{repo}.epub` fallback.
fn find_epub(state: &AppState, repo: &str) -> (Option<Book>, Option<PathBuf>) {
    let epubs_dir = &state.store.epubs_dir;
    let book = state
        .store
        .all()
        .into_iter()
        .find(|b| ebook_manager::books::repo_name(b) == repo);

    if let Some(b) = &book {
        let p = ebook_manager::books::epub_path(epubs_dir, b);
        if !p.as_os_str().is_empty() && p.exists() {
            return (book, Some(p));
        }
    }
    let flat = epubs_dir.join(format!("{repo}.epub"));
    let path = flat.exists().then_some(flat);
    (book, path)
}

#[tauri::command]
async fn open_epub(handle: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>, repo: String) -> Result<(), String> {
    state.ensure_ready()?;
    match find_epub(&state, &repo).1 {
        Some(path) => {
            let path_str = path.to_string_lossy().to_string();
            handle
                .opener()
                .open_path(&path_str, None::<&str>)
                .map_err(|e| format!("open epub: {e}"))?;
            Ok(())
        }
        None => Err("epub not found".to_string()),
    }
}

// --- Instance lock / startup ---

#[derive(Serialize)]
struct StatusResponse {
    ready: bool,
    edition: &'static str,
    held_by: Option<HolderInfo>,
    migration: Option<MigrationReport>,
    warnings: Vec<String>,
}

/// Called by the frontend at boot and every 2 s while blocked. Tries to take the
/// instance lock; on success migrates the old library, loads data and marks ready.
#[tauri::command]
async fn instance_status(state: tauri::State<'_, Arc<AppState>>) -> Result<StatusResponse, String> {
    let mut status = StatusResponse {
        ready: true,
        edition: EDITION,
        held_by: None,
        migration: None,
        warnings: vec![],
    };
    let mut guard = state.lock.lock().unwrap();
    if state.is_ready() {
        return Ok(status);
    }

    let lock = match instance::try_acquire(&state.data_dir, EDITION) {
        Err(e) => {
            return Err(format!(
                "cannot create the instance lock in {}: {e}",
                state.data_dir.display()
            ))
        }
        Ok(Acquire::HeldBy(info)) => {
            status.ready = false;
            status.held_by = info;
            return Ok(status);
        }
        Ok(Acquire::Acquired(lock)) => lock,
    };
    *guard = Some(lock);

    let mut report = MigrationReport::default();
    for sub in ["epubs", "covers"] {
        library::migrate_dir(
            &state.project_dir.join(sub),
            &state.store.epubs_dir.parent().unwrap_or(&state.project_dir).join(sub),
            &mut report,
        );
    }
    if !report.is_empty() {
        log::info!("library migration: {report:?}");
    }
    status.migration = Some(report);

    if let Err(e) = state.store.load() {
        log::warn!("could not load books.json: {e}");
        status.warnings.push(e);
    }

    state.ready.store(true, Ordering::SeqCst);
    Ok(status)
}

fn main() {
    env_logger::init();

    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Project root: two levels up from the executable (target/debug/exe → src-tauri → project)
            // In dev mode, use the manifest dir via env; in release, resolve from exe path.
            let project_dir = if cfg!(debug_assertions) {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
            } else {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
            };

            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            std::fs::create_dir_all(&data_dir).ok();

            // Shared by both editions: Documents\StandardIssue Library\{epubs,covers}
            let library_dir = app
                .path()
                .document_dir()
                .unwrap_or_else(|_| project_dir.clone())
                .join(library::LIBRARY_DIR_NAME);
            let books_json = data_dir.join("books.json");
            let epubs_dir = library_dir.join("epubs");
            let covers_dir = library_dir.join("covers");

            std::fs::create_dir_all(&epubs_dir).ok();
            std::fs::create_dir_all(&covers_dir).ok();

            // The webview may read only the library folders through the asset protocol
            let scope = app.asset_protocol_scope();
            scope.allow_directory(&epubs_dir, true)?;
            scope.allow_directory(&covers_dir, true)?;

            // Data is loaded by instance_status once the instance lock is held
            let store = BookStore::new(books_json, epubs_dir, covers_dir);

            let state = Arc::new(AppState {
                store,
                operation: Mutex::new(None),
                cancel: Arc::new(AtomicBool::new(false)),
                client: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .unwrap(),
                ready: AtomicBool::new(false),
                lock: Mutex::new(None),
                data_dir,
                project_dir,
            });

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_books,
            search_books,
            get_stats,
            cancel_operation,
            sync_catalog,
            enrich_all,
            fetch_covers,
            download_all,
            download_single,
            get_cover_path,
            open_epub,
            instance_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
