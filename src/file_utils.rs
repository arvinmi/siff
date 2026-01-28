use std::{
  collections::HashMap,
  path::{Component, Path, PathBuf},
};

use anyhow::Result;
use walkdir::WalkDir;

use crate::types::{FileNode, ScanResult};

// ============================================================================
// Shared utilities
// ============================================================================

/// Validates a file path for safe processing.
/// Returns the relative path string if valid, None if should be skipped.
pub fn validate_file_path(file_path: &Path, root_path: &Path) -> Option<String> {
  let relative_path = file_path.strip_prefix(root_path).ok()?;

  // skip paths that try to escape
  if relative_path.components().any(|component| matches!(component, Component::ParentDir)) {
    let path_str = relative_path.to_string_lossy();
    eprintln!("Warning: Skipping file with path traversal attempt: {}", path_str);
    return None;
  }

  let path_str = relative_path.to_string_lossy();

  // skip empty or dangerous paths
  if path_str.is_empty() || path_str.starts_with('-') {
    eprintln!("Warning: Skipping file with invalid path: {}", path_str);
    return None;
  }

  Some(path_str.to_string())
}

/// Builds a map of directories that have selected descendants.
pub fn build_directories_with_descendants_map(file_tree: &HashMap<PathBuf, FileNode>) -> HashMap<PathBuf, bool> {
  let mut dir_has_selected_descendant: HashMap<PathBuf, bool> = HashMap::new();

  // find all selected nodes and mark their parent directories
  for (path, node) in file_tree {
    if node.is_selected {
      // mark all parent directories as having selected descendants
      let mut current = path.parent();
      while let Some(parent_path) = current {
        dir_has_selected_descendant.insert(parent_path.to_path_buf(), true);
        current = parent_path.parent();
      }
    }
  }

  dir_has_selected_descendant
}

// ============================================================================
// File scanning
// ============================================================================

/// Scans a directory and builds a complete file tree.
/// Walks through all files and dirs recursively.
/// Creates a flat hashmap for efficient lookups.
pub fn scan_directory(root_path: &Path) -> ScanResult {
  let mut file_tree = HashMap::new();

  // use walkdir to recursively traverse the dir tree
  // walkdir handles symlinks and permissions
  for entry in WalkDir::new(root_path)
    .follow_links(false) // don't follow symbolic links to avoid cycles
    .into_iter()
    .filter_map(|e| e.ok())
  // skip entries can't read (permissions issues)
  {
    let path = entry.path().to_path_buf();
    let is_directory = entry.file_type().is_dir();

    // calculate depth relative to root dir
    // helps indent the tree view
    let depth = entry.depth();

    // skip problematic files and dirs
    if should_skip_file(&path) {
      continue;
    }

    let node = FileNode::new(path.clone(), is_directory, depth);

    // if is a dir, populate children later
    // for now, just create the node
    file_tree.insert(path, node);
  }

  // now build parent-child relationships
  // happens in second pass to make sure all nodes exist
  build_parent_child_relationships(&mut file_tree, root_path)?;

  Ok(file_tree)
}

/// Builds parent-child relationships in the file tree.
/// Creates the hierarchical structure needed for tree navigation.
fn build_parent_child_relationships(file_tree: &mut HashMap<PathBuf, FileNode>, root_path: &Path) -> Result<()> {
  // collect all paths first to avoid borrowing issues
  let all_paths: Vec<PathBuf> = file_tree.keys().cloned().collect();

  for path in all_paths {
    if let Some(parent_path) = path.parent() {
      // only process if parent is within our scanned tree
      if parent_path >= root_path && file_tree.contains_key(parent_path) {
        // add path as a child of its parent (parent node)
        if let Some(parent_node) = file_tree.get_mut(parent_path)
          && parent_node.is_directory
          && !parent_node.children.contains(&path)
        {
          parent_node.children.push(path.clone());
        }
      }
    }
  }

  // sort children for consistent display order
  // dirs first, then files, both alphabetically
  // need to collect the paths first to avoid borrowing issues
  let directory_paths: Vec<PathBuf> =
    file_tree.iter().filter(|(_, node)| node.is_directory).map(|(path, _)| path.clone()).collect();

  for dir_path in directory_paths {
    if let Some(node) = file_tree.get_mut(&dir_path) {
      node.children.sort_by(|a, b| {
        // need to look up the nodes without borrowing file_tree mutably
        let a_name = a.file_name().unwrap_or_default();
        let b_name = b.file_name().unwrap_or_default();

        // determine if paths are dirs by checking if they end with known dir patterns
        // or by checking the file extension (is heuristic since can't borrow file_tree)
        let a_is_likely_dir = a.extension().is_none() || a.to_string_lossy().ends_with('/');
        let b_is_likely_dir = b.extension().is_none() || b.to_string_lossy().ends_with('/');

        match (a_is_likely_dir, b_is_likely_dir) {
          // dirs first
          (true, false) => std::cmp::Ordering::Less,
          // files second
          (false, true) => std::cmp::Ordering::Greater,
          // same type, alphabetical
          _ => a_name.cmp(b_name),
        }
      });
    }
  }

  Ok(())
}

