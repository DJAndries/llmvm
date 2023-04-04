use llmvm_core::LoadedPromptInfo;
use std::collections::HashMap;

fn main() {
    println!(
        "{:?}",
        LoadedPromptInfo::load_prompt(
            "simple-math-chat",
            &HashMap::from([("problem".to_string(), "4 + 2 * 9".to_string())]),
        )
    );
}
