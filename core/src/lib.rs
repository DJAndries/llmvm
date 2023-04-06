mod util;

use directories::ProjectDirs;
use handlebars::{
    Context, Handlebars, Helper, HelperDef, HelperResult, Output, RenderContext, Renderable,
    StringOutput,
};
use llmvm_protocol::stdio::{BackendRequest, BackendResponse, StdioClient, StdioError};
use llmvm_protocol::tower::timeout::Timeout;
use llmvm_protocol::tower::Service;
use llmvm_protocol::{
    Backend, BackendGenerationRequest, BackendGenerationResponse, Core, GenerationRequest,
    GenerationResponse, Message, MessageRole, ModelDescription, ProtocolError, ProtocolErrorType,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
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
use util::{
    current_timestamp_secs, get_presets_path, get_project_dirs, get_prompts_path, get_threads_path,
};

const BACKEND_COMMAND_PREFIX: &str = "llmvm-";
const BACKEND_COMMAND_SUFFIX: &str = "-cli";
const BACKEND_BIN_PATH_ENV_KEY: &str = "BACKEND_BIN_PATH";

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("io error: {0}")]
    IO(#[from] std::io::Error),
    #[error("template not found")]
    TemplateNotFound,
    #[error("utf8 error: {0}")]
    Utf8Error(#[from] Utf8Error),
    #[error("unable to load app data, could not find user home folder")]
    UserHomeNotFound,
    #[error("json serialization error: {0}")]
    JsonSerialization(#[from] serde_json::Error),
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
}

impl Into<ProtocolError> for CoreError {
    fn into(self) -> ProtocolError {
        let error_type = match &self {
            CoreError::IO(_) => ProtocolErrorType::Internal,
            CoreError::TemplateNotFound => ProtocolErrorType::BadRequest,
            CoreError::Utf8Error(_) => ProtocolErrorType::BadRequest,
            CoreError::UserHomeNotFound => ProtocolErrorType::Internal,
            CoreError::JsonSerialization { .. } => ProtocolErrorType::BadRequest,
            CoreError::ThreadNotFound => ProtocolErrorType::BadRequest,
            CoreError::PresetNotFound => ProtocolErrorType::BadRequest,
            CoreError::ModelDescriptionParse => ProtocolErrorType::BadRequest,
            CoreError::BackendStdio(error) => error.error_type.clone(),
            CoreError::Protocol(error) => error.error_type.clone(),
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
#[folder = "../prompts"]
struct BuiltInPrompts;

#[derive(Debug)]
pub struct ReadyPrompt {
    system_prompt: Option<String>,
    main_prompt: String,
}

impl ReadyPrompt {
    async fn load_template(template_id: &str) -> Result<String> {
        let template_file_name = format!("{}.hbs", template_id);
        let template_path = get_prompts_path()?.join(&template_file_name);
        if fs::try_exists(&template_path).await.unwrap_or_default() {
            return Ok(fs::read_to_string(template_path).await?);
        };
        let embedded_file =
            BuiltInPrompts::get(&template_file_name).ok_or(CoreError::TemplateNotFound)?;
        Ok(std::str::from_utf8(embedded_file.data.as_ref())?.to_string())
    }

    fn process(
        template: &str,
        parameters: &HashMap<String, String>,
        is_chat_model: bool,
    ) -> Result<Self> {
        let mut handlebars = Handlebars::new();
        let system_role_helper_state =
            Arc::new(std::sync::Mutex::new(SystemRoleHelperState::default()));
        handlebars.register_helper(
            "system_role",
            Box::new(SystemRoleHelper(system_role_helper_state.clone())),
        );

        let mut main_prompt = handlebars
            .render_template(template, parameters)
            .unwrap()
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
        parameters: &HashMap<String, String>,
        is_chat_model: bool,
    ) -> Result<Self> {
        let template = Self::load_template(template_id).await?;
        Self::process(&template, parameters, is_chat_model)
    }

    pub async fn from_custom_template(
        template: &str,
        parameters: &HashMap<String, String>,
        is_chat_model: bool,
    ) -> Result<Self> {
        Self::process(template, parameters, is_chat_model)
    }
}

pub async fn load_preset(preset_id: &str) -> Result<HashMap<String, Value>> {
    let preset_file_name = format!("{}.json", preset_id);
    let preset_path = get_presets_path()?.join(&preset_file_name);
    if !fs::try_exists(&preset_path).await.unwrap_or_default() {
        return Err(CoreError::PresetNotFound);
    }
    let preset_json = fs::read(preset_path).await?;
    Ok(serde_json::from_slice(&preset_json)?)
}

const META_JSON_FILENAME: &str = "meta.json";

#[derive(Default, Serialize, Deserialize)]
struct GenerationThreadMeta {
    last_id: u64,
    thread_ids_to_timestamps: HashMap<u64, u64>,
}

pub struct GenerationThreadManager {
    meta: Option<GenerationThreadMeta>,
    threads_path: PathBuf,
}

impl GenerationThreadManager {
    pub async fn new() -> Result<Self> {
        let mut new_self = Self {
            meta: None,
            threads_path: get_threads_path()?,
        };
        let meta_path = new_self.meta_path();
        if !fs::try_exists(&meta_path).await.unwrap_or_default() {
            return Ok(new_self);
        }
        new_self.meta = Some(serde_json::from_slice(&fs::read(&meta_path).await?)?);
        Ok(new_self)
    }

    fn meta_path(&self) -> PathBuf {
        self.threads_path.join(META_JSON_FILENAME)
    }

    fn thread_path(&self, id: u64) -> PathBuf {
        self.threads_path.join(format!("thread_{}.json", id))
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
                if let Err(e) = fs::remove_file(&self.thread_path(*id)).await {
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
            fs::write(self.meta_path(), meta_json).await?;
        }
        Ok(())
    }

    pub async fn new_thread_id(&mut self) -> Result<u64> {
        fs::create_dir_all(&self.threads_path).await?;

        let meta = self.meta.get_or_insert_with(|| Default::default());

        let new_id = meta.last_id + 1;
        meta.last_id = new_id;
        meta.thread_ids_to_timestamps
            .insert(new_id, current_timestamp_secs());

        let meta_json = serde_json::to_vec(meta)?;
        fs::write(self.meta_path(), meta_json).await?;
        fs::write(self.thread_path(new_id), "[]").await?;

        Ok(new_id)
    }

    pub async fn save_thread(&self, thread_id: u64, messages: Vec<Message>) -> Result<()> {
        self.check_thread_exists(&thread_id)?;
        let thread_path = self.thread_path(thread_id);

        fs::write(thread_path, serde_json::to_vec(&messages)?).await?;
        Ok(())
    }

    pub async fn get_thread_messages(&self, thread_id: u64) -> Result<Vec<Message>> {
        self.check_thread_exists(&thread_id)?;
        let thread_path = self.thread_path(thread_id);
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
}

impl LLMVMCore {
    pub async fn new() -> Result<Self> {
        Ok(Self {
            thread_manager: Mutex::new(GenerationThreadManager::new().await?),
            stdio_clients: Mutex::new(HashMap::new()),
            raw_clients: Mutex::new(HashMap::new()),
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
                    Arc::new(Mutex::new(StdioClient::new(&command, &[]).await?)),
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

#[llmvm_protocol::async_trait]
impl Core for LLMVMCore {
    async fn generate(
        &self,
        mut request: GenerationRequest,
    ) -> std::result::Result<GenerationResponse, ProtocolError> {
        async {
            let model_description = ModelDescription::from_str(request.model.as_str())
                .map_err(|_| CoreError::ModelDescriptionParse)?;
            let is_chat_model = model_description.is_chat_model();
            let mut prompt = match request.custom_prompt_template.take() {
                Some(template) => {
                    ReadyPrompt::from_custom_template(
                        &template,
                        &request.prompt_parameters,
                        is_chat_model,
                    )
                    .await?
                }
                None => match request.prompt_template_id.take() {
                    Some(template_id) => {
                        ReadyPrompt::from_stored_template(
                            &template_id,
                            &request.prompt_parameters,
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
            if let Some(content) = prompt.system_prompt.take() {
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

            let mut model_parameters = match request.model_parameters_preset_id {
                Some(id) => Some(load_preset(&id).await?),
                None => None,
            };

            if let Some(explicit_parameters) = request.model_parameters {
                match model_parameters.as_mut() {
                    Some(preset_parameters) => preset_parameters.extend(explicit_parameters),
                    None => model_parameters = Some(explicit_parameters),
                };
            }

            let backend_request = BackendGenerationRequest {
                model: request.model,
                prompt: prompt.main_prompt,
                max_tokens: request.max_tokens,
                thread_messages,
                model_parameters,
            };

            let response = self
                .send_generate_request(backend_request, &model_description)
                .await?;

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
}
