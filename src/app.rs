use std::{
  collections::HashMap,
  io,
  path::{Path, PathBuf},
  sync::Arc,
  time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
  event::{self, Event, KeyCode, MouseEvent},
  execute,
  terminal::{EnterAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
  config::SifConfig,
  file_utils,
  repomix_integration::Repomix,
  token_counter::TokenCounter,
  types::{AppState, Backend, BackendRequest, BackendResult, RepomixOptions},
  ui::{UIState, handle_input, render_app, update_ui_state},
  yek_integration::Yek,
};

/// Main app struct that manages the entire siff app.
/// Coordinates between the UI, file system, and backend
pub struct App {
  /// Current app state
  pub state: AppState,
  /// UI-specific state for rendering
  pub ui_state: UIState,
  /// Whether the app should quit
  pub should_quit: bool,
  /// Last time performed an update (for periodic tasks)
  pub last_update: Instant,
  /// Token counter for calculating file sizes
  pub status_message: String,
  /// When the status message was last updated
  pub status_updated_at: Instant,
  /// Whether backend is currently running
  pub is_processing: bool,
  /// Current token count for selected files
  pub token_count: usize,
  /// Persistent configuration for user preferences
  pub config: SifConfig,
  /// Repomix manager for isolated repomix execution (lazy)
  pub repomix: Option<Repomix>,
  /// Sender for token calculation requests
  token_request_sender: mpsc::UnboundedSender<PathBuf>,
  /// Receiver for token calculation results
  token_result_receiver: mpsc::UnboundedReceiver<(PathBuf, usize)>,
  /// Sender for backend execution requests
  backend_request_sender: mpsc::UnboundedSender<BackendRequest>,
  /// Receiver for backend execution results
  backend_result_receiver: mpsc::UnboundedReceiver<BackendResult>,
  /// Counter for generating unique request IDs
  next_request_id: u64,
  /// Current active request ID (for cancellation)
  current_request_id: Option<u64>,
  /// Cancellation token for running processes
  cancellation_token: CancellationToken,
  /// Last time token count was updated (for debouncing)
  last_token_update: Instant,
  /// Debounce duration to prevent excessive updates
  token_update_debounce: Duration,
  /// Track pending token calculations
  pending_token_calculations: std::collections::HashSet<PathBuf>,
  /// Whether in bulk token calculation (select all/unselect all)
  is_bulk_token_calculation: bool,
  /// Suppress status messages during nav
  suppress_status_messages: bool,
}

impl App {
  /// Creates a new app instance.
  /// Scans the given directory and initializes all state.
  pub async fn new(root_path: &Path, backend: Backend) -> Result<Self> {
    // load user config
    let config = SifConfig::load().context("Failed to load configuration")?;

    // if a specific backend was requested via command line, use that
    // otherwise use the saved default backend
    let effective_backend = backend;

    // create repomix options from saved config
    let repomix_options = RepomixOptions {
      backend: effective_backend.clone(),
      compress: config.compress,
      remove_comments: config.remove_comments,
      file_tree: config.include_file_tree,
      output_format: config.output_format.clone(),
      output_file: None, // output file is not persisted (for file tree)
    };

    // scan the directory to build the file tree (shows all files by default)
    let file_tree = file_utils::scan_directory(root_path).context("Failed to scan directory")?;

    // create initial visible files list (just the root directory)
    let visible_paths = file_utils::flatten_visible_tree(&file_tree, root_path);

    // create initial app state
    let state = AppState {
      file_tree,
      root_path: root_path.to_path_buf(),
      visible_paths,
      selected_index: 0,
      repomix_options,
      individual_token_counts: HashMap::new(),
      dir_descendants_map: HashMap::new(),
      dir_descendants_dirty: true,
      status_message: String::new(),
      is_processing: false,
      token_count: 0,
      focus: crate::types::Focus::FileTree,
    };

    // initialize repomix only if using repomix backend
    let repomix = if matches!(effective_backend, Backend::Repomix) {
      let mut r = Repomix::new()?;
      r.start_background_download().await;
      Some(r)
    } else {
      None
    };

    // setups for file tree
    // create channels for background token calculation
    let (token_request_sender, token_request_receiver) = mpsc::unbounded_channel::<PathBuf>();
    let (token_result_sender, token_result_receiver) = mpsc::unbounded_channel::<(PathBuf, usize)>();

    // create channels for non-blocking backend execution
    let (backend_request_sender, backend_request_receiver) = mpsc::unbounded_channel::<BackendRequest>();
    let (backend_result_sender, backend_result_receiver) = mpsc::unbounded_channel::<BackendResult>();

    // spawn background token calculation task
    let token_counter_for_task = TokenCounter::new()?;
    tokio::spawn(async move {
      Self::token_calculation_task(token_counter_for_task, token_request_receiver, token_result_sender).await;
    });

    // spawn background backend execution task with lazy init
    let current_backend_for_task = effective_backend.clone();
    tokio::spawn(async move {
      Self::backend_execution_task_lazy(current_backend_for_task, backend_request_receiver, backend_result_sender)
        .await;
    });

    Ok(Self {
      state,
      ui_state: UIState::default(),
      should_quit: false,
      last_update: Instant::now(),
      status_message: String::new(),
      status_updated_at: Instant::now(),
      is_processing: false,
      token_count: 0,
      config,
      repomix,
      token_request_sender,
      token_result_receiver,
      backend_request_sender,
      backend_result_receiver,
      next_request_id: 0,
      current_request_id: None,
      cancellation_token: CancellationToken::new(),
      last_token_update: Instant::now(),
      token_update_debounce: Duration::from_millis(300),
      pending_token_calculations: std::collections::HashSet::new(),
      is_bulk_token_calculation: false,
      suppress_status_messages: false,
    })
  }

  /// Updates the token count for currently selected files whenever file selection changes.
  /// Returns immediately and updates counts in background, non-blocking.
  pub fn update_token_count_non_blocking(&mut self) -> Result<()> {
    let selected_files = file_utils::get_selected_files(&self.state.file_tree);

    // if no files selected, set count to 0 and clear cache
    if selected_files.is_empty() {
      self.token_count = 0;
      self.state.individual_token_counts.clear();
      self.pending_token_calculations.clear();
      self.is_bulk_token_calculation = false;
      self.suppress_status_messages = false;
      return Ok(());
    }

    // clean up cache, remove entries for files that are no longer selected
    let mut paths_to_remove = Vec::new();
    for path in self.state.individual_token_counts.keys() {
      if let Some(node) = self.state.file_tree.get(path) {
        // remove if file is not selected and not a directory with selected descendants
        if !(node.is_selected || node.is_directory && self.has_selected_descendants(path)) {
          paths_to_remove.push(path.clone());
        }
      } else {
        // remove if file no longer exists
        paths_to_remove.push(path.clone());
      }
    }

    // remove the collected paths
    for path in &paths_to_remove {
      self.state.individual_token_counts.remove(path);
      self.pending_token_calculations.remove(path);
    }

    // calculate total from already cached individual counts
    let mut total_from_cache = 0;
    let mut uncached_files = Vec::new();

    for file_path in &selected_files {
      if let Some(cached_count_opt) = self.state.individual_token_counts.get(file_path) {
        if let Some(cached_count) = cached_count_opt {
          total_from_cache += cached_count;
        } else {
          // none means calc is pending
          uncached_files.push(file_path.clone());
        }
      } else {
        uncached_files.push(file_path.clone());
      }
    }

    // set current total (will be updated as more files are calculated)
    self.token_count = total_from_cache;

    // recalculate directory token counts
    self.recalculate_directory_token_counts();

    // only show calculation messages during bulk operations and when not suppressed
    if !uncached_files.is_empty() && self.is_bulk_token_calculation && !self.suppress_status_messages {
      // message will be updated in process_token_results with progress
    }

    // queue individual calculations for background processing (non-blocking)
    self.queue_individual_token_calculations(selected_files);

    Ok(())
  }

  /// Checks if a directory has any selected descendants
  fn has_selected_descendants(&self, dir_path: &std::path::Path) -> bool {
    for (path, node) in &self.state.file_tree {
      if node.is_selected && path.starts_with(dir_path) && path != dir_path {
        return true;
      }
    }
    false
  }

  /// Debouncing that only updates if enough time has passed.
  /// Prevents UI slow down from rapid selection changes.
  pub fn update_token_count_debounced(&mut self) -> Result<()> {
    let now = Instant::now();

    // if not enough time has passed since last update, skip
    if now.duration_since(self.last_token_update) < self.token_update_debounce {
      return Ok(());
    }

    self.last_token_update = now;
    self.update_token_count_non_blocking()
  }

  /// Recalculates directory token counts from scratch based on currently selected files.
  fn recalculate_directory_token_counts(&mut self) {
    // build map of directories with selected descendants
    let dir_descendants_map = self.build_directories_with_descendants_map();

    // clear all directory token counts first
    let directory_paths: Vec<PathBuf> =
      self.state.file_tree.iter().filter(|(_, node)| node.is_directory).map(|(path, _)| path.clone()).collect();

    // reset all directory counts to 0
    for dir_path in &directory_paths {
      // include directories that are selected or have selected descendants
      if let Some(dir_node) = self.state.file_tree.get(dir_path)
        && (dir_node.is_selected || *dir_descendants_map.get(dir_path).unwrap_or(&false))
      {
        self.state.individual_token_counts.insert(dir_path.clone(), Some(0));
      }
    }

    // recalculate from scratch by summing up all selected files
    for (file_path, token_count_opt) in &self.state.individual_token_counts.clone() {
      if let Some(token_count) = token_count_opt {
        // check if file is actually selected
        if let Some(file_node) = self.state.file_tree.get(file_path)
          && file_node.is_selected
          && !file_node.is_directory
        {
          // find if file is selected, add its tokens to all parent directories that should show counts
          self.add_file_tokens_to_directories_with_selections(file_path, *token_count, &dir_descendants_map);
        }
      }
    }
  }

  /// Adds a file's token count to parent directories that should show token counts.
  /// Includes both selected directories and directories with selected descendants.
  fn add_file_tokens_to_directories_with_selections(
    &mut self,
    file_path: &Path,
    file_tokens: usize,
    dir_descendants_map: &HashMap<PathBuf, bool>,
  ) {
    let mut current_path = file_path.parent();
    while let Some(parent_path) = current_path {
      if let Some(parent_node) = self.state.file_tree.get(parent_path)
        && parent_node.is_directory
      {
        // add tokens if directory is selected or has selected descendants
        if parent_node.is_selected || *dir_descendants_map.get(parent_path).unwrap_or(&false) {
          let current_dir_tokens =
            self.state.individual_token_counts.get(parent_path).and_then(|opt| *opt).unwrap_or(0);
          self.state.individual_token_counts.insert(parent_path.to_path_buf(), Some(current_dir_tokens + file_tokens));
        }
      }
      current_path = parent_path.parent();
    }
  }

  /// Builds a map of directories that have selected descendants.
  fn build_directories_with_descendants_map(&self) -> HashMap<PathBuf, bool> {
    file_utils::build_directories_with_descendants_map(&self.state.file_tree)
  }

  /// Queues individual token calculations for background processing with batching.
  fn queue_individual_token_calculations(&mut self, files: Vec<PathBuf>) {
    // build map of directories with selected descendants
    let dir_descendants_map = self.build_directories_with_descendants_map();

    // clear old individual counts for files that are no longer selected
    // but keep directories that have selected descendants
    let mut new_token_counts = HashMap::new();

    for (path, count) in &self.state.individual_token_counts {
      if let Some(node) = self.state.file_tree.get(path) {
        let should_keep = if node.is_directory {
          // keep directories that are selected or have selected descendants
          node.is_selected || *dir_descendants_map.get(path).unwrap_or(&false)
        } else {
          // keep files that are selected
          node.is_selected
        };

        if should_keep {
          new_token_counts.insert(path.clone(), *count);
        }
      }
    }

    self.state.individual_token_counts = new_token_counts;

    // limit the number of files to process
    // if user selects too many files, only process a subset for token calc
    const MAX_FILES_FOR_TOKEN_CALC: usize = 1000;
    let files_to_process = if files.len() > MAX_FILES_FOR_TOKEN_CALC {
      // only show this message during bulk operations, not regular nav
      if self.is_bulk_token_calculation {
        self.set_status_message(format!("Processing {} files (showing first 1000)...", files.len()));
      }
      files.into_iter().take(MAX_FILES_FOR_TOKEN_CALC).collect()
    } else {
      files
    };

    // clear pending calculations and start fresh
    self.pending_token_calculations.clear();

    // group files by directory for batching
    let mut directory_batches: std::collections::HashMap<PathBuf, Vec<PathBuf>> = std::collections::HashMap::new();
    let mut individual_files = Vec::new();

    for file_path in files_to_process {
      if !self.state.individual_token_counts.contains_key(&file_path) {
        // mark as none to indicate calculation is pending
        self.state.individual_token_counts.insert(file_path.clone(), None);
        // track this file as pending
        self.pending_token_calculations.insert(file_path.clone());

        // group by parent directory for batching
        if let Some(parent) = file_path.parent() {
          directory_batches.entry(parent.to_path_buf()).or_default().push(file_path);
        } else {
          individual_files.push(file_path);
        }
      }
    }

    // send directory batches with queue throttling
    let mut files_queued = 0;
    for (_directory, dir_files) in directory_batches {
      for file_path in dir_files {
        if files_queued >= MAX_FILES_FOR_TOKEN_CALC {
          // remove from pending if we're not going to calculate it
          self.pending_token_calculations.remove(&file_path);
          break;
        }
        if self.token_request_sender.send(file_path).is_err() {
          break;
        }
        files_queued += 1;
      }
      if files_queued >= MAX_FILES_FOR_TOKEN_CALC {
        break;
      }
    }

    // send individual files with queue throttling
    for file_path in individual_files {
      if files_queued >= MAX_FILES_FOR_TOKEN_CALC {
        // remove from pending if we're not going to calculate it
        self.pending_token_calculations.remove(&file_path);
        break;
      }
      if self.token_request_sender.send(file_path).is_err() {
        break;
      }
      files_queued += 1;
    }

    // queue directory calculations for directories that should show counts
    self.queue_directory_calculations();
  }

  /// Queues directory token calculations for directories that should show counts.
  fn queue_directory_calculations(&mut self) {
    // build map of directories with selected descendants
    let dir_descendants_map = self.build_directories_with_descendants_map();

    // include both selected directories and directories with selected descendants
    let relevant_dirs: Vec<PathBuf> = self
      .state
      .file_tree
      .iter()
      .filter(|(path, node)| {
        node.is_directory && (node.is_selected || *dir_descendants_map.get(*path).unwrap_or(&false))
      })
      .map(|(path, _)| path.clone())
      .collect();

    for dir_path in relevant_dirs {
      self.state.individual_token_counts.entry(dir_path).or_insert(Some(0));
    }
  }

  /// Processes token calculation results from the background task (non-blocking).
  fn process_token_results(&mut self) -> bool {
    let mut processed_any = false;

    // receive all available results
    while let Ok((file_path, token_count)) = self.token_result_receiver.try_recv() {
      // update individual token count
      self.state.individual_token_counts.insert(file_path.clone(), Some(token_count));
      // remove from pending calculations
      self.pending_token_calculations.remove(&file_path);
      processed_any = true;
    }

    // only recalculate totals if we processed results and no calculations are pending
    if processed_any {
      if self.pending_token_calculations.is_empty() {
        // all calculations complete, recalculate totals
        self.recalculate_final_token_totals();

        // clear bulk calculation flag and show completion message
        if self.is_bulk_token_calculation {
          self.is_bulk_token_calculation = false;
          let selected_count = file_utils::get_selected_files(&self.state.file_tree).len();
          self.set_status_message(format!("✓ Calculated tokens for {} files", selected_count));
        }
      } else {
        // still have pending calculations, show progress
        let completed = self.state.individual_token_counts.values().filter(|v| v.is_some()).count();
        let total = completed + self.pending_token_calculations.len();

        if self.is_bulk_token_calculation {
          self.set_status_message(format!("Calculating tokens... {}/{}", completed, total));
        }

        // recalculate partial totals for feedback
        self.recalculate_partial_token_totals();
      }
    }

    processed_any
  }

  /// Recalculates totals when all calculations are complete.
  fn recalculate_final_token_totals(&mut self) {
    // recalculate total token count
    let selected_files = file_utils::get_selected_files(&self.state.file_tree);
    let mut total_tokens = 0;

    for file_path in &selected_files {
      if let Some(Some(token_count)) = self.state.individual_token_counts.get(file_path) {
        total_tokens += token_count;
      }
    }

    self.token_count = total_tokens;

    // recalculate directory token counts from scratch
    self.recalculate_directory_token_counts();
  }

  /// Recalculates partial token totals for feedback during calculations.
  fn recalculate_partial_token_totals(&mut self) {
    // only count files that have completed calculations
    let selected_files = file_utils::get_selected_files(&self.state.file_tree);
    let mut total_tokens = 0;

    for file_path in &selected_files {
      if let Some(Some(token_count)) = self.state.individual_token_counts.get(file_path) {
        total_tokens += token_count;
      }
    }

    self.token_count = total_tokens;

    // recalculate directory token counts
    self.recalculate_directory_token_counts();
  }

  /// Processes backend execution results from the background task (non-blocking).
  fn process_backend_results(&mut self) -> bool {
    let mut processed_any = false;

    // receive all available results
    while let Ok(result) = self.backend_result_receiver.try_recv() {
      // check if result is from the current request (ignore cancelled requests)
      if let Some(current_id) = self.current_request_id {
        if result.request_id != current_id {
          // from a cancelled request, ignore it
          continue;
        }
      } else {
        // no current request, ignore result
        continue;
      }

      // update processing state
      self.is_processing = false;
      self.current_request_id = None;

      // handle the result
      if result.success {
        // successful execution
        let message = if result.message.len() > 100 {
          format!("{}...", &result.message[..100])
        } else {
          result.message.to_string()
        };
        self.set_status_message(message);

        // if an output file was created, print it
        if let Some(output_file) = result.output_file {
          self.set_status_message(format!("{} | Output: {}", result.message, output_file.display()));
        }
      } else {
        // failed execution
        if let Some(error) = result.error {
          self.set_status_message(error);
        } else {
          self.set_status_message("Error: Backend execution failed".to_string());
        }
      }

      processed_any = true;
    }

    processed_any
  }

  /// Runs the main application loop.
  pub async fn run(&mut self, terminal: &mut Terminal<impl ratatui::backend::Backend>) -> Result<()> {
    // initial token count calculation, with no debouncing
    self.update_token_count_non_blocking()?;

    loop {
      // sync app state with UI state
      self.sync_app_state();

      // render the UI
      terminal
        .draw(|frame| {
          render_app(frame, &self.state, &mut self.ui_state);
        })
        .map_err(|e| anyhow::anyhow!("draw error: {}", e))?;

      // update UI state to match app state
      update_ui_state(&self.state, &mut self.ui_state);

      // handle events with timeout for periodic updates
      if crossterm::event::poll(Duration::from_millis(100))? {
        match event::read()? {
          Event::Key(key) => {
            let should_continue = self.handle_key_event(key).await?;
            if !should_continue {
              break;
            }
          }
          Event::Mouse(mouse) => {
            self.handle_mouse_event(mouse).await?;
          }
          Event::Resize(_, _) => {
            // let ratatui handle terminal resize
          }
          _ => {}
        }
      }

      // perform periodic updates
      self.periodic_update();

      // update background repomix download if needed
      if matches!(self.state.repomix_options.backend, Backend::Repomix)
        && let Ok(status_changed) = self.update_repomix_download().await
        && status_changed
      {
        // status changed, update UI
        continue;
      }

      // process token calculation results
      if self.process_token_results() {
        // if processed tokens, continue to update UI
        continue;
      }

      // process backend execution results
      if self.process_backend_results() {
        // if processed backend results, continue to update UI
        continue;
      }

      // check if should quit
      if self.should_quit {
        break;
      }

      // update last update time
      self.last_update = Instant::now();
    }

    Ok(())
  }

  /// Handles keyboard input events.
  async fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
    // handle global quit commands
    match key.code {
      KeyCode::Char('c') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
        return Ok(false);
      }
      KeyCode::Char('q') | KeyCode::Esc => {
        return Ok(false);
      }
      KeyCode::Char('r') => {
        self.run_backend().await?;
        return Ok(true);
      }
      // repomix configuration shortcuts
      KeyCode::Char('c') if self.state.repomix_options.backend == crate::types::Backend::Repomix => {
        // toggle compress
        self.state.repomix_options.compress = !self.state.repomix_options.compress;
        if let Err(e) = self.save_repomix_options() {
          self.set_status_message(format!("Error: config save error {}", e));
        } else {
          self.set_status_message(format!(
            "Compress: {}",
            if self.state.repomix_options.compress { "enabled" } else { "disabled" }
          ));
        }
        return Ok(true);
      }
      KeyCode::Char('m') if self.state.repomix_options.backend == crate::types::Backend::Repomix => {
        // toggle remove comments
        self.state.repomix_options.remove_comments = !self.state.repomix_options.remove_comments;
        if let Err(e) = self.save_repomix_options() {
          self.set_status_message(format!("Error: config save error {}", e));
        } else {
          self.set_status_message(format!(
            "Remove comments: {}",
            if self.state.repomix_options.remove_comments { "enabled" } else { "disabled" }
          ));
        }
        return Ok(true);
      }
      KeyCode::Char('f') if self.state.repomix_options.backend == crate::types::Backend::Repomix => {
        // cycle output format (XML, Markdown, Plain Text)
        use crate::types::OutputFormat;
        self.state.repomix_options.output_format = match self.state.repomix_options.output_format {
          OutputFormat::PlainText => OutputFormat::Markdown,
          OutputFormat::Markdown => OutputFormat::Xml,
          OutputFormat::Xml => OutputFormat::PlainText,
        };
        if let Err(e) = self.save_repomix_options() {
          self.set_status_message(format!("Error: config save error {}", e));
        } else {
          self
            .set_status_message(format!("Output format: {}", self.state.repomix_options.output_format.display_name()));
        }
        return Ok(true);
      }
      KeyCode::Char('t') if self.state.repomix_options.backend == crate::types::Backend::Repomix => {
        // toggle file tree
        self.state.repomix_options.file_tree = !self.state.repomix_options.file_tree;
        if let Err(e) = self.save_repomix_options() {
          self.set_status_message(format!("Error: config save error {}", e));
        } else {
          self.set_status_message(format!(
            "File tree: {}",
            if self.state.repomix_options.file_tree { "enabled" } else { "disabled" }
          ));
        }
        return Ok(true);
      }
      // global bulk operations (will work regardless of focus)
      KeyCode::Char('E') => {
        // expand all directories
        crate::file_utils::expand_all_directories(&mut self.state.file_tree);
        self.state.visible_paths =
          crate::file_utils::flatten_visible_tree(&self.state.file_tree, &self.state.root_path);
        self.set_status_message("Expanded all directories".to_string());
        return Ok(true);
      }
      KeyCode::Char('C') => {
        // collapse all directories (keep root expanded)
        crate::file_utils::collapse_all_directories(&mut self.state.file_tree);
        // re-expand the root directory
        if let Some(root_node) = self.state.file_tree.get_mut(&self.state.root_path) {
          root_node.is_expanded = true;
        }
        self.state.visible_paths =
          crate::file_utils::flatten_visible_tree(&self.state.file_tree, &self.state.root_path);
        self.set_status_message("Collapsed all directories".to_string());
        return Ok(true);
      }
      KeyCode::Char('A') => {
        // select all visible items (files and directories)
        match crate::file_utils::select_all_visible_files(&mut self.state.file_tree, &self.state.visible_paths) {
          Ok(()) => {
            self.state.dir_descendants_dirty = true;
            // clear token cache
            self.state.individual_token_counts.clear();
            self.pending_token_calculations.clear();
            // set bulk calculation flag
            self.is_bulk_token_calculation = true;
            // allow status messages
            self.suppress_status_messages = false;
            self.set_status_message("Selected all items - calculating tokens...".to_string());
          }
          Err(e) => {
            self.set_status_message(format!("Error selecting items: {}", e));
          }
        }
        // force token count update without debouncing
        if let Err(e) = self.update_token_count_non_blocking() {
          self.set_status_message(format!("Error: token count error {}", e));
        }
        return Ok(true);
      }
      KeyCode::Char('U') => {
        // unselect all items
        crate::file_utils::unselect_all_items(&mut self.state.file_tree);
        self.state.dir_descendants_dirty = true;
        // clear token cache
        self.state.individual_token_counts.clear();
        self.pending_token_calculations.clear();
        self.token_count = 0;
        self.is_bulk_token_calculation = false;
        self.suppress_status_messages = false;
        self.set_status_message("Unselected all items".to_string());
        // no need to update token count since we know it's 0
        return Ok(true);
      }
      _ => {}
    }

    // let the UI components handle the input
    let input_handled = handle_input(&mut self.state, &mut self.ui_state, key);

    // if input was handled and might have changed file selection, update token count
    if input_handled {
      // check if key was a selection-changing key
      match key.code {
        KeyCode::Char(' ') => {
          // space key toggles selection, so update token count
          self.suppress_status_messages = false;
          self.state.dir_descendants_dirty = true;
          self.update_token_count_debounced()?;
        }
        KeyCode::Char('h') | KeyCode::Char('l') | KeyCode::Left | KeyCode::Right => {
          // h/l and arrow keys can toggle directory expansion, which might affect visible selections
          // but don't trigger token recalculation unless selections changed
          // only update if in bulk calculation
          if self.is_bulk_token_calculation {
            self.update_token_count_debounced()?;
          }
        }
        KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k') => {
          // navigation keys don't change selections, so don't update token count
          // clear any existing calculation messages and suppress new ones
          if self.status_message.contains("Calculating tokens") && !self.is_bulk_token_calculation {
            self.clear_status_message();
          }
          self.suppress_status_messages = true;
        }
        _ => {}
      }
    }

    Ok(true)
  }

  /// Handles mouse input events.
  async fn handle_mouse_event(&mut self, mouse: MouseEvent) -> Result<()> {
    use crossterm::event::MouseEventKind;

    match mouse.kind {
      MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
        // handle left mouse click
        self.handle_mouse_click(mouse.column, mouse.row).await?;
      }
      MouseEventKind::ScrollUp => {
        // scroll up
        if self.state.visible_paths.is_empty() {
          // no files to navigate
        } else if self.state.selected_index == 0 {
          // wrap to bottom
          self.state.selected_index = self.state.visible_paths.len() - 1;
        } else {
          self.state.selected_index -= 1;
        }
      }
      MouseEventKind::ScrollDown => {
        // scroll down
        if self.state.visible_paths.is_empty() {
          // no files to navigate
        } else if self.state.selected_index >= self.state.visible_paths.len() - 1 {
          // wrap to top
          self.state.selected_index = 0;
        } else {
          self.state.selected_index += 1;
        }
      }
      _ => {
        // ignore other mouse events
      }
    }

    Ok(())
  }

  /// Handles mouse clicks by determining which UI element was clicked,
  /// and performing the corresponding action (selection, expansion).
  async fn handle_mouse_click(&mut self, column: u16, row: u16) -> Result<()> {
    // check if click is in the file tree area
    if let Some(clicked_file_index) = self.calculate_clicked_file_index(row)
      && clicked_file_index < self.state.visible_paths.len()
    {
      // update selection to clicked item
      self.state.selected_index = clicked_file_index;

      // get the clicked path
      if let Some(clicked_path) = self.state.visible_paths.get(clicked_file_index) {
        let clicked_path = clicked_path.clone();

        // determine action based on click position and file type
        if let Some(node) = self.state.file_tree.get(&clicked_path) {
          let mut selection_changed = false;
          if node.is_directory {
            // for directories, check if click was on expansion icon or name
            // adjust depth for rootless tree view
            let display_depth = node.depth.saturating_sub(1);
            let indent_width = display_depth * 2; // 2 spaces per depth level
            let icon_start = indent_width;
            let icon_end = icon_start + 3; // "[+]" or "[-]" is 3 characters

            if column as usize >= icon_start && column as usize <= icon_end + 1 {
              // clicked on expansion icon, then toggle expansion
              if let Some(node_mut) = self.state.file_tree.get_mut(&clicked_path) {
                node_mut.toggle_expansion();
                self.update_visible_files();
              }
            } else {
              // clicked on directory name, then toggle selection
              if crate::file_utils::toggle_selection_recursive(&mut self.state.file_tree, &clicked_path).is_err() {
                // silently handle errors
              } else {
                selection_changed = true;
              }
            }
          } else {
            // for files, toggle selection
            if crate::file_utils::toggle_selection_recursive(&mut self.state.file_tree, &clicked_path).is_err() {
              // silently handle errors
            } else {
              selection_changed = true;
            }
          }

          // update token count
          if selection_changed {
            self.state.dir_descendants_dirty = true;
          }
          self.update_token_count_debounced()?;
        }
      }
    }

    Ok(())
  }

  /// Calculates which file index was clicked based on the row position.
  /// Returns none if the click was outside the file list area.
  fn calculate_clicked_file_index(&self, row: u16) -> Option<usize> {
    let config_height = match self.state.repomix_options.backend {
      Backend::Repomix => 3, // repomix config box height
      Backend::Yek => 0,     // yek has no config
    };

    let file_list_start_row = config_height + 2; // +1 for border, +1 for directory info

    if row < file_list_start_row {
      return None; // click was above file list area
    }

    let file_row = (row - file_list_start_row) as usize;

    // check if have enough files and the click is within bounds
    if file_row < self.state.visible_paths.len() { Some(file_row) } else { None }
  }

  /// Updates the visible files list after expansion changes.
  fn update_visible_files(&mut self) {
    self.state.visible_paths = crate::file_utils::flatten_visible_tree(&self.state.file_tree, &self.state.root_path);

    // see if selected index is still valid
    if self.state.selected_index >= self.state.visible_paths.len() {
      self.state.selected_index = self.state.visible_paths.len().saturating_sub(1);
    }
  }

  /// Runs the selected backend with the currently selected files and options.
  async fn run_backend(&mut self) -> Result<()> {
    // get selected files
    let selected_files = file_utils::get_selected_files(&self.state.file_tree);

    if selected_files.is_empty() {
      self.set_status_message("No files selected for processing".to_string());
      return Ok(());
    }

    // validate options based on backend
    let warnings = match self.state.repomix_options.backend {
      crate::types::Backend::Repomix => {
        crate::repomix_integration::validate_isolated_repomix_options(&self.state.repomix_options, &selected_files)
      }
      crate::types::Backend::Yek => crate::yek_integration::validate_yek_options(&selected_files),
    };

    if !warnings.is_empty() {
      self.set_status_message(format!("Warning: {}", warnings.join(", ")));
      // continue anyway, but show the warning
    }

    // check if already processing, then cancel and restart
    if self.is_processing {
      self.set_status_message("Cancelling previous run and restarting...".to_string());
      // cancel the current running process
      self.cancellation_token.cancel();
      self.is_processing = false;
      self.current_request_id = None;
    }

    // create new cancellation token for request
    self.cancellation_token = CancellationToken::new();

    // for repomix, check download status first
    if matches!(self.state.repomix_options.backend, Backend::Repomix) {
      let download_status = self.repomix.as_ref().unwrap().download_status().clone();
      match download_status {
        crate::repomix_integration::DownloadStatus::Downloading(msg) => {
          self.set_status_message(format!("Downloading: {}", msg));
          return Ok(());
        }
        crate::repomix_integration::DownloadStatus::Failed(err) => {
          // try to restart download
          self.repomix.as_mut().unwrap().start_background_download().await;
          self.set_status_message(format!("Repomix download failed: {}", err));
          return Ok(());
        }
        crate::repomix_integration::DownloadStatus::NotStarted => {
          // start download
          self.repomix.as_mut().unwrap().start_background_download().await;
          self.set_status_message("Starting repomix download...".to_string());
          return Ok(());
        }
        crate::repomix_integration::DownloadStatus::Ready => {
          // download is ready
        }
      }
    }

    // generate new request ID
    let request_id = self.next_request_id;
    self.next_request_id += 1;
    self.current_request_id = Some(request_id);

    // set processing state
    self.is_processing = true;
    let backend_name = self.state.repomix_options.backend.display_name();

    self.set_status_message(format!("Running {} on {} files...", backend_name, selected_files.len()));

    // create backend request
    let request = BackendRequest {
      backend: self.state.repomix_options.backend.clone(),
      repomix_options: self.state.repomix_options.clone(),
      selected_files,
      root_path: self.state.root_path.clone(),
      file_tree: self.state.file_tree.clone(),
      request_id,
      cancellation_token: self.cancellation_token.clone(),
    };

    // send request to background thread (non-blocking)
    if self.backend_request_sender.send(request).is_err() {
      self.is_processing = false;
      self.set_status_message("Failed to start backend execution".to_string());
    }

    Ok(())
  }

  /// Performs periodic updates.
  fn periodic_update(&mut self) {
    // clear old status messages
    if !self.status_message.is_empty() && !self.is_processing {
      let should_clear = if self.is_bulk_token_calculation {
        // keep bulk calculation messages longer (5 seconds)
        self.status_updated_at.elapsed() > Duration::from_secs(5)
      } else if self.status_message.contains("Calculating tokens") {
        // clear token calculation messages after 1 second
        self.status_updated_at.elapsed() > Duration::from_secs(1)
      } else if self.status_message.starts_with("✓") {
        // clear completion messages after 2 seconds
        self.status_updated_at.elapsed() > Duration::from_secs(2)
      } else {
        // clear other messages normally (3 seconds)
        self.status_updated_at.elapsed() > Duration::from_secs(3)
      };

      if should_clear {
        self.status_message.clear();
      }
    }

    // clear suppress flag after 2 seconds
    if self.suppress_status_messages && self.last_update.elapsed() > Duration::from_secs(2) {
      self.suppress_status_messages = false;
    }
  }

  /// Sets a status message and updates the timestamp.
  fn set_status_message(&mut self, message: String) {
    self.status_message = message;
    self.status_updated_at = Instant::now();
  }

  /// Clears the current status message.
  fn clear_status_message(&mut self) {
    self.status_message.clear();
  }

  /// Expands the root directory to show initial files.
  pub fn expand_root(&mut self) {
    if let Some(root_node) = self.state.file_tree.get_mut(&self.state.root_path)
      && root_node.is_directory
      && !root_node.is_expanded
    {
      root_node.is_expanded = true;
      self.state.visible_paths = file_utils::flatten_visible_tree(&self.state.file_tree, &self.state.root_path);
    }
  }

  /// Syncs app state with UI state.
  fn sync_app_state(&mut self) {
    self.state.status_message = self.status_message.clone();
    self.state.is_processing = self.is_processing;
    self.state.token_count = self.token_count;
    if self.state.dir_descendants_dirty {
      self.state.dir_descendants_map = file_utils::build_directories_with_descendants_map(&self.state.file_tree);
      self.state.dir_descendants_dirty = false;
    }
  }

  /// Updates repomix background download and returns true if status changed.
  async fn update_repomix_download(&mut self) -> Result<bool> {
    let status_changed = self.repomix.as_mut().unwrap().update_background_download().await;

    if status_changed {
      // update status message
      match self.repomix.as_ref().unwrap().download_status() {
        crate::repomix_integration::DownloadStatus::Ready => {
          self.set_status_message("Repomix ready!".to_string());
        }
        crate::repomix_integration::DownloadStatus::Downloading(msg) => {
          self.set_status_message(format!("Downloading repomix: {}", msg));
        }
        crate::repomix_integration::DownloadStatus::Failed(err) => {
          self.set_status_message(format!("Repomix download failed: {}", err));
        }
        crate::repomix_integration::DownloadStatus::NotStarted => {
          // restart download if failed
          self.repomix.as_mut().unwrap().start_background_download().await;
        }
      }
    }

    Ok(status_changed)
  }

  /// Saves the current repomix options to persistent configuration.
  pub fn save_repomix_options(&mut self) -> Result<()> {
    self.config.update_repomix_options(
      self.state.repomix_options.compress,
      self.state.repomix_options.remove_comments,
      self.state.repomix_options.file_tree,
      self.state.repomix_options.output_format.clone(),
    )?;
    Ok(())
  }

  /// Background task that processes token calculation requests.
  /// Runs independently from the main UI thread, uses shared cache with semaphore concurrency control.
  async fn token_calculation_task(
    _token_counter: TokenCounter,
    mut request_receiver: mpsc::UnboundedReceiver<PathBuf>,
    result_sender: mpsc::UnboundedSender<(PathBuf, usize)>,
  ) {
    // create a shared cache that all TokenCounters will use
    let shared_cache = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let max_concurrency = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(4).clamp(2, 8);
    let semaphore = std::sync::Arc::new(Semaphore::new(max_concurrency));

    // process files as they come in, with controlled concurrency
    while let Some(file_path) = request_receiver.recv().await {
      let shared_cache = shared_cache.clone();
      let result_sender = result_sender.clone();
      let permit = match semaphore.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => break,
      };

      // spawn a task for each file with semaphore concurrency control
      tokio::spawn(async move {
        let _permit = permit;
        // create a TokenCounter that shares the cache
        let token_counter = TokenCounter::with_shared_cache(shared_cache);

        // calculate token count for file
        match token_counter.count_file_tokens(&file_path).await {
          Ok(count) => {
            // send result back to main thread
            if result_sender.send((file_path, count)).is_err() {
              // main thread has closed, exit
            }
          }
          Err(_) => {
            // send 0 for files that can't be read
            if result_sender.send((file_path, 0)).is_err() {}
          }
        }
      });
    }
  }

  /// Background task that handles backend execution requests.
  /// Runs independently from the main UI thread, supports immediate cancellation.
  async fn backend_execution_task_lazy(
    _current_backend: Backend,
    mut request_receiver: mpsc::UnboundedReceiver<BackendRequest>,
    result_sender: mpsc::UnboundedSender<BackendResult>,
  ) {
    // lazily initialize backends only when needed
    let mut yek_instance: Option<Arc<Yek>> = None;
    let mut repomix_instance: Option<Arc<Mutex<Repomix>>> = None;

    // process requests until the receiver is closed
    while let Some(request) = request_receiver.recv().await {
      let cancellation_token = request.cancellation_token.clone();
      let result_sender = result_sender.clone();

      // initialize the required backend if not already done
      match request.backend {
        Backend::Repomix => {
          if repomix_instance.is_none() {
            match Repomix::new() {
              Ok(mut r) => {
                r.start_background_download().await;
                repomix_instance = Some(Arc::new(Mutex::new(r)));
              }
              Err(e) => {
                let _ = result_sender.send(BackendResult {
                  success: false,
                  message: String::new(),
                  output_file: None,
                  error: Some(format!("Failed to initialize repomix: {}", e)),
                  request_id: request.request_id,
                });
                continue;
              }
            }
          }
        }
        Backend::Yek => {
          if yek_instance.is_none() {
            match Yek::new() {
              Ok(y) => {
                yek_instance = Some(Arc::new(y));
              }
              Err(e) => {
                let _ = result_sender.send(BackendResult {
                  success: false,
                  message: String::new(),
                  output_file: None,
                  error: Some(format!("Failed to initialize yek: {}", e)),
                  request_id: request.request_id,
                });
                continue;
              }
            }
          }
        }
      };

      // clone the instances for the spawned task
      let repomix_clone = repomix_instance.clone();
      let yek_clone = yek_instance.clone();

      // spawn a cancellable task
      tokio::spawn(async move {
        // execute the backend op
        let result = match request.backend {
          Backend::Repomix => {
            if let Some(repomix_arc) = repomix_clone {
              // run repomix with cancellation support
              tokio::select! {
                  result = async {
                      let mut manager = repomix_arc.lock().await;
                      manager.run_isolated_repomix(
                          &request.selected_files,
                          &request.repomix_options,
                          &request.root_path,
                          &request.file_tree,
                      ).await
                  } => {
                      match result {
                          Ok(output) => BackendResult {
                              success: true,
                              message: output,
                              output_file: request.repomix_options.output_file.map(PathBuf::from),
                              error: None,
                              request_id: request.request_id,
                          },
                          Err(e) => BackendResult {
                              success: false,
                              message: String::new(),
                              output_file: None,
                              error: Some(format!("Error: repomix error {}", e)),
                              request_id: request.request_id,
                          },
                      }
                  }
                  _ = cancellation_token.cancelled() => {
                      // operation was cancelled, the process will be killed by the os
                      // when the parent task is dropped
                      BackendResult {
                          success: false,
                          message: String::new(),
                          output_file: None,
                          error: Some("Operation cancelled".to_string()),
                          request_id: request.request_id,
                      }
                  }
              }
            } else {
              BackendResult {
                success: false,
                message: String::new(),
                output_file: None,
                error: Some("Repomix not initialized".to_string()),
                request_id: request.request_id,
              }
            }
          }
          Backend::Yek => {
            if let Some(yek_arc) = yek_clone {
              // run yek with cancellation support
              tokio::select! {
                  result = yek_arc.run_yek_integrated(&request.selected_files, &request.root_path) => {
                      match result {
                          Ok(output) => BackendResult {
                              success: true,
                              message: output,
                              // yek doesn't create output files
                              output_file: None,
                              error: None,
                              request_id: request.request_id,
                          },
                          Err(e) => BackendResult {
                              success: false,
                              message: String::new(),
                              output_file: None,
                              error: Some(format!("Error: yek error {}", e)),
                              request_id: request.request_id,
                          },
                      }
                  }
                  _ = cancellation_token.cancelled() => {
                      // operation was cancelled, the process will be killed by the os
                      // when the parent task is dropped
                      BackendResult {
                          success: false,
                          message: String::new(),
                          output_file: None,
                          error: Some("Operation cancelled".to_string()),
                          request_id: request.request_id,
                      }
                  }
              }
            } else {
              BackendResult {
                success: false,
                message: String::new(),
                output_file: None,
                error: Some("Yek not initialized".to_string()),
                request_id: request.request_id,
              }
            }
          }
        };

        // send result back to main thread (non-blocking)
        if result_sender.send(result).is_err() {
          // main thread has closed, exit
        }
      });
    }
  }
}

