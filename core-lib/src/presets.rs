use llmvm_protocol::GenerationParameters;
use llmvm_util::{get_file_path, DirType};
use rust_embed::RustEmbed;
use tokio::fs;

use crate::{error::CoreError, Result};

#[derive(RustEmbed)]
#[folder = "./presets"]
struct BuiltInPresets;

/// Loads a preset from the current project or user home directory.
pub async fn load_preset(preset_id: &str) -> Result<GenerationParameters> {
    let preset_file_name = format!("{}.toml", preset_id);
    let preset_path = get_file_path(DirType::Presets, &preset_file_name, false)
        .ok_or(CoreError::UserHomeNotFound)?;
    let preset_toml = match fs::try_exists(&preset_path).await.unwrap_or_default() {
        true => fs::read_to_string(preset_path).await?,
        false => std::str::from_utf8(
            &BuiltInPresets::get(&preset_file_name)
                .ok_or(CoreError::PresetNotFound)?
                .data,
        )?
        .to_string(),
    };
    Ok(toml::from_str(&preset_toml)?)
}
