use async_stream::stream;
use futures::stream::StreamExt;
use handlebars::{
    no_escape, Context, Handlebars, Helper, HelperDef, HelperResult, Output, RenderContext,
    RenderError, Renderable, StringOutput,
};
use llmvm_protocol::error::{ProtocolErrorType, SerializableProtocolError};
use llmvm_protocol::http::client::{HttpClient, HttpClientConfig};
use llmvm_protocol::service::{
    BackendRequest, BackendResponse, BoxedService, NotificationStream, ServiceResponse,
};
use llmvm_protocol::stdio::client::{StdioClient, StdioClientConfig};
use llmvm_protocol::tower::Service;
use llmvm_protocol::{
    BackendGenerationRequest, BackendGenerationResponse, Core, GenerationParameters,
    GenerationRequest, GenerationResponse, Message, MessageRole, ModelDescription, ProtocolError,
    ThreadInfo,
};
use llmvm_util::{get_file_path, get_home_dirs, get_project_dir, DirType};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use std::fs::create_dir;
use std::time::Duration;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    str::{FromStr, Utf8Error},
    sync::Arc,
};
use thiserror::Error;
use tokio::fs;
use tokio::sync::Mutex;
use tracing::{debug, info};

const BACKEND_COMMAND_PREFIX: &str = "llmvm-";
const BACKEND_COMMAND_SUFFIX: &str = "-cli";
const PROJECT_DIR_NAME: &str = ".llmvm";
const DEFAULT_TTL_SECS: u64 = 14 * 24 * 3600;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("failed to start stdio backend: {0}")]
    StdioBackendStart(std::io::Error),
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
    BackendStdio(#[from] SerializableProtocolError),
    #[error("backend error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("tempate render error: {0}")]
    TemplateRender(#[from] RenderError),
    #[error("missing generation parameters")]
    MissingParameters,
    #[error("missing required generation parameter: {0}")]
    MissingParameter(&'static str),
    #[error("failed to create http backend service")]
    HttpServiceCreate,
    #[error("unexpected service response type")]
    UnexpectedServiceResponse,
}

impl Into<ProtocolError> for CoreError {
    fn into(self) -> ProtocolError {
        let error_type = match &self {
            CoreError::IO(_) => ProtocolErrorType::Internal,
            CoreError::StdioBackendStart(_) => ProtocolErrorType::Internal,
            CoreError::TemplateNotFound => ProtocolErrorType::BadRequest,
            CoreError::Utf8Error(_) => ProtocolErrorType::BadRequest,
            CoreError::UserHomeNotFound => ProtocolErrorType::Internal,
            CoreError::JsonSerialization(_) => ProtocolErrorType::BadRequest,
            CoreError::TomlSerialization(_) => ProtocolErrorType::BadRequest,
            CoreError::ThreadNotFound => ProtocolErrorType::NotFound,
            CoreError::PresetNotFound => ProtocolErrorType::BadRequest,
            CoreError::ModelDescriptionParse => ProtocolErrorType::BadRequest,
            CoreError::BackendStdio(error) => error.error_type.clone(),
            CoreError::Protocol(error) => error.error_type.clone(),
            CoreError::TemplateRender(_) => ProtocolErrorType::BadRequest,
            CoreError::MissingParameters => ProtocolErrorType::BadRequest,
            CoreError::MissingParameter(_) => ProtocolErrorType::BadRequest,
            CoreError::HttpServiceCreate => ProtocolErrorType::Internal,
            CoreError::UnexpectedServiceResponse => ProtocolErrorType::Internal,
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

    pub fn from_custom_template(
        template: &str,
        parameters: &Value,
        is_chat_model: bool,
    ) -> Result<Self> {
        Self::process(template, parameters, is_chat_model)
    }

    pub fn from_custom_prompt(main_prompt: String) -> Self {
        Self {
            system_prompt: None,
            main_prompt,
        }
    }
}

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

fn thread_path(id: &str) -> Result<PathBuf> {
    get_file_path(DirType::Threads, &format!("{}.json", id), true)
        .ok_or(CoreError::UserHomeNotFound)
}

async fn clean_old_threads_in_dir(directory: &Path, ttl_secs: u64) -> Result<()> {
    let ttl_duration = Duration::from_secs(ttl_secs);
    if let Ok(mut dir_entries) = fs::read_dir(directory).await {
        while let Some(entry) = dir_entries.next_entry().await? {
            let path = entry.path();
            if path.extension() != Some("json".as_ref()) {
                continue;
            }
            let metadata = entry.metadata().await?;
            if let Ok(modified) = metadata.modified() {
                if let Ok(age) = modified.elapsed() {
                    if age > ttl_duration {
                        fs::remove_file(path).await?;
                    }
                }
            }
        }
    }

    Ok(())
}
async fn clean_old_threads(ttl_secs: u64) -> Result<()> {
    if let Some(project_dir) = get_project_dir() {
        clean_old_threads_in_dir(&project_dir.join(DirType::Threads.to_string()), ttl_secs).await?;
    }
    if let Some(home_dirs) = get_home_dirs() {
        clean_old_threads_in_dir(
            &home_dirs.data_dir().join(DirType::Threads.to_string()),
            ttl_secs,
        )
        .await?;
    }
    Ok(())
}

async fn get_thread_infos_in_dir(directory: &Path) -> Result<Vec<ThreadInfo>> {
    let mut result = Vec::new();
    if let Ok(mut dir_entries) = fs::read_dir(directory).await {
        while let Some(entry) = dir_entries.next_entry().await? {
            let path = entry.path();
            if path.extension() != Some("json".as_ref()) {
                continue;
            }
            let metadata = entry.metadata().await?;
            if let Ok(modified) = metadata.modified() {
                if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                    result.push(ThreadInfo {
                        id: id.to_string(),
                        modified: OffsetDateTime::from(modified).format(&Rfc3339).unwrap(),
                    });
                }
            }
        }
    }
    result.sort_by_cached_key(|ti| ti.modified.clone());
    result.reverse();
    Ok(result)
}

async fn get_thread_infos() -> Result<Vec<ThreadInfo>> {
    if let Some(project_dir) = get_project_dir() {
        let result =
            get_thread_infos_in_dir(&project_dir.join(DirType::Threads.to_string())).await?;
        if !result.is_empty() {
            return Ok(result);
        }
    }
    if let Some(home_dirs) = get_home_dirs() {
        return Ok(get_thread_infos_in_dir(
            &home_dirs.data_dir().join(DirType::Threads.to_string()),
        )
        .await?);
    }
    Ok(Vec::new())
}

fn new_thread_id() -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect()
}

async fn save_thread(thread_id: &str, messages: Vec<Message>) -> Result<()> {
    let thread_path = thread_path(thread_id)?;

    fs::write(thread_path, serde_json::to_vec(&messages)?).await?;
    Ok(())
}

async fn maybe_save_thread_messages_and_get_thread_id(
    request: &GenerationRequest,
    new_text: String,
    messages: Option<Vec<Message>>,
) -> Result<Option<String>> {
    Ok(match messages {
        Some(mut messages) => {
            messages.push(Message {
                role: MessageRole::Assistant,
                content: new_text,
            });
            let thread_id = request
                .existing_thread_id
                .clone()
                .unwrap_or_else(new_thread_id);
            save_thread(&thread_id, messages).await?;
            Some(thread_id)
        }
        None => None,
    })
}

pub async fn get_thread_messages(thread_id: &str) -> Result<Vec<Message>> {
    let thread_path = thread_path(thread_id)?;
    Ok(
        match fs::try_exists(&thread_path).await.unwrap_or_default() {
            true => serde_json::from_slice(&fs::read(&thread_path).await?)?,
            false => Vec::new(),
        },
    )
}

#[derive(Deserialize)]
pub struct LLMVMCoreConfig {
    pub tracing_directive: Option<String>,
    pub stdio_client: Option<StdioClientConfig>,
    pub thread_ttl_secs: Option<u64>,

    pub http_backends: HashMap<String, HttpClientConfig>,
}

pub struct LLMVMCore {
    clients: Mutex<HashMap<String, BoxedService<BackendRequest, BackendResponse>>>,
    config: LLMVMCoreConfig,
}

impl LLMVMCore {
    pub async fn new(mut config: LLMVMCoreConfig) -> Result<Self> {
        clean_old_threads(config.thread_ttl_secs.unwrap_or(DEFAULT_TTL_SECS)).await?;

        let mut clients: HashMap<String, BoxedService<BackendRequest, BackendResponse>> =
            HashMap::new();

        for (name, config) in config.http_backends.drain() {
            debug!("loading {} http backend", name);
            clients.insert(
                name,
                Box::new(HttpClient::new(config).map_err(|_| CoreError::HttpServiceCreate)?),
            );
        }

        Ok(Self {
            clients: Mutex::new(clients),
            config,
        })
    }

    async fn get_client<'a>(
        &self,
        clients_guard: &'a mut HashMap<String, BoxedService<BackendRequest, BackendResponse>>,
        model_description: &ModelDescription,
    ) -> Result<&'a mut BoxedService<BackendRequest, BackendResponse>> {
        let command = format!(
            "{}{}{}",
            BACKEND_COMMAND_PREFIX, model_description.backend, BACKEND_COMMAND_SUFFIX
        );
        if !clients_guard.contains_key(&model_description.backend) {
            debug!(
                "starting backend {command} in {:?}",
                self.config.stdio_client.as_ref().map(|c| &c.bin_path)
            );
            let backend = model_description.backend.as_str();
            clients_guard.insert(
                backend.to_string(),
                Box::new(
                    StdioClient::new(
                        &command,
                        &["--log-to-file"],
                        self.config.stdio_client.clone().unwrap_or_default(),
                    )
                    .await
                    .map_err(|e| CoreError::StdioBackendStart(e))?,
                ),
            );
        }
        Ok(clients_guard.get_mut(&model_description.backend).unwrap())
    }

    async fn send_generate_request(
        &self,
        request: BackendGenerationRequest,
        model_description: &ModelDescription,
    ) -> Result<BackendGenerationResponse> {
        let mut clients_guard = self.clients.lock().await;
        let client = self
            .get_client(&mut clients_guard, model_description)
            .await?;
        let resp_future = client.call(BackendRequest::Generation(request));
        drop(clients_guard);
        let resp = resp_future
            .await
            .map_err(|e| CoreError::Protocol(e.into()))?;
        match resp {
            ServiceResponse::Single(response) => match response {
                BackendResponse::Generation(response) => Ok(response),
                _ => Err(CoreError::UnexpectedServiceResponse),
            },
            _ => Err(CoreError::UnexpectedServiceResponse),
        }
    }
    async fn send_generate_request_for_stream(
        &self,
        request: BackendGenerationRequest,
        model_description: &ModelDescription,
    ) -> Result<NotificationStream<BackendResponse>> {
        let mut clients_guard = self.clients.lock().await;
        let client = self
            .get_client(&mut clients_guard, model_description)
            .await?;
        let resp_future = client.call(BackendRequest::GenerationStream(request));
        drop(clients_guard);
        let resp = resp_future
            .await
            .map_err(|e| CoreError::Protocol(e.into()))?;
        match resp {
            ServiceResponse::Multiple(stream) => Ok(stream),
            _ => Err(CoreError::UnexpectedServiceResponse),
        }
    }

    async fn prepare_for_generate(
        &self,
        request: &GenerationRequest,
    ) -> Result<(
        BackendGenerationRequest,
        ModelDescription,
        Option<Vec<Message>>,
    )> {
        let parameters = match &request.preset_id {
            Some(preset_id) => {
                let mut parameters = load_preset(&preset_id).await?;
                if let Some(request_parameters) = request.parameters.clone() {
                    parameters = merge_generation_parameters(parameters, request_parameters);
                }
                parameters
            }
            None => request
                .parameters
                .clone()
                .ok_or(CoreError::MissingParameters)?,
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
                ReadyPrompt::from_custom_template(&template, &prompt_parameters, is_chat_model)?
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
                None => ReadyPrompt::from_custom_prompt(
                    request
                        .custom_prompt
                        .as_ref()
                        .ok_or(CoreError::TemplateNotFound)?
                        .clone(),
                ),
            },
        };

        let mut thread_messages = match request.existing_thread_id.as_ref() {
            Some(thread_id) => Some(get_thread_messages(thread_id).await?),
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
        Ok((backend_request, model_description, thread_messages_to_save))
    }

    pub async fn close_client(&self, model: &str) {
        self.clients.lock().await.remove(model);
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
            let (backend_request, model_description, thread_messages_to_save) =
                self.prepare_for_generate(&request).await?;

            let response = self
                .send_generate_request(backend_request, &model_description)
                .await?;

            debug!("Response: {}", response.response);

            let thread_id = maybe_save_thread_messages_and_get_thread_id(
                &request,
                response.response.clone(),
                thread_messages_to_save,
            )
            .await?;

            Ok(GenerationResponse {
                response: response.response,
                thread_id,
            })
        }
        .await
        .map_err(|e: CoreError| e.into())
    }

    async fn generate_stream(
        &self,
        request: GenerationRequest,
    ) -> std::result::Result<NotificationStream<GenerationResponse>, ProtocolError> {
        async {
            let (backend_request, model_description, thread_messages_to_save) =
                self.prepare_for_generate(&request).await?;

            let mut stream = self
                .send_generate_request_for_stream(backend_request, &model_description)
                .await?;

            Ok(stream! {
                let mut full_response = String::new();
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(response) => match response {
                            BackendResponse::GenerationStream(response) => {
                                full_response.push_str(&response.response);
                                yield Ok(GenerationResponse {
                                    response: response.response,
                                    thread_id: None
                                });
                            }
                            _ => yield Err(CoreError::UnexpectedServiceResponse.into())
                        },
                        Err(e) => yield Err(e)
                    }
                }
                if let Ok(thread_id) = maybe_save_thread_messages_and_get_thread_id(&request, full_response, thread_messages_to_save).await {
                    yield Ok(GenerationResponse { response: String::new(), thread_id });
                }
            }.boxed())
        }
        .await
        .map_err(|e: CoreError| e.into())
    }

    async fn get_last_thread_info(&self) -> std::result::Result<Option<ThreadInfo>, ProtocolError> {
        async { Ok(get_thread_infos().await?.drain(0..1).next()) }
            .await
            .map_err(|e: CoreError| e.into())
    }

    async fn get_all_thread_infos(&self) -> std::result::Result<Vec<ThreadInfo>, ProtocolError> {
        get_thread_infos().await.map_err(|e| e.into())
    }

    async fn get_thread_messages(
        &self,
        id: String,
    ) -> std::result::Result<Vec<Message>, ProtocolError> {
        get_thread_messages(&id).await.map_err(|e| e.into())
    }

    fn init_project(&self) -> std::result::Result<(), ProtocolError> {
        create_dir(PROJECT_DIR_NAME).map_err(|error| ProtocolError {
            error_type: ProtocolErrorType::Internal,
            error: Box::new(error),
        })?;
        Ok(())
    }
}
