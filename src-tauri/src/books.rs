// Michael Burgus — https://github.com/NeuralDrifter
// Books JSON storage, search, and download status tracking

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Book {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cover: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub epub: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub long_description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub se_published: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub word_count: i64,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub reading_ease: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reading_time: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wikipedia: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub series: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub series_position: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subjects: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

pub struct BookStore {
    books: RwLock<Vec<Book>>,
    downloaded: RwLock<HashMap<String, bool>>,
    path: PathBuf,
    pub epubs_dir: PathBuf,
    pub covers_dir: PathBuf,
}

impl BookStore {
    pub fn new(json_path: PathBuf, epubs_dir: PathBuf, covers_dir: PathBuf) -> Self {
        Self {
            books: RwLock::new(Vec::new()),
            downloaded: RwLock::new(HashMap::new()),
            path: json_path,
            epubs_dir,
            covers_dir,
        }
    }

    pub fn load(&self) -> Result<(), String> {
        let data = match std::fs::read_to_string(&self.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("read books.json: {e}")),
        };

        let books: Vec<Book> =
            serde_json::from_str(&data).map_err(|e| format!("parse books.json: {e}"))?;

        let mut dl = HashMap::with_capacity(books.len());
        for b in &books {
            let rn = repo_name(b);
            if rn.is_empty() {
                continue;
            }
            // Check organized path
            let ep = epub_path(&self.epubs_dir, b);
            if !ep.as_os_str().is_empty() && ep.exists() {
                dl.insert(rn, true);
                continue;
            }
            // Fallback: flat path
            let flat = self.epubs_dir.join(format!("{rn}.epub"));
            if flat.exists() {
                dl.insert(rn, true);
            }
        }

        *self.books.write().unwrap() = books;
        *self.downloaded.write().unwrap() = dl;
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        let books = self.books.read().unwrap();
        let data =
            serde_json::to_string_pretty(&*books).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&self.path, data).map_err(|e| format!("write books.json: {e}"))
    }

    pub fn all(&self) -> Vec<Book> {
        self.books.read().unwrap().clone()
    }

    pub fn search(&self, query: &str) -> Vec<Book> {
        let q = query.to_lowercase();
        self.books
            .read()
            .unwrap()
            .iter()
            .filter(|b| {
                b.title.to_lowercase().contains(&q) || b.author.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    pub fn is_downloaded(&self, b: &Book) -> bool {
        let rn = repo_name(b);
        *self.downloaded.read().unwrap().get(&rn).unwrap_or(&false)
    }

    pub fn count_downloaded(&self) -> usize {
        self.downloaded.read().unwrap().len()
    }

    pub fn count(&self) -> usize {
        self.books.read().unwrap().len()
    }

    pub fn set_books(&self, books: Vec<Book>) {
        *self.books.write().unwrap() = books;
    }

    /// Refresh download status cache after downloads complete.
    pub fn refresh_download_status(&self) {
        let books = self.books.read().unwrap();
        let mut dl = HashMap::with_capacity(books.len());
        for b in books.iter() {
            let rn = repo_name(b);
            if rn.is_empty() {
                continue;
            }
            let ep = epub_path(&self.epubs_dir, b);
            if !ep.as_os_str().is_empty() && ep.exists() {
                dl.insert(rn, true);
                continue;
            }
            let flat = self.epubs_dir.join(format!("{rn}.epub"));
            if flat.exists() {
                dl.insert(rn, true);
            }
        }
        *self.downloaded.write().unwrap() = dl;
    }
}

/// Derive repo name from a book's repo_name field or URL.
pub fn repo_name(b: &Book) -> String {
    if !b.repo_name.is_empty() {
        return b.repo_name.clone();
    }
    if let Some(idx) = b.url.find("/ebooks/") {
        let path = b.url[idx + "/ebooks/".len()..].trim_end_matches('/');
        return path.replace('/', "_");
    }
    String::new()
}

/// Organized epub path: {epubs_dir}/{Category}/{Author}/{Series?}/{Title}.epub
pub fn epub_path(epubs_dir: &Path, b: &Book) -> PathBuf {
    let rn = repo_name(b);
    if rn.is_empty() {
        return PathBuf::new();
    }

    let category = if b.categories.is_empty() {
        "Uncategorized".to_string()
    } else {
        sanitize_path(&b.categories[0])
    };

    let author = if b.author.is_empty() {
        "Unknown".to_string()
    } else {
        sanitize_path(&b.author)
    };

    let title = if b.title.is_empty() {
        rn
    } else {
        sanitize_path(&b.title)
    };

    let filename = format!("{title}_{author} (Standard Ebooks)");

    if !b.series.is_empty() {
        let series = sanitize_path(&b.series);
        epubs_dir
            .join(category)
            .join(author)
            .join(series)
            .join(format!("{filename}.epub"))
    } else {
        epubs_dir
            .join(category)
            .join(author)
            .join(format!("{filename}.epub"))
    }
}

/// Remove characters unsafe for filenames/directories.
pub fn sanitize_path(s: &str) -> String {
    let s = s.trim();
    let s = s
        .replace(['/', '\\'], "-")
        .replace(':', " -")
        .replace(['*', '?'], "")
        .replace('"', "'")
        .replace(['<', '>'], "")
        .replace('|', "-");
    // Collapse multiple spaces
    let mut result = s;
    while result.contains("  ") {
        result = result.replace("  ", " ");
    }
    result.trim_end_matches(['.', ' ']).to_string()
}
