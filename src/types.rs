use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Backend options for file processing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum Backend {
  #[default]
  Repomix,
  Yek,
}

impl Backend {
  /// Returns the display name for backend.
  pub fn display_name(&self) -> &'static str {
    match self {
      Backend::Repomix => "Repomix",
      Backend::Yek => "Yek",
    }
  }
}

/// Output format options for repomix (not needed for yek).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum OutputFormat {
  #[default]
  PlainText,
  Markdown,
  Xml,
}

impl OutputFormat {
  /// Returns the display name for format.
  pub fn display_name(&self) -> &'static str {
    match self {
      OutputFormat::PlainText => "Plain Text",
      OutputFormat::Markdown => "Markdown",
      OutputFormat::Xml => "XML",
    }
  }

  /// Returns the repomix command line flag for format.
  pub fn repomix_flag(&self) -> Option<&'static str> {
    match self {
      OutputFormat::PlainText => Some("--style=plain"),
      OutputFormat::Markdown => Some("--style=markdown"),
      OutputFormat::Xml => Some("--style=xml"),
    }
  }
}

/// Represents a single file or directory in our file tree.
/// Holds core data for the file tree.
#[derive(Debug, Clone, PartialEq)]
pub struct FileNode {
  /// The full path to file or directory
  pub path: PathBuf,
  /// Just the filename or directory name (last component of path)
  pub name: String,
  /// True if is a directory, false if is a file
  pub is_directory: bool,
  /// Whether file/directory is currently selected for repomix processing
  pub is_selected: bool,
  /// For directories: whether the directory is expanded to show children
  pub is_expanded: bool,
  /// For directories: list of child file/directory paths
  pub children: Vec<PathBuf>,
  /// How deep node is in the tree (0 = root level)
  pub depth: usize,
}

/// Configuration options for repomix execution.
/// These mirror the command-line options that repomix accepts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepomixOptions {
  /// whether to compress the output
  pub compress: bool,
  /// whether to remove comments from source code
  pub remove_comments: bool,
  /// whether to include complete file tree in output
  pub file_tree: bool,
  /// Custom output filename (if not specified, repomix uses default)
  pub output_file: Option<String>,
  /// Output format for the generated file
  pub output_format: OutputFormat,
  /// Backend to use for processing
  pub backend: Backend,
}

/// Represents which UI component currently has focus.
/// TODO: remove old tab ui compoents
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Focus {
  #[default]
  FileTree,
}

/// Main application state that holds all the data needed for the UI,
/// central state that gets passed around to different components.
#[derive(Debug)]
pub struct AppState {
  /// The root directory being scanned
  pub root_path: PathBuf,
  /// Hierarchical file tree structure for navigation
  pub file_tree: HashMap<PathBuf, FileNode>,
  /// Flat list of currently visible paths in the tree view
  pub visible_paths: Vec<PathBuf>,
  /// Index of currently selected item in the visible list
  pub selected_index: usize,
  /// Configuration options for repomix execution
  pub repomix_options: RepomixOptions,
  /// Individual token counts for each file and directory
  pub individual_token_counts: HashMap<PathBuf, Option<usize>>,
  /// Cached map of directories that have selected descendants
  pub dir_descendants_map: HashMap<PathBuf, bool>,
  /// Whether cached descendant map needs a rebuild
  pub dir_descendants_dirty: bool,
  /// Current status message to display to user
  pub status_message: String,
  /// Whether the app is currently processing files
  pub is_processing: bool,
  /// Total token count for selected files
  pub token_count: usize,
  /// Which UI component currently has focus
  pub focus: Focus,
}

/// Result type for file scanning operations.
/// Handles errors gracefully when reading the file system.
pub type ScanResult = anyhow::Result<HashMap<PathBuf, FileNode>>;

impl Default for RepomixOptions {
  /// Provides sensible default values for repomix options.
  fn default() -> Self {
    Self {
      compress: false,
      remove_comments: false,
      file_tree: false,
      output_file: None,
      output_format: OutputFormat::default(),
      backend: Backend::default(),
    }
  }
}

impl FileNode {
  /// Creates a new file node with the given path and metadata.
  /// Automatically determines if it's a directory and extracts the name.
  pub fn new(path: PathBuf, is_directory: bool, depth: usize) -> Self {
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();

    Self { path, name, is_directory, is_selected: false, is_expanded: false, children: Vec::new(), depth }
  }

  /// Toggles the expanded state of a directory.
  /// Only applies to directories, files cannot be expanded.
  pub fn toggle_expansion(&mut self) {
    if self.is_directory {
      self.is_expanded = !self.is_expanded;
    }
  }
}

/// Request type for backend execution in separate thread.
#[derive(Debug)]
pub struct BackendRequest {
  /// Backend to use for processing
  pub backend: Backend,
  /// Repomix options for configuration
  pub repomix_options: RepomixOptions,
  /// List of selected files to process
  pub selected_files: Vec<PathBuf>,
  /// Root directory path
  pub root_path: PathBuf,
  /// Complete file tree for generating directory structure
  pub file_tree: HashMap<PathBuf, FileNode>,
  /// Unique request id for cancellation
  pub request_id: u64,
  /// Cancellation token to immediately stop the process
  pub cancellation_token: CancellationToken,
}

/// Result type for backend execution.
#[derive(Debug, Clone)]
pub struct BackendResult {
  /// Whether the operation was successful
  pub success: bool,
  /// Output message from the backend
  pub message: String,
  /// Optional output file path if created
  pub output_file: Option<PathBuf>,
  /// Optional error message if failed
  pub error: Option<String>,
  /// Request id that result corresponds to
  pub request_id: u64,
}
