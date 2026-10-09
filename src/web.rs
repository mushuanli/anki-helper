use crate::{
    cli::{PackArgs, ServeArgs},
    models::WordData,
    modules::{init::utils, pack},
    settings::{atomic_write, Settings},
};
use anyhow::{anyhow, bail, Context};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::io::ReaderStream;

type AppResult<T> = Result<T, ApiError>;
struct ApiError(anyhow::Error);
impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{:#}", self.0)})),
        )
            .into_response()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Project {
    id: String,
    name: String,
    path: PathBuf,
}
#[derive(Clone, Serialize)]
struct Card {
    id: String,
    data: WordData,
    media_status: Value,
}
#[derive(Clone, Default, Serialize)]
struct Job {
    kind: String,
    status: String,
    phase: String,
    current: usize,
    total: usize,
    media_files: usize,
    failed_cards: Vec<String>,
    scope: String,
    force: bool,
    written_bytes: u64,
    total_bytes: u64,
    failed: usize,
    message: String,
    errors: Vec<String>,
    output_path: Option<String>,
    elapsed_seconds: u64,
    started_at: u64,
}
struct Inner {
    config: Settings,
    projects: Vec<Project>,
    jobs: HashMap<String, Job>,
}
struct App {
    root: PathBuf,
    inner: Mutex<Inner>,
    token: String,
    origin: String,
    client: reqwest::Client,
    tests_dir: tempfile::TempDir,
}
type Shared = Arc<App>;

fn component(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\\', ':'])
        || value.chars().any(char::is_control)
    {
        bail!("无效的文件标识");
    }
    Ok(())
}
fn project(state: &App, id: &str) -> anyhow::Result<Project> {
    component(id)?;
    state
        .inner
        .lock()
        .unwrap()
        .projects
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .context("项目不存在")
}
fn idle(inner: &Inner, id: &str) -> anyhow::Result<()> {
    if inner.jobs.get(id).is_some_and(|j| j.status == "running") {
        bail!("项目正在处理，请等待任务完成");
    }
    Ok(())
}
fn checked_file(project: &Project, dir: &str, name: &str) -> anyhow::Result<PathBuf> {
    component(name)?;
    let base = project.path.join(dir).canonicalize()?;
    if !base.starts_with(&project.path) {
        bail!("目录链接指向了项目外部");
    }
    let target = base.join(name);
    if target.exists() && !target.canonicalize()?.starts_with(&base) {
        bail!("文件链接指向了项目外部");
    }
    Ok(target)
}
fn read_cards(project: &Project) -> anyhow::Result<Vec<Card>> {
    let mut cards = Vec::new();
    let base = project.path.join("word_json");
    for entry in std::fs::read_dir(base)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("文件名不是有效 UTF-8")?
            .to_string();
        let path = checked_file(project, "word_json", &format!("{id}.json"))?;
        let data: WordData = serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("卡片 JSON 无效: {}", path.display()))?;
        let exists = |dir: &str, name: &Option<String>| {
            name.as_ref().is_some_and(|name| {
                checked_file(project, dir, name)
                    .ok()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .is_some_and(|m| m.is_file() && m.len() > 0)
            })
        };
        let media_status = json!({"audio": exists("audio", &data.audio),
            "audio_example": exists("audio", &data.audio_example), "image": exists("images", &data.image)});
        cards.push(Card {
            id,
            data,
            media_status,
        });
    }
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cards)
}
fn save_registry(state: &App, projects: &[Project]) -> anyhow::Result<()> {
    atomic_write(
        &state.root.join("projects.json"),
        &serde_json::to_vec_pretty(projects)?,
        true,
    )
}

pub async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    let root = std::env::current_dir()?.canonicalize()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".helper.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("当前运行目录已有 Anki Helper 实例，请使用已打开的页面")?;
    let config = Settings::load(&root)?;
    config.save(&root)?;
    let projects = if root.join("projects.json").exists() {
        serde_json::from_slice(&std::fs::read(root.join("projects.json"))?)
            .context("projects.json 无效")?
    } else {
        Vec::new()
    };
    let listener = tokio::net::TcpListener::bind((
        std::net::Ipv4Addr::LOCALHOST,
        args.port.unwrap_or(config.port),
    ))
    .await?;
    let origin = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
    let token = format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    );
    let state = Arc::new(App {
        tests_dir: tempfile::Builder::new()
            .prefix(".service-tests-")
            .tempdir_in(&root)?,
        root: root.clone(),
        inner: Mutex::new(Inner {
            config,
            projects,
            jobs: HashMap::new(),
        }),
        token: token.clone(),
        origin: origin.clone(),
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(20))
            .build()?,
    });
    let url = format!("{origin}/#{token}");
    println!(
        "Anki Helper: {url}\n运行目录: {}\n配置: {}\n按 Ctrl+C 退出。",
        root.display(),
        root.join("config.toml").display()
    );
    if !args.no_browser {
        if let Err(error) = open_browser(&url) {
            eprintln!("无法自动打开浏览器: {error}，请打开上面的地址。");
        }
    }
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    drop(lock);
    Ok(())
}

