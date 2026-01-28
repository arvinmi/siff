mod app;
mod clipboard;
mod config;
mod file_utils;
mod repomix_integration;
mod token_counter;
mod types;
mod ui;
mod yek_integration;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

/// Main siff entry point
#[derive(Parser)]
#[command(name = "siff")]
#[command(about = "File browser with repomix and yek as supported parsing backends")]
#[command(version = "0.1.1")]
#[command(long_about = None)]
struct Cli {
  /// Directory to scan for files (defaults to current dir)
  #[arg(value_name = "DIRECTORY")]
  directory: Option<PathBuf>,

  /// Enable verbose output for debugging
  #[arg(short, long)]
  verbose: bool,

  /// Use yek backend instead of repomix
  #[arg(long)]
  yek: bool,

  /// Use repomix backend (default)
  #[arg(long)]
  repomix: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
  // parse command line arguments
  let cli = Cli::parse();

  // determine the backend to use
  let backend = if cli.yek && cli.repomix {
    eprintln!("Error: Cannot specify both --yek and --repomix");
    std::process::exit(1);
  } else if cli.yek {
    types::Backend::Yek
  } else if cli.repomix {
    types::Backend::Repomix
  } else {
    // no specific backend requested, use saved default or fallback to repomix
    match config::SifConfig::load() {
      Ok(config) => config.default_backend,
      Err(_) => types::Backend::Repomix,
    }
  };

  // determine the directory to scan
  let target_directory =
    cli.directory.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

  // validate that the dir exists
  if !target_directory.exists() {
    anyhow::bail!("Directory does not exist: {}", target_directory.display());
  }

  if !target_directory.is_dir() {
    anyhow::bail!("Path is not a directory: {}", target_directory.display());
  }

  // print startup info if verbose
  if cli.verbose {
    println!("Starting Siff...");
    println!("Backend: {}", backend.display_name());
    println!("Target directory: {}", target_directory.display());
    println!("Scanning for files...");
  }

  // check if the chosen backend is available before starting the app
  if let Err(e) = check_backend_availability(&backend).await {
    eprintln!("Error: {}", e);
    match backend {
      types::Backend::Repomix => {
        eprintln!("\nSif requires Node.js and npm to run repomix.");
        eprintln!("Please install Node.js (which includes npm):");
        eprintln!("  macOS: brew install node");
        eprintln!("  Ubuntu/Debian: sudo apt-get install nodejs npm");
        eprintln!("  Windows: Download from https://nodejs.org/");
        eprintln!("\nAfter installing Node.js, Siff will automatically download and cache repomix.");
        eprintln!("This is a one-time setup and subsequent runs will be fast.");
      }
      types::Backend::Yek => {
        eprintln!("\nSiff includes yek integration but failed to initialize.");
        eprintln!("This is likely a build or installation issue.");
        eprintln!("Please try reinstalling Siff:");
        eprintln!("  cargo install --force siff");
      }
    }
    std::process::exit(1);
  }

  // run the app
  if let Err(e) = app::run_app(&target_directory, backend).await {
    eprintln!("Error: {}", e);

    // print the error chain for debugging
    let mut source = e.source();
    while let Some(err) = source {
      eprintln!("  Caused by: {}", err);
      source = err.source();
    }

    std::process::exit(1);
  }

  Ok(())
}

/// Checks if the chosen backend is available in the system PATH.
/// To check if can actually run the backend before starting the app.
async fn check_backend_availability(backend: &types::Backend) -> Result<()> {
  match backend {
    types::Backend::Repomix => {
      // check if npm is available for downloading repomix
      crate::repomix_integration::Repomix::check_build_dependencies().await
    }
    types::Backend::Yek => {
      // for yek, use the embedded binary so it's always available
      // just need to check if it can be initialized
      match crate::yek_integration::Yek::new() {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow::anyhow!("Yek backend failed: {}", e)),
      }
    }
  }
}

// test for cli parsing and directory validation
// TODO: move tests to main testing file
#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_cli_parsing() {
    // test default case
    let cli = Cli::parse_from(["siff"]);
    assert!(cli.directory.is_none());
    assert!(!cli.verbose);

    // test with directory
    let cli = Cli::parse_from(["siff", "/tmp"]);
    assert_eq!(cli.directory, Some(PathBuf::from("/tmp")));

    // test with verbose flag
    let cli = Cli::parse_from(["siff", "--verbose"]);
    assert!(cli.verbose);
  }
}
