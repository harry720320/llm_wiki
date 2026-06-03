# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

```bash
npm install                    # Install dependencies
npm run dev                    # Start Vite dev server (frontend only, port 1420)
npm run tauri dev              # Start Tauri desktop app in dev mode
npm run tauri build            # Production build (macOS/Windows/Linux)
npm run build                  # Typecheck + Vite production build
npm run typecheck              # TypeScript type checking only (tsc --build)
npm run test                   # Run all tests: mocks first, then real-LLM
npm run test:mocks             # Fast unit tests (excludes *.real-llm.test.ts)
npx vitest run -- path/to/file  # Run a single test file
npx vitest run -- -t "pattern"  # Run tests matching a pattern
```

The dev server runs on port 1420 with strict port checking. Tauri expects this exact port.

## Architecture

LLM Wiki is a **Tauri v2 desktop app** that implements Karpathy's LLM Wiki pattern: an LLM incrementally builds a structured, interlinked wiki from documents.

### Three-tier application

| Layer | Technology |
|-------|-----------|
| Frontend | React 19 + TypeScript + Vite |
| Desktop shell | Tauri v2 (Rust) |
| UI framework | Tailwind CSS v4 + shadcn/ui (base-nova style) |

### Rust backend (`src-tauri/`)

The `lib.rs` entry point registers all Tauri commands and plugins. Key modules:
- **`commands/fs.rs`** — File I/O, PDF/DOCX/PPTX/XLSX extraction, directory listing
- **`commands/project.rs`** — Project creation and opening (scaffolds `purpose.md`, `schema.md`, directory structure)
- **`commands/search.rs`** — Tokenized search with CJK bigram support, title match boosting
- **`commands/vectorstore.rs`** — LanceDB integration for optional vector semantic search
- **`commands/claude_cli.rs` / `codex_cli.rs`** — Subprocess management for Claude Code / Codex CLI as LLM providers
- **`commands/extract_images.rs`** — PDF/Office image extraction via pdfium + office_oxide
- **`commands/file_sync.rs`** — File watcher for `raw/sources/` auto-detection
- **`api_server.rs`** — HTTP API at `127.0.0.1:19828` for external agent integration
- **`clip_server.rs`** — HTTP server at `127.0.0.1:19827` for Chrome extension communication
- **`tray.rs`** — System tray icon with quick actions
- **`proxy.rs`** — Global HTTP/HTTPS proxy env var management

### Frontend structure (`src/`)

**Entry**: `main.tsx` → `App.tsx`. `App.tsx` handles: project lifecycle (create/open/switch), config hydration from disk into Zustand stores, ingest queue restore, file watcher startup, and auto-save setup.

**State management**: Zustand stores in `src/stores/`:
- `wiki-store.ts` — Central store: project, file tree, active view, LLM config, all settings (provider, embedding, multimodal, proxy, API server, source watch, scheduled import, output language). Also holds `dataVersion` counter bumped on content changes to invalidate caches.
- `chat-store.ts` — Multi-conversation chat with per-conversation message persistence
- `review-store.ts` — Async review items flagged during ingest
- `lint-store.ts` — Lint results for wiki health
- `activity-store.ts` — Ingest queue progress tracking
- `research-store.ts` — Deep research panel state
- `file-sync-store.ts` — File watcher change queue
- `update-store.ts` — App update check (GitHub releases)

**Layout**: `AppLayout` uses a three-column design:
1. **Left panel**: Icon sidebar (vertical icon nav for Wiki/Sources/Search/Graph/Lint/Review/Research/Settings) + resizable file tree + activity panel
2. **Center**: Content area that switches between views based on `activeView`
3. **Right panel**: File preview + research panel, both resizable

