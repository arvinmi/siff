use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::process::Command;

/// Yek integration module with runtime download.
pub struct Yek {
  /// Path to the yek binary (downloaded on first use).
  yek_binary_path: PathBuf,
}

impl Yek {
  /// Creates a new yek instance.
  /// Downloads yek binary on first use if not already available.
  pub fn new() -> Result<Self> {
    // determine where to store the yek binary
    let yek_binary_path = Self::get_yek_binary_path()?;

    Ok(Self { yek_binary_path })
  }

  /// Gets the path where yek binary should be stored and downloads it if needed.
  fn get_yek_binary_path() -> Result<PathBuf> {
    // create a runtime directory for the binary
    let runtime_dir = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".")).join("siff").join("bin");

    std::fs::create_dir_all(&runtime_dir).context("Failed to create runtime directory")?;

    let binary_name = if cfg!(windows) { "yek.exe" } else { "yek" };
    let runtime_binary_path = runtime_dir.join(binary_name);

    // if binary doesn't exist, download it
    if !runtime_binary_path.exists() {
      Self::download_yek_binary(&runtime_binary_path)?;
    }

    Ok(runtime_binary_path)
  }

  /// Downloads yek binary from crates.io using cargo install.
  fn download_yek_binary(target_path: &Path) -> Result<()> {
    use std::process::Command as StdCommand;

    // create a temporary directory for cargo install
    let temp_dir = tempfile::tempdir().context("Failed to create temporary directory")?;

    let temp_bin_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&temp_bin_dir).context("Failed to create temporary bin directory")?;

    // install yek to temporary directory
    const YEK_VERSION: &str = "0.22.1";
    let output = StdCommand::new("cargo")
      .args(["install", "yek", "--version", YEK_VERSION, "--locked", "--root", temp_dir.path().to_str().unwrap(), "--quiet"])
      .output()
      .context("Failed to execute cargo install yek")?;

    if !output.status.success() {
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(anyhow::anyhow!("Failed to install yek: {}", stderr));
    }

    // find the installed yek binary
    let binary_name = if cfg!(windows) { "yek.exe" } else { "yek" };
    let temp_yek_path = temp_bin_dir.join(binary_name);

    if !temp_yek_path.exists() {
      return Err(anyhow::anyhow!("Yek binary not found after installation"));
    }

    // copy to final location
    std::fs::copy(&temp_yek_path, target_path).context("Failed to copy yek binary to final location")?;

    // make executable on unix systems
    #[cfg(unix)]
    {
      use std::os::unix::fs::PermissionsExt;
      let mut perms = std::fs::metadata(target_path)?.permissions();
      perms.set_mode(0o755);
      std::fs::set_permissions(target_path, perms)?;
    }

    Ok(())
  }

  /// Processes selected files using yek binary.
  /// Returns the serialized content as a string.
  pub async fn process_files(&self, selected_files: &[PathBuf], root_path: &Path) -> Result<String> {
    if selected_files.is_empty() {
      return Err(anyhow::anyhow!("Error: No files selected for processing"));
    }

    // check for extremely large file counts
    if selected_files.len() > 10000 {
      return Err(anyhow::anyhow!(
        "Error: Too many files selected ({}). Yek may fail with large file counts. Please select fewer files.",
        selected_files.len()
      ));
    }

    // build yek command arguments
    let mut yek_args = Vec::new();

    // add selected files as arguments
    for file_path in selected_files {
      if let Some(path_str) = crate::file_utils::validate_file_path(file_path, root_path) {
        yek_args.push(path_str);
      }
    }

    // make sure have files to process after validation
    if yek_args.is_empty() {
      return Err(anyhow::anyhow!("Error: No valid files to process after security validation"));
    }

    // warn about large file counts
    if yek_args.len() > 1000 {
      eprintln!("Warning: Large file count ({}), processing may take some time", yek_args.len());
    }

    // execute yek with the selected files
    let output = Command::new(&self.yek_binary_path)
      .args(&yek_args)
      .current_dir(root_path)
      .output()
      .await
      .context("Failed to execute yek binary")?;

    if output.status.success() {
      let content = String::from_utf8_lossy(&output.stdout);
      Ok(content.to_string())
    } else {
      let stderr = String::from_utf8_lossy(&output.stderr);
      Err(anyhow::anyhow!("Error: Yek failed with exit code {}: {}", output.status.code().unwrap_or(-1), stderr))
    }
  }

  /// Copies content to clipboard using shared clipboard utility.
  pub async fn copy_to_clipboard(&self, content: &str) -> Result<String> {
    crate::clipboard::copy_to_clipboard(content).await
  }

  /// Processes files and copies to clipboard in one operation.
  /// Main entry point that replaces run_yek function.
  pub async fn run_yek_integrated(&self, selected_files: &[PathBuf], root_path: &Path) -> Result<String> {
    // process files using yek library
    let content = self.process_files(selected_files, root_path).await?;

    // copy to clipboard
    self.copy_to_clipboard(&content).await?;

    Ok(format!("{} files processed and copied to clipboard", selected_files.len()))
  }
}

/// Validates yek options and selected files.
/// Returns a list of warnings if any issues are found.
pub fn validate_yek_options(selected_files: &[PathBuf]) -> Vec<String> {
  let mut warnings = Vec::new();

  if selected_files.is_empty() {
    warnings.push("No files selected".to_string());
  }

  // yek is more permissive than repomix, fewer validations needed
  if selected_files.len() > 1000 {
    warnings.push("Large number of files selected, May take a moment to process".to_string());
  }

  warnings
}
