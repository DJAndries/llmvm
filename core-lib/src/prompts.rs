use std::{cell::RefCell, sync::Arc};

use handlebars::{
    no_escape, Context, Handlebars, Helper, HelperDef, HelperResult, Output, RenderContext,
    Renderable, StringOutput,
};
use llmvm_util::{get_file_path, DirType};
use rust_embed::RustEmbed;
use serde_json::Value;
use tokio::fs;
use tracing::debug;

use crate::{error::CoreError, Result};

#[derive(RustEmbed)]
#[folder = "./prompts"]
struct BuiltInPrompts;

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

/// A prompt that is ready to use for text generation.
/// May contain an optional prompt for the system role.
#[derive(Debug, Default)]
pub struct ReadyPrompt {
    /// A system role prompt. For chat generation requests, this will be appended
    /// to the `thread_messages` of the backend generation request. For non-chat
    /// generation requests, the system prompt will be prepended to the main prompt.
    pub system_prompt: Option<String>,
    /// The main prompt. For chat generation requests, this prompt should use the user role.
    pub main_prompt: Option<String>,
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

        Ok(Self {
            system_prompt,
            main_prompt: match main_prompt.is_empty() {
                true => None,
                false => Some(main_prompt),
            },
        })
    }

    /// Loads a Handlebars prompt template from the current project or
    /// user home directory, and generates a full prompt using the
    /// given template parameters.
    pub async fn from_stored_template(
        template_id: &str,
        parameters: &Value,
        is_chat_model: bool,
    ) -> Result<Self> {
        let template = Self::load_template(template_id).await?;
        Self::process(&template, parameters, is_chat_model)
    }

    /// Generates a full prompt using a Handlebars template and parameters.
    pub fn from_custom_template(
        template: &str,
        parameters: &Value,
        is_chat_model: bool,
    ) -> Result<Self> {
        Self::process(template, parameters, is_chat_model)
    }
}