fn router(state: Shared) -> Router {
    let api = Router::new()
        .route("/config", get(get_config).put(save_config))
        .route(
            "/model-presets",
            get(|| async { Json(crate::providers::catalog()) }),
        )
        .route("/test", post(test_service))
        .route(
            "/extract-words",
            post(extract_words).layer(DefaultBodyLimit::max(20 * 1024 * 1024)),
        )
        .route("/tests/{name}", get(test_asset))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", get(get_project))
        .route("/projects/{id}/import", post(import_words))
        .route("/projects/{id}/cards/{card}", put(save_card))
        .route("/projects/{id}/generate", post(generate))
        .route("/projects/{id}/export", post(export))
        .route("/projects/{id}/open", post(open_folder))
        .route("/projects/{id}/media/{dir}/{name}", get(media))
        .route("/projects/{id}/download", get(download))
        .route("/projects/{id}/job", get(job))
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(script))
        .route("/style.css", get(style))
        .nest("/api", api)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .with_state(state)
}
async fn host_guard(State(state): State<Shared>, request: Request<Body>, next: Next) -> Response {
    if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(state.origin.trim_start_matches("http://"))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob:; media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
    response
}
async fn authenticate(State(state): State<Shared>, request: Request<Body>, next: Next) -> Response {
    let key = request
        .headers()
        .get("x-helper-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let cookie_ok = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .any(|s| s.trim() == format!("helper_session={}", state.token));
    let origin_ok = request
        .headers()
        .get(header::ORIGIN)
        .is_none_or(|v| v.to_str().ok() == Some(&state.origin));
    if (key != state.token && !cookie_ok) || !origin_ok {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "helper_session={}; HttpOnly; SameSite=Strict; Path=/api",
            state.token
        )
        .parse()
        .unwrap(),
    );
    response
}
async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}
async fn script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../web/app.js"),
    )
}
async fn style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../web/style.css"),
    )
}
async fn get_config(State(state): State<Shared>) -> Json<Value> {
    let inner = state.inner.lock().unwrap();
    let mut config = inner.config.clone();
    config.migrate_connections();
    let mut value = serde_json::to_value(&config).unwrap();
    for connection in value["connections"].as_array_mut().unwrap() {
        connection["has_api_key"] = json!(!connection["api_key"].as_str().unwrap_or("").is_empty());
        connection["api_key"] = json!("");
    }
    for field in ["openai_api_key", "tts_api_key", "image_api_key"] {
        value[format!("has_{field}")] = json!(!value[field].as_str().unwrap_or("").is_empty());
        value[field] = json!("");
    }
    value["system_audio_available"] = json!(cfg!(target_os = "macos"));
    value["runtime_dir"] = json!(display_path(&state.root));
    Json(value)
}
#[derive(Deserialize)]
struct ConfigUpdate {
    #[serde(flatten)]
    config: Settings,
    #[serde(default)]
    clear_keys: Vec<String>,
    #[serde(default)]
    clear_connections: Vec<String>,
}
async fn save_config(
    State(state): State<Shared>,
    Json(update): Json<ConfigUpdate>,
) -> AppResult<Json<Value>> {
    let mut inner = state.inner.lock().unwrap();
    let config = merge_config(update, &inner.config).resolved()?;
    if config.audio_enabled {
        require_audio(&config)?;
    }
    if config.images_enabled {
        require_image(&config)?;
    }
    config.save(&state.root)?;
    inner.config = config;
    Ok(Json(
        json!({"message": "配置已保存到运行目录的 config.toml"}),
    ))
}
fn merge_config(update: ConfigUpdate, previous: &Settings) -> Settings {
    let mut config = update.config;
    let mut previous_connections = previous.clone();
    previous_connections.migrate_connections();
    for connection in &mut config.connections {
        let old = previous_connections
            .connections
            .iter()
            .find(|c| c.id == connection.id);
        if connection.api_key.is_empty() && !update.clear_connections.contains(&connection.id) {
            if let Some(old) = old.filter(|old| {
                old.provider == connection.provider
                    && old.base_url == connection.base_url
                    && old.text_url == connection.text_url
                    && old.image_url == connection.image_url
                    && old.tts_url == connection.tts_url
            }) {
                connection.api_key = old.api_key.clone();
            }
        }
    }
    for (name, target, old) in [
        (
            "openai_api_key",
            &mut config.openai_api_key,
            &previous.openai_api_key,
        ),
        (
            "tts_api_key",
            &mut config.tts_api_key,
            &previous.tts_api_key,
        ),
        (
            "image_api_key",
            &mut config.image_api_key,
            &previous.image_api_key,
        ),
    ] {
        let same_service = match name {
            "openai_api_key" => {
                config.text_provider == previous.text_provider
                    && config.openai_base_url == previous.openai_base_url
            }
            "tts_api_key" => {
                config.tts_provider == previous.tts_provider && config.tts_url == previous.tts_url
            }
            _ => config.image_url == previous.image_url,
        };
        if target.is_empty() && same_service && !update.clear_keys.iter().any(|v| v == name) {
            *target = old.clone();
        }
    }
    config
}
#[derive(Deserialize)]
struct TestService {
    kind: String,
    config: ConfigUpdate,
}
async fn test_service(
    State(state): State<Shared>,
    Json(input): Json<TestService>,
) -> AppResult<Json<Value>> {
    let config = {
        let inner = state.inner.lock().unwrap();
        merge_config(input.config, &inner.config).resolved()?
    };
    config.validate(false)?;
    let started = Instant::now();
    let id = format!("{:016x}", rand::random::<u64>());
    let mut result = match input.kind.as_str() {
        "text" => {
            if config.openai_api_key.trim().is_empty() || config.openai_model.trim().is_empty() {
                return Err(anyhow!("请填写文本 API Key 和模型 ID").into());
            }
            let card = utils::ai_chat(&state.client, "apple", &config).await?;
            json!({"message":"文本模型测试成功", "model":config.openai_model, "card":card})
        }
        "vision" => test_vision(&state.client, &config).await?,
        "audio" => {
            require_audio(&config)?;
            let filename = format!("{id}.{}", utils::audio_extension(&config));
            utils::generate_audio(
                &state.client,
                "Apple. An apple a day. 你好，这是语音测试。",
                &state.tests_dir.path().join(&filename),
                &config,
            )
            .await?;
            json!({"message":"语音模型测试成功，可试听", "model":if config.tts_provider == "system" { "macOS 系统语音" } else { &config.tts_model }, "voice":config.tts_voice, "asset_url":format!("/api/tests/{filename}")})
        }
        "image" => {
            if config.image_key().trim().is_empty() || config.image_model.trim().is_empty() {
                return Err(anyhow!("请填写图片服务 API Key 和模型 ID").into());
            }
            let filename = format!("{id}.png");
            utils::generate_image(&state.client, "A single red apple on a clean white background, educational flashcard illustration.", &state.tests_dir.path().join(&filename), &config).await?;
            json!({"message":"图片模型测试成功，可预览", "model":config.image_model, "asset_url":format!("/api/tests/{filename}")})
        }
        _ => return Err(anyhow!("未知的测试类型").into()),
    };
    result["duration_ms"] = json!(started.elapsed().as_millis());
    result["status"] = json!("success");
    let connection_id = match input.kind.as_str() {
        "vision" => &config.vision_connection,
        "text" => &config.text_connection,
        "audio" => &config.tts_connection,
        _ => &config.image_connection,
    };
    result["connection"] = json!(
        if input.kind == "audio" && config.tts_provider == "system" {
            "macOS 系统语音"
        } else {
            config
                .connections
                .iter()
                .find(|c| &c.id == connection_id)
                .map(|c| c.name.as_str())
                .unwrap_or("当前服务")
        }
    );
    Ok(Json(result))
}
async fn test_vision(client: &reqwest::Client, config: &Settings) -> anyhow::Result<Value> {
    use base64::Engine;
    if config.vision_api_key.trim().is_empty() || config.vision_model.trim().is_empty() {
        bail!("请填写识图连接 API Key 和模型 ID");
    }
    let image = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(include_bytes!("../web/test-word.png"))
    );
    let words = utils::extract_words_from_photos(client, &[image], config).await?;
    if !words.iter().any(|word| {
        word.name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case("apple"))
    }) {
        bail!("识图模型未正确识别测试图片中的 APPLE，请检查模型是否支持图片输入");
    }
    Ok(
        json!({"message":"识图模型测试成功，已识别测试图片", "model": config.vision_model, "words": words}),
    )
}

async fn test_asset(State(state): State<Shared>, Path(name): Path<String>) -> AppResult<Response> {
    component(&name)?;
    let mime = if name.ends_with(".mp3") {
        "audio/mpeg"
    } else if name.ends_with(".wav") {
        "audio/wav"
    } else if name.ends_with(".png") {
        "image/png"
    } else {
        return Err(anyhow!("无效的测试媒体格式").into());
    };
    stream_file(state.tests_dir.path().join(name), mime, false).await
}

