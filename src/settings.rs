use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{env, path::Path};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub connections: Vec<Connection>,
    pub vision_connection: String,
    pub vision_model: String,
    #[serde(skip)]
    pub vision_base_url: String,
    #[serde(skip)]
    pub vision_api_key: String,
    #[serde(skip)]
    pub vision_provider: String,
    pub text_connection: String,
    pub image_connection: String,
    pub tts_connection: String,
    pub image_provider: String,
    pub port: u16,
    pub text_provider: String,
    pub text_thinking: bool,
    pub openai_base_url: String,
    pub openai_api_key: String,
    pub openai_model: String,
    pub audio_enabled: bool,
    pub audio_backend: String,
    pub system_voice: String,
    pub tts_provider: String,
    pub tts_url: String,
    pub tts_api_key: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub images_enabled: bool,
    pub image_url: String,
    pub image_api_key: String,
    pub image_model: String,
    pub concurrency: usize,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub text_url: String,
    pub image_url: String,
    pub tts_url: String,
    pub models: Vec<ConnectionModel>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ConnectionModel {
    pub id: String,
    pub name: String,
    pub kind: String,
}

impl Connection {
    pub fn supports(&self, kind: &str) -> bool {
        matches!(
            (self.provider.as_str(), kind),
            ("ark", "text" | "image" | "vision")
                | ("doubao", "audio")
                | ("openai", "text" | "image" | "audio" | "vision")
        )
    }

    pub fn url(&self, kind: &str) -> String {
        let override_url = match kind {
            "text" | "vision" => &self.text_url,
            "image" => &self.image_url,
            _ => &self.tts_url,
        };
        if !override_url.is_empty() {
            return override_url.clone();
        }
        if self.provider == "doubao" {
            return self.base_url.clone();
        }
        let base = self.base_url.trim_end_matches('/');
        let suffix = match kind {
            "text" | "vision" => "chat/completions",
            "image" => "images/generations",
            _ => "audio/speech",
        };
        format!("{base}/{suffix}")
    }
}

fn validate_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("服务地址必须是完整的 HTTP/HTTPS URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("服务地址必须是没有内嵌凭据的 HTTP/HTTPS URL");
    }
    Ok(())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            connections: Vec::new(),
            vision_connection: String::new(),
            vision_model: "doubao-seed-2-1-lite-260915".into(),
            vision_base_url: String::new(),
            vision_api_key: String::new(),
            vision_provider: String::new(),
            text_connection: String::new(),
            image_connection: String::new(),
            tts_connection: String::new(),
            image_provider: "ark".into(),
            port: 0,
            text_provider: "ark".into(),
            text_thinking: false,
            openai_base_url: "https://ark.cn-beijing.volces.com/api/v3/chat/completions".into(),
            openai_api_key: String::new(),
            openai_model: "deepseek-v4-1-flash-260910".into(),
            audio_enabled: false,
            audio_backend: "doubao".into(),
            system_voice: String::new(),
            tts_provider: "doubao".into(),
            tts_url: "https://openspeech.bytedance.com/api/v3/tts/unidirectional".into(),
            tts_api_key: String::new(),
            tts_model: "seed-tts-2.0".into(),
            tts_voice: "zh_female_vv_uranus_bigtts".into(),
            images_enabled: false,
            image_url: "https://ark.cn-beijing.volces.com/api/v3/images/generations".into(),
            image_api_key: String::new(),
            image_model: "doubao-seedream-5-0-flash-260915".into(),
            concurrency: 4,
        }
    }
}

