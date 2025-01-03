use std::path::Path;

use anyhow::{anyhow, Result};

pub fn to_relative_path_string(path: &Path) -> Result<String> {
    std::env::current_dir()
        .map_err(|e| anyhow!("failed to get current dir: {}", e))
        .and_then(|cd| {
            path.strip_prefix(cd)
                .map(|rel| rel.to_string_lossy().into_owned())
                .map_err(|_| anyhow!("failed to get relative path"))
        })
}
