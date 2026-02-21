// Michael Burgus — https://github.com/NeuralDrifter
// GitHub catalog sync — fetches all ebook repos from standardebooks org

use crate::books::{repo_name, BookStore, Book};
use serde::Deserialize;
use std::collections::HashMap;

const ORG: &str = "standardebooks";
const EBOOK_MARKER: &str = "Standard Ebooks edition";

#[derive(Deserialize)]
struct GhRepo {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// Fetch all ebook repos from the GitHub org, paginated.
async fn fetch_catalog(
    client: &reqwest::Client,
    progress: &impl Fn(String),
) -> Result<Vec<Book>, String> {
    progress("Fetching repo list from GitHub...".into());

    let mut all_repos = Vec::new();
    let mut page = 1u32;

    loop {
        let url = format!(
            "https://api.github.com/orgs/{ORG}/repos?per_page=100&page={page}"
        );

        let resp = client
            .get(&url)
            .header("Accept", "application/vnd.github.v3+json")
            .header("User-Agent", "ebook-manager/1.0")
            .send()
            .await
            .map_err(|e| format!("GitHub API error: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!("GitHub API HTTP {}", resp.status()));
        }

        let batch: Vec<GhRepo> = resp
            .json()
            .await
            .map_err(|e| format!("JSON parse error: {e}"))?;

        if batch.is_empty() {
            break;
        }

        all_repos.extend(batch);
        progress(format!("Fetched {} repos...", all_repos.len()));
        page += 1;
    }

    let mut books = Vec::new();
    for repo in &all_repos {
        let desc = repo.description.as_deref().unwrap_or("");
        if !desc.contains(EBOOK_MARKER) {
            continue;
        }

        let (mut title, mut author) = parse_description(desc);

        // Fallback to slug parsing
        let parts: Vec<&str> = repo.name.splitn(2, '_').collect();
        if author.is_empty() && !parts.is_empty() {
            author = slug_to_title(parts[0]);
        }
        if title.is_empty() && parts.len() >= 2 {
            let title_slug = parts[1].split('_').next().unwrap_or("");
            title = slug_to_title(title_slug);
        }
        if title.is_empty() {
            title = repo.name.clone();
        }

        let path = repo.name.replace('_', "/");
        let book_url = format!("https://standardebooks.org/ebooks/{path}");

        books.push(Book {
            title,
            author,
            url: book_url,
            repo: format!("https://github.com/{ORG}/{}", repo.name),
            repo_name: repo.name.clone(),
            ..Default::default()
        });
    }

    progress(format!("Found {} books on GitHub.", books.len()));
    Ok(books)
}

/// Merge fresh catalog with existing books, preserving enriched metadata.
pub async fn sync_catalog(
    store: &BookStore,
    client: &reqwest::Client,
    progress: impl Fn(String),
) -> Result<serde_json::Value, String> {
    let existing = store.all();
    let mut existing_by_repo: HashMap<String, Book> = HashMap::with_capacity(existing.len());
    for b in &existing {
        let rn = repo_name(b);
        if !rn.is_empty() {
            existing_by_repo.insert(rn, b.clone());
        }
    }

    let fresh = fetch_catalog(client, &progress).await?;

    let mut fresh_repos: HashMap<String, bool> = HashMap::with_capacity(fresh.len());
    for b in &fresh {
        fresh_repos.insert(b.repo_name.clone(), true);
    }

    let mut updated = Vec::new();
    let mut new_count = 0usize;
    for book in &fresh {
        if let Some(existing) = existing_by_repo.get(&book.repo_name) {
            updated.push(existing.clone()); // preserve enriched metadata
        } else {
            new_count += 1;
            updated.push(book.clone());
        }
    }

    let mut removed_count = 0usize;
    for rn in existing_by_repo.keys() {
        if !fresh_repos.contains_key(rn) {
            removed_count += 1;
        }
    }

    store.set_books(updated.clone());
    store.save()?;

    let total = updated.len();
    progress(format!(
        "Sync complete: {new_count} new, {removed_count} removed, {total} total"
    ));

    Ok(serde_json::json!({
        "new": new_count,
        "removed": removed_count,
        "total": total,
    }))
}

fn parse_description(desc: &str) -> (String, String) {
    let idx = match desc.find("edition of ") {
        Some(i) => i,
        None => return (String::new(), String::new()),
    };
    let after = &desc[idx + "edition of ".len()..];

    if let Some(by_idx) = after.find(", by ") {
        let title = after[..by_idx].to_string();
        let rest = &after[by_idx + ", by ".len()..];

        for sep in &[". Translated by", ". Illustrated by", ". Edited by"] {
            if let Some(sep_idx) = rest.find(sep) {
                let author = rest[..sep_idx].to_string();
                return (title.trim().to_string(), author.trim().to_string());
            }
        }
        let author = rest.trim_end_matches('.').to_string();
        (title.trim().to_string(), author.trim().to_string())
    } else {
        let title = after.trim_end_matches('.').to_string();
        (title.trim().to_string(), String::new())
    }
}

fn slug_to_title(slug: &str) -> String {
    slug.split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