impl Settings {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("config.toml");
        let mut config: Self = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path)?)
                .context("config.toml 格式错误，请检查配置文件")?
        } else {
            Self::default()
        };
        // Existing environment-based CLI configuration remains supported.
        for (name, target) in [
            ("OPENAI_BASEURL", &mut config.openai_base_url),
            ("OPENAI_API_KEY", &mut config.openai_api_key),
            ("OPENAI_MODEL", &mut config.openai_model),
            ("ARK_API_KEY", &mut config.image_api_key),
            ("TTS_API_KEY", &mut config.tts_api_key),
        ] {
            if target.is_empty() || !path.exists() {
                if let Ok(value) = env::var(name) {
                    *target = value;
                }
            }
        }
        if config.openai_api_key.is_empty() && config.text_provider == "ark" {
            if let Ok(key) = env::var("ARK_API_KEY") {
                config.openai_api_key = key;
            }
        }
        if config.connections.is_empty() {
            config.migrate_connections();
        }
        config = config.resolved()?;
        config.validate(false)?;
        Ok(config)
    }

    pub fn validate(&self, generating: bool) -> Result<()> {
        if !matches!(self.text_provider.as_str(), "ark" | "openai") {
            bail!("文本供应商必须是 ark 或 openai");
        }
        if !matches!(self.tts_provider.as_str(), "doubao" | "openai" | "system") {
            bail!("语音供应商必须是 doubao 或 openai");
        }
        if !matches!(self.audio_backend.as_str(), "doubao" | "system") {
            bail!("音频方式必须是豆包或系统语音");
        }
        if self.audio_backend == "system" && !cfg!(target_os = "macos") {
            bail!("系统语音仅支持 macOS");
        }
        if !(1..=16).contains(&self.concurrency) {
            bail!("并发数必须在 1–16 之间");
        }
        if self.connections.is_empty() {
            validate_url(&self.openai_base_url)?;
            if self.audio_enabled && self.tts_provider != "system" {
                validate_url(&self.tts_url)?;
            }
            if self.images_enabled {
                validate_url(&self.image_url)?;
            }
        } else {
            let mut ids = std::collections::HashSet::new();
            for connection in &self.connections {
                if connection.id.is_empty()
                    || connection.name.trim().is_empty()
                    || !ids.insert(&connection.id)
                    || !matches!(connection.provider.as_str(), "ark" | "doubao" | "openai")
                {
                    bail!("服务连接需要唯一 ID、名称和支持的协议");
                }
                validate_url(&connection.base_url)?;
                for value in [
                    &connection.text_url,
                    &connection.image_url,
                    &connection.tts_url,
                ] {
                    if !value.is_empty() {
                        validate_url(value)?;
                    }
                }
                for model in &connection.models {
                    if model.id.trim().is_empty() || !connection.supports(&model.kind) {
                        bail!("连接模型的 ID 或用途无效");
                    }
                }
            }
        }
        if generating {
            if self.openai_api_key.trim().is_empty() {
                bail!("请先在设置中填写文本生成 API Key");
            }
            if self.openai_model.trim().is_empty() {
                bail!("请填写文本生成模型");
            }
            if self.audio_enabled
                && self.tts_provider != "system"
                && (self.tts_api_key.trim().is_empty()
                    || self.tts_model.trim().is_empty()
                    || self.tts_voice.trim().is_empty())
            {
                bail!("已启用音频，请填写 TTS Key、模型和声音");
            }
            if self.images_enabled
                && (self.image_key().trim().is_empty() || self.image_model.trim().is_empty())
            {
                bail!("已启用图片，请填写图片 Key 和模型");
            }
        }
        Ok(())
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        self.resolved()?.validate(false)?;
        let mut value = toml::Value::try_from(self)?;
        if !self.connections.is_empty() {
            for key in [
                "text_provider",
                "openai_base_url",
                "openai_api_key",
                "tts_provider",
                "tts_url",
                "tts_api_key",
                "image_url",
                "image_api_key",
                "image_provider",
            ] {
                value.as_table_mut().unwrap().remove(key);
            }
        }
        atomic_write(
            &root.join("config.toml"),
            toml::to_string_pretty(&value)?.as_bytes(),
            true,
        )
    }

    pub fn migrate_connections(&mut self) {
        if !self.connections.is_empty() {
            return;
        }
        let text = Connection {
            id: "legacy-text".into(),
            name: "火山方舟".into(),
            provider: self.text_provider.clone(),
            base_url: self.chat_url().trim_end_matches("/chat/completions").into(),
            text_url: self.chat_url(),
            api_key: self.openai_api_key.clone(),
            ..Default::default()
        };
        let image_key = self.image_key().to_string();
        let origin = |value: &str| reqwest::Url::parse(value).ok().map(|url| url.origin());
        let shared = origin(&self.image_url) == origin(&self.openai_base_url)
            && text.supports("image")
            && (self.image_api_key.is_empty() || self.image_api_key == self.openai_api_key);
        self.text_connection = text.id.clone();
        self.vision_connection = self.text_connection.clone();
        if self.text_provider != "ark" {
            self.vision_model = self.openai_model.clone();
        }
        self.connections.push(text);
        if shared {
            self.image_connection = self.text_connection.clone();
            self.connections[0].image_url = self.image_url.clone();
        } else {
            self.image_connection = "legacy-image".into();
            self.connections.push(Connection {
                id: self.image_connection.clone(),
                name: "图片服务".into(),
                provider: self.image_provider.clone(),
                base_url: self.image_url.clone(),
                image_url: self.image_url.clone(),
                api_key: image_key,
                ..Default::default()
            });
        }
        self.tts_connection = "legacy-tts".into();
        self.connections.push(Connection {
            id: self.tts_connection.clone(),
            name: "豆包语音".into(),
            provider: self.tts_provider.clone(),
            base_url: self.tts_url.clone(),
            tts_url: self.tts_url.clone(),
            api_key: self.tts_api_key.clone(),
            ..Default::default()
        });
    }

    pub fn resolved(&self) -> Result<Self> {
        let mut config = self.clone();
        if self.connections.is_empty() {
            if self.audio_backend == "system" {
                config.tts_provider = "system".into();
            }
            return Ok(config);
        }
        // Never fall back to a different connection's credentials.
        config.openai_api_key.clear();
        config.image_api_key.clear();
        config.tts_api_key.clear();
        config.vision_api_key.clear();
        for (kind, id) in [
            ("text", &self.text_connection),
            ("image", &self.image_connection),
            ("audio", &self.tts_connection),
            ("vision", &self.vision_connection),
        ] {
            if id.is_empty() {
                continue;
            }
            let connection = self
                .connections
                .iter()
                .find(|c| &c.id == id)
                .context("所选服务连接不存在，请重新选择")?;
            if !connection.supports(kind) {
                bail!("所选连接不支持 {kind}");
            }
            match kind {
                "text" => {
                    config.text_provider = connection.provider.clone();
                    config.openai_base_url = connection.url(kind);
                    config.openai_api_key = connection.api_key.clone();
                }
                "vision" => {
                    config.vision_provider = connection.provider.clone();
                    config.vision_base_url = connection.url(kind);
                    config.vision_api_key = connection.api_key.clone();
                }
                "image" => {
                    config.image_provider = connection.provider.clone();
                    config.image_url = connection.url(kind);
                    config.image_api_key = connection.api_key.clone();
                }
                _ => {
                    config.tts_provider = connection.provider.clone();
                    config.tts_url = connection.url(kind);
                    config.tts_api_key = connection.api_key.clone();
                }
            }
        }
        if self.audio_backend == "system" {
            config.tts_provider = "system".into();
        }
        Ok(config)
    }

    pub fn image_key(&self) -> &str {
        if !self.connections.is_empty() || !self.image_api_key.is_empty() {
            return &self.image_api_key;
        }
        let image_host = reqwest::Url::parse(&self.image_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        let text_host = reqwest::Url::parse(&self.openai_base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string));
        if image_host == text_host && image_host.is_some() {
            &self.openai_api_key
        } else {
            ""
        }
    }

    pub fn chat_url(&self) -> String {
        let base = self.openai_base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.into()
        } else if base.ends_with("/v1") || base.ends_with("/api/v3") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        }
    }
}

