//! Clipboard utilities shared between backends.

use anyhow::{Context, Result};
use tokio::process::Command;

/// Gets the platform-specific clipboard command.
fn get_clipboard_command() -> Result<Vec<&'static str>> {
  if cfg!(target_os = "macos") {
    Ok(vec!["pbcopy"])
  } else if cfg!(target_os = "windows") {
    Ok(vec!["clip"])
  } else {
    Err(anyhow::anyhow!("Unsupported platform for clipboard operations"))
  }
}

/// Checks for linux clipboard utilities asynchronously.
async fn get_linux_clipboard_command() -> Result<Vec<&'static str>> {
  let xclip = Command::new("which").arg("xclip").output().await;
  if matches!(xclip, Ok(output) if output.status.success()) {
    Ok(vec!["xclip", "-selection", "clipboard"])
  } else {
    let xsel = Command::new("which").arg("xsel").output().await;
    if matches!(xsel, Ok(output) if output.status.success()) {
      Ok(vec!["xsel", "--clipboard", "--input"])
    } else {
      Err(anyhow::anyhow!(
        "No clipboard utility found. Please install xclip or xsel:\n\
         sudo apt-get install xclip  # or\n\
         sudo apt-get install xsel"
      ))
    }
  }
}

/// Copies content to the system clipboard.
pub async fn copy_to_clipboard(content: &str) -> Result<String> {
  let clipboard_cmd =
    if cfg!(target_os = "linux") { get_linux_clipboard_command().await? } else { get_clipboard_command()? };

  let mut cmd = Command::new(clipboard_cmd[0]);
  for arg in &clipboard_cmd[1..] {
    cmd.arg(arg);
  }

  let mut child = cmd
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .context("Failed to spawn clipboard command")?;

  if let Some(stdin) = child.stdin.take() {
    use tokio::io::AsyncWriteExt;
    let mut stdin = stdin;
    stdin.write_all(content.as_bytes()).await.context("Failed to write to clipboard")?;
    stdin.shutdown().await.context("Failed to close clipboard stdin")?;
  }

  let output = child.wait_with_output().await.context("Failed to wait for clipboard command")?;

  if output.status.success() {
    Ok("Content copied to clipboard".to_string())
  } else {
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow::anyhow!("Clipboard command failed: {}", stderr))
  }
}
