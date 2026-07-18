// src/modules/init.rs

mod config;
mod utils;

use crate::cli::InitArgs;
use crate::models::WordData;
use anyhow::{anyhow, Context, Result};
// <<< 引入并发处理所需要的包
use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::path::{Path};
use tokio::fs;

// <<< 定义一个常量来控制并发请求的数量，避免对API造成过大压力
const CONCURRENT_REQUESTS: usize = 15;

/// `init` 命令的主处理函数
pub async fn handle_init_command(args: InitArgs) -> Result<()> {
    let output_dir = args.output_dir;
    let template_path = args.template.unwrap_or_else(|| output_dir.join("index.json"));

    if !template_path.exists() {
        return Err(anyhow!("错误: 索引文件不存在: {}", template_path.display()));
    }
    
    utils::ensure_directories(&output_dir).await?;
    println!("目录结构已确认/创建于: {}", output_dir.display());

    let client = Client::new();

    let extension = template_path.extension().and_then(|s| s.to_str()).unwrap_or("");
    match extension {
        "json" => init_from_json(&client, &template_path, &output_dir).await?,
        "txt" => init_from_txt(&client, &template_path, &output_dir).await?,
        _ => return Err(anyhow!("不支持的文件类型: {}. 请提供 .json 或 .txt 文件。", extension)),
    }

    println!("\n===== 开始处理多媒体文件生成 =====");
    init_multimedia(&client, &output_dir).await?;
    println!("所有多媒体任务处理完毕。");

    Ok(())
}

#[derive(Deserialize, Debug, Clone)] // <<< 添加 Clone
struct IndexEntry {
    name: String,
    chn: Option<String>,
    ext: Option<String>,
}

// <<<  重构为并发执行
async fn init_from_json(client: &Client, template_path: &Path, output_dir: &Path) -> Result<()> {
    let content = fs::read_to_string(template_path).await?;
    let entries: Vec<IndexEntry> = serde_json::from_str(&content)?;
    
    let json_dir = output_dir.join(config::JSON_DIR);
    let mut current_unit = 1;

    // 1. 将所有任务收集到一个 Vec 中
    let mut tasks = vec![];
    for entry in entries {
        if let Ok(unit_num) = entry.name.parse::<i32>() {
            if entry.chn.is_none() && entry.ext.is_none() {
                current_unit = unit_num;
                println!("\n切换到 Unit {}", current_unit);
                continue;
            }
        }
        
        let file_path = json_dir.join(format!("{}.json", utils::get_safe_base_name(&entry.name)));
        if file_path.exists() {
            println!("已跳过 (已存在): {}", entry.name);
            continue;
        }
        tasks.push((entry, current_unit));
    }

    // 2. 将任务转换为一个异步流，并进行并发处理
    stream::iter(tasks)
        .map(|(entry, unit)| {
            // 为每个任务克隆所需的数据
            let client = client.clone();
            let entry_name = entry.name.clone();
            let json_dir = json_dir.clone();
            
            // 创建一个异步任务块
            async move {
                println!("正在通过 AI 生成: {}", entry_name);
                match utils::ai_chat(&client, &entry_name).await {
                    Ok(mut word_data) => {
                        // 在任务内部处理结果
                        word_data.unit = Some(serde_json::Value::from(unit));
                        if let Some(chn) = entry.chn { word_data.chn = Some(chn); }
                        if let Some(ext) = entry.ext {
                            let original_tips = word_data.memory_tips.unwrap_or_default();
                            word_data.memory_tips = Some(format!("{}\n{}", ext, original_tips));
                        }
                        
                        let file_path = json_dir.join(format!("{}.json", utils::get_safe_base_name(&entry_name)));
                        let json_string = serde_json::to_string_pretty(&word_data)
                            .with_context(|| format!("序列化 '{}' 失败", entry_name))?;
                        fs::write(&file_path, &json_string).await
                            .with_context(|| format!("写入文件失败: {}", file_path.display()))?;
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("为 '{}' 生成数据时出错: {}", entry_name, e);
                        Err(e)
                    }
                }
            }
        })
        .buffer_unordered(CONCURRENT_REQUESTS) // <<< 核心：并发执行，最多同时运行 N 个任务
        .collect::<Vec<Result<()>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    Ok(())
}


// <<< 重构为并发执行
async fn init_from_txt(client: &Client, template_path: &Path, output_dir: &Path) -> Result<()> {
    let content = fs::read_to_string(template_path).await?;
    let lines: Vec<String> = content.lines().map(String::from).collect();
    
    let json_dir = output_dir.join(config::JSON_DIR);
    let mut current_unit = 1;

    let mut tasks = vec![];
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        if trimmed.to_lowercase().starts_with("unit") {
             if let Some(num_str) = trimmed.split_whitespace().nth(1) {
                if let Ok(num) = num_str.parse::<i32>() {
                    current_unit = num;
                    println!("\n切换到 Unit {}", current_unit);
                    continue;
                }
             }
        }

        let file_path = json_dir.join(format!("{}.json", utils::get_safe_base_name(trimmed)));
        if file_path.exists() {
            println!("已跳过 (已存在): {}", trimmed);
            continue;
        }
        tasks.push((trimmed.to_string(), current_unit));
    }

    stream::iter(tasks)
        .map(|(word, unit)| {
            let client = client.clone();
            let json_dir = json_dir.clone();
            async move {
                println!("正在通过 AI 生成: {}", word);
                match utils::ai_chat(&client, &word).await {
                    Ok(mut word_data) => {
                        word_data.unit = Some(serde_json::Value::from(unit));
                        let file_path = json_dir.join(format!("{}.json", utils::get_safe_base_name(&word)));
                        let json_string = serde_json::to_string_pretty(&word_data)
                            .with_context(|| format!("序列化 '{}' 失败", word))?;
                        fs::write(&file_path, &json_string).await
                            .with_context(|| format!("写入文件失败: {}", file_path.display()))?;
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("为 '{}' 生成数据时出错: {}", word, e);
                        Err(e)
                    }
                }
            }
        })
        .buffer_unordered(CONCURRENT_REQUESTS)
        .collect::<Vec<Result<()>>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    Ok(())
}

// <<< 重构为并发执行
async fn init_multimedia(client: &Client, output_dir: &Path) -> Result<()> {
    let json_dir = output_dir.join(config::JSON_DIR);
    let mut read_dir = fs::read_dir(&json_dir).await?;
    let mut paths_to_process = Vec::new();

    // 1. 先收集所有需要处理的文件路径
    while let Some(entry) = read_dir.next_entry().await? {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
            paths_to_process.push(path);
        }
    }

    // 2. 并发处理这些文件
    let results = stream::iter(paths_to_process)
        .map(|path| {
            let client = client.clone();
            let output_dir = output_dir.to_path_buf();
            async move {
                let content = fs::read_to_string(&path).await?;
                let mut word_data: WordData = serde_json::from_str(&content)?;

                let needs_update = utils::generate_multimedia_for_word(&client, &mut word_data, &output_dir).await?;

                if needs_update {
                    let updated_content = serde_json::to_string_pretty(&word_data)?;
                    fs::write(&path, updated_content).await?;
                    println!("更新了JSON文件: {}", path.display());
                }

                Ok(())
            }
        })
        .buffer_unordered(CONCURRENT_REQUESTS)
        .collect::<Vec<Result<()>>>()
        .await;

    results.into_iter().collect::<Result<Vec<_>>>()?;

    Ok(())
}