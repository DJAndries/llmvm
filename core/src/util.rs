use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{CoreError, Result};
use llmvm_util::DirType;

pub fn current_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn get_data_path(dir_type: DirType) -> Result<PathBuf> {
    llmvm_util::get_data_path(dir_type).ok_or(CoreError::UserHomeNotFound)
}
