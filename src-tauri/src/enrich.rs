// Michael Burgus — https://github.com/NeuralDrifter
// Metadata enrichment from GitHub content.opf files

use crate::books::{repo_name, BookStore};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

const ORG: &str = "standardebooks";

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

        if book.word_count > 0 {
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

pub struct OpfMeta {
    description: String,
    language: String,
    date: String,
    subjects: Vec<String>,
    sources: Vec<String>,
    metas: Vec<MetaEntry>,
}

pub struct MetaEntry {
    id: String,
    property: String,
    refines: String,
    text: String,
}

pub async fn fetch_and_parse_opf(client: &reqwest::Client, repo_name: &str) -> Result<OpfMeta, String> {
    let url = format!(
        "https://raw.githubusercontent.com/{ORG}/{repo_name}/master/src/epub/content.opf"
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("{e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let body = resp.text().await.map_err(|e| format!("{e}"))?;
    parse_opf_xml(&body)
}

fn parse_opf_xml(xml: &str) -> Result<OpfMeta, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("XML parse: {e}"))?;

    let mut meta = OpfMeta {
        description: String::new(),
        language: String::new(),
        date: String::new(),
        subjects: Vec::new(),
        sources: Vec::new(),
        metas: Vec::new(),
    };

    // Find the metadata element
    let metadata = doc
        .descendants()
        .find(|n| n.has_tag_name("metadata"))
        .ok_or("no metadata element")?;

    for child in metadata.children() {
        if !child.is_element() {
            continue;
        }

        let tag = child.tag_name().name();
        let text = child.text().unwrap_or("").trim().to_string();

        match tag {
            "description" => {
                if meta.description.is_empty() && !text.is_empty() {
                    meta.description = text;
                }
            }
            "language" => {
                if meta.language.is_empty() && !text.is_empty() {
                    meta.language = text;
                }
            }
            "date" => {
                if meta.date.is_empty() && !text.is_empty() {
                    meta.date = text;
                }
            }
            "subject" => {
                if !text.is_empty() {
                    meta.subjects.push(text);
                }
            }
            "source" => {
                if !text.is_empty() {
                    meta.sources.push(text);
                }
            }
            "meta" => {
                let id = child.attribute("id").unwrap_or("").to_string();
                let property = child.attribute("property").unwrap_or("").to_string();
                let refines = child.attribute("refines").unwrap_or("").to_string();
                if !property.is_empty() {
                    meta.metas.push(MetaEntry {
                        id,
                        property,
                        refines,
                        text,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(meta)
}

pub fn apply_metadata(book: &mut crate::books::Book, meta: &OpfMeta) {
    if !meta.description.is_empty() {
        book.description = meta.description.clone();
    }
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

    // Build refines lookup: id -> vec of metas that refine it
    let mut refines_of: std::collections::HashMap<String, Vec<&MetaEntry>> =
        std::collections::HashMap::new();
    for m in &meta.metas {
        if !m.refines.is_empty() {
            let id = m.refines.trim_start_matches('#').to_string();
            refines_of.entry(id).or_default().push(m);
        }
    }

    for m in &meta.metas {
        let val = m.text.trim();
        if val.is_empty() {
            continue;
        }
        match m.property.as_str() {
            "se:long-description" => {
                book.long_description = val.to_string();
            }
            "se:word-count" => {
                if let Ok(n) = val.parse::<i64>() {
                    book.word_count = n;
                }
            }
            "se:reading-ease.flesch" => {
                if let Ok(f) = val.parse::<f64>() {
                    book.reading_ease = f;
                }
            }
            "se:url.encyclopedia.wikipedia" => {
                if m.refines.is_empty() {
                    book.wikipedia = val.to_string();
                }
            }
            "se:subject" => {
                book.categories.push(val.to_string());
            }
            "belongs-to-collection" => {
                book.collections.push(val.to_string());

                // Check if this collection is a series
                if !m.id.is_empty() {
                    let mut is_series = false;
                    let mut position = 0i64;
                    if let Some(refs) = refines_of.get(&m.id) {
                        for r in refs {
                            match r.property.as_str() {
                                "collection-type" => {
                                    if r.text.trim() == "series" {
                                        is_series = true;
                                    }
                                }
                                "group-position" => {
                                    if let Ok(n) = r.text.trim().parse::<i64>() {
                                        position = n;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    if is_series && book.series.is_empty() {
                        book.series = val.to_string();
                        book.series_position = position;
                    }
                }
            }
            _ => {}
        }
    }

    // Reading time
    if book.word_count > 0 {
        let minutes = book.word_count / 250;
        let hours = minutes / 60;
        let mins = minutes % 60;
        if hours > 0 {
            book.reading_time = format!("{hours}h {mins}m");
        } else {
            book.reading_time = format!("{mins}m");
        }
    }
}
