use super::filter;
use super::result::PickerResult;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// State for the LiveGrep picker mode.
pub struct GrepState {
    pub grep_rx: Option<mpsc::Receiver<Vec<PickerResult>>>,
    pub grep_cancel: Option<Arc<AtomicBool>>,
    pub last_grep_query: String,
    pub grep_stale: bool,
    pub loading: bool,
    pub last_filtered_file_filter: String,
}

impl GrepState {
    pub fn new() -> Self {
        Self {
            grep_rx: None,
            grep_cancel: None,
            last_grep_query: String::new(),
            grep_stale: false,
            loading: false,
            last_filtered_file_filter: String::new(),
        }
    }

    /// Starts an in-process grep search, cancelling any previous one.
    pub fn start_search(
        &mut self,
        query: &str,
        base_dir: &Path,
        preferred_dir: &Path,
        all_results: &mut Vec<PickerResult>,
        filtered_results: &mut Vec<PickerResult>,
        selected_index: &mut usize,
    ) {
        self.cancel();

        if query.is_empty() {
            all_results.clear();
            filtered_results.clear();
            *selected_index = 0;
            return;
        }

        self.loading = true;
        self.last_grep_query = query.to_string();

        let cancel = Arc::new(AtomicBool::new(false));
        self.grep_cancel = Some(cancel.clone());

        let rx = super::super::grep::spawn_grep_search(
            query.to_string(),
            base_dir.to_path_buf(),
            preferred_dir.to_path_buf(),
            cancel,
        );
        self.grep_rx = Some(rx);
        self.grep_stale = true;
    }

    /// Drains grep result batches from the channel with a time budget
    /// (checked per batch). Returns true if any new results were added.
    pub fn drain_results(
        &mut self,
        file_filter: &str,
        all_results: &mut Vec<PickerResult>,
        filtered_results: &mut Vec<PickerResult>,
        selected_index: &mut usize,
    ) -> bool {
        let rx = match self.grep_rx.as_mut() {
            Some(rx) => rx,
            None => return false,
        };

        let start = std::time::Instant::now();
        let budget = std::time::Duration::from_millis(3);
        let mut added = false;

        loop {
            if start.elapsed() >= budget {
                break;
            }

            match rx.try_recv() {
                Ok(batch) => {
                    if self.grep_stale {
                        all_results.clear();
                        filtered_results.clear();
                        *selected_index = 0;
                        self.grep_stale = false;
                    }
                    for result in batch {
                        if filter::matches_file_filter(file_filter, &result.display) {
                            filtered_results.push(result.clone());
                        }
                        all_results.push(result);
                        added = true;
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    if self.grep_stale {
                        all_results.clear();
                        filtered_results.clear();
                        *selected_index = 0;
                        self.grep_stale = false;
                    }
                    self.grep_rx = None;
                    self.loading = false;
                    break;
                }
            }
        }

        added
    }

    /// Cancels any in-flight grep search.
    pub fn cancel(&mut self) {
        if let Some(cancel) = self.grep_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.grep_rx = None;
        self.loading = false;
    }
}
