# StandardIssue

A fast, native desktop app for managing the entire [Standard Ebooks](https://standardebooks.org) catalog. Built with **Tauri v2** and **Rust** — no JavaScript frameworks, no bundlers, just a single HTML frontend backed by a pure Rust backend.

## What It Does

StandardIssue syncs every ebook from the Standard Ebooks GitHub organization, enriches each entry with full metadata from the original OPF files, fetches cover art, and builds valid `.epub` files locally by shallow-cloning repos and zipping their `src/` directories to spec.

All 1,300+ books are organized automatically:

```
epubs/
├── Fiction/
│   ├── James Joyce/
│   │   └── Dubliners_James Joyce (Standard Ebooks).epub
│   ├── Agatha Christie/
│   │   └── Poirot/
│   │       └── The Mysterious Affair at Styles_Agatha Christie (Standard Ebooks).epub
│   └── ...
├── Science Fiction/
├── Philosophy/
├── Poetry/
└── ...
```

## Features

- **Catalog Sync** — Pulls the full repo list from the Standard Ebooks GitHub org via API, parses descriptions, and merges with existing data
- **Metadata Enrichment** — Fetches each book's `content.opf` from GitHub and extracts word count, reading ease, subjects, categories, series info, collections, sources, and more
- **Cover Fetching** — Extracts embedded JPEG data from SVG cover files, resizes to thumbnails, and caches locally
- **Epub Building** — Shallow-clones each repo, zips `src/` into a valid EPUB (mimetype first, uncompressed, per spec), and organizes by category/author/series
- **Search** — Filter by title or author instantly
- **Single Book Download** — Click any book to build just that one
- **Open in Reader** — Opens downloaded epubs in your system's default reader
- **Cancel Operations** — All long-running operations support cancellation
- **Concurrent Downloads** — Parallel fetching with configurable semaphore limits

## Requirements

- [Rust](https://rustup.rs/) (stable)
- [Tauri CLI v2](https://v2.tauri.app/start/prerequisites/)
- `git` (for cloning Standard Ebooks repos during epub building)
- Linux: `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `librsvg2-dev`, `patchelf`

## Build & Run

```bash
# Install Tauri CLI if you haven't
cargo install tauri-cli --version "^2"

# Dev mode (hot reload)
cd src-tauri
cargo tauri dev

# Release build
cargo tauri build
```

## Project Structure

```
src-tauri/
├── src/
│   ├── main.rs        # Tauri app setup, all IPC command handlers
│   ├── lib.rs         # Module declarations
│   ├── books.rs       # Book model, JSON storage, path generation, search
│   ├── catalog.rs     # GitHub org sync — fetches all ebook repos
│   ├── enrich.rs      # OPF metadata parser — word count, categories, series
│   ├── covers.rs      # SVG cover extraction, JPEG resize, caching
│   └── download.rs    # Repo cloning, EPUB zip building, batch downloads
├── frontend/
│   └── index.html     # Complete UI — grid layout, detail panels, progress
├── capabilities/
│   └── default.json   # Tauri permission scopes
├── Cargo.toml
└── tauri.conf.json
```

## How It Works

1. **Sync** pulls every repo from `github.com/standardebooks` and filters for actual ebook repos (those with "Standard Ebooks edition" in the description)
2. **Enrich** hits each repo's raw `content.opf` on GitHub and parses the XML for metadata — categories, word count, Flesch reading ease, series membership, Wikipedia links, and more
3. **Covers** fetches the SVG cover from each repo, extracts the base64-encoded JPEG embedded inside, resizes it to a 280×420 thumbnail, and saves it
4. **Download** shallow-clones each repo (`git clone --depth 1`), then zips the `src/` directory into a valid EPUB file — mimetype entry first and uncompressed per the EPUB spec, everything else deflated

All operations run concurrently with semaphore-bounded parallelism, emit progress events to the frontend via Tauri's event system, and support cancellation through an atomic flag.

## Data Storage

- **Books metadata** is stored as JSON in the Tauri app data directory (`~/.local/share/com.neuraldrifter.ebook-manager/books.json` on Linux)
- **Epubs** are saved relative to the project directory in `epubs/`
- **Covers** are cached in `covers/`

## Tech Stack

| Layer    | Technology |
|----------|-----------|
| Backend  | Rust, Tokio, reqwest, roxmltree, zip, image |
| Frontend | Vanilla HTML/CSS/JS, Tauri IPC |
| Desktop  | Tauri v2 |

No Electron. No React. No webpack. The entire frontend is a single 600-line HTML file.

## License

MIT

---

*Built by [NeuralDrifter](https://github.com/NeuralDrifter)*
