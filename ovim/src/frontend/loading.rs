use std::collections::HashMap;

use crate::editor::{self, Editor};
use crate::syntax::{LanguageRegistry, SyntaxHighlighter};

/// Spawns a background task to load picker preview if debounce time has elapsed
/// Returns immediately without blocking input handling
pub fn spawn_picker_preview_loading(
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

/// Spawns a background task to load files for file finder picker
/// Returns immediately without blocking - files are sent via channel as they're discovered
/// Uses cache when available to speed up repeated picker opens
pub fn spawn_file_finder_loading(
    editor: &mut Editor,
    file_tx: &tokio::sync::mpsc::Sender<editor::PickerResult>,
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

            // Spawn quick task to send cached results
            tokio::spawn(async move {
                for result in cached_files {
                    if tx.send(result).await.is_err() {
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
        let base_dir_clone = base_dir.clone();
        let preferred_dir_clone = preferred_dir.clone();

        // Spawn background task - doesn't block!
        // Also collects results for cache update
        tokio::spawn(async move {
            use ignore::WalkBuilder;

            let mut collected_files = Vec::new();

            let mut roots: Vec<(std::path::PathBuf, bool)> = Vec::new();
            if preferred_dir_clone != base_dir_clone
                && preferred_dir_clone.starts_with(&base_dir_clone)
            {
                roots.push((preferred_dir_clone.clone(), true));
            }
            roots.push((base_dir_clone.clone(), false));

            // Walk preferred subtree first, then base (excluding preferred) for local-first ordering.
            for (root, is_preferred_root) in roots {
                let base_dir_for_strip = base_dir_clone.clone();
                let preferred_for_filter = preferred_dir_clone.clone();

                // Use ignore crate's WalkBuilder which respects .gitignore
                let walker = WalkBuilder::new(&root)
                    .hidden(false) // Don't automatically skip hidden files (keep .env, .eslintrc, etc.)
                    .git_ignore(true) // Respect .gitignore files
                    .git_global(true) // Respect global gitignore
                    .git_exclude(true) // Respect .git/info/exclude
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
                    .build();

                // Walk the directory tree and send files as we find them
                for entry in walker.filter_map(|e| e.ok()) {
                    let path = entry.path();

                    if path.is_file() {
                        if let Ok(relative_path) = path.strip_prefix(&base_dir_clone) {
                            let display_path = relative_path.to_string_lossy().to_string();
                            let result = editor::PickerResult {
                                display: display_path,
                                location: path.to_string_lossy().to_string(),
                                line: 0,
                                col: 0,
                                match_positions: Vec::new(),
                                content: None,
                            };

                            // Collect for cache
                            collected_files.push(result.clone());

                            // Send result back (non-blocking)
                            // If channel is closed (picker was closed), task will exit
                            if tx.send(result).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }

            // Store collected files in a static to be picked up by cache update
            // This is a workaround since we can't update Editor state from within a spawned task
            FILE_LIST_CACHE_RESULTS.lock().await.replace((
                base_dir_clone,
                preferred_dir_clone,
                collected_files,
            ));
        });
    }
}

/// Helper to process preview and file picker results
pub fn process_picker_results(
    editor: &mut Editor,
    preview_rx: &mut tokio::sync::mpsc::Receiver<(String, editor::PreviewCache)>,
    file_rx: &mut tokio::sync::mpsc::Receiver<editor::PickerResult>,
) {
    // Try to drain pending preview loads (single mark_dirty after batch)
    let mut previews_loaded = false;
    while let Ok((path, cache)) = preview_rx.try_recv() {
        editor.insert_preview(path, cache);
        previews_loaded = true;
    }
    if previews_loaded {
        editor.mark_dirty();
    }
    // Drain pending file results with a time budget to avoid stalling input
    let mut files_added = false;
    let drain_start = std::time::Instant::now();
    let drain_budget = std::time::Duration::from_millis(2);
    loop {
        if drain_start.elapsed() >= drain_budget {
            break;
        }
        match file_rx.try_recv() {
            Ok(result) => {
                if let Some(picker) = editor.picker_mut() {
                    picker.add_file_result(result);
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
    update_file_list_cache_from_background(editor);
}

/// Temporary storage for file list results from background task
/// The main event loop will pick these up and update the Editor cache
static FILE_LIST_CACHE_RESULTS: tokio::sync::Mutex<
    Option<(
        std::path::PathBuf,
        std::path::PathBuf,
        Vec<editor::PickerResult>,
    )>,
> = tokio::sync::Mutex::const_new(None);

/// Picks up cached file list results from the background task and updates Editor cache
pub(super) fn update_file_list_cache_from_background(editor: &mut Editor) {
    // Non-blocking try_lock to avoid any contention with background task
    if let Ok(mut guard) = FILE_LIST_CACHE_RESULTS.try_lock() {
        if let Some((base_dir, preferred_dir, files)) = guard.take() {
            editor.update_file_list_cache(base_dir, preferred_dir, files);
        }
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
