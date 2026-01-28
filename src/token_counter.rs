use std::{
  collections::HashMap,
  path::{Path, PathBuf},
  sync::{Arc, OnceLock},
};

use anyhow::{Context, Result};
use tiktoken_rs::{CoreBPE, o200k_base};
use tokio::sync::{Mutex, OnceCell, Semaphore};

// global shared encoder pool to avoid expensive recreation
static ENCODER_POOL: OnceCell<Arc<CoreBPE>> = OnceCell::const_new();

// global semaphore to limit concurrent tokenization tasks, preventing overload when processing many files
static TOKENIZATION_SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Gets or creates the shared encoder instance.
async fn get_shared_encoder() -> Result<Arc<CoreBPE>> {
  ENCODER_POOL
    .get_or_try_init(|| async {
      // create the encoder once and share it
      // use o200k_base encoding for newest model support
      // TODO: add to user config for more customization
      let encoder = o200k_base().map_err(|e| anyhow::anyhow!("Failed to create encoder: {}", e))?;
      Ok(Arc::new(encoder))
    })
    .await
    .cloned()
}

/// Gets or creates the shared semaphore for limiting concurrent tokenization.
fn get_tokenization_semaphore() -> &'static Arc<Semaphore> {
  TOKENIZATION_SEMAPHORE.get_or_init(|| {
    // limit concurrent tokenization tasks to 2x cpu cores
    let cpu_count = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4); // fallback to 4 if detection fails
    Arc::new(Semaphore::new(cpu_count * 2))
  })
}

/// Token counter for calculating token counts of selected files, using shared encoder pool.
#[derive(Debug)]
pub struct TokenCounter {
  /// Cached token counts for files to avoid recalculating, using mutex for sharing across tasks.
  file_token_cache: Arc<Mutex<HashMap<PathBuf, usize>>>,
}

impl TokenCounter {
  /// Creates a new token counter with shared cache.
  pub fn new() -> Result<Self> {
    Ok(Self { file_token_cache: Arc::new(Mutex::new(HashMap::new())) })
  }

  /// Creates a token counter that shares cache with another instance.
  /// Allows multiple TokenCounter instances to share the same cache.
  pub fn with_shared_cache(shared_cache: Arc<Mutex<HashMap<PathBuf, usize>>>) -> Self {
    Self { file_token_cache: shared_cache }
  }

  /// Calculates token count for a single file with concurrency limiting.
  /// Returns cached result if available, otherwise reads and tokenizes the file.
  pub async fn count_file_tokens(&self, file_path: &Path) -> Result<usize> {
    // check cache first (fastest path)
    {
      let cache = self.file_token_cache.lock().await;
      if let Some(&cached_count) = cache.get(file_path) {
        return Ok(cached_count);
      }
    }

    // get semaphore permit to limit concurrent tokenization tasks
    let semaphore = get_tokenization_semaphore();
    let _permit = semaphore.acquire().await.map_err(|_| anyhow::anyhow!("Semaphore closed"))?;

    // check cache again after acquiring permit (task might have computed it)
    {
      let cache = self.file_token_cache.lock().await;
      if let Some(&cached_count) = cache.get(file_path) {
        return Ok(cached_count);
      }
    }

    // read file content
    let content = match tokio::fs::read_to_string(file_path).await {
      Ok(content) => content,
      Err(_) => {
        // if can't read the file (binary or permission issues), cache and return 0
        let mut cache = self.file_token_cache.lock().await;
        cache.insert(file_path.to_path_buf(), 0);
        return Ok(0);
      }
    };

    // get the shared encoder
    let encoder = get_shared_encoder().await?;

    // move the cpu intensive tokenization to a background thread, using shared encoder
    let encoder_clone = encoder.clone();
    let token_count = tokio::task::spawn_blocking(move || {
      // use the shared encoder
      let tokens = encoder_clone.encode_with_special_tokens(&content);
      tokens.len()
    })
    .await
    .context("Tokenization task failed")?;

    // cache the result
    {
      let mut cache = self.file_token_cache.lock().await;
      cache.insert(file_path.to_path_buf(), token_count);
    }

    Ok(token_count)
  }
}

/// Format token count.
pub fn format_token_count(count: usize) -> String {
  if count < 1_000 {
    format!("{}", count)
  } else if count < 1_000_000 {
    format!("{:.1}K", count as f64 / 1_000.0)
  } else {
    format!("{:.1}M", count as f64 / 1_000_000.0)
  }
}

// test for format token count
// TODO: move tests to main testing file
#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_format_token_count() {
    assert_eq!(format_token_count(500), "500");
    assert_eq!(format_token_count(1500), "1.5K");
    assert_eq!(format_token_count(1500000), "1.5M");
  }
}
