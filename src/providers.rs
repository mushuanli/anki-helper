use serde_json::{json, Value};

/// Curated defaults from official Volcengine documentation, checked 2026-10-08.
/// These are suggestions; account permissions are verified by the test requests.
pub fn catalog() -> Value {
    json!({
        "text": [
            {"id":"deepseek-v4-1-flash-260910", "name":"DeepSeek V4.1 Flash（默认）"},
            {"id":"doubao-seed-2-1-pro-260915", "name":"豆包 Seed 2.1 Pro"},
            {"id":"doubao-seed-2-1-lite-260915", "name":"豆包 Seed 2.1 Lite"},
            {"id":"doubao-seed-2-0-mini-260428", "name":"豆包 Seed 2.0 Mini"}
        ],
        "vision": [
            {"id":"doubao-seed-2-1-lite-260915", "name":"豆包 Seed 2.1 Lite（默认）"},
            {"id":"doubao-seed-2-1-pro-260915", "name":"豆包 Seed 2.1 Pro"}
        ],
        "image": [
            {"id":"doubao-seedream-5-0-flash-260915", "name":"Seedream 5.0 Flash（默认）"},
            {"id":"doubao-seedream-5-0-pro-260628", "name":"Seedream 5.0 Pro"},
            {"id":"doubao-seedream-5-0-260128", "name":"Seedream 5.0 Lite"}
        ],
        "tts": [
            {"id":"seed-tts-2.0", "name":"豆包语音合成 2.0（默认）"},
            {"id":"seed-icl-2.0", "name":"豆包声音复刻 2.0（需已开通复刻音色）"}
        ],
        "voices": [
            {"id":"zh_female_vv_uranus_bigtts", "name":"Vivi · 中英音色"}
        ],
        "checked_at":"2026-10-08",
        "console_url":"https://ark.volcengine.com/region:cn-beijing/model?view=CARD_VIEW",
        "text_source":"https://docs.volcengine.com/docs/ark/model-deprecation-notice?lang=zh",
        "image_source":"https://docs.volcengine.com/docs/ark/seedream-4-0-5-0?lang=zh",
        "tts_source":"https://docs.volcengine.com/docs/DoubaoVoice/unidirectional-streaming-text-to-speech-http?lang=zh"
    })
}
