use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use directories::ProjectDirs;

use crate::{CoreError, Result};

const PROMPTS_DIR: &str = "prompts";
const PRESETS_DIR: &str = "presets";
const THREADS_DIR: &str = "threads";

pub fn get_project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "djandries", "llmvm").ok_or(CoreError::UserHomeNotFound)
}

pub fn get_prompts_path() -> Result<PathBuf> {
    get_project_dirs().map(|p| p.data_dir().join(PROMPTS_DIR))
}

pub fn get_threads_path() -> Result<PathBuf> {
    get_project_dirs().map(|p| p.data_dir().join(THREADS_DIR))
}

pub fn get_presets_path() -> Result<PathBuf> {
    get_project_dirs().map(|p| p.data_dir().join(PRESETS_DIR))
}

pub fn current_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