**Key `lib/` modules**:
- `llm-client.ts` — Unified streaming LLM client routing to provider-specific implementations (OpenAI, Anthropic, Google, Azure, Ollama, Custom, Claude Code CLI, Codex CLI). Uses `tauri-plugin-http` for Rust-backed fetch (avoids CORS issues).
- `ingest.ts` — Two-step chain-of-thought: Step 1 (analysis) reads source and structures knowledge, Step 2 (generation) creates wiki pages. Features SHA256 incremental cache, source traceability via `sources[]` frontmatter, language-aware generation.
- `graph-relevance.ts` — 4-signal relevance model (direct links ×3.0, source overlap ×4.0, Adamic-Adar ×1.5, type affinity ×1.0)
- `graph-insights.ts` — Surprising connections and knowledge gap detection from graph structure
- `deep-research.ts` — Web search (Tavily/SerpApi/SearXNG) + LLM synthesis with auto-ingest
- `dedup.ts` — LLM-driven duplicate entity/concept detection and merging
- `lint.ts` — Wiki health checks
- `embedding.ts` — Vector embedding client for LanceDB semantic search
- `ingest-queue.ts` — Persistent serial ingest queue with crash recovery
- `search-pipeline.ts` — Multi-phase retrieval (tokenized → vector → graph expansion → context assembly)
- `context-budget.ts` — Token budget allocation across wiki pages, chat history, index, and system prompt
- `enrich-wikilinks.ts` — Dead wikilink cleanup and cross-reference maintenance
- `dedup-queue.ts` / `dedup-runner.ts` / `dedup-storage.ts` — Dedup merge queue infrastructure

**Commands layer** (`src/commands/`): Thin wrappers around `invoke()` for Tauri IPC calls.

### Wiki project structure on disk

```
project/
├── purpose.md              # Goals, key questions, research scope
├── schema.md               # Wiki structure rules, page types
├── raw/
│   ├── sources/            # Uploaded documents (immutable)
│   └── assets/             # Local images
├── wiki/
│   ├── index.md            # Content catalog (LLM navigation entry point)
│   ├── log.md              # Chronological operation record
│   ├── overview.md         # Global summary (auto-updated on every ingest)
│   ├── entities/           # People, organizations, products
│   ├── concepts/           # Theories, methods, techniques
│   ├── sources/            # Source summaries
│   ├── queries/            # Saved chat answers + research
│   ├── synthesis/          # Cross-source analysis
│   └── comparisons/        # Side-by-side comparisons
├── .obsidian/              # Auto-generated Obsidian vault config
└── .llm-wiki/              # App config, chat history, review items
```

Pages use YAML frontmatter with `[[wikilinks]]` syntax. The wiki directory is an Obsidian-compatible vault.

### Chrome Extension (`extension/`)

Manifest V3. Uses Mozilla Readability.js for content extraction and Turndown.js for HTML→Markdown. Communicates with the app via the clip server at port 19827.

### Tests

Vitest with `environment: "node"`. Two test suites:
- **`test:mocks`** — Fast unit/property tests. Files named `*.test.ts`/`*.test.tsx` (not `*.real-llm.test.ts`).
- **`test:llm`** — Integration tests that hit real LLM APIs. Files named `*.real-llm.test.ts`. Run with `--no-file-parallelism --reporter=verbose`. Loads `.env.test.local` for API keys.

Test helpers are in `src/test-helpers/`.

## Key conventions

- Path alias: `@/` maps to `./src/`
- File I/O in the frontend goes through `src/commands/fs.ts` wrappers → Tauri IPC → Rust `commands/fs.rs`
- All user-editable files live inside the project directory; global settings are in Tauri's app data store (`app-state.json`)
- The app uses `__APP_VERSION__` (defined in vite.config.ts from package.json) for the settings UI and update checker
- LLM providers implement a common streaming interface in `llm-providers.ts`; `streamChat` in `llm-client.ts` routes to the active provider
- The `claude-code` and `codex-cli` providers spawn local subprocesses managed by the Rust backend, streaming stdout line-by-line via Tauri events
- `dataVersion` in wiki-store is bumped on any content mutation; components watching it re-derive (graph, search index, etc.)
- Path normalization (`normalizePath` in `path-utils.ts`) converts backslashes to forward slashes — used across 22+ files