/// Determines if a file should be skipped during scanning.
/// Only skips files that would cause technical issues or performance problems.
/// Respects user choice for everything else.
fn should_skip_file(path: &Path) -> bool {
  let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

  // always skip .git dir
  if file_name == ".git" {
    return true;
  }

  // always skip .gitignore
  if file_name == ".gitignore" {
    return true;
  }

  // skip common large build/dependency directories that cause performance issues
  // these typically contain thousands of generated files that users don't want to process
  // TODO: make this configurable, or test speedups for token counter
  let large_dirs_to_skip = [
    "target",
    "node_modules",
    "build",
    "dist",
    ".next",
    ".nuxt",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    "venv",
    ".venv",
    "env",
    ".env",
    "coverage",
    ".coverage",
    "tmp",
    "temp",
    ".tmp",
    "logs",
    ".DS_Store",
    "Thumbs.db",
  ];

  if large_dirs_to_skip.iter().any(|&skip_name| file_name.eq_ignore_ascii_case(skip_name)) {
    return true;
  }

  // skip very large files that are likely binary or large data files (>100MB)
  if let Ok(metadata) = path.metadata()
    && metadata.len() > 100_000_000
  {
    return true;
  }

  false
}

/// Flattens the file tree into a list of visible paths for rendering.
/// Only includes expanded dirs and their visible children.
/// Creates the linear list that the user sees in the file tree.
pub fn flatten_visible_tree(file_tree: &HashMap<PathBuf, FileNode>, root_path: &Path) -> Vec<PathBuf> {
  let mut visible_paths = Vec::new();

  // start with the root dir's children instead of the root itself
  // creates a rootless tree view
  if let Some(root_node) = file_tree.get(root_path)
    && root_node.is_directory
    && root_node.is_expanded
  {
    // add each child of the root directory
    for child_path in &root_node.children {
      if let Some(child_node) = file_tree.get(child_path) {
        flatten_node_recursive(file_tree, child_node, &mut visible_paths);
      }
    }
  }

  visible_paths
}

/// Recursively flattens a single node and its children, using core algo for creating tree view.
fn flatten_node_recursive(file_tree: &HashMap<PathBuf, FileNode>, node: &FileNode, visible_paths: &mut Vec<PathBuf>) {
  // add node to the visible list
  visible_paths.push(node.path.clone());

  // if is an expanded dir, add its children
  if node.is_directory && node.is_expanded {
    for child_path in &node.children {
      if let Some(child_node) = file_tree.get(child_path) {
        flatten_node_recursive(file_tree, child_node, visible_paths);
      }
    }
  }
}

/// Gets all selected files from the tree, respecting user choice.
/// Returns a list of file paths that are currently selected for processing.
/// Only filters out files that would cause technical issues (binaries, circular references).
pub fn get_selected_files(file_tree: &HashMap<PathBuf, FileNode>) -> Vec<PathBuf> {
  file_tree
    .values()
    .filter(|node| node.is_selected && !node.is_directory)
    .filter(|node| is_text_file(&node.path))
    .map(|node| node.path.clone())
    .collect()
}

