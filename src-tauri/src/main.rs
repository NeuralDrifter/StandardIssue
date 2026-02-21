// Michael Burgus — https://github.com/NeuralDrifter
// Pure Tauri v2 Rust backend for Standard Ebooks Manager

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use ebook_manager::books::{Book, BookStore};
use ebook_manager::catalog;
use ebook_manager::covers;
use ebook_manager::download;
use ebook_manager::enrich;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

pub struct AppState {
    pub store: BookStore,
    pub operation: Mutex<Option<String>>,
    pub cancel: Arc<AtomicBool>,
    pub client: reqwest::Client,
}

impl AppState {
    fn start_op(&self, name: &str) -> bool {
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

#[tauri::command]
async fn open_epub(handle: tauri::AppHandle, state: tauri::State<'_, Arc<AppState>>, repo: String) -> Result<(), String> {
    let books = state.store.all();
    let epubs_dir = &state.store.epubs_dir;

    // Find the book's epub path
    let mut found = None;
    for b in &books {
        if ebook_manager::books::repo_name(b) == repo {
            let p = ebook_manager::books::epub_path(epubs_dir, b);
            if !p.as_os_str().is_empty() && p.exists() {
                found = Some(p);
                break;
            }
        }
    }
    // Fallback: flat path
    if found.is_none() {
        let flat = epubs_dir.join(format!("{repo}.epub"));
        if flat.exists() {
            found = Some(flat);
        }
    }

    match found {
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

            let books_json = data_dir.join("books.json");
            let epubs_dir = project_dir.join("epubs");
            let covers_dir = project_dir.join("covers");

            std::fs::create_dir_all(&epubs_dir).ok();
            std::fs::create_dir_all(&covers_dir).ok();

            let store = BookStore::new(books_json, epubs_dir, covers_dir);
            if let Err(e) = store.load() {
                log::warn!("could not load books.json: {e}");
            } else {
                log::info!(
                    "Loaded {} books, {} downloaded",
                    store.count(),
                    store.count_downloaded()
                );
            }

            let state = Arc::new(AppState {
                store,
                operation: Mutex::new(None),
                cancel: Arc::new(AtomicBool::new(false)),
                client: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .unwrap(),
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