/// Initializes the terminal for TUI mode.
/// Sets up raw mode and alternate screen.
pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
  enable_raw_mode().context("Error: failed to enable raw mode")?;
  let mut stdout = io::stdout();
  execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture)
    .context("Error: failed to enter alternate screen and enable mouse")?;
  let backend = CrosstermBackend::new(stdout);
  let terminal = Terminal::new(backend).context("Error: failed to create terminal")?;
  Ok(terminal)
}

/// Restores the terminal to normal mode.
/// Cleans up raw mode and alternate screen.
pub fn restore_terminal(_terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
  use std::io::{self, Write};

  // disable all mouse tracking modes with direct escape sequences to avoid crossterm issues
  // disable mouse tracking
  print!("\x1b[?1000l");
  // disable button event tracking
  print!("\x1b[?1002l");
  // disable any event tracking
  print!("\x1b[?1003l");
  // disable SGR mouse mode
  print!("\x1b[?1006l");
  // leave alternate screen
  print!("\x1b[?1049l");

  // flush immediately so escape sequences are sent
  let _ = io::stdout().flush();

  // small delay so terminal processes the escape sequences
  std::thread::sleep(std::time::Duration::from_millis(100));

  // disable raw mode
  let _ = disable_raw_mode().is_err();

  // final flush
  let _ = io::stdout().flush();

  Ok(())
}

/// Runs the siff app, sets up terminal, runs the app, and cleans up.
pub async fn run_app(root_path: &Path, backend: crate::types::Backend) -> Result<()> {
  // setup terminal
  let mut terminal = setup_terminal()?;

  // create and run the app
  let result = async {
    let mut app = App::new(root_path, backend).await?;

    // expand root directory (default)
    app.expand_root();

    // run the main app loop
    app.run(&mut terminal).await
  }
  .await;

  // always restore terminal, even if the app fails
  restore_terminal(&mut terminal)?;

  result
}
