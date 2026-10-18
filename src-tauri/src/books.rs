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
    /// Metadata parser revision that last enriched this book (0 = never).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub meta_rev: u32,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}
fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

pub struct BookStore {
    books: RwLock<Vec<Book>>,
    /// repo name -> epub file on disk
    downloaded: RwLock<HashMap<String, PathBuf>>,
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

        *self.books.write().unwrap() = books;
        self.refresh_download_status();
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
        self.downloaded.read().unwrap().contains_key(&repo_name(b))
    }

    /// The epub on disk for this repo, as of the last status refresh.
    pub fn epub_file(&self, repo: &str) -> Option<PathBuf> {
        self.downloaded.read().unwrap().get(repo).cloned()
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
        let found: HashMap<String, PathBuf> = self
            .books
            .read()
            .unwrap()
            .iter()
            .filter_map(|b| locate(&self.epubs_dir, b).map(|p| (repo_name(b), p)))
            .collect();
        *self.downloaded.write().unwrap() = found;
    }
}

/// An existing epub for this book. The library folder is shared with the Advanced
/// edition, which may file books by genre or under Uncategorized, so look in the
/// genre path, the Uncategorized path, then the old flat `{repo}.epub`.
pub fn locate(epubs_dir: &Path, b: &Book) -> Option<PathBuf> {
    let rn = repo_name(b);
    if rn.is_empty() {
        return None;
    }
    [
        epub_path(epubs_dir, b),
        epub_path_in(epubs_dir, b, "Uncategorized"),
        epubs_dir.join(format!("{rn}.epub")),
    ]
    .into_iter()
    .find(|p| p.is_file())
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

/// Organized epub path: {epubs_dir}/{Category}/{Author}/{Series?}/{Title}_{Author} (Standard Ebooks).epub
pub fn epub_path(epubs_dir: &Path, b: &Book) -> PathBuf {
    let category = match b.categories.first() {
        Some(c) => sanitize_path(c),
        None => "Uncategorized".to_string(),
    };
    epub_path_in(epubs_dir, b, &category)
}

/// The organized path with the top folder given explicitly.
fn epub_path_in(epubs_dir: &Path, b: &Book, category: &str) -> PathBuf {
    let rn = repo_name(b);
    if rn.is_empty() {
        return PathBuf::new();
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> Book {
        Book {
            title: "The Lost World".into(),
            author: "Arthur Conan Doyle".into(),
            repo_name: "arthur-conan-doyle_the-lost-world".into(),
            categories: vec!["Adventure".into()],
            ..Default::default()
        }
    }

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"epub").unwrap();
    }

    #[test]
    fn locate_checks_genre_then_uncategorized_then_flat() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        let b = book();
        assert_eq!(locate(dir, &b), None);

        let flat = dir.join("arthur-conan-doyle_the-lost-world.epub");
        touch(&flat);
        assert_eq!(locate(dir, &b), Some(flat));

        let uncategorized = dir
            .join("Uncategorized")
            .join("Arthur Conan Doyle")
            .join("The Lost World_Arthur Conan Doyle (Standard Ebooks).epub");
        touch(&uncategorized);
        assert_eq!(locate(dir, &b), Some(uncategorized));

        let genre = epub_path(dir, &b);
        assert!(genre.starts_with(dir.join("Adventure")));
        touch(&genre);
        assert_eq!(locate(dir, &b), Some(genre));
    }

    #[test]
    fn enriched_book_still_finds_its_uncategorized_download() {
        let t = tempfile::tempdir().unwrap();
        let epubs = t.path().join("epubs");
        let json = t.path().join("books.json");
        // Downloaded before enrichment worked, so it was filed under Uncategorized
        let mut b = book();
        b.categories.clear();
        let old_file = epub_path(&epubs, &b);
        assert!(old_file.starts_with(epubs.join("Uncategorized")));
        touch(&old_file);

        // Enrichment now fills in the genre
        b.categories = vec!["Adventure".into()];
        std::fs::write(&json, serde_json::to_string(&vec![b.clone()]).unwrap()).unwrap();
        let store = BookStore::new(json, epubs, t.path().join("covers"));
        store.load().unwrap();

        assert!(store.is_downloaded(&b));
        assert_eq!(store.epub_file(&repo_name(&b)), Some(old_file));
        assert_eq!(store.count_downloaded(), 1);
    }
}
