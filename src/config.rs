use crate::types::{Backend, OutputFormat};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Persistent configuration for siff user preferences.
/// Stores settings that should persist between sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SifConfig {
  /// whether to compress output
  pub compress: bool,
  /// whether to remove comments
  pub remove_comments: bool,
  /// whether to include file tree in output
  pub include_file_tree: bool,
  /// output format for repomix
  pub output_format: OutputFormat,
  /// last used backend
  pub default_backend: Backend,
}

impl Default for SifConfig {
  /// Provides sensible default values (first-time users).
  fn default() -> Self {
    Self {
      compress: false,
      remove_comments: false,
      include_file_tree: false,
      output_format: OutputFormat::Xml,
      default_backend: Backend::Repomix,
    }
  }
}

impl SifConfig {
  /// Loads configuration from the user's config file.
  /// Creates default config if file doesn't exist
  pub fn load() -> Result<Self> {
    let config_path = get_config_path()?;

    if config_path.exists() {
      // load existing config
      let config_content = fs::read_to_string(&config_path).with_context(|| format!("Error: failed to read config file: {}", config_path.display()))?;

      // handle corrupted config file
      let config: SifConfig = serde_json::from_str(&config_content).with_context(|| "Error: failed to parse config file")?;

      Ok(config)
    } else {
      // create default config and save it
      let default_config = SifConfig::default();
      default_config.save()?;
      Ok(default_config)
    }
  }

  /// Saves the current config to the user's config file.
  pub fn save(&self) -> Result<()> {
    let config_path = get_config_path()?;

    // make sure config dir exists
    if let Some(parent) = config_path.parent() {
      fs::create_dir_all(parent).with_context(|| format!("Error: failed to create config directory: {}", parent.display()))?;
    }

    // serialize and write the config
    let config_content = serde_json::to_string_pretty(self).context("Error: failed to serialize config")?;

    fs::write(&config_path, config_content).with_context(|| format!("Error: failed to write config file: {}", config_path.display()))?;

    Ok(())
  }

  /// Updates the config with new repomix options and saves.
  pub fn update_repomix_options(&mut self, compress: bool, remove_comments: bool, include_file_tree: bool, output_format: OutputFormat) -> Result<()> {
    self.compress = compress;
    self.remove_comments = remove_comments;
    self.include_file_tree = include_file_tree;
    self.output_format = output_format;
    self.save()
  }
}

/// Gets the path to the siff config file.
///
/// Returns the path to the config file, creating parent directories if they don't exist.
fn get_config_path() -> Result<PathBuf> {
  let config_dir = dirs::config_dir().context("Error: failed to get config directory")?;
  Ok(config_dir.join("siff").join("config.json"))
}

// test for serialization and default config
// TODO: move tests to main testing file
#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_config_serialization() {
    let config = SifConfig {
      compress: true,
      remove_comments: false,
      include_file_tree: true,
      output_format: OutputFormat::Markdown,
      default_backend: Backend::Yek,
    };

    // test serialization
    let json = serde_json::to_string(&config).unwrap();
    assert!(json.contains("compress"));
    assert!(json.contains("true"));

    // test deserialization
    let deserialized: SifConfig = serde_json::from_str(&json).unwrap();
    assert!(deserialized.compress);
    assert!(!deserialized.remove_comments);
    assert!(deserialized.include_file_tree);
    assert_eq!(deserialized.output_format, OutputFormat::Markdown);
    assert_eq!(deserialized.default_backend, Backend::Yek);
  }

  #[test]
  fn test_default_config() {
    let config = SifConfig::default();
    assert!(!config.compress);
    assert!(!config.remove_comments);
    assert!(!config.include_file_tree);
    assert_eq!(config.output_format, OutputFormat::Xml);
    assert_eq!(config.default_backend, Backend::Repomix);
  }
}
