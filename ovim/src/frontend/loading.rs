use std::collections::HashMap;

use super::channels::FrontendChannels;
use crate::editor::{self, Editor};
use crate::syntax::{LanguageRegistry, SyntaxHighlighter};

/// Spawns a background task to load picker preview if debounce time has elapsed
/// Returns immediately without blocking input handling
pub(super) fn spawn_picker_preview_loading(
    editor: &mut Editor,
    preview_tx: &tokio::sync::mpsc::Sender<(String, editor::PreviewCache)>,
) {
    if !editor.should_load_picker_preview(50) {
        return;
    }

    // Get the file to load (returns None if already cached/loading)
    if let Some(file_path) = editor.get_preview_to_load() {
        let tx = preview_tx.clone();

        // Spawn background task - doesn't block!
        tokio::spawn(async move {
            // Load preview asynchronously
            if let Some(cache) = load_preview_async(&file_path).await {
                // Send result back (non-blocking)
                let _ = tx.send((file_path, cache)).await;
            }
        });
    }
}

/// Batch size for streaming discovered files to the UI. Small enough that
/// the first screen of results appears immediately, large enough that a
/// 100k-file repo needs hundreds of channel sends instead of 100k.
const FILE_BATCH_SIZE: usize = 512;

/// Parallel-walk visitor that batches discovered files to the UI channel and
/// mirrors them into a shared vec for the file-list cache. `Drop` flushes the
/// final partial batch when the walker shuts a worker down.
struct FileFinderVisitor {
    base_dir: std::path::PathBuf,
    tx: tokio::sync::mpsc::Sender<Vec<editor::PickerResult>>,
    collected: std::sync::Arc<std::sync::Mutex<Vec<editor::PickerResult>>>,
    batch: Vec<editor::PickerResult>,
    channel_closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl FileFinderVisitor {
    fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.batch);
        self.collected.lock().unwrap().extend(batch.iter().cloned());
        // blocking_send is safe here: visitors run on the walker's own
        // threads, never on the tokio runtime.
        if self.tx.blocking_send(batch).is_err() {
            self.channel_closed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl ignore::ParallelVisitor for FileFinderVisitor {
    fn visit(&mut self, entry: Result<ignore::DirEntry, ignore::Error>) -> ignore::WalkState {
        if self
            .channel_closed
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return ignore::WalkState::Quit;
        }
        let Ok(entry) = entry else {
            return ignore::WalkState::Continue;
        };
        if entry.file_type().is_none_or(|ft| !ft.is_file()) {
            return ignore::WalkState::Continue;
        }
        let path = entry.path();
        if let Ok(relative_path) = path.strip_prefix(&self.base_dir) {
            self.batch.push(editor::PickerResult {
                display: relative_path.to_string_lossy().to_string(),
                location: path.to_string_lossy().to_string(),
                line: 0,
                col: 0,
                match_positions: Vec::new(),
                content: None,
            });
            if self.batch.len() >= FILE_BATCH_SIZE {
                self.flush();
            }
        }
        ignore::WalkState::Continue
    }
}

impl Drop for FileFinderVisitor {
    fn drop(&mut self) {
        self.flush();
    }
}

struct FileFinderVisitorBuilder {
    base_dir: std::path::PathBuf,
    tx: tokio::sync::mpsc::Sender<Vec<editor::PickerResult>>,
    collected: std::sync::Arc<std::sync::Mutex<Vec<editor::PickerResult>>>,
    channel_closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl<'s> ignore::ParallelVisitorBuilder<'s> for FileFinderVisitorBuilder {
    fn build(&mut self) -> Box<dyn ignore::ParallelVisitor + 's> {
        Box::new(FileFinderVisitor {
            base_dir: self.base_dir.clone(),
            tx: self.tx.clone(),
            collected: self.collected.clone(),
            batch: Vec::with_capacity(FILE_BATCH_SIZE),
            channel_closed: self.channel_closed.clone(),
        })
    }
}

/// Spawns a background task to load files for file finder picker
/// Returns immediately without blocking - files are sent via channel as they're discovered
/// Uses cache when available to speed up repeated picker opens
pub(super) fn spawn_file_finder_loading(
    editor: &mut Editor,
    file_tx: &tokio::sync::mpsc::Sender<Vec<editor::PickerResult>>,
    file_list_cache_tx: &tokio::sync::mpsc::Sender<(
        std::path::PathBuf,
        std::path::PathBuf,
        Vec<editor::PickerResult>,
    )>,
) {
    // Check if we should spawn file loading
    if let Some(picker) = editor.picker() {
        if !picker.should_spawn_file_loading() {
            return;
        }

        // Get the base directory for file search (git root when available)
        let base_dir = picker.base_dir().to_path_buf();
        // Preferred directory for local-first ordering (typically current file's folder)
        let preferred_dir = picker.preferred_dir().to_path_buf();

        // Check for cached file list (5-minute TTL)
        if let Some(cached_files) = editor.get_cached_file_list(&base_dir, &preferred_dir) {
            // Use cache! Send all files via channel immediately
            let cached_files: Vec<editor::PickerResult> = cached_files.to_vec();
            let tx = file_tx.clone();

            // Mark as spawned to avoid spawning multiple tasks
            if let Some(picker_mut) = editor.picker_mut() {
                picker_mut.mark_loading_spawned();
            }

            // Spawn quick task to send cached results in chunks. Chunked so a
            // huge cached list doesn't process as one frame-stalling batch.
            tokio::spawn(async move {
                let mut rest = cached_files;
                while !rest.is_empty() {
                    let take = rest.len().min(4096);
                    let batch: Vec<editor::PickerResult> = rest.drain(..take).collect();
                    if tx.send(batch).await.is_err() {
                        break;
                    }
                }
            });
            return;
        }

        // Mark as spawned to avoid spawning multiple tasks
        if let Some(picker_mut) = editor.picker_mut() {
            picker_mut.mark_loading_spawned();
        }

        let tx = file_tx.clone();
        let cache_tx = file_list_cache_tx.clone();
        let base_dir_clone = base_dir.clone();
        let preferred_dir_clone = preferred_dir.clone();

        // Walk on a blocking thread: the parallel walker spawns its own
        // worker pool and joins it, which must not run on the async runtime.
        // Results stream to the UI in batches; a mirror copy feeds the cache.
        tokio::task::spawn_blocking(move || {
            use ignore::WalkBuilder;

            let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let channel_closed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

            let mut roots: Vec<(std::path::PathBuf, bool)> = Vec::new();
            if preferred_dir_clone != base_dir_clone
                && preferred_dir_clone.starts_with(&base_dir_clone)
            {
                roots.push((preferred_dir_clone.clone(), true));
            }
            roots.push((base_dir_clone.clone(), false));

            // Walk preferred subtree first, then base (excluding preferred) for local-first ordering.
            for (root, is_preferred_root) in roots {
                if channel_closed.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let base_dir_for_strip = base_dir_clone.clone();
                let preferred_for_filter = preferred_dir_clone.clone();

                // Use ignore crate's WalkBuilder which respects .gitignore
                let mut builder = FileFinderVisitorBuilder {
                    base_dir: base_dir_clone.clone(),
                    tx: tx.clone(),
                    collected: collected.clone(),
                    channel_closed: channel_closed.clone(),
                };
                WalkBuilder::new(&root)
                    .hidden(false) // Don't automatically skip hidden files (keep .env, .eslintrc, etc.)
                    .git_ignore(true) // Respect .gitignore files
                    .git_global(true) // Respect global gitignore
                    .git_exclude(true) // Respect .git/info/exclude
                    .threads(editor::grep::walk_threads())
                    .filter_entry(move |entry| {
                        // Skip .git directory (not in .gitignore but shouldn't be shown)
                        if entry.file_name() == ".git" {
                            return false;
                        }
                        // For the base-dir pass, skip the preferred subtree entirely to avoid duplicates.
                        if !is_preferred_root
                            && preferred_for_filter != base_dir_for_strip
                            && entry.path().starts_with(&preferred_for_filter)
                        {
                            return false;
                        }
                        true
                    })
                    .build_parallel()
                    .visit(&mut builder);
            }

            // Hand collected files back through the channel so the owning
            // Editor's cache is updated from the frontend's tick, not from
            // inside this spawned task. Skip if the walk was aborted — a
            // partial list must not be cached as complete.
            if !channel_closed.load(std::sync::atomic::Ordering::Relaxed) {
                let collected = std::mem::take(&mut *collected.lock().unwrap());
                let _ = cache_tx.blocking_send((base_dir_clone, preferred_dir_clone, collected));
            }
        });
    }
}

/// Helper to process preview and file picker results
pub fn process_picker_results(editor: &mut Editor, channels: &mut FrontendChannels) {
    // Try to drain pending preview loads (single mark_dirty after batch)
    let mut previews_loaded = false;
    while let Ok((path, cache)) = channels.preview_rx.try_recv() {
        editor.insert_preview(path, cache);
        previews_loaded = true;
    }
    if previews_loaded {
        editor.mark_dirty();
    }
    // Drain pending file result batches with a time budget (checked per
    // batch) to avoid stalling input while tens of thousands of files stream in
    let mut files_added = false;
    let drain_start = std::time::Instant::now();
    let drain_budget = std::time::Duration::from_millis(4);
    loop {
        if drain_start.elapsed() >= drain_budget {
            break;
        }
        match channels.file_rx.try_recv() {
            Ok(batch) => {
                if let Some(picker) = editor.picker_mut() {
                    picker.add_file_results(batch);
                    files_added = true;
                }
            }
            Err(_) => break,
        }
    }
    if files_added {
        editor.mark_dirty();
    }
    // Update file list cache from background task (if completed)
    update_file_list_cache_from_background(editor, channels);
}

/// Drains completed file-list results from the background finder task and
/// updates the Editor's cache. Owned by a channel on [`FrontendChannels`]
/// rather than a process-global slot, since more than one `Editor` (e.g. GUI
/// tabs/splits) can exist in the same process.
pub(super) fn update_file_list_cache_from_background(
    editor: &mut Editor,
    channels: &mut FrontendChannels,
) {
    while let Ok((base_dir, preferred_dir, files)) = channels.file_list_cache_rx.try_recv() {
        editor.update_file_list_cache(base_dir, preferred_dir, files);
    }
}

/// Loads a file preview asynchronously (can be called from background task)
async fn load_preview_async(file_path: &str) -> Option<editor::PreviewCache> {
    // Check file size before loading (max 1MB for preview)
    const MAX_PREVIEW_SIZE: u64 = 1024 * 1024;
    if let Ok(metadata) = tokio::fs::metadata(file_path).await {
        if metadata.len() > MAX_PREVIEW_SIZE {
            // File too large, create a placeholder
            return Some(editor::PreviewCache {
                content: format!("File too large for preview ({} bytes)", metadata.len()),
                highlighted_lines: std::cell::RefCell::new(HashMap::new()),
                language: None,
            });
        }
    }

    // Load the file
    let content = tokio::fs::read_to_string(file_path).await.ok()?;

    // Detect language
    let language = LanguageRegistry::detect_from_path(file_path);

    // Parse syntax highlights in a background thread so the render thread doesn't block
    let highlighted_lines = if let Some(lang) = language {
        let content_for_parse = content.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(mut h) = SyntaxHighlighter::new(lang) {
                h.parse(&content_for_parse);
                let all = h.highlights_for_all_lines(&content_for_parse);
                let mut map = HashMap::new();
                for (i, line_h) in all.into_iter().enumerate() {
                    if !line_h.is_empty() {
                        map.insert(i, line_h);
                    }
                }
                map
            } else {
                HashMap::new()
            }
        })
        .await
        .unwrap_or_default()
    } else {
        HashMap::new()
    };

    // Create cache entry with pre-populated highlights
    Some(editor::PreviewCache {
        content,
        highlighted_lines: std::cell::RefCell::new(highlighted_lines),
        language,
    })
}