#[derive(Deserialize)]
struct ExtractWords {
    images: Vec<String>,
}
async fn extract_words(
    State(state): State<Shared>,
    Json(input): Json<ExtractWords>,
) -> AppResult<Json<Value>> {
    let config = state.inner.lock().unwrap().config.resolved()?;
    if config.vision_api_key.trim().is_empty() || config.vision_model.trim().is_empty() {
        return Err(anyhow!("请先在模型与服务设置中配置识图连接、API Key 和模型并保存").into());
    }
    if input.images.is_empty() || input.images.len() > 6 {
        return Err(anyhow!("请上传 1–6 张单词表照片").into());
    }
    let mut total = 0;
    for image in &input.images {
        total += validate_photo(image)?;
    }
    if total > 12 * 1024 * 1024 {
        return Err(anyhow!("照片总大小不能超过 12 MB").into());
    }
    let words = utils::extract_words_from_photos(&state.client, &input.images, &config).await?;
    let words: Vec<Value> = words
        .into_iter()
        .map(|word| {
            let mut value = serde_json::to_value(word).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .retain(|_, value| !value.is_null());
            value
        })
        .collect();
    Ok(Json(json!({"words": words, "model": config.vision_model,
        "message": "识别完成，请检查拼写、音标和释义后再导入"})))
}
fn validate_photo(image: &str) -> anyhow::Result<usize> {
    use base64::Engine;
    let (header, encoded) = image.split_once(',').context("图片编码无效")?;
    if encoded.len() > 12 * 1024 * 1024 {
        bail!("单张照片不能超过 8 MB");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("图片编码无效")?;
    let valid = match header {
        "data:image/jpeg;base64" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "data:image/png;base64" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "data:image/webp;base64" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    };
    if !valid || bytes.len() > 8 * 1024 * 1024 {
        bail!("请上传有效的 JPG、PNG 或 WebP 照片（单张最多 8 MB）");
    }
    Ok(bytes.len())
}

async fn list_projects(State(state): State<Shared>) -> Json<Value> {
    Json(
        json!({"projects": state.inner.lock().unwrap().projects, "runtime_dir": display_path(&state.root)}),
    )
}
#[derive(Deserialize)]
struct CreateProject {
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    words: String,
}
async fn create_project(
    State(state): State<Shared>,
    Json(input): Json<CreateProject>,
) -> AppResult<Json<Project>> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(anyhow!("请填写项目名称（最多 80 个字符）").into());
    }
    let id = format!("{:016x}", rand::random::<u64>());
    let path = if input.path.trim().is_empty() {
        state.root.join("projects").join(&id)
    } else {
        state.root.join(input.path.trim())
    };
    std::fs::create_dir_all(&path).context("无法创建项目目录")?;
    let path = path.canonicalize()?;
    let mut inner = state.inner.lock().unwrap();
    if inner.projects.iter().any(|p| p.path == path) {
        return Err(anyhow!("此目录已在项目列表中").into());
    }
    let project = Project {
        id,
        name: name.into(),
        path,
    };
    for dir in ["word_json", "audio", "images"] {
        std::fs::create_dir_all(project.path.join(dir))?;
        checked_file(&project, dir, "check")?;
    }
    let words = if input.words.trim().is_empty() && project.path.join("index.json").exists() {
        std::fs::read_to_string(project.path.join("index.json"))?
    } else {
        input.words
    };
    add_words(&project, &words)?;
    // Verify existing card files before registering a directory.
    read_cards(&project)?;
    let mut projects = inner.projects.clone();
    projects.push(project.clone());
    save_registry(&state, &projects)?;
    inner.projects = projects;
    Ok(Json(project))
}
fn add_words(project: &Project, text: &str) -> anyhow::Result<usize> {
    let text = text.trim();
    let mut inputs = Vec::<WordData>::new();
    if text.starts_with('[') {
        let entries: Vec<WordData> =
            serde_json::from_str(text).context("导入 JSON 必须是卡片数组")?;
        let mut unit = 1u32;
        for mut data in entries {
            if data.chn.is_none() && data.example_en.is_none() {
                if let Some(number) = data.name.as_deref().and_then(|n| n.parse::<u32>().ok()) {
                    unit = number;
                    continue;
                }
            }
            if data.unit.is_none() {
                data.unit = Some(json!(unit));
            }
            inputs.push(data);
        }
    } else {
        let mut unit = 1u32;
        for line in text.lines().map(str::trim).filter(|v| !v.is_empty()) {
            if line.to_lowercase().starts_with("unit ") {
                unit = line[5..].trim().parse().context("Unit 后必须是数字")?;
                continue;
            }
            inputs.push(WordData {
                name: Some(line.into()),
                unit: Some(json!(unit)),
                ..Default::default()
            });
        }
    }
    let mut seen = HashMap::<String, String>::new();
    let existing = read_cards(project)?;
    let mut writes = Vec::new();
    for mut data in inputs {
        let name = data.name.as_deref().unwrap_or("").trim().to_string();
        if name.is_empty() || name.len() > 300 {
            bail!("单词不能为空且最多 300 字节");
        }
        let base = utils::get_safe_base_name(&name);
        component(&base)?;
        validate_media(&data)?;
        if let Some(previous) = seen.insert(base.clone(), name.clone()) {
            if previous.eq_ignore_ascii_case(&name) {
                continue;
            }
            bail!("单词文件名冲突: {previous} / {name}");
        }
        if let Some(old) = existing
            .iter()
            .find(|c| utils::get_safe_base_name(c.data.name.as_deref().unwrap_or("")) == base)
        {
            if old
                .data
                .name
                .as_deref()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case(&name)
            {
                continue;
            }
            bail!("单词文件名冲突: {name}");
        }
        let path = checked_file(project, "word_json", &format!("{base}.json"))?;
        if path.exists() {
            bail!("文件名被其他卡片占用: {base}.json");
        }
        data.name = Some(name);
        writes.push((path, data));
    }
    let count = writes.len();
    for (path, data) in writes {
        atomic_write(&path, &serde_json::to_vec_pretty(&data)?, false)?;
    }
    Ok(count)
}
#[derive(Deserialize)]
struct ImportWords {
    words: String,
}
async fn import_words(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<ImportWords>,
) -> AppResult<Json<Value>> {
    let project = project(&state, &id)?;
    let inner = state.inner.lock().unwrap();
    idle(&inner, &id)?;
    let count = add_words(&project, &input.words)?;
    Ok(Json(
        json!({"message": format!("已导入 {count} 个新单词，已有单词保持原样")}),
    ))
}
async fn get_project(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let project = project(&state, &id)?;
    let cards = read_cards(&project)?;
    let mut value = json!({"project": project, "cards": cards});
    value["project"]["path"] = json!(display_path(&project.path));
    Ok(Json(value))
}
fn validate_media(data: &WordData) -> anyhow::Result<()> {
    for name in [&data.audio, &data.audio_example, &data.image]
        .into_iter()
        .flatten()
    {
        component(name)?;
    }
    Ok(())
}
async fn save_card(
    State(state): State<Shared>,
    Path((id, card)): Path<(String, String)>,
    Json(mut data): Json<WordData>,
) -> AppResult<Json<Value>> {
    component(&card)?;
    validate_media(&data)?;
    let project = project(&state, &id)?;
    let inner = state.inner.lock().unwrap();
    idle(&inner, &id)?;
    let path = checked_file(&project, "word_json", &format!("{card}.json"))?;
    let old: WordData = serde_json::from_slice(&std::fs::read(&path)?)?;
    // Identity stays stable; changing a word is a separate import operation.
    data.name = old.name;
    if data.example_en != old.example_en {
        data.audio_example = None;
    }
    if data.image_prompt != old.image_prompt {
        data.image = None;
    }
    atomic_write(&path, &serde_json::to_vec_pretty(&data)?, false)?;
    Ok(Json(json!({"message": "卡片已保存"})))
}
#[derive(Deserialize)]
struct Generate {
    card: Option<String>,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    retry_failed: bool,
}
fn default_scope() -> String {
    "all".into()
}
async fn generate(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(mut input): Json<Generate>,
) -> AppResult<Json<Value>> {
    let project = project(&state, &id)?;
    if !matches!(input.scope.as_str(), "all" | "text" | "audio" | "image") {
        return Err(anyhow!("生成用途必须是 all、text、audio 或 image").into());
    }
    let mut cards = read_cards(&project)?;
    if input.retry_failed {
        let inner = state.inner.lock().unwrap();
        let previous = inner.jobs.get(&id).context("没有可重试的生成任务")?;
        input.scope = previous.scope.clone();
        input.force = previous.force;
        let failed = Some(&previous.failed_cards);
        cards.retain(|c| failed.is_some_and(|ids| ids.contains(&c.id)));
    }
    if let Some(card) = input.card {
        component(&card)?;
        cards.retain(|c| c.id == card);
        if cards.is_empty() {
            return Err(anyhow!("卡片不存在").into());
        }
    }
    if cards.is_empty() {
        return Err(anyhow!("项目没有单词，请创建含单词的新项目或在目录中添加 JSON").into());
    }
    let config;
    {
        let mut inner = state.inner.lock().unwrap();
        idle(&inner, &id)?;
        let mut selected = inner.config.resolved()?;
        if input.scope == "audio" && !selected.audio_enabled
            || input.scope == "image" && !selected.images_enabled
        {
            return Err(anyhow!("请先在制卡设置中启用对应媒体服务").into());
        }
        selected.audio_enabled &= matches!(input.scope.as_str(), "all" | "audio");
        selected.images_enabled &= matches!(input.scope.as_str(), "all" | "image");
        selected.validate(false)?;
        if matches!(input.scope.as_str(), "all" | "text")
            && cards.iter().any(|c| input.force || text_missing(&c.data))
        {
            require_text(&selected)?;
        }
        if selected.audio_enabled
            && cards.iter().any(|c| {
                input.force
                    || !c.media_status["audio"].as_bool().unwrap_or(false)
                    || !c.media_status["audio_example"].as_bool().unwrap_or(false)
            })
        {
            require_audio(&selected)?;
        }
        if selected.images_enabled
            && cards
                .iter()
                .any(|c| input.force || !c.media_status["image"].as_bool().unwrap_or(false))
        {
            require_image(&selected)?;
        }
        config = selected;
        // Check media paths before sending any paid requests.
        let mut media_names = HashSet::new();
        for card in &cards {
            validate_media(&card.data)?;
            let name = card.data.name.as_deref().context("卡片缺少 name")?;
            component(&utils::get_safe_base_name(name))?;
            if !media_names.insert(utils::get_safe_base_name(name)) {
                return Err(anyhow!("单词名称导致媒体文件冲突: {name}").into());
            }
            for (dir, file) in [
                (
                    "audio",
                    format!(
                        "{}.{}",
                        utils::get_safe_base_name(name),
                        utils::audio_extension(&config)
                    ),
                ),
                (
                    "audio",
                    format!(
                        "{}_example.{}",
                        utils::get_safe_base_name(name),
                        utils::audio_extension(&config)
                    ),
                ),
                ("images", format!("{}.png", utils::get_safe_base_name(name))),
            ] {
                checked_file(&project, dir, &file)?;
            }
        }
        inner.jobs.insert(
            id.clone(),
            Job {
                kind: "generate".into(),
                scope: input.scope.clone(),
                force: input.force,
                status: "running".into(),
                started_at: unix_millis(),
                phase: "generate".into(),
                total: cards.len(),
                message: "正在生成卡片；已完成内容会自动跳过".into(),
                ..Default::default()
            },
        );
    }
    tokio::spawn(async move {
        let started = Instant::now();
        stream::iter(cards)
            .for_each_concurrent(config.concurrency, |mut card| {
                let state = state.clone();
                let project = project.clone();
                let config = config.clone();
                let id = id.clone();
                let scope = input.scope.clone();
                let force = input.force;
                async move {
                    let result =
                        generate_card(&state, &project, &mut card, &config, &scope, force).await;
                    let mut inner = state.inner.lock().unwrap();
                    let job = inner.jobs.get_mut(&id).unwrap();
                    job.current += 1;
                    job.elapsed_seconds = started.elapsed().as_secs();
                    if let Err(error) = result {
                        job.failed += 1;
                        job.failed_cards.push(card.id.clone());
                        job.errors.push(format!(
                            "{}: {error:#}",
                            card.data.name.as_deref().unwrap_or(&card.id)
                        ));
                    }
                    job.message =
                        format!("已处理 {}/{}，失败 {}", job.current, job.total, job.failed);
                }
            })
            .await;
        let mut inner = state.inner.lock().unwrap();
        let job = inner.jobs.get_mut(&id).unwrap();
        job.status = if job.failed == 0 { "done" } else { "partial" }.into();
        job.elapsed_seconds = started.elapsed().as_secs();
    });
    Ok(Json(json!({"message": "生成任务已启动"})))
}
const TEXT_FIELDS: [&str; 8] = [
    "symbol",
    "chn",
    "example_en",
    "example_cn",
    "word_family",
    "collocations",
    "memory_tips",
    "image_prompt",
];
fn text_missing(data: &WordData) -> bool {
    let value = serde_json::to_value(data).unwrap();
    TEXT_FIELDS
        .iter()
        .any(|field| value[field].as_str().unwrap_or("").trim().is_empty())
}
fn require_text(config: &Settings) -> anyhow::Result<()> {
    if config.openai_api_key.trim().is_empty() || config.openai_model.trim().is_empty() {
        bail!("请配置文本服务连接、API Key 和模型");
    }
    Ok(())
}
fn require_audio(config: &Settings) -> anyhow::Result<()> {
    if config.tts_provider == "system" {
        if !cfg!(target_os = "macos") {
            bail!("系统语音仅支持 macOS");
        }
        return Ok(());
    }
    if config.tts_api_key.trim().is_empty()
        || config.tts_model.trim().is_empty()
        || config.tts_voice.trim().is_empty()
    {
        bail!("请配置语音服务连接、API Key、模型和音色");
    }
    Ok(())
}
fn require_image(config: &Settings) -> anyhow::Result<()> {
    if config.image_key().trim().is_empty() || config.image_model.trim().is_empty() {
        bail!("请配置图片服务连接、API Key 和模型");
    }
    Ok(())
}
fn merge_text(old: &WordData, generated: WordData, force: bool) -> WordData {
    let mut value = serde_json::to_value(old).unwrap();
    let generated = serde_json::to_value(generated).unwrap();
    for field in TEXT_FIELDS.into_iter().chain(["difficulty"]) {
        let empty =
            value[field].is_null() || value[field].as_str().is_some_and(|s| s.trim().is_empty());
        if (force || empty) && !generated[field].is_null() {
            value[field] = generated[field].clone();
        }
    }
    let mut merged: WordData = serde_json::from_value(value).unwrap();
    if merged.example_en != old.example_en {
        merged.audio_example = None;
    }
    if merged.image_prompt != old.image_prompt {
        merged.image = None;
    }
    merged
}
async fn generate_card(
    state: &App,
    project: &Project,
    card: &mut Card,
    config: &Settings,
    scope: &str,
    force: bool,
) -> anyhow::Result<()> {
    let path = checked_file(project, "word_json", &format!("{}.json", card.id))?;
    if matches!(scope, "all" | "text") && (force || text_missing(&card.data)) {
        let name = card.data.name.as_deref().context("缺少单词")?;
        let generated =
            utils::ai_chat_with_context(&state.client, name, config, Some(&card.data)).await?;
        card.data = merge_text(&card.data, generated, force);
        atomic_write(&path, &serde_json::to_vec_pretty(&card.data)?, false)?;
    }
    let result = utils::generate_multimedia_with_force(
        &state.client,
        &mut card.data,
        &project.path,
        config,
        force && scope != "text",
    )
    .await;
    atomic_write(&path, &serde_json::to_vec_pretty(&card.data)?, false)?;
    result?;
    Ok(())
}
#[derive(Deserialize)]
struct Export {
    format: String,
}
async fn export(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(input): Json<Export>,
) -> AppResult<Json<Value>> {
    if !matches!(input.format.as_str(), "apkg" | "md") {
        return Err(anyhow!("不支持的导出格式").into());
    }
    let project = project(&state, &id)?;
    {
        let mut inner = state.inner.lock().unwrap();
        idle(&inner, &id)?;
        let cards = read_cards(&project)?;
        if cards.is_empty() {
            return Err(anyhow!("项目没有可导出的卡片").into());
        }
        if cards
            .iter()
            .any(|c| c.data.chn.as_deref().unwrap_or("").trim().is_empty())
        {
            return Err(anyhow!("还有卡片缺少释义，请生成或编辑后再导出").into());
        }
        for card in &cards {
            validate_media(&card.data)?;
            for (dir, name) in [
                ("audio", &card.data.audio),
                ("audio", &card.data.audio_example),
                ("images", &card.data.image),
            ] {
                if let Some(name) = name {
                    checked_file(&project, dir, name)?;
                }
            }
        }
        inner.jobs.insert(
            id.clone(),
            Job {
                kind: "export".into(),
                status: "running".into(),
                started_at: unix_millis(),
                phase: "prepare".into(),
                message: "正在准备导出".into(),
                ..Default::default()
            },
        );
    }
    tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let result: anyhow::Result<PathBuf> = (|| {
            if input.format == "md" {
                let path = project.path.join("cards.md");
                let mut text =
                    "| 单词 | 音标 | 中文 | 例句 | 翻译 |\n| --- | --- | --- | --- | --- |\n"
                        .to_string();
                let escape =
                    |s: Option<&str>| s.unwrap_or("").replace('|', "\\|").replace('\n', "<br>");
                for card in read_cards(&project)? {
                    let d = card.data;
                    text.push_str(&format!(
                        "| {} | {} | {} | {} | {} |\n",
                        escape(d.name.as_deref()),
                        escape(d.symbol.as_deref()),
                        escape(d.chn.as_deref()),
                        escape(d.example_en.as_deref()),
                        escape(d.example_cn.as_deref())
                    ));
                }
                atomic_write(&path, text.as_bytes(), false)?;
                return Ok(path);
            }
            pack::handle_pack_with_progress(
                PackArgs {
                    pack_dir: project.path.clone(),
                    template: None,
                },
                |progress| {
                    let mut inner = state.inner.lock().unwrap();
                    let job = inner.jobs.get_mut(&id).unwrap();
                    job.phase = progress.phase;
                    job.current = progress.current;
                    job.total = progress.total;
                    job.media_files = progress.media_files;
                    job.written_bytes = progress.written_bytes;
                    job.total_bytes = progress.total_bytes;
                    job.elapsed_seconds = started.elapsed().as_secs();
                    job.message = match job.phase.as_str() {
                        "database" => "正在生成 Anki 数据库…".into(),
                        "media" => format!(
                            "正在写入媒体 {}/{} · {:.1}/{:.1} MB",
                            job.current,
                            job.total,
                            job.written_bytes as f64 / 1_048_576.0,
                            job.total_bytes as f64 / 1_048_576.0
                        ),
                        "finishing" => "正在完成文件写入…".into(),
                        _ => format!(
                            "已处理 {}/{} 张卡片，{} 个媒体文件",
                            job.current, job.total, job.media_files
                        ),
                    };
                },
            )?;
            let name = project
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("UnnamedDeck");
            Ok(project.path.join(format!("{name}_Deck.apkg")))
        })();
        let mut inner = state.inner.lock().unwrap();
        let job = inner.jobs.get_mut(&id).unwrap();
        job.elapsed_seconds = started.elapsed().as_secs();
        match result {
            Ok(path) => {
                job.status = "done".into();
                job.phase = "done".into();
                job.output_path = Some(display_path(&path));
                job.message = "导出成功，文件已保存在本地目录".into();
            }
            Err(error) => {
                job.status = "failed".into();
                job.message = format!("导出失败: {error:#}");
            }
        }
    });
    Ok(Json(json!({"message": "导出任务已启动"})))
}
fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
async fn job(State(state): State<Shared>, Path(id): Path<String>) -> AppResult<Json<Option<Job>>> {
    project(&state, &id)?;
    let mut job = state.inner.lock().unwrap().jobs.get(&id).cloned();
    if let Some(job) = job.as_mut().filter(|j| j.status == "running") {
        job.elapsed_seconds = unix_millis().saturating_sub(job.started_at) / 1000;
    }
    Ok(Json(job))
}
async fn stream_file(path: PathBuf, mime: &'static str, attachment: bool) -> AppResult<Response> {
    let file = tokio::fs::File::open(&path)
        .await
        .context("文件尚未生成或已经移走")?;
    let length = file.metadata().await?.len();
    let mut response = Body::from_stream(ReaderStream::new(file)).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, mime.parse().unwrap());
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, length.into());
    if attachment {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, "attachment".parse().unwrap());
    }
    Ok(response)
}
async fn media(
    State(state): State<Shared>,
    Path((id, dir, name)): Path<(String, String, String)>,
) -> AppResult<Response> {
    if !matches!(dir.as_str(), "audio" | "images") {
        return Err(anyhow!("无效的媒体目录").into());
    }
    let project = project(&state, &id)?;
    let path = checked_file(&project, &dir, &name)?;
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => return Err(anyhow!("不支持的媒体格式").into()),
    };
    stream_file(path, mime, false).await
}
async fn download(State(state): State<Shared>, Path(id): Path<String>) -> AppResult<Response> {
    let project = project(&state, &id)?;
    let output = state
        .inner
        .lock()
        .unwrap()
        .jobs
        .get(&id)
        .filter(|j| j.status == "done")
        .and_then(|j| j.output_path.clone())
        .context("还没有完成的导出")?;
    let path = PathBuf::from(output).canonicalize()?;
    if !path.starts_with(&project.path) {
        return Err(anyhow!("文件不在项目目录内").into());
    }
    stream_file(path, "application/octet-stream", true).await
}
async fn open_folder(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let project = project(&state, &id)?;
    tokio::task::spawn_blocking(move || launch_folder(&project.path)).await??;
    Ok(Json(
        json!({"message": "已打开项目文件夹，请在 Anki 中选择「文件 → 导入」并选取卡片包"}),
    ))
}
fn open_browser(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}
/// Windows canonical paths use a verbatim prefix; Explorer and copied paths need
/// the ordinary drive/UNC representation, while file IO retains canonical paths.
fn display_path(path: &FsPath) -> String {
    let path = path.to_string_lossy();
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_string()
    }
}
fn launch_folder(path: &FsPath) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer.exe")
            .arg(path)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    fn app() -> (tempfile::TempDir, Shared) {
        let temp = tempfile::tempdir().unwrap();
        let app = Arc::new(App {
            tests_dir: tempfile::tempdir_in(temp.path()).unwrap(),
            root: temp.path().to_path_buf(),
            token: "test-token".into(),
            origin: "http://127.0.0.1:8765".into(),
            client: reqwest::Client::new(),
            inner: Mutex::new(Inner {
                config: Settings::default(),
                projects: vec![],
                jobs: HashMap::new(),
            }),
        });
        (temp, app)
    }
    async fn request(app: Shared, method: &str, path: &str, data: Value) -> (StatusCode, Value) {
        let response = router(app)
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", "127.0.0.1:8765")
                    .header("x-helper-token", "test-token")
                    .header("content-type", "application/json")
                    .body(Body::from(data.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }
    async fn wait_job(app: Shared, id: &str) -> Job {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let job = app.inner.lock().unwrap().jobs.get(id).cloned().unwrap();
                if job.status != "running" {
                    return job;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[test]
    fn shows_windows_paths_in_explorer_format() {
        assert_eq!(
            display_path(FsPath::new(r"\\?\C:\Anki\cards.apkg")),
            r"C:\Anki\cards.apkg"
        );
        assert_eq!(
            display_path(FsPath::new(r"\\?\UNC\server\share\cards.apkg")),
            r"\\server\share\cards.apkg"
        );
    }
    #[tokio::test]
    async fn rejects_other_hosts_tokens_and_origins() {
        let (_temp, app) = app();
        for (host, token, origin) in [
            (
                "attacker.example:8765",
                "test-token",
                "http://127.0.0.1:8765",
            ),
            ("127.0.0.1:8765", "wrong", "http://127.0.0.1:8765"),
            ("127.0.0.1:8765", "test-token", "https://attacker.example"),
        ] {
            let response = router(app.clone())
                .oneshot(
                    Request::builder()
                        .uri("/api/config")
                        .header("host", host)
                        .header("x-helper-token", token)
                        .header("origin", origin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    #[tokio::test]
    async fn config_masks_keys_retains_blanks_and_can_clear() {
        let (_temp, app) = app();
        let mut config = serde_json::to_value(Settings::default()).unwrap();
        config["openai_api_key"] = json!("secret");
        assert_eq!(
            request(app.clone(), "PUT", "/api/config", config).await.0,
            StatusCode::OK
        );
        let (_, mut masked) = request(app.clone(), "GET", "/api/config", Value::Null).await;
        assert_eq!(masked["openai_api_key"], "");
        assert_eq!(masked["has_openai_api_key"], true);
        request(app.clone(), "PUT", "/api/config", masked.clone()).await;
        assert_eq!(app.inner.lock().unwrap().config.openai_api_key, "secret");
        masked["clear_connections"] = json!(["legacy-text"]);
        request(app.clone(), "PUT", "/api/config", masked).await;
        assert_eq!(app.inner.lock().unwrap().config.openai_api_key, "");
    }
    #[tokio::test]
    async fn connection_keys_are_bound_to_identity_protocol_and_endpoints() {
        let (_temp, app) = app();
        {
            let mut inner = app.inner.lock().unwrap();
            inner.config.openai_api_key = "ark-secret".into();
        }
        let (_, mut masked) = request(app.clone(), "GET", "/api/config", Value::Null).await;
        assert_eq!(masked["connections"][0]["api_key"], "");
        assert_eq!(masked["connections"][0]["has_api_key"], true);
        masked["connections"][0]["name"] = json!("Renamed connection");
        request(app.clone(), "PUT", "/api/config", masked.clone()).await;
        assert_eq!(
            app.inner.lock().unwrap().config.openai_api_key,
            "ark-secret"
        );
        masked["connections"][0]["text_url"] = json!("https://different.example/chat/completions");
        request(app.clone(), "PUT", "/api/config", masked).await;
        let inner = app.inner.lock().unwrap();
        assert_eq!(inner.config.openai_api_key, "");
        assert_eq!(inner.config.image_key(), "");
        assert_eq!(inner.config.vision_api_key, "");
    }

    #[test]
    fn completion_preserves_edits_identity_and_media() {
        let old: WordData = serde_json::from_value(json!({"name":"apple", "chn":"手写释义", "example_en":"My example.",
            "memory_tips":"My memory tip", "audio":"apple.mp3", "audio_example":"example.mp3", "image":"apple.png",
            "image_prompt":"Original prompt", "unit":2, "custom_field":"keep"})).unwrap();
        let generated: WordData = serde_json::from_value(json!({"name":"wrong", "chn":"替换释义", "example_en":"Another example.",
            "symbol":"/æpl/", "example_cn":"我的例句。", "memory_tips":"New tip", "image_prompt":"New prompt"})).unwrap();
        let merged = merge_text(&old, generated.clone(), false);
        assert_eq!(merged.name, old.name);
        assert_eq!(merged.chn, old.chn);
        assert_eq!(merged.example_en, old.example_en);
        assert_eq!(merged.memory_tips, old.memory_tips);
        assert_eq!(merged.audio_example, old.audio_example);
        assert_eq!(merged.image, old.image);
        assert_eq!(merged.symbol.as_deref(), Some("/æpl/"));
        assert_eq!(merged.extra["custom_field"], "keep");
        let regenerated = merge_text(&old, generated, true);
        assert_eq!(regenerated.name, old.name);
        assert_eq!(regenerated.unit, old.unit);
        assert_eq!(regenerated.audio, old.audio);
        assert!(regenerated.audio_example.is_none());
        assert!(regenerated.image.is_none());
    }

    #[tokio::test]
    async fn photos_use_vision_model_and_return_reviewable_sanitized_words() {
        use base64::Engine;
        let (_temp, app) = app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v3", listener.local_addr().unwrap());
        let mock = Router::new().route("/api/v3/chat/completions", post(|headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers["authorization"], "Bearer vision-key");
            assert_eq!(body["model"], "vision-only-model");
            assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
            assert!(body["messages"][1]["content"][0]["text"].as_str().unwrap().contains("单词表"));
            Json(json!({"choices":[{"message":{"content":"```json\n{\"words\":[{\"name\":\"apple\",\"chn\":\"苹果\",\"unit\":2,\"audio\":\"evil.mp3\",\"extra\":\"discard\"}]}\n```"}}]}))
        })).layer(DefaultBodyLimit::max(20 * 1024 * 1024));
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        {
            let mut inner = app.inner.lock().unwrap();
            inner.config.migrate_connections();
            inner.config.connections[0].base_url = base.clone();
            inner.config.connections[0].text_url = format!("{base}/chat/completions");
            inner.config.connections[0].api_key = "vision-key".into();
            inner.config.vision_model = "vision-only-model".into();
        }
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(1_600_000, 0);
        let photo = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        let (status, result) = request(
            app.clone(),
            "POST",
            "/api/extract-words",
            json!({"images":[photo]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        assert_eq!(result["words"][0]["name"], "apple");
        assert_eq!(result["words"][0]["unit"], 2);
        assert!(result["words"][0]["audio"].is_null());
        assert!(result["words"][0].get("extra").is_none());
        assert!(app.inner.lock().unwrap().projects.is_empty());
        let (status, _) = request(
            app,
            "POST",
            "/api/extract-words",
            json!({"images":["data:image/png;base64,c2VjcmV0"]}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        server.abort();
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "Requires the macOS system speech service"]
    async fn system_audio_generates_and_exports_without_cloud_credentials() {
        let (_temp, app) = app();
        {
            let mut inner = app.inner.lock().unwrap();
            inner.config.audio_backend = "system".into();
            inner.config.audio_enabled = true;
        }
        let (_, created) = request(
            app.clone(),
            "POST",
            "/api/projects",
            json!({"name":"Native speech", "words":
            json!([{"name":"apple", "example_en":"An apple."}]).to_string()}),
        )
        .await;
        let id = created["id"].as_str().unwrap();
        let (status, result) = request(
            app.clone(),
            "POST",
            &format!("/api/projects/{id}/generate"),
            json!({"scope":"audio"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let job = wait_job(app.clone(), id).await;
        assert_eq!(job.status, "done", "{:?}", job.errors);
        let (_, value) = request(
            app.clone(),
            "GET",
            &format!("/api/projects/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(value["cards"][0]["data"]["audio"], "apple.wav");
        assert_eq!(value["cards"][0]["media_status"]["audio"], true);
        request(
            app.clone(),
            "POST",
            &format!("/api/projects/{id}/export"),
            json!({"format":"apkg"}),
        )
        .await;
        assert_eq!(wait_job(app.clone(), id).await.status, "done");
    }

    #[tokio::test]
    async fn directory_workflow_edit_export_and_reopen() {
        let (_temp, app) = app();
        let data_dir = tempfile::tempdir().unwrap();
        let path = data_dir.path().join("本地 卡片");
        let words = json!([{ "name":"apple", "chn":"苹果", "example_en":"An apple.", "unit":1, "custom_note":"preserve me" }])
            .to_string();
        let (status, value) = request(
            app.clone(),
            "POST",
            "/api/projects",
            json!({"name":"英语", "path":path, "words":words}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{value}");
        let id = value["id"].as_str().unwrap();
        assert!(path.join("word_json/apple.json").exists());
        assert!(app.root.join("projects.json").exists());
        let (status, imported) = request(
            app.clone(),
            "POST",
            &format!("/api/projects/{id}/import"),
            json!({"words":"apple\nbanana"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{imported}");
        assert!(path.join("word_json/banana.json").exists());
        // Complete the appended placeholder so the following export remains valid.
        request(
            app.clone(),
            "PUT",
            &format!("/api/projects/{id}/cards/banana"),
            json!({"name":"banana","chn":"香蕉","example_en":"A banana."}),
        )
        .await;
        let (_, project) = request(
            app.clone(),
            "GET",
            &format!("/api/projects/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(project["cards"][0]["data"]["custom_note"], "preserve me");
        let mut card = project["cards"][0]["data"].clone();
        card["chn"] = json!("编辑的释义");
        assert_eq!(
            request(
                app.clone(),
                "PUT",
                &format!("/api/projects/{id}/cards/apple"),
                card.clone()
            )
            .await
            .0,
            StatusCode::OK
        );
        card["audio"] = json!("../../secret");
        assert_eq!(
            request(
                app.clone(),
                "PUT",
                &format!("/api/projects/{id}/cards/apple"),
                card
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(
                app.clone(),
                "POST",
                &format!("/api/projects/{id}/export"),
                json!({"format":"apkg"})
            )
            .await
            .0,
            StatusCode::OK
        );
        let result = wait_job(app.clone(), id).await;
        assert_eq!(result.status, "done", "{}", result.message);
        assert!(FsPath::new(result.output_path.as_ref().unwrap()).is_file());
        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(result.output_path.unwrap()).unwrap())
                .unwrap();
        assert!(archive.by_name("collection.anki2").is_ok());
        let edited: Value =
            serde_json::from_slice(&std::fs::read(path.join("word_json/apple.json")).unwrap())
                .unwrap();
        assert_eq!(edited["custom_note"], "preserve me");
        let restored: Vec<Project> =
            serde_json::from_slice(&std::fs::read(app.root.join("projects.json")).unwrap())
                .unwrap();
        assert_eq!(restored[0].path, path.canonicalize().unwrap());
        assert_eq!(
            read_cards(&restored[0]).unwrap()[0].data.chn.as_deref(),
            Some("编辑的释义")
        );
    }
    #[tokio::test]
    async fn cannot_edit_during_generation_and_missing_config_is_actionable() {
        let (_temp, app) = app();
        let (_, value) = request(
            app.clone(),
            "POST",
            "/api/projects",
            json!({"name":"Test", "words":"apple"}),
        )
        .await;
        let id = value["id"].as_str().unwrap();
        let (status, error) = request(
            app.clone(),
            "POST",
            &format!("/api/projects/{id}/generate"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"].as_str().unwrap().contains("API Key"));
        app.inner.lock().unwrap().jobs.insert(
            id.into(),
            Job {
                status: "running".into(),
                started_at: unix_millis(),
                ..Default::default()
            },
        );
        assert_eq!(
            request(
                app.clone(),
                "PUT",
                &format!("/api/projects/{id}/cards/apple"),
                json!({"name":"apple","chn":"苹果"})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    #[cfg(unix)]
    #[test]
    fn rejects_links_to_files_outside_project() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("audio")).unwrap();
        std::fs::write(outside.path().join("secret.mp3"), b"secret").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.mp3"),
            temp.path().join("audio/link.mp3"),
        )
        .unwrap();
        let project = Project {
            id: "test".into(),
            name: "test".into(),
            path: temp.path().into(),
        };
        assert!(checked_file(&project, "audio", "link.mp3").is_err());
        assert!(checked_file(&project, "audio", "../secret.mp3").is_err());
    }
    #[tokio::test]
    async fn tests_unsaved_models_and_doubao_protocol_without_overwriting_config() {
        let (_temp, app) = app();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let image_base = base.clone();
        let mock = Router::new().route("/api/v3/chat/completions", post(|headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers["authorization"], "Bearer test-ark-key");
            if body["model"] == "vision-no-images" {
                return Json(json!({"choices":[{"message":{"content":"{\"words\":[{\"name\":\"banana\"}]}"}}]}));
            }
            if body["model"] == "doubao-seed-2-1-lite-260915" {
                assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
                return Json(json!({"choices":[{"message":{"content":"{\"words\":[{\"name\":\"APPLE\"}]}"}}]}));
            }
            assert_eq!(body["model"], "deepseek-v4-1-flash-260910");
            assert_eq!(body["thinking"]["type"], "disabled");
            Json(json!({"choices":[{"message":{"content":json!({"name":"apple","chn":"苹果","example_en":"An apple."}).to_string()}}]}))
        })).route("/tts", post(|headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers["x-api-key"], "test-speech-key");
            assert_eq!(headers["x-api-resource-id"], "seed-tts-2.0");
            assert_eq!(headers["x-api-request-id"].to_str().unwrap().len(), 36);
            assert_eq!(body["req_params"]["speaker"], "custom-speaker");
            assert_eq!(body["req_params"]["audio_params"]["format"], "mp3");
            // Split both an NDJSON line and its base64 across HTTP chunks.
            let chunks = ["{\"code\":0,\"data\":\"b", "XAz\"}\n{\"code\":0,\"data\":\"YWJj\"}\n"];
            Body::from_stream(stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>)))
        })).route("/images", post(move |headers: axum::http::HeaderMap, Json(body): Json<Value>| {
            let base = image_base.clone(); async move {
                assert_eq!(headers["authorization"], "Bearer test-ark-key");
                assert_eq!(body["model"], "doubao-seedream-5-0-pro-260628");
                Json(json!({"data":[{"url":format!("{base}/image.png")}]}))
            }
        })).route("/image.png", get(|| async { b"image-bytes".as_slice() }));
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        let mut config = Settings::default();
        config.openai_base_url = format!("{base}/api/v3");
        config.openai_api_key = "test-ark-key".into();
        config.tts_url = format!("{base}/tts");
        config.tts_api_key = "test-speech-key".into();
        config.tts_voice = "custom-speaker".into();
        config.image_url = format!("{base}/images");
        config.image_model = "doubao-seedream-5-0-pro-260628".into();
        config.migrate_connections();
        for kind in ["text", "vision", "audio", "image"] {
            let (status, result) = request(
                app.clone(),
                "POST",
                "/api/test",
                json!({"kind":kind, "config":config}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{result}");
            assert_eq!(result["status"], "success");
            assert!(result["connection"].is_string());
            if kind == "vision" {
                assert_eq!(result["words"][0]["name"], "APPLE");
            } else if kind == "text" {
                assert_eq!(result["card"]["chn"], "苹果");
            } else {
                let path = result["asset_url"].as_str().unwrap();
                let name = path.rsplit('/').next().unwrap();
                let bytes = std::fs::read(app.tests_dir.path().join(name)).unwrap();
                assert_eq!(
                    bytes,
                    if kind == "audio" {
                        b"mp3abc".as_slice()
                    } else {
                        b"image-bytes".as_slice()
                    }
                );
            }
        }
        let mut unsupported = config.clone();
        unsupported.vision_model = "vision-no-images".into();
        let (status, result) = request(
            app.clone(),
            "POST",
            "/api/test",
            json!({"kind":"vision", "config":unsupported}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(result["error"].as_str().unwrap().contains("未正确识别"));
        assert!(app.inner.lock().unwrap().config.openai_api_key.is_empty());
        assert!(!app.root.join("config.toml").exists());
        server.abort();
    }

    #[tokio::test]
    async fn retries_media_without_regenerating_paid_text_or_audio() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (_temp, app) = app();
        let chat_count = Arc::new(AtomicUsize::new(0));
        let audio_count = Arc::new(AtomicUsize::new(0));
        let image_count = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let chat_counter = chat_count.clone();
        let audio_counter = audio_count.clone();
        let image_counter = image_count.clone();
        let image_base = base.clone();
        let mock = Router::new().route("/v1/chat/completions", post(move || { let count = chat_counter.clone(); async move {
            count.fetch_add(1, Ordering::SeqCst);
            Json(json!({"choices":[{"message":{"content":json!({"name":"wrong","symbol":"/æpl/","chn":"苹果","example_en":"An apple.","example_cn":"一个苹果。",
                "word_family":"apples","collocations":"an apple","memory_tips":"apple","image_prompt":"An apple"}).to_string()}}]}))
        }})).route("/tts", post(move || { let count = audio_counter.clone(); async move { count.fetch_add(1, Ordering::SeqCst); b"test-mp3".as_slice() } }))
        .route("/images", post(move || { let count = image_counter.clone(); let base = image_base.clone(); async move {
            if count.fetch_add(1, Ordering::SeqCst) == 0 { (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":"retry"}))) }
            else { (StatusCode::OK, Json(json!({"data":[{"url":format!("{base}/image.png")}]}))) }
        }})).route("/image.png", get(|| async { b"test-png".as_slice() }));
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        {
            let mut inner = app.inner.lock().unwrap();
            let config = &mut inner.config;
            config.openai_base_url = base.clone();
            config.openai_api_key = "mock".into();
            config.audio_enabled = true;
            config.tts_provider = "openai".into();
            config.tts_url = format!("{base}/tts");
            config.tts_api_key = "mock".into();
            config.images_enabled = true;
            config.image_url = format!("{base}/images");
            config.image_api_key = "mock".into();
        }
        let (_, created) = request(
            app.clone(),
            "POST",
            "/api/projects",
            json!({"name":"Test", "words":"apple"}),
        )
        .await;
        let id = created["id"].as_str().unwrap();
        assert_eq!(
            request(
                app.clone(),
                "POST",
                &format!("/api/projects/{id}/generate"),
                json!({})
            )
            .await
            .0,
            StatusCode::OK
        );
        let first = wait_job(app.clone(), id).await;
        assert_eq!(first.status, "partial");
        assert_eq!(first.failed, 1);
        assert_eq!(
            request(
                app.clone(),
                "POST",
                &format!("/api/projects/{id}/generate"),
                json!({})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(wait_job(app.clone(), id).await.status, "done");
        assert_eq!(chat_count.load(Ordering::SeqCst), 1);
        assert_eq!(audio_count.load(Ordering::SeqCst), 2);
        assert_eq!(image_count.load(Ordering::SeqCst), 2);
        let project = project(&app, id).unwrap();
        assert_eq!(
            read_cards(&project).unwrap()[0].data.name.as_deref(),
            Some("apple")
        );
        assert!(project.path.join("images/apple.png").is_file());
        server.abort();
    }
}
