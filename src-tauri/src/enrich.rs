// Michael Burgus — https://github.com/NeuralDrifter
// Metadata enrichment from GitHub content.opf files

use crate::books::{repo_name, Book, BookStore};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

const ORG: &str = "standardebooks";

/// Revision of the metadata parser. Books enriched by an older revision are
/// enriched again; 2 reads the schema.org vocabulary Standard Ebooks moved to.
pub const META_REV: u32 = 2;

pub struct EnrichResult {
    pub enriched: usize,
    pub skipped: usize,
    pub errors: usize,
}

pub async fn enrich_all(
    store: &BookStore,
    client: &reqwest::Client,
    concurrency: usize,
    progress: impl Fn(usize, usize, String, String) + Send + Sync + 'static,
    cancel: Arc<AtomicBool>,
) -> EnrichResult {
    let mut books = store.all();
    let total = books.len();

    let enriched = Arc::new(AtomicUsize::new(0));
    let skipped = Arc::new(AtomicUsize::new(0));
    let errors = Arc::new(AtomicUsize::new(0));
    let sem = Arc::new(Semaphore::new(concurrency));
    let progress = Arc::new(progress);

    let mut handles = Vec::new();

    for (i, book) in books.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }

        let book = book.clone();
        let rn = repo_name(&book);
        let title = book.title.clone();

        if book.meta_rev >= META_REV {
            skipped.fetch_add(1, Ordering::Relaxed);
            progress(i + 1, total, title, "skipped".into());
            continue;
        }

        if rn.is_empty() {
            errors.fetch_add(1, Ordering::Relaxed);
            progress(i + 1, total, title, "no repo".into());
            continue;
        }

        let permit = sem.clone().acquire_owned().await.unwrap();
        let client = client.clone();
        let cancel = cancel.clone();
        let enriched = enriched.clone();
        let errors = errors.clone();
        let progress = progress.clone();

        let handle = tokio::spawn(async move {
            let _permit = permit;

            if cancel.load(Ordering::SeqCst) {
                return None;
            }

            match fetch_and_parse_opf(&client, &rn).await {
                Ok(meta) => {
                    enriched.fetch_add(1, Ordering::Relaxed);
                    progress(i + 1, total, title, "enriched".into());
                    Some((i, meta))
                }
                Err(e) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    progress(i + 1, total, title, format!("error: {e}"));
                    None
                }
            }
        });
        handles.push(handle);
    }

    // Collect results and apply metadata
    for handle in handles {
        if let Ok(Some((idx, meta))) = handle.await {
            apply_metadata(&mut books[idx], &meta);
        }
    }

    store.set_books(books);
    let _ = store.save();

    EnrichResult {
        enriched: enriched.load(Ordering::Relaxed),
        skipped: skipped.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed),
    }
}

#[derive(Default)]
pub struct OpfMeta {
    description: String,
    language: String,
    date: String,
    subjects: Vec<String>,
    sources: Vec<String>,
    metas: Vec<MetaEntry>,
    /// dc:creator / dc:contributor by id
    people: HashMap<String, String>,
    links: Vec<LinkEntry>,
}

struct MetaEntry {
    id: String,
    property: String,
    refines: String,
    text: String,
}

struct LinkEntry {
    href: String,
    rel: String,
    refines: String,
}

pub async fn fetch_and_parse_opf(client: &reqwest::Client, repo_name: &str) -> Result<OpfMeta, String> {
    let url = format!("https://raw.githubusercontent.com/{ORG}/{repo_name}/master/src/epub/content.opf");
    let resp = client.get(&url).send().await.map_err(|e| format!("{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.text().await.map_err(|e| format!("{e}"))?;
    parse_opf_xml(&body)
}

fn parse_opf_xml(xml: &str) -> Result<OpfMeta, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("XML parse: {e}"))?;
    let metadata = doc
        .descendants()
        .find(|n| n.has_tag_name("metadata"))
        .ok_or("no metadata element")?;

    let mut meta = OpfMeta::default();
    for child in metadata.children().filter(|n| n.is_element()) {
        let text = child.text().unwrap_or("").trim().to_string();
        let attr = |name| child.attribute(name).unwrap_or("").to_string();
        match child.tag_name().name() {
            "description" if meta.description.is_empty() => meta.description = text,
            "language" if meta.language.is_empty() => meta.language = text,
            "date" if meta.date.is_empty() => meta.date = text,
            "subject" if !text.is_empty() => meta.subjects.push(text),
            "source" if !text.is_empty() => meta.sources.push(text),
            "creator" | "contributor" => {
                let id = attr("id");
                if !id.is_empty() {
                    meta.people.insert(id, text);
                }
            }
            "meta" => {
                let property = attr("property");
                if !property.is_empty() {
                    meta.metas.push(MetaEntry { id: attr("id"), property, refines: attr("refines"), text });
                }
            }
            "link" => meta.links.push(LinkEntry { href: attr("href"), rel: attr("rel"), refines: attr("refines") }),
            _ => {}
        }
    }
    Ok(meta)
}