/// Determines if a file should be processed.
/// Only filters out files that would cause technical issues.
fn is_text_file(path: &Path) -> bool {
  let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

  // skip repomix output files to avoid circular references
  // TODO: remove this, if can handle circular references and tested it
  if file_name.starts_with("repomix-output")
    || file_name.ends_with("-repomix.txt")
    || file_name.ends_with("-repomix.md")
    || file_name.ends_with("-repomix.xml")
  {
    return false;
  }

  // skip files without extensions only if they're likely binaries
  match path.extension().and_then(|ext| ext.to_str()) {
    Some(_ext) => {
      // if has extension, include it
      true
    }
    None => {
      // if there is no extension, check if it's a known text file
      // TODO: move this to a config file for user to customize
      let allowed_no_ext = [
        "README",
        "LICENSE",
        "CHANGELOG",
        "CONTRIBUTING",
        "Dockerfile",
        "Makefile",
        "Gemfile",
        "Rakefile",
        "Procfile",
        "Vagrantfile",
        "Jenkinsfile",
        "BUILD",
        "WORKSPACE",
        "justfile",
        "gradlew",
        "mvnw",
      ];
      if allowed_no_ext.iter().any(|&name| file_name.eq_ignore_ascii_case(name)) {
        return true;
      }

      // for other extensionless files, do a basic binary check
      if let Ok(metadata) = std::fs::metadata(path) {
        // skip very large files that might be binaries (> 50 MB)
        if metadata.len() > 50 * 1024 * 1024 {
          return false;
        }
      }

      // try to read first few bytes to detect binary content
      if let Ok(mut file) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buffer = [0; 512];
        if let Ok(bytes_read) = file.read(&mut buffer) {
          // check for null bytes (for common binary files)
          if buffer[..bytes_read].contains(&0) {
            return false;
          }
        }
      }

      // assume text if can't detect otherwise
      true
    }
  }
}

/// Toggles selection of a file or directory.
/// For dirs, recursively selects/deselects all children.
pub fn toggle_selection_recursive(file_tree: &mut HashMap<PathBuf, FileNode>, path: &Path) -> Result<()> {
  if let Some(node) = file_tree.get(path) {
    let new_selection_state = !node.is_selected;
    set_selection_recursive(file_tree, path, new_selection_state)?;
  }

  Ok(())
}

/// Expands all dirs in the file tree recursively.
/// Makes all nested dirs visible in the file tree.
pub fn expand_all_directories(file_tree: &mut HashMap<PathBuf, FileNode>) {
  for node in file_tree.values_mut() {
    if node.is_directory {
      node.is_expanded = true;
    }
  }
}

/// Collapses all dirs in the file tree.
/// Hides all nested content and shows only the root level.
pub fn collapse_all_directories(file_tree: &mut HashMap<PathBuf, FileNode>) {
  for node in file_tree.values_mut() {
    if node.is_directory {
      node.is_expanded = false;
    }
  }
}

/// Selects all items (files and directories) that are currently visible in the file tree.
/// For directories, select all their contents.
pub fn select_all_visible_files(file_tree: &mut HashMap<PathBuf, FileNode>, visible_files: &[PathBuf]) -> Result<()> {
  // first, unselect everything
  unselect_all_items(file_tree);

  // select each visible item and all its contents
  for path in visible_files {
    if let Some(node) = file_tree.get(path)
      && !node.is_selected
    {
      set_selection_recursive(file_tree, path, true)?;
    }
  }

  Ok(())
}

/// Sets the selection state of a file or directory recursively.
/// For directories, apply the same selection state to all children.
fn set_selection_recursive(
  file_tree: &mut HashMap<PathBuf, FileNode>,
  path: &Path,
  selection_state: bool,
) -> Result<()> {
  let mut stack = vec![path.to_path_buf()];
  while let Some(current) = stack.pop() {
    if let Some(node) = file_tree.get_mut(&current) {
      node.is_selected = selection_state;
      if node.is_directory {
        stack.extend(node.children.iter().cloned());
      }
    }
  }

  Ok(())
}

/// Unselects all files and dirs in the file tree.
/// Clears all selections for a fresh start.
pub fn unselect_all_items(file_tree: &mut HashMap<PathBuf, FileNode>) {
  for node in file_tree.values_mut() {
    node.is_selected = false;
  }
}

