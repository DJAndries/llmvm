use serde::{Deserialize, Serialize};

use crate::lsp::LspMessage;

#[derive(Serialize, Deserialize)]
pub enum ActionType {
    Complete,
}

struct Action {
    model: String,
    action_type: ActionType,
}

pub struct ActionList {
    actions: Vec<Action>,
}

// impl ActionList {
//     pub fn modify_actions_list(message: &mut LspMessage) -> Result<()> {
//         let action_list = serde_json::from_value(message.payload);
//     }
// }