/// Write alongside the destination and atomically replace it, including on Windows.
pub fn atomic_write(path: &Path, content: &[u8], private: bool) -> Result<()> {
    let parent = path.parent().context("文件没有父目录")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = private;
    use std::io::Write;
    file.write_all(content)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|e| e.error)
        .context("保存文件失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_roundtrip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Settings::default();
        config.openai_api_key = "test-key".into();
        config.save(dir.path()).unwrap();
        assert_eq!(
            Settings::load(dir.path()).unwrap().openai_api_key,
            "test-key"
        );
        config.openai_model = "updated".into();
        config.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap().openai_model, "updated");
        config.concurrency = 0;
        assert!(config.save(dir.path()).is_err());
    }
    #[test]
    fn migrates_shared_credentials_and_persists_only_connections() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Settings::default();
        config.openai_api_key = "one-shared-key".into();
        config.tts_api_key = "speech-key".into();
        config.migrate_connections();
        assert_eq!(config.connections.len(), 2);
        assert_eq!(config.text_connection, config.image_connection);
        assert_eq!(config.text_connection, config.vision_connection);
        config.save(dir.path()).unwrap();
        let file = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!file.contains("openai_api_key"));
        assert!(!file.contains("image_api_key"));
        assert_eq!(file.matches("one-shared-key").count(), 1);
        let loaded = Settings::load(dir.path()).unwrap();
        assert_eq!(loaded.openai_api_key, "one-shared-key");
        assert_eq!(loaded.image_key(), "one-shared-key");
        assert_eq!(loaded.vision_api_key, "one-shared-key");
        assert_eq!(loaded.tts_api_key, "speech-key");
    }

    #[test]
    fn empty_image_connection_never_borrows_another_key() {
        let mut config = Settings::default();
        config.openai_api_key = "text-secret".into();
        config.migrate_connections();
        config.image_connection.clear();
        assert_eq!(config.resolved().unwrap().image_key(), "");
    }

    #[test]
    fn accepts_compatible_base_urls() {
        let mut config = Settings::default();
        for base in [
            "https://example.com",
            "https://example.com/v1/",
            "https://example.com/v1/chat/completions",
        ] {
            config.openai_base_url = base.into();
            assert_eq!(config.chat_url(), "https://example.com/v1/chat/completions");
        }
    }
}
