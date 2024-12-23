use std::{env::current_dir, fmt::Display, fs::create_dir_all, path::PathBuf};

use directories::ProjectDirs;

const PROMPTS_DIR: &str = "prompts";
const PRESETS_DIR: &str = "presets";
const THREADS_DIR: &str = "threads";
const LOGS_DIR: &str = "logs";
const CONFIG_DIR: &str = "config";
const WEIGHTS_DIR: &str = "weights";
const SESSIONS_DIR: &str = "sessions";

pub const PROJECT_DIR_NAME: &str = ".llmvm";

pub enum DirType {
    Prompts,
    Presets,
    Threads,
    Logs,
    Config,
    Weights,
    Sessions,
}

impl Display for DirType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                DirType::Prompts => PROMPTS_DIR,
                DirType::Presets => PRESETS_DIR,
                DirType::Threads => THREADS_DIR,
                DirType::Logs => LOGS_DIR,
                DirType::Config => CONFIG_DIR,
                DirType::Weights => WEIGHTS_DIR,
                DirType::Sessions => SESSIONS_DIR,
            }
        )
    }
}

pub fn get_home_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "djandries", "llmvm")
}

pub fn get_project_dir() -> Option<PathBuf> {
    current_dir().ok().map(|p| p.join(PROJECT_DIR_NAME))
}

fn get_home_file_path(dir_type: DirType, filename: &str) -> Option<PathBuf> {
    get_home_dirs().map(|p| {
        let subdir = match dir_type {
            DirType::Config => p.config_dir().into(),
            _ => p.data_dir().join(dir_type.to_string()),
        };
        create_dir_all(&subdir).ok();
        subdir.join(filename)
    })
}

pub fn get_file_path(dir_type: DirType, filename: &str, will_create: bool) -> Option<PathBuf> {
    // Check for project file path, if it exists or if creating new file
    let project_dir = get_project_dir();
    if let Some(project_dir) = project_dir {
        if project_dir.exists() {
            let type_dir = project_dir.join(dir_type.to_string());
            create_dir_all(&type_dir).ok();
            let file_dir = type_dir.join(filename);
            if will_create || file_dir.exists() {
                return Some(file_dir);
            }
        }
    }
    // Return user home file path
    get_home_file_path(dir_type, filename)
}

pub fn generate_client_id(prefix: &str) -> String {
    let random_hex = format!("{:x}", rand::random::<u32>());
    format!("{}-{}", prefix, random_hex)
}

#[cfg(feature = "logging")]
pub mod logging {
    use std::str::FromStr;

    use std::fs::OpenOptions;
    use tracing_subscriber::{filter::Directive, EnvFilter};

    use super::{get_file_path, DirType};

    pub fn setup_subscriber(directive: Option<&str>, log_filename: Option<&str>) {
        let subscriber_builder = tracing_subscriber::fmt().with_env_filter(
            EnvFilter::builder()
                .with_default_directive(
                    directive
                        .map(|d| Directive::from_str(d).expect("logging directive should be valid"))
                        .unwrap_or_default(),
                )
                .from_env()
                .expect("should be able to read logging directive from env"),
        );

        match log_filename {
            None => subscriber_builder.with_writer(std::io::stderr).init(),
            Some(filename) => {
                subscriber_builder
                    .with_writer(
                        OpenOptions::new()
                            .create(true)
                            .truncate(true)
                            .write(true)
                            .open(
                                get_file_path(DirType::Logs, filename, true)
                                    .expect("should be able to find log path"),
                            )
                            .expect("should be able to open log file"),
                    )
                    .init();
            }
        };
    }
}

#[cfg(feature = "config")]
pub mod config {
    use config::{Config, ConfigError, Environment, File, FileFormat};
    use multilink::ConfigExampleSnippet;
    use serde::de::DeserializeOwned;
    use std::{fs, io::Write};

    use crate::{get_home_file_path, DirType};

    use super::get_file_path;

    fn maybe_save_example_config<T: ConfigExampleSnippet>(config_filename: &str) {
        let home_config_path = get_home_file_path(DirType::Config, config_filename)
            .expect("should be able to find home config path");
        if home_config_path.exists() {
            return;
        }
        fs::File::create(home_config_path)
            .and_then(|mut f| f.write_all(T::config_example_snippet().as_bytes()))
            .ok();
    }

    pub fn load_config<T: DeserializeOwned + ConfigExampleSnippet>(
        config_filename: &str,
    ) -> Result<T, ConfigError> {
        // TODO: add both root and project configs as sources
        let config_path = get_file_path(DirType::Config, config_filename, false)
            .expect("should be able to find config path");

        maybe_save_example_config::<T>(config_filename);

        Config::builder()
            .add_source(
                File::new(
                    config_path
                        .to_str()
                        .expect("config path should return to str"),
                    FileFormat::Toml,
                )
                .required(false),
            )
            .add_source(Environment::with_prefix("LLMVM"))
            .build()?
            .try_deserialize()
    }
}
