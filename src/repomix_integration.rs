use std::{
  collections::HashMap,
  path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::types::RepomixOptions;

/// Download status for repomix.
#[derive(Debug, Clone)]
pub enum DownloadStatus {
  /// Repomix is ready to use
  Ready,
  /// Currently downloading repomix
  Downloading(String),
  /// Download failed
  Failed(String),
  /// Not started yet
  NotStarted,
}

/// Repomix manager that downloads, caches, and runs repomix in complete isolation.
/// Makes sure siff has full control over repomix behavior without config interference.
pub struct Repomix {
  /// Path to cached repomix installation
  cache_dir: PathBuf,
  /// Specific repomix version to use (pinned)
  version: String,
  /// Path to the cached repomix entry point
  repomix_entry: PathBuf,
  /// Current download status
  download_status: DownloadStatus,
}

impl Repomix {
  /// Creates a new repomix manager instance.
  /// Sets up cache directory structure for isolated repomix installation.
  pub fn new() -> Result<Self> {
    // pin repomix version
    let version = "1.11.1".to_string();

    // create cache dir: ~/.cache/siff/repomix/<version>/
    let cache_dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("siff").join("repomix").join(&version);

    // repomix entry point will be at node_modules/repomix/bin/repomix.cjs
    let repomix_entry = cache_dir.join("node_modules").join("repomix").join("bin").join("repomix.cjs");

    // check if repomix is already cached
    let download_status = if repomix_entry.exists() { DownloadStatus::Ready } else { DownloadStatus::NotStarted };

    Ok(Self { cache_dir, version, repomix_entry, download_status })
  }

  /// Gets the current download status.
  pub fn download_status(&self) -> &DownloadStatus {
    &self.download_status
  }

  /// Starts background download if needed.
  /// Returns true if download was started, false if already ready/downloading.
  pub async fn start_background_download(&mut self) -> bool {
    match self.download_status {
      DownloadStatus::NotStarted | DownloadStatus::Failed(_) => {
        self.download_status = DownloadStatus::Downloading("Initializing...".to_string());
        true
      }
      // already ready or downloading
      _ => false,
    }
  }

  /// Continues the background download process.
  /// Returns true if status changed, false if no change.
  pub async fn update_background_download(&mut self) -> bool {
    match &self.download_status {
      DownloadStatus::Downloading(_) => {
        // perform the actual download
        match self.download_and_cache_repomix().await {
          Ok(()) => {
            self.download_status = DownloadStatus::Ready;
            true
          }
          Err(e) => {
            self.download_status = DownloadStatus::Failed(e.to_string());
            true
          }
        }
      }
      _ => false,
    }
  }

  /// Make sure repomix is available in cache, download if needed.
  /// Called before every repomix execution.
  pub async fn ensure_repomix(&mut self) -> Result<PathBuf> {
    match &self.download_status {
      DownloadStatus::Ready => {
        if self.repomix_entry.exists() {
          Ok(self.repomix_entry.clone())
        } else {
          // cache was deleted, restart download
          self.download_status = DownloadStatus::NotStarted;
          Err(anyhow::anyhow!("Repomix cache was deleted, restarting download..."))
        }
      }
      DownloadStatus::Downloading(msg) => Err(anyhow::anyhow!("Repomix is still downloading: {}", msg)),
      DownloadStatus::Failed(err) => Err(anyhow::anyhow!("Repomix download failed: {}", err)),
      DownloadStatus::NotStarted => Err(anyhow::anyhow!("Repomix download not started yet")),
    }
  }

  /// Downloads repomix npm package to cache directory.
  /// Runs once per version and creates an isolated repomix installation.
  async fn download_and_cache_repomix(&mut self) -> Result<()> {
    // update status
    self.download_status = DownloadStatus::Downloading("Creating cache directory...".to_string());

    // create cache directory
    std::fs::create_dir_all(&self.cache_dir).context("Failed to create repomix cache directory")?;

    // create package.json for repomix installation
    self.download_status = DownloadStatus::Downloading("Creating package.json...".to_string());

    let package_json = format!(
      r#"{{
            "name": "siff-repomix-cache",
            "version": "1.0.0",
            "dependencies": {{
                "repomix": "{}"
            }}
        }}"#,
      self.version
    );

    std::fs::write(self.cache_dir.join("package.json"), package_json)?;

    // install repomix to cache directory
    self.download_status = DownloadStatus::Downloading(format!("Installing repomix {}...", self.version));

    let npm_install = Command::new("npm")
      .args(["install", "--no-audit", "--no-fund", "--silent"])
      .current_dir(&self.cache_dir)
      .output()
      .await
      .context("Failed to run npm install")?;

    if !npm_install.status.success() {
      let stderr = String::from_utf8_lossy(&npm_install.stderr);
      let stdout = String::from_utf8_lossy(&npm_install.stdout);

      // cleanup cache directory on failure
      let _ = std::fs::remove_dir_all(&self.cache_dir);

      return Err(anyhow::anyhow!("npm install failed:\nstdout: {}\nstderr: {}", stdout, stderr));
    }

    // verify repomix was installed correctly
    self.download_status = DownloadStatus::Downloading("Verifying installation...".to_string());

    if !self.repomix_entry.exists() {
      // try alternative entry points for different repomix versions (debug only)
      let alternative_entries = vec![
        self.cache_dir.join("node_modules").join("repomix").join("bin").join("repomix.js"),
        self.cache_dir.join("node_modules").join("repomix").join("bin").join("repomix.cjs"),
        self.cache_dir.join("node_modules").join("repomix").join("dist").join("cli.js"),
        self.cache_dir.join("node_modules").join("repomix").join("lib").join("cli.js"),
        self.cache_dir.join("node_modules").join("repomix").join("index.js"),
      ];

      let mut found_entry = None;
      for entry in &alternative_entries {
        if entry.exists() {
          found_entry = Some(entry.clone());
          // update our main entry point to the found one
          self.repomix_entry = entry.clone();
          break;
        }
      }

      if found_entry.is_none() {
        // cleanup cache directory on failure
        let _ = std::fs::remove_dir_all(&self.cache_dir);

        return Err(anyhow::anyhow!(
          "Repomix installation failed, no valid entry point found. Searched: {}",
          alternative_entries.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        ));
      }
    }

    Ok(())
  }

  /// Runs repomix with complete isolation and siff only configuration.
  /// Main entry point that replaces the old repomix runner.
  pub async fn run_isolated_repomix(
    &mut self,
    selected_files: &[PathBuf],
    options: &RepomixOptions,
    working_directory: &Path,
    file_tree: &std::collections::HashMap<PathBuf, crate::types::FileNode>,
  ) -> Result<String> {
    if selected_files.is_empty() {
      return Err(anyhow::anyhow!("No files selected for processing"));
    }

    // check if repomix is available
    let repomix_path = self.ensure_repomix().await?;

    // build isolated command arguments
    let args = self.build_isolated_args(selected_files, options, working_directory)?;

    // create isolated environment
    let env = self.create_isolated_environment()?;

    // execute repomix using node with isolated environment
    let output = Command::new("node")
      .arg(&repomix_path)
      .args(&args)
      .env_clear() // clear all env vars
      .envs(&env) // only siff controlled env vars
      .current_dir(working_directory)
      .output()
      .await
      .context("Failed to execute isolated repomix")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      let stdout = String::from_utf8_lossy(&output.stdout);

      let error_msg = if !stderr.is_empty() && !stdout.is_empty() {
        format!("stderr: {} | stdout: {}", stderr, stdout)
      } else if !stderr.is_empty() {
        stderr.to_string()
      } else if !stdout.is_empty() {
        stdout.to_string()
      } else {
        "Command failed with no error output".to_string()
      };

      return Err(anyhow::anyhow!(
        "Repomix failed with exit code {}: {}",
        output.status.code().unwrap_or(-1),
        error_msg
      ));
    }

    // read the output file
    let temp_file = working_directory.join(format!("siff-repomix-{}.md", std::process::id()));
    if !temp_file.exists() {
      return Err(anyhow::anyhow!("Repomix did not create the expected output file"));
    }

    let mut content = std::fs::read_to_string(&temp_file).context("Failed to read repomix output file")?;

    // if file tree is enabled, prepend it to the content
    if options.file_tree {
      let file_tree_text = crate::file_utils::generate_file_tree_text(file_tree, working_directory);

      // format the file tree section based on output format
      let formatted_tree = match options.output_format {
        crate::types::OutputFormat::Xml => {
          format!("<directory_structure>\n{}</directory_structure>\n\n", file_tree_text)
        }
        crate::types::OutputFormat::Markdown => {
          format!("## Directory Structure\n\n```\n{}\n```\n\n", file_tree_text)
        }
        crate::types::OutputFormat::PlainText => {
          format!("Directory Structure:\n{}\n", file_tree_text)
        }
      };

      // prepend the file tree to the existing content
      content = format!("{}{}", formatted_tree, content);
    }

    // copy to clipboard
    self.copy_to_clipboard(&content).await?;

    // cleanup temp file
    let _ = std::fs::remove_file(&temp_file);

    Ok(format!("{} files processed and copied to clipboard", selected_files.len()))
  }

  /// Builds command arguments with complete siff control and no config interference.
  fn build_isolated_args(
    &self,
    selected_files: &[PathBuf],
    options: &RepomixOptions,
    working_directory: &Path,
  ) -> Result<Vec<String>> {
    let mut args = vec![
      "--no-gitignore".to_string(),
      "--no-default-patterns".to_string(),
      "--no-directory-structure".to_string(),
      // Note: repomix runs security-check by default. Since siff is opinionated, keep these check enabled by default.
      // TODO: could add this as option in repomix config
      "--output".to_string(),
      working_directory.join(format!("siff-repomix-{}.md", std::process::id())).to_string_lossy().to_string(),
    ];

    // add siff controlled options only
    if options.compress {
      args.push("--compress".to_string());
    }

    if options.remove_comments {
      args.push("--remove-comments".to_string());
    }

    // add output format if not plain text (default)
    if let Some(format_flag) = options.output_format.repomix_flag() {
      args.push(format_flag.to_string());
    }

    // for extremely large file counts, use directory-based patterns to avoid command line limits
    if selected_files.len() > 1000 {
      let patterns = self.build_directory_patterns(selected_files, working_directory)?;

      if !patterns.is_empty() {
        args.push("--include".to_string());
        args.push(patterns.join(","));
      } else {
        return Err(anyhow::anyhow!("Error: No valid directory patterns could be created"));
      }
    } else {
      // for smaller file counts, use the direct include approach
      let mut valid_files = Vec::new();

      // convert selected files to relative paths with proper validation
      for file_path in selected_files {
        if let Some(path_str) = crate::file_utils::validate_file_path(file_path, working_directory) {
          // extra check: skip files with commas (comma is separator in --include)
          if path_str.contains(',') {
            eprintln!("Warning: Skipping file with comma in filename (security): {}", path_str);
            continue;
          }
          valid_files.push(path_str);
        }
      }

      // make sure have files to process after validation
      if valid_files.is_empty() {
        return Err(anyhow::anyhow!("Error: No valid files to process after security validation"));
      }

      // use comma separated approach for smaller file counts
      args.push("--include".to_string());
      args.push(valid_files.join(","));
    }

    // add target directory
    args.push(".".to_string());

    Ok(args)
  }

  /// Builds directory-based patterns for large file counts
  /// Groups files by directory and creates glob patterns.
  fn build_directory_patterns(&self, selected_files: &[PathBuf], working_directory: &Path) -> Result<Vec<String>> {
    use std::collections::HashMap;

    // group files by parent directory
    let mut dir_files: HashMap<PathBuf, Vec<String>> = HashMap::new();

    for file_path in selected_files {
      let relative_path = match file_path.strip_prefix(working_directory) {
        Ok(rel_path) => rel_path,
        Err(_) => {
          eprintln!("Warning: Skipping file outside working directory: {}", file_path.display());
          continue;
        }
      };

      let path_str = relative_path.to_string_lossy().to_string();

      // skip invalid paths
      if path_str.contains("..") || path_str.is_empty() || path_str.starts_with('-') {
        eprintln!("Warning: Skipping invalid file path: {}", path_str);
        continue;
      }

      // get the parent directory (or root if no parent)
      let parent_dir = relative_path.parent().unwrap_or_else(|| Path::new(""));
      let filename = relative_path.file_name().and_then(|name| name.to_str()).unwrap_or("").to_string();

      if !filename.is_empty() {
        dir_files.entry(parent_dir.to_path_buf()).or_default().push(filename);
      }
    }

    let mut patterns = Vec::new();

    for (dir_path, files) in dir_files {
      let dir_str = if dir_path == Path::new("") { String::new() } else { format!("{}/", dir_path.to_string_lossy()) };

      if files.len() > 10 {
        // if many files in the same directory, use a wildcard pattern
        patterns.push(format!("{}**", dir_str));
      } else {
        // for few files, list them individually
        for file in files {
          patterns.push(format!("{}{}", dir_str, file));
        }
      }
    }

    Ok(patterns)
  }

  /// Creates an isolated environment for repomix execution.
  fn create_isolated_environment(&self) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();

    // essential env variables only
    env.insert("NODE_ENV".to_string(), "production".to_string());
    env.insert("NO_UPDATE_NOTIFIER".to_string(), "1".to_string());
    env.insert("NO_COLOR".to_string(), "1".to_string());

    // create a fake home dir to avoid global config
    let temp_home = std::env::temp_dir().join("siff-fake-home");
    std::fs::create_dir_all(&temp_home).ok(); // ignore errors
    env.insert("HOME".to_string(), temp_home.to_string_lossy().to_string());
    env.insert("USERPROFILE".to_string(), temp_home.to_string_lossy().to_string());

    // prevent config loading by setting fake config paths
    env.insert("XDG_CONFIG_HOME".to_string(), temp_home.to_string_lossy().to_string());
    env.insert("APPDATA".to_string(), temp_home.to_string_lossy().to_string());

    // minimal path for security (but include node)
    let node_path = self.get_node_path();
    env.insert("PATH".to_string(), node_path);

    Ok(env)
  }

  /// Gets the path that includes node but is otherwise minimal.
  fn get_node_path(&self) -> String {
    // try to find where node is installed
    let common_node_paths = if cfg!(windows) {
      vec!["C:\\Program Files\\nodejs", "C:\\Program Files (x86)\\nodejs", "C:\\Windows\\System32"]
    } else {
      vec![
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        // for apple silicon macs
        "/opt/homebrew/bin",
      ]
    };

    common_node_paths.join(if cfg!(windows) { ";" } else { ":" })
  }

  /// Copies content to clipboard using shared clipboard utility.
  async fn copy_to_clipboard(&self, content: &str) -> Result<()> {
    crate::clipboard::copy_to_clipboard(content).await?;
    Ok(())
  }

  /// Checks if node and npm are available for downloading and running repomix.
  pub async fn check_build_dependencies() -> Result<()> {
    // check node
    let node_check = Command::new("node").arg("--version").output().await;

    if node_check.is_err() {
      return Err(anyhow::anyhow!("Node.js not found. Please install Node.js to use repomix integration."));
    }

    // check npm
    let npm_check = Command::new("npm").arg("--version").output().await;

    if npm_check.is_err() {
      return Err(anyhow::anyhow!("Npm not found. Please install Node.js and npm to use repomix integration."));
    }

    Ok(())
  }
}

/// Validates repomix options for the isolated execution.
pub fn validate_isolated_repomix_options(_options: &RepomixOptions, selected_files: &[PathBuf]) -> Vec<String> {
  let mut warnings = Vec::new();

  if selected_files.is_empty() {
    warnings.push("No files selected for processing".to_string());
  }

  // warn about large number of files
  if selected_files.len() > 100 {
    warnings.push(format!("Large number of files selected ({}). May take a moment to process.", selected_files.len()));
  }

  warnings
}
