use std::{fmt::Display, fs::create_dir_all, path::PathBuf};

use directories::ProjectDirs;

const PROMPTS_DIR: &str = "prompts";
const PRESETS_DIR: &str = "presets";
const THREADS_DIR: &str = "threads";
const LOGS_DIR: &str = "logs";

pub enum DirType {
    Prompts,
    Presets,
    Threads,
    Logs,
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
            }
        )
    }
}

pub fn get_project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "djandries", "llmvm")
}

pub fn get_data_path(dir_type: DirType) -> Option<PathBuf> {
    get_project_dirs().map(|p| {
        let path = p.data_dir().join(dir_type.to_string());
        create_dir_all(&path).ok();
        path
    })
}

pub fn get_config_path() -> Option<PathBuf> {
    get_project_dirs().map(|p| {
        let path = p.config_dir().into();
        create_dir_all(&path).ok();
        path
    })
}

#[cfg(feature = "logging")]
pub mod logging {
    use std::str::FromStr;

    use std::fs::OpenOptions;
    use tracing_subscriber::{filter::Directive, EnvFilter};

    use super::{get_data_path, DirType};

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
                                get_data_path(DirType::Logs)
                                    .expect("should be able to find log path")
                                    .join(filename),
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
    use serde::de::DeserializeOwned;

    use super::get_config_path;

    pub fn load_config<T: DeserializeOwned>(config_filename: &str) -> Result<T, ConfigError> {
        let config_path = get_config_path()
            .expect("should be able to find config path")
            .join(config_filename);
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
