mod util;

use handlebars::{
    no_escape, Context, Handlebars, Helper, HelperDef, HelperResult, Output, RenderContext,
    RenderError, Renderable, StringOutput,
};
use llmvm_protocol::stdio::{BackendRequest, BackendResponse, StdioClient, StdioError};
use llmvm_protocol::tower::timeout::Timeout;
use llmvm_protocol::tower::Service;
use llmvm_protocol::{
    Backend, BackendGenerationRequest, BackendGenerationResponse, Core, GenerationParameters,
    GenerationRequest, GenerationResponse, Message, MessageRole, ModelDescription, ProtocolError,
    ProtocolErrorType,
};
use llmvm_util::{get_file_path, DirType};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::env::{self, current_dir};
use std::fs::create_dir;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    io,
    ops::DerefMut,
    path::{Path, PathBuf},
    rc::Rc,
    str::{FromStr, Utf8Error},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::fs;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info};
use util::current_timestamp_secs;

const BACKEND_COMMAND_PREFIX: &str = "llmvm-";
const BACKEND_COMMAND_SUFFIX: &str = "-cli";
const PROJECT_DIR_NAME: &str = ".llmvm";

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("failed to start backend: {0}")]
    BackendStart(std::io::Error),
    #[error("template not found")]
    TemplateNotFound,
    #[error("utf8 error: {0}")]
    Utf8Error(#[from] Utf8Error),
    #[error("unable to load app data, could not find user home folder")]
    UserHomeNotFound,
    #[error("json serialization error: {0}")]
    JsonSerialization(#[from] serde_json::Error),
    #[error("toml serialization error: {0}")]
    TomlSerialization(#[from] toml::de::Error),
    #[error("thread not found")]
    ThreadNotFound,
    #[error("preset not found")]
    PresetNotFound,
    #[error("failed to parse model name")]
    ModelDescriptionParse,
    #[error("backend error (via stdio): {0}")]
    BackendStdio(#[from] StdioError),
    #[error("backend error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("tempate render error: {0}")]
    TemplateRender(#[from] RenderError),
    #[error("missing generation parameters")]
    MissingParameters,
    #[error("missing required generation parameter: {0}")]
    MissingParameter(&'static str),
}

impl Into<ProtocolError> for CoreError {
    fn into(self) -> ProtocolError {
        let error_type = match &self {
            CoreError::IO(_) => ProtocolErrorType::Internal,
            CoreError::BackendStart(_) => ProtocolErrorType::Internal,
            CoreError::TemplateNotFound => ProtocolErrorType::BadRequest,
            CoreError::Utf8Error(_) => ProtocolErrorType::BadRequest,
            CoreError::UserHomeNotFound => ProtocolErrorType::Internal,
            CoreError::JsonSerialization(_) => ProtocolErrorType::BadRequest,
            CoreError::TomlSerialization(_) => ProtocolErrorType::BadRequest,
            CoreError::ThreadNotFound => ProtocolErrorType::BadRequest,
            CoreError::PresetNotFound => ProtocolErrorType::BadRequest,
            CoreError::ModelDescriptionParse => ProtocolErrorType::BadRequest,
            CoreError::BackendStdio(error) => error.error_type.clone(),
            CoreError::Protocol(error) => error.error_type.clone(),
            CoreError::TemplateRender(_) => ProtocolErrorType::BadRequest,
            CoreError::MissingParameters => ProtocolErrorType::BadRequest,
            CoreError::MissingParameter(_) => ProtocolErrorType::BadRequest,
        };
        ProtocolError {
            error_type,
            error: Box::new(self),
        }
    }
}

struct SystemRoleHelper(Arc<std::sync::Mutex<SystemRoleHelperState>>);

#[derive(Default)]
struct SystemRoleHelperState {
    used: bool,
    out: RefCell<StringOutput>,
}

impl HelperDef for SystemRoleHelper {
    fn call<'reg: 'rc, 'rc>(
        &self,
        h: &Helper<'reg, 'rc>,
        r: &'reg Handlebars<'reg>,
        ctx: &'rc Context,
        rc: &mut RenderContext<'reg, 'rc>,
        _out: &mut dyn Output,
    ) -> HelperResult {
        h.template()
            .map(|t| {
                let mut state = self.0.lock().unwrap();
                state.used = true;
                t.render(r, ctx, rc, &mut *state.out.get_mut())
            })
            .unwrap_or(Ok(()))
    }
}

impl SystemRoleHelperState {
    fn system_prompt(&self) -> Option<String> {
        match self.used {
            false => None,
            true => self.out.take().into_string().ok(),
        }
    }
}

#[derive(RustEmbed)]
#[folder = "./prompts"]
struct BuiltInPrompts;

#[derive(RustEmbed)]
#[folder = "./presets"]
struct BuiltInPresets;

#[derive(Debug)]
pub struct ReadyPrompt {
    system_prompt: Option<String>,
    main_prompt: String,
}

impl ReadyPrompt {
    async fn load_template(template_id: &str) -> Result<String> {
        let template_file_name = format!("{}.hbs", template_id);
        let template_path = get_file_path(DirType::Prompts, &template_file_name, false)
            .ok_or(CoreError::UserHomeNotFound)?;
        if fs::try_exists(&template_path).await.unwrap_or_default() {
            return Ok(fs::read_to_string(template_path).await?);
        };
        let embedded_file =
            BuiltInPrompts::get(&template_file_name).ok_or(CoreError::TemplateNotFound)?;
        Ok(std::str::from_utf8(embedded_file.data.as_ref())?.to_string())
    }

    fn process(template: &str, parameters: &Value, is_chat_model: bool) -> Result<Self> {
        let mut handlebars = Handlebars::new();
        handlebars.register_escape_fn(no_escape);
        let system_role_helper_state =
            Arc::new(std::sync::Mutex::new(SystemRoleHelperState::default()));
        handlebars.register_helper(
            "system_role",
            Box::new(SystemRoleHelper(system_role_helper_state.clone())),
        );

        debug!("prompt parameters = {:?}", parameters);

        let mut main_prompt = handlebars
            .render_template(template, parameters)?
            .trim()
            .to_string();

        let mut system_prompt = system_role_helper_state
            .lock()
            .unwrap()
            .system_prompt()
            .map(|s| s.trim().to_string());

        if system_prompt.is_some() && !is_chat_model {
            main_prompt = format!("{}\n\n{}", system_prompt.take().unwrap(), main_prompt);
        }

        // TODO: Append system prompt to beginning of non chat prompt
        Ok(Self {
            system_prompt,
            main_prompt,
        })
    }

    pub async fn from_stored_template(
        template_id: &str,
        parameters: &Value,
        is_chat_model: bool,
    ) -> Result<Self> {
        let template = Self::load_template(template_id).await?;
        Self::process(&template, parameters, is_chat_model)
    }

    pub async fn from_custom_template(
        template: &str,
        parameters: &Value,
        is_chat_model: bool,
    ) -> Result<Self> {
        Self::process(template, parameters, is_chat_model)
    }
}

pub async fn load_preset(preset_id: &str) -> Result<GenerationParameters> {
    let preset_file_name = format!("{}.toml", preset_id);
    let preset_path = get_file_path(DirType::Presets, &preset_file_name, false)
        .ok_or(CoreError::UserHomeNotFound)?;
    if !fs::try_exists(&preset_path).await.unwrap_or_default() {
        return Err(CoreError::PresetNotFound);
    }
    let preset_toml = fs::read_to_string(preset_path).await?;
    Ok(toml::from_str(&preset_toml)?)
}

const META_JSON_FILENAME: &str = "meta.json";

#[derive(Default, Serialize, Deserialize)]
struct GenerationThreadMeta {
    last_id: u64,
    thread_ids_to_timestamps: HashMap<u64, u64>,
}

pub struct GenerationThreadManager {
    meta: Option<GenerationThreadMeta>,
}

impl GenerationThreadManager {
    // Rewrite this func to
    pub async fn new() -> Result<Self> {
        let mut new_self = Self { meta: None };
        let meta_path = Self::meta_path()?;
        if !fs::try_exists(&meta_path).await.unwrap_or_default() {
            return Ok(new_self);
        }
        new_self.meta = Some(serde_json::from_slice(&fs::read(&meta_path).await?)?);
        Ok(new_self)
    }

    fn meta_path() -> Result<PathBuf> {
        get_file_path(DirType::Threads, META_JSON_FILENAME, true).ok_or(CoreError::UserHomeNotFound)
    }

    fn thread_path(id: u64) -> Result<PathBuf> {
        get_file_path(DirType::Threads, &format!("thread_{}.json", id), true)
            .ok_or(CoreError::UserHomeNotFound)
    }

    fn check_thread_exists(&self, thread_id: &u64) -> Result<()> {
        if !self
            .meta
            .as_ref()
            .map(|m| m.thread_ids_to_timestamps.contains_key(thread_id))
            .unwrap_or_default()
        {
            return Err(CoreError::ThreadNotFound);
        }
        Ok(())
    }

    pub async fn clean_old_threads(&mut self, ttl_secs: u64) -> Result<()> {
        if let Some(meta) = self.meta.as_ref() {
            let mut purged_thread_ids = Vec::with_capacity(meta.thread_ids_to_timestamps.len());
            let current_timestamp_secs = current_timestamp_secs();
            for (id, timestamp_secs) in &meta.thread_ids_to_timestamps {
                if current_timestamp_secs > *timestamp_secs
                    && (current_timestamp_secs - timestamp_secs) <= ttl_secs
                {
                    continue;
                }
                if let Err(e) = fs::remove_file(&Self::thread_path(*id)?).await {
                    match e.kind() {
                        io::ErrorKind::NotFound => (),
                        _ => return Err(e.into()),
                    };
                }
                purged_thread_ids.push(*id);
            }
            let meta = self.meta.as_mut().unwrap();
            for id in purged_thread_ids {
                meta.thread_ids_to_timestamps.remove(&id);
            }
            let meta_json = serde_json::to_vec(meta)?;
            fs::write(Self::meta_path()?, meta_json).await?;
        }
        Ok(())
    }

    pub async fn new_thread_id(&mut self) -> Result<u64> {
        let meta = self.meta.get_or_insert_with(|| Default::default());

        let new_id = meta.last_id + 1;
        meta.last_id = new_id;
        meta.thread_ids_to_timestamps
            .insert(new_id, current_timestamp_secs());

        let meta_json = serde_json::to_vec(meta)?;
        fs::write(Self::meta_path()?, meta_json).await?;
        fs::write(Self::thread_path(new_id)?, "[]").await?;

        Ok(new_id)
    }

    pub async fn save_thread(&self, thread_id: u64, messages: Vec<Message>) -> Result<()> {
        self.check_thread_exists(&thread_id)?;
        let thread_path = Self::thread_path(thread_id)?;

        fs::write(thread_path, serde_json::to_vec(&messages)?).await?;
        Ok(())
    }

    pub async fn get_thread_messages(&self, thread_id: u64) -> Result<Vec<Message>> {
        self.check_thread_exists(&thread_id)?;
        let thread_path = Self::thread_path(thread_id)?;
        Ok(
            match fs::try_exists(&thread_path).await.unwrap_or_default() {
                true => serde_json::from_slice(&fs::read(&thread_path).await?)?,
                false => Vec::new(),
            },
        )
    }

    pub fn get_thread_ids(&self) -> Option<Vec<u64>> {
        self.meta
            .as_ref()
            .map(|m| m.thread_ids_to_timestamps.keys().cloned().collect())
    }
}

pub struct LLMVMCore {
    thread_manager: Mutex<GenerationThreadManager>,
    stdio_clients:
        Mutex<HashMap<String, Arc<Mutex<Timeout<StdioClient<BackendRequest, BackendResponse>>>>>>,
    raw_clients: Mutex<HashMap<String, Arc<Mutex<Box<dyn Backend>>>>>,
    backend_bin_path: Option<String>,
}

impl LLMVMCore {
    pub async fn new(backend_bin_path: Option<String>) -> Result<Self> {
        Ok(Self {
            thread_manager: Mutex::new(GenerationThreadManager::new().await?),
            stdio_clients: Mutex::new(HashMap::new()),
            raw_clients: Mutex::new(HashMap::new()),
            backend_bin_path,
        })
    }

    async fn send_generate_request(
        &self,
        request: BackendGenerationRequest,
        model_description: &ModelDescription,
    ) -> Result<BackendGenerationResponse> {
        let command = format!(
            "{}{}{}",
            BACKEND_COMMAND_PREFIX, model_description.backend, BACKEND_COMMAND_SUFFIX
        );
        let mut stdio_clients_guard = self.stdio_clients.lock().await;
        let client = match stdio_clients_guard.get(&request.model) {
            None => {
                stdio_clients_guard.insert(
                    request.model.to_string(),
                    Arc::new(Mutex::new(
                        StdioClient::new(
                            self.backend_bin_path.as_ref().map(|v| v.as_str()),
                            &command,
                            &[],
                        )
                        .await
                        .map_err(|e| CoreError::BackendStart(e))?,
                    )),
                );
                stdio_clients_guard.get(&request.model).unwrap()
            }
            Some(client) => client,
        }
        .clone();
        drop(stdio_clients_guard);

        let mut client_lock = client.lock().await;
        let resp_result = client_lock
            .call(BackendRequest::Generation(request))
            .await
            .map_err(|e| CoreError::Protocol(ProtocolError::new(ProtocolErrorType::Internal, e)))?;
        Ok(match resp_result? {
            BackendResponse::Generation(response) => response,
        })
    }

    pub async fn close_client(&self, model: &str) {
        self.stdio_clients.lock().await.remove(model);
    }
}

fn merge_generation_parameters(
    preset_parameters: GenerationParameters,
    mut request_parameters: GenerationParameters,
) -> GenerationParameters {
    GenerationParameters {
        model: request_parameters.model.or(preset_parameters.model),
        prompt_template_id: request_parameters
            .prompt_template_id
            .or(preset_parameters.prompt_template_id),
        custom_prompt_template: request_parameters
            .custom_prompt_template
            .or(preset_parameters.custom_prompt_template),
        max_tokens: request_parameters
            .max_tokens
            .or(preset_parameters.max_tokens),
        model_parameters: preset_parameters
            .model_parameters
            .map(|mut parameters| {
                parameters.extend(
                    request_parameters
                        .model_parameters
                        .take()
                        .unwrap_or_default(),
                );
                parameters
            })
            .or(request_parameters.model_parameters),
        prompt_parameters: request_parameters
            .prompt_parameters
            .or(preset_parameters.prompt_parameters),
    }
}

#[llmvm_protocol::async_trait]
impl Core for LLMVMCore {
    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> std::result::Result<GenerationResponse, ProtocolError> {
        async {
            let parameters = match request.preset_id {
                Some(preset_id) => {
                    let mut parameters = load_preset(&preset_id).await?;
                    if let Some(request_parameters) = request.parameters {
                        parameters = merge_generation_parameters(parameters, request_parameters);
                    }
                    parameters
                }
                None => request.parameters.ok_or(CoreError::MissingParameters)?,
            };
            debug!("generation parameters: {:?}", parameters);

            let model = parameters
                .model
                .ok_or(CoreError::MissingParameter("model"))?;
            let model_description =
                ModelDescription::from_str(&model).map_err(|_| CoreError::ModelDescriptionParse)?;

            let is_chat_model = model_description.is_chat_model();
            let prompt_parameters = parameters
                .prompt_parameters
                .unwrap_or(Value::Object(Default::default()));

            let prompt = match parameters.custom_prompt_template {
                Some(template) => {
                    ReadyPrompt::from_custom_template(&template, &prompt_parameters, is_chat_model)
                        .await?
                }
                None => match parameters.prompt_template_id {
                    Some(template_id) => {
                        ReadyPrompt::from_stored_template(
                            &template_id,
                            &prompt_parameters,
                            is_chat_model,
                        )
                        .await?
                    }
                    None => return Err(CoreError::TemplateNotFound),
                },
            };

            let mut thread_messages = match request.existing_thread_id.clone() {
                Some(thread_id) => Some(
                    self.thread_manager
                        .lock()
                        .await
                        .get_thread_messages(thread_id)
                        .await?,
                ),
                None => None,
            };
            if let Some(content) = prompt.system_prompt {
                thread_messages
                    .get_or_insert_with(|| Vec::with_capacity(1))
                    .push(Message {
                        role: MessageRole::System,
                        content,
                    });
            }

            let thread_messages_to_save = match request.save_thread {
                true => {
                    let mut clone = thread_messages.clone().unwrap_or_default();
                    clone.push(Message {
                        role: MessageRole::User,
                        content: prompt.main_prompt.clone(),
                    });
                    Some(clone)
                }
                false => None,
            };

            let backend_request = BackendGenerationRequest {
                model,
                prompt: prompt.main_prompt,
                max_tokens: parameters
                    .max_tokens
                    .ok_or(CoreError::MissingParameter("max_tokens"))?,
                thread_messages,
                model_parameters: parameters.model_parameters,
            };

            info!(
                "Sending backend request with prompt: {}",
                backend_request.prompt
            );
            debug!(
                "Thread messages for requests: {:#?}",
                backend_request.thread_messages
            );
            let response = self
                .send_generate_request(backend_request, &model_description)
                .await?;

            debug!("Response: {}", response.response);

            let thread_id = if let Some(mut thread_messages) = thread_messages_to_save {
                thread_messages.push(Message {
                    role: MessageRole::Assistant,
                    content: response.response.clone(),
                });
                let thread_id = match request.existing_thread_id {
                    Some(id) => id,
                    None => self.thread_manager.lock().await.new_thread_id().await?,
                };
                self.thread_manager
                    .lock()
                    .await
                    .save_thread(thread_id, thread_messages)
                    .await?;
                Some(thread_id)
            } else {
                None
            };

            Ok(GenerationResponse {
                response: response.response,
                thread_id,
            })
        }
        .await
        .map_err(|e| e.into())
    }

    fn init_project(&self) -> std::result::Result<(), ProtocolError> {
        create_dir(PROJECT_DIR_NAME).map_err(|error| ProtocolError {
            error_type: ProtocolErrorType::Internal,
            error: Box::new(error),
        })?;
        Ok(())
    }
}