/// Creates a file tree structure as text for repomix output.
/// Uses a format similar to the `tree` command.
pub fn generate_file_tree_text(file_tree: &HashMap<PathBuf, FileNode>, root_path: &Path) -> String {
  let mut result = String::new();

  // get the root directory name
  let root_name = root_path.file_name().and_then(|name| name.to_str()).unwrap_or(".");

  result.push_str(&format!("{}/\n", root_name));

  // get root node and generate tree from its children
  if let Some(root_node) = file_tree.get(root_path)
    && root_node.is_directory
  {
    // collect and sort children for consistent output
    let mut children: Vec<&PathBuf> = root_node.children.iter().collect();
    children.sort_by(|a, b| {
      let a_name = a.file_name().unwrap_or_default();
      let b_name = b.file_name().unwrap_or_default();

      // get node types for sorting (directories first)
      let a_is_dir = file_tree.get(*a).map(|n| n.is_directory).unwrap_or(false);
      let b_is_dir = file_tree.get(*b).map(|n| n.is_directory).unwrap_or(false);

      match (a_is_dir, b_is_dir) {
        // directories first
        (true, false) => std::cmp::Ordering::Less,
        // files second
        (false, true) => std::cmp::Ordering::Greater,
        // alphabetical within same type
        _ => a_name.cmp(b_name),
      }
    });

    // generate tree structure for each child
    for (index, child_path) in children.iter().enumerate() {
      let is_last = index == children.len() - 1;
      if let Some(child_node) = file_tree.get(*child_path) {
        generate_tree_node_recursive(file_tree, child_node, &mut result, "", is_last);
      }
    }
  }

  result
}

/// Recursively creates tree structure for a single node and its children.
/// Uses box-drawing characters for a clean tree appearance.
fn generate_tree_node_recursive(
  file_tree: &HashMap<PathBuf, FileNode>,
  node: &FileNode,
  result: &mut String,
  prefix: &str,
  is_last: bool,
) {
  // choose the appropriate tree characters
  let connector = if is_last { "└── " } else { "├── " };
  let extension = if is_last { "    " } else { "│   " };

  // add the current node
  let node_name = node.name.clone();
  let display_name = if node.is_directory { format!("{}/", node_name) } else { node_name };

  result.push_str(&format!("{}{}{}\n", prefix, connector, display_name));

  // if it's a directory, add its children
  if node.is_directory {
    // collect and sort children
    let mut children: Vec<&PathBuf> = node.children.iter().collect();
    children.sort_by(|a, b| {
      let a_name = a.file_name().unwrap_or_default();
      let b_name = b.file_name().unwrap_or_default();

      // get node types for sorting (directories first)
      let a_is_dir = file_tree.get(*a).map(|n| n.is_directory).unwrap_or(false);
      let b_is_dir = file_tree.get(*b).map(|n| n.is_directory).unwrap_or(false);

      match (a_is_dir, b_is_dir) {
        // directories first
        (true, false) => std::cmp::Ordering::Less,
        // files second
        (false, true) => std::cmp::Ordering::Greater,
        // alphabetical within same type
        _ => a_name.cmp(b_name),
      }
    });

    // create tree for each child
    for (index, child_path) in children.iter().enumerate() {
      let is_last_child = index == children.len() - 1;
      if let Some(child_node) = file_tree.get(*child_path) {
        let new_prefix = format!("{}{}", prefix, extension);
        generate_tree_node_recursive(file_tree, child_node, result, &new_prefix, is_last_child);
      }
    }
  }
}

// test for file tree scanning and selection
// TODO: move tests to main testing file
#[cfg(test)]
mod tests {
  use std::fs;

  use tempfile::TempDir;

  use super::*;

  #[test]
  fn test_scan_directory() {
    // create a temporary dir structure for testing
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // create some test files and dirs
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(root.join("README.md"), "# Test Project").unwrap();

    // scan the dir
    let file_tree = scan_directory(root).unwrap();

    // verify found the expected files and dirs
    assert!(file_tree.contains_key(root));
    assert!(file_tree.contains_key(&root.join("src")));
    assert!(file_tree.contains_key(&root.join("src/main.rs")));
    assert!(file_tree.contains_key(&root.join("README.md")));
  }
}
