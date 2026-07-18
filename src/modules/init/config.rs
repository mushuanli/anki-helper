// src/modules/init/config.rs
use std::env;

// --- AI 服务配置 ---
pub fn openai_base_url() -> String {
    env::var("OPENAI_BASEURL").unwrap_or_else(|_| "https://api.deepseek.com/v1/chat/completions".to_string())
}
pub fn openai_api_key() -> String {
    env::var("OPENAI_API_KEY").expect("错误：未设置 OPENAI_API_KEY 环境变量")
}
pub fn openai_model() -> String {
    env::var("OPENAI_MODEL").unwrap_or_else(|_| "deepseek-v4-pro".to_string())
}

// --- 火山方舟图片生成配置 ---
pub fn ark_api_key() -> String {
    env::var("ARK_API_KEY").expect("错误：未设置 ARK_API_KEY 环境变量")
}
pub const ARK_API_GEN_URL: &str = "https://ark.cn-beijing.volces.com/api/v3/images/generations";
pub const ARK_API_MODEL: &str = "doubao-seedream-5-0-260128";


// --- 目录配置 ---
pub const AUDIO_DIR: &str = "audio";
pub const IMAGE_DIR: &str = "images";
pub const JSON_DIR: &str = "word_json"; // 注意：与 pack 模块对齐