/// Copy OPF metadata onto a book. Lists are replaced, never appended, so
/// re-enriching is idempotent. Reads both the current schema.org vocabulary
/// and the older `se:` one.
pub fn apply_metadata(book: &mut Book, meta: &OpfMeta) {
    if !meta.language.is_empty() {
        book.language = meta.language.clone();
    }
    if !meta.date.is_empty() {
        book.se_published = meta.date.clone();
    }
    if !meta.subjects.is_empty() {
        book.subjects = meta.subjects.clone();
    }
    if !meta.sources.is_empty() {
        book.sources = meta.sources.clone();
    }

    let top = |props: &[&str]| -> Vec<&str> {
        meta.metas
            .iter()
            .filter(|m| m.refines.is_empty() && props.contains(&m.property.as_str()) && !m.text.is_empty())
            .map(|m| m.text.as_str())
            .collect()
    };
    let first = |props: &[&str]| top(props).first().map(|s| s.to_string());

    let genres = top(&["schema:genre", "se:subject"]);
    if !genres.is_empty() {
        book.categories = genres.iter().map(|s| s.to_string()).collect();
    }
    if let Some(n) = first(&["schema:wordCount", "se:word-count"]).and_then(|s| s.parse().ok()) {
        book.word_count = n;
    }
    if let Some(f) = first(&["schema:educationalLevel", "se:reading-ease.flesch"]).and_then(|s| s.parse().ok()) {
        book.reading_ease = f;
    }
    // The short summary: schema:abstract now, dc:description before that
    if let Some(a) = first(&["schema:abstract"]) {
        book.description = a;
    } else if !meta.description.is_empty() && !meta.description.starts_with('<') {
        book.description = meta.description.clone();
    }
    // The long description: the legacy meta refines #description; now dc:description holds HTML
    let long = meta
        .metas
        .iter()
        .find(|m| m.property == "se:long-description" && !m.text.is_empty())
        .map(|m| m.text.clone())
        .unwrap_or_else(|| meta.description.clone());
    if !long.is_empty() {
        book.long_description = long;
    }

    let wiki = meta
        .links
        .iter()
        .find(|l| l.refines.is_empty() && l.rel == "schema:sameAs" && l.href.contains("wikipedia.org"))
        .map(|l| l.href.clone())
        .or_else(|| first(&["se:url.encyclopedia.wikipedia"]));
    if let Some(w) = wiki {
        book.wikipedia = w;
    }

    // Collections and series
    fn refines_of<'a>(meta: &'a OpfMeta, id: &str) -> impl Iterator<Item = &'a MetaEntry> + 'a {
        let target = format!("#{id}");
        meta.metas.iter().filter(move |m| m.refines == target)
    }
    let mut collections = Vec::new();
    for m in meta.metas.iter().filter(|m| m.property == "belongs-to-collection" && !m.text.is_empty()) {
        collections.push(m.text.clone());
        if m.id.is_empty() || !book.series.is_empty() && book.series != m.text {
            continue;
        }
        let is_series = refines_of(meta, &m.id).any(|r| r.property == "collection-type" && r.text.trim() == "series");
        if is_series {
            book.series = m.text.clone();
            book.series_position = refines_of(meta, &m.id)
                .find(|r| r.property == "group-position")
                .and_then(|r| r.text.trim().parse().ok())
                .unwrap_or(0);
        }
    }
    if !collections.is_empty() {
        book.collections = collections;
    }

    if book.word_count > 0 {
        let minutes = book.word_count / 250;
        let (hours, mins) = (minutes / 60, minutes % 60);
        book.reading_time = if hours > 0 { format!("{hours}h {mins}m") } else { format!("{mins}m") };
    }
    book.meta_rev = META_REV;
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURRENT: &str = include_str!("../tests/fixtures/opf_current.opf");
    const LEGACY: &str = include_str!("../tests/fixtures/opf_legacy.opf");

    #[test]
    fn current_schema_vocabulary() {
        let mut b = Book::default();
        apply_metadata(&mut b, &parse_opf_xml(CURRENT).unwrap());
        assert_eq!(b.categories, vec!["Adventure", "Fantasy"]);
        assert_eq!(b.word_count, 75964);
        assert_eq!(b.reading_ease, 70.23);
        assert_eq!(b.description, "A team of scientists and adventurers explores a prehistoric land.");
        assert!(b.long_description.contains("<p>Even though"));
        assert_eq!(b.wikipedia, "https://en.wikipedia.org/wiki/The_Lost_World_%28Conan_Doyle_novel%29");
        assert_eq!(b.series, "Professor Challenger");
        assert_eq!(b.series_position, 1);
        assert_eq!(b.subjects.len(), 5);
        assert_eq!(b.meta_rev, META_REV);
        assert_eq!(b.reading_time, "5h 3m");
    }

    #[test]
    fn legacy_se_vocabulary() {
        let mut b = Book::default();
        apply_metadata(&mut b, &parse_opf_xml(LEGACY).unwrap());
        assert_eq!(b.categories, vec!["Fiction", "Adventure"]);
        assert_eq!(b.word_count, 12345);
        assert_eq!(b.reading_ease, 65.5);
        assert_eq!(b.long_description, "<p>Long.</p>");
        assert_eq!(b.wikipedia, "https://en.wikipedia.org/wiki/Old");
    }

    #[test]
    fn re_enriching_does_not_duplicate_lists() {
        let mut b = Book::default();
        let meta = parse_opf_xml(CURRENT).unwrap();
        apply_metadata(&mut b, &meta);
        apply_metadata(&mut b, &meta);
        assert_eq!(b.categories.len(), 2);
        assert_eq!(b.collections, vec!["Professor Challenger"]);
    }
}
