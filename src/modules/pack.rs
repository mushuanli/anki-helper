// src/modules/pack.rs

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use genanki_rs::{Deck, Field, Model, Note, Package, Template};
use rand::Rng;
use serde_json::Value;

// 导入在项目其他文件中定义的结构
use crate::cli::PackArgs;
use crate::models::{AnkiModelTemplate, ReciteData, WordData};

/// “pack” 子命令的公共处理函数
///
/// 这是此模块的入口点，由 main.rs 调用。
#[derive(Clone, Debug, serde::Serialize)]
pub struct PackProgress {
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub media_files: usize,
    pub written_bytes: u64,
    pub total_bytes: u64,
}

pub fn handle_pack_command(args: PackArgs) -> Result<()> {
    handle_pack_with_progress(args, |_| {})
}

pub fn handle_pack_with_progress(args: PackArgs, progress: impl Fn(PackProgress)) -> Result<()> {
    let base_dir = args
        .pack_dir
        .canonicalize()
        .with_context(|| format!("错误: 无法解析目录路径 {}", args.pack_dir.display()))?;
    let deck_name = base_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("UnnamedDeck");

    // 配置路径
    let word_json_dir = base_dir.join("word_json");
    let recite_json_dir = base_dir.join("recite_json");
    let audio_dir = base_dir.join("audio");
    let images_dir = base_dir.join("images");
    let output_file = base_dir.join(format!("{}_Deck.apkg", deck_name));

    // 确保媒体目录存在 (如果不存在则创建)
    for dir in [&audio_dir, &images_dir] {
        if !dir.exists() {
            println!("创建目录: {}", dir.display());
            fs::create_dir_all(dir)?;
        }
    }

    // 加载模板
    let model_template = match &args.template {
        Some(path) => load_model_template(path)?,
        None => serde_json::from_str(include_str!("../../template/ankimodel.json"))?,
    };
    let model = create_genanki_model(model_template);

    // 创建牌组
    let id_path = base_dir.join(".anki-deck-id");
    let deck_id: i64 = if id_path.exists() {
        fs::read_to_string(&id_path)?
            .trim()
            .parse()
            .context("牌组 ID 文件无效")?
    } else {
        let id = rand::thread_rng().gen_range(1..(1i64 << 53));
        crate::settings::atomic_write(&id_path, id.to_string().as_bytes(), false)?;
        id
    };
    let mut deck = Deck::new(deck_id, &format!("{} 综合学习", deck_name), "");

    // 媒体文件集合和笔记计数器
    let mut media_files: HashSet<PathBuf> = HashSet::new();
    let mut note_count = 0;

    let total = [&word_json_dir, &recite_json_dir]
        .iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count();
    let report = |current, media_files| {
        progress(PackProgress {
            phase: "cards".into(),
            current,
            total,
            media_files,
            written_bytes: 0,
            total_bytes: 0,
        })
    };
    report(0, 0);
    // 处理文件
    process_word_files(
        &model,
        &word_json_dir,
        (&audio_dir, &images_dir),
        &mut deck,
        &mut media_files,
        &mut note_count,
        &report,
        deck_id,
    )?;
    process_recite_files(
        &model,
        &recite_json_dir,
        (&audio_dir, &images_dir),
        &mut deck,
        &mut media_files,
        &mut note_count,
        &report,
        deck_id,
    )?;

    // 检查是否有笔记被添加
    if note_count == 0 {
        return Err(anyhow!("错误: 没有找到可用的笔记，请检查JSON文件"));
    }
    println!("正在生成Anki包，包含 {} 张卡片...", note_count);

    // Build the collection separately, then stream media into the final ZIP.
    // MP3 and images are already compressed, so avoid expensive recompression.
    let mut media: Vec<PathBuf> = media_files.into_iter().collect();
    media.sort();
    let total_bytes = media.iter().try_fold(0u64, |sum, path| {
        fs::metadata(path).map(|metadata| sum + metadata.len())
    })?;
    progress(PackProgress {
        phase: "database".into(),
        current: note_count,
        total,
        media_files: media.len(),
        written_bytes: 0,
        total_bytes,
    });
    let mut collection = tempfile::NamedTempFile::new_in(&base_dir)?;
    Package::new(vec![deck], vec![])?.write(collection.as_file_mut())?;
    let mut source = zip::ZipArchive::new(collection.reopen()?)?;
    let mut output = tempfile::NamedTempFile::new_in(&base_dir)?;
    let mut archive = zip::ZipWriter::new(output.as_file_mut());
    archive.start_file(
        "collection.anki2",
        zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(1)),
    )?;
    std::io::copy(&mut source.by_name("collection.anki2")?, &mut archive)?;
    let media_map: std::collections::BTreeMap<String, String> = media
        .iter()
        .enumerate()
        .map(|(index, path)| {
            (
                index.to_string(),
                path.file_name().unwrap().to_string_lossy().into_owned(),
            )
        })
        .collect();
    archive.start_file(
        "media",
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )?;
    archive.write_all(&serde_json::to_vec(&media_map)?)?;
    let mut written_bytes = 0;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut last_report = std::time::Instant::now();
    for (index, path) in media.iter().enumerate() {
        archive.start_file(
            index.to_string(),
            zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)
                .large_file(true),
        )?;
        let mut input = File::open(path)?;
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            archive.write_all(&buffer[..count])?;
            written_bytes += count as u64;
            if last_report.elapsed() >= std::time::Duration::from_millis(100) {
                progress(PackProgress {
                    phase: "media".into(),
                    current: index,
                    total: media.len(),
                    media_files: media.len(),
                    written_bytes,
                    total_bytes,
                });
                last_report = std::time::Instant::now();
            }
        }
        progress(PackProgress {
            phase: "media".into(),
            current: index + 1,
            total: media.len(),
            media_files: media.len(),
            written_bytes,
            total_bytes,
        });
    }
    progress(PackProgress {
        phase: "finishing".into(),
        current: media.len(),
        total: media.len(),
        media_files: media.len(),
        written_bytes,
        total_bytes,
    });
    archive.finish()?;
    drop(archive);
    output.as_file().sync_all()?;
    output.persist(&output_file).map_err(|e| e.error)?;
    progress(PackProgress {
        phase: "done".into(),
        current: media.len(),
        total: media.len(),
        media_files: media.len(),
        written_bytes,
        total_bytes,
    });
    println!("成功生成Anki包: {}", output_file.display());

    Ok(())
}

// --- 以下是所有的辅助函数，它们是此模块的私有实现细节 ---

/// 加载并解析 ankimodel.json 模板
fn load_model_template(path: &Path) -> Result<AnkiModelTemplate> {
    let file =
        File::open(path).with_context(|| format!("错误: 无法打开模板文件 {}", path.display()))?;
    let reader = BufReader::new(file);
    let template: AnkiModelTemplate = serde_json::from_reader(reader)
        .with_context(|| format!("错误: 模板文件JSON解析失败 {}", path.display()))?;
    Ok(template)
}

/// 根据模板创建 genanki-rs 模型
fn create_genanki_model(template: AnkiModelTemplate) -> Model {
    let fields = template
        .flds
        .into_iter()
        .map(|f| Field::new(&f.name))
        .collect();
    let templates = template
        .tmpls
        .into_iter()
        .map(|t| Template::new(&t.name).qfmt(&t.qfmt).afmt(&t.afmt))
        .collect();
    let is_cloze = template.model_type.unwrap_or(0) == 1;
    let css = template.css;

    Model::new_with_options(
        template.id,
        &template.name,
        fields,
        templates,
        css.as_deref(),
        None,
        None,
        Some(if is_cloze { "1" } else { "0" }),
        None,
    )
}

/// 处理单词JSON文件
fn process_word_files(
    model: &Model,
    json_dir: &Path,
    media_dirs: (&Path, &Path),
    deck: &mut Deck,
    media_files: &mut HashSet<PathBuf>,
    note_count: &mut usize,
    report: &impl Fn(usize, usize),
    deck_id: i64,
) -> Result<()> {
    if !json_dir.exists() {
        println!("警告: 单词JSON目录不存在 {}", json_dir.display());
        return Ok(());
    }

    let word_files = json_files(json_dir)?;
    if !word_files.is_empty() {
        println!("找到 {} 个单词文件", word_files.len());
    }

    for word_file in word_files {
        let file = File::open(&word_file)?;
        let data: WordData = serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("JSON 解析失败: {}", word_file.display()))?;

        let mut current_media: Vec<PathBuf> = Vec::new();
        let audio_filename = check_media_file(data.audio, media_dirs.0, "音频", &mut current_media);
        let example_audio_filename = check_media_file(
            data.audio_example,
            media_dirs.0,
            "例句音频",
            &mut current_media,
        );
        let image_filename = check_media_file(data.image, media_dirs.1, "图片", &mut current_media);

        let value_to_string = |v: Option<Value>| {
            v.map_or(String::new(), |val| {
                val.to_string().trim_matches('"').to_string()
            })
        };

        let mut fields = vec![String::new(); 51];
        fields[0] = data.name.unwrap_or_default();
        fields[2] = value_to_string(data.grade);
        fields[3] = value_to_string(data.unit);
        fields[4] = data.symbol.unwrap_or_default();
        fields[5] = data.chn.unwrap_or_default();
        fields[6] = image_filename
            .map(|f| format!(r#"<img src="{}">"#, f))
            .unwrap_or_default();
        fields[7] = data.example_en.unwrap_or_default();
        fields[8] = example_audio_filename
            .map(|f| format!("[sound:{}]", f))
            .unwrap_or_default();
        fields[9] = audio_filename
            .map(|f| format!("[sound:{}]", f))
            .unwrap_or_default();
        fields[11] = data.example_cn.unwrap_or_default();
        fields[12] = data.word_family.unwrap_or_default();
        fields[13] = data.memory_tips.unwrap_or_default();
        fields[14] = value_to_string(data.difficulty);
        fields[15] = data.collocations.unwrap_or_default();

        let fields_str: Vec<&str> = fields.iter().map(AsRef::as_ref).collect();
        let identity = format!(
            "{deck_id}:{}",
            word_file.file_name().unwrap().to_string_lossy()
        );
        let note = Note::new(model.clone(), fields_str)?.guid(stable_guid(&identity));
        deck.add_note(note);
        *note_count += 1;
        media_files.extend(current_media);
        report(*note_count, media_files.len());
        println!("添加单词: {}", &fields[0]);
    }

    Ok(())
}

/// 处理背诵JSON文件
fn process_recite_files(
    model: &Model,
    json_dir: &Path,
    media_dirs: (&Path, &Path),
    deck: &mut Deck,
    media_files: &mut HashSet<PathBuf>,
    note_count: &mut usize,
    report: &impl Fn(usize, usize),
    deck_id: i64,
) -> Result<()> {
    if !json_dir.exists() {
        println!("警告: 背诵JSON目录不存在 {}", json_dir.display());
        return Ok(());
    }

    let recite_files = json_files(json_dir)?;
    if !recite_files.is_empty() {
        println!("找到 {} 个背诵文件", recite_files.len());
    }

    for recite_file in recite_files {
        let file = File::open(&recite_file)?;
        let data: ReciteData = serde_json::from_reader(BufReader::new(file))
            .with_context(|| format!("JSON 解析失败: {}", recite_file.display()))?;

        let mut current_media: Vec<PathBuf> = Vec::new();
        let replace_br = |s: Option<String>| s.unwrap_or_default().replace('\n', "<br>");

        let audios = [
            check_media_file(data.audio, media_dirs.0, "音频", &mut current_media),
            check_media_file(data.audio1, media_dirs.0, "音频1", &mut current_media),
            check_media_file(data.audio2, media_dirs.0, "音频2", &mut current_media),
            check_media_file(data.audio3, media_dirs.0, "音频3", &mut current_media),
            check_media_file(data.audio4, media_dirs.0, "音频4", &mut current_media),
            check_media_file(data.audio5, media_dirs.0, "音频5", &mut current_media),
            check_media_file(data.audio6, media_dirs.0, "音频6", &mut current_media),
            check_media_file(data.audio7, media_dirs.0, "音频7", &mut current_media),
            check_media_file(data.audio8, media_dirs.0, "音频8", &mut current_media),
            check_media_file(data.audio9, media_dirs.0, "音频9", &mut current_media),
        ];

        let image_filename = check_media_file(data.image, media_dirs.1, "图片", &mut current_media);

        let mut fields = vec![String::new(); 51];
        fields[16] = data.name.clone().unwrap_or_default();
        fields[17] = data.author.unwrap_or_default();
        fields[18] = replace_br(data.text);
        fields[19] = replace_br(data.hint);
        fields[20] = audios[0]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[21] = replace_br(data.text1);
        fields[22] = replace_br(data.hint1);
        fields[23] = audios[1]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[24] = replace_br(data.text2);
        fields[25] = replace_br(data.hint2);
        fields[26] = audios[2]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[27] = replace_br(data.text3);
        fields[28] = replace_br(data.hint3);
        fields[29] = audios[3]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[30] = replace_br(data.text4);
        fields[31] = replace_br(data.hint4);
        fields[32] = audios[4]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[33] = replace_br(data.text5);
        fields[34] = replace_br(data.hint5);
        fields[35] = audios[5]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[36] = replace_br(data.text6);
        fields[37] = replace_br(data.hint6);
        fields[38] = audios[6]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[39] = replace_br(data.text7);
        fields[40] = replace_br(data.hint7);
        fields[41] = audios[7]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[42] = replace_br(data.text8);
        fields[43] = replace_br(data.hint8);
        fields[44] = audios[8]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[45] = replace_br(data.text9);
        fields[46] = replace_br(data.hint9);
        fields[47] = audios[9]
            .as_ref()
            .map_or(String::new(), |f| format!("[sound:{}]", f));
        fields[48] = replace_br(data.translate);
        fields[49] = image_filename.map_or(String::new(), |f| format!(r#"<img src="{}">"#, f));
        fields[50] = data.imageprompt.unwrap_or_default();

        let fields_str: Vec<&str> = fields.iter().map(AsRef::as_ref).collect();
        let identity = format!(
            "{deck_id}:recite:{}",
            recite_file.file_name().unwrap().to_string_lossy()
        );
        let note = Note::new(model.clone(), fields_str)?.guid(stable_guid(&identity));
        deck.add_note(note);
        *note_count += 1;
        media_files.extend(current_media);
        report(*note_count, media_files.len());
        println!(
            "添加背诵: {}",
            data.name.unwrap_or_else(|| "无标题".to_string())
        );
    }
    Ok(())
}

/// 辅助函数：检查媒体文件是否存在并返回其文件名
fn check_media_file(
    filename_opt: Option<String>,
    media_dir: &Path,
    file_type: &str,
    media_list: &mut Vec<PathBuf>,
) -> Option<String> {
    filename_opt.and_then(|filename| {
        if filename.is_empty() {
            return None;
        }
        let path = media_dir.join(&filename);
        if path.exists() {
            media_list.push(path);
            Some(filename)
        } else {
            eprintln!("警告: {}文件不存在 {}", file_type, path.display());
            None
        }
    })
}

fn json_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(directory)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
    files.sort();
    Ok(files)
}

fn stable_guid(identity: &str) -> String {
    let hash = identity.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[test]
    fn export_streams_media_and_preserves_identity_after_edits() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("英语 [项目]");
        for sub in ["word_json", "audio", "images"] {
            fs::create_dir_all(dir.join(sub)).unwrap();
        }
        let audio = vec![42u8; 256 * 1024];
        fs::write(dir.join("audio/apple.mp3"), &audio).unwrap();
        let card_path = dir.join("word_json/apple.json");
        let mut card = serde_json::json!({"name":"apple","chn":"苹果","example_en":"An apple.","audio":"apple.mp3"});
        fs::write(&card_path, card.to_string()).unwrap();
        let events = Mutex::new(Vec::new());
        let run = || {
            handle_pack_with_progress(
                PackArgs {
                    pack_dir: dir.clone(),
                    template: None,
                },
                |event| events.lock().unwrap().push(event),
            )
            .unwrap()
        };
        run();
        let package = dir.join("英语 [项目]_Deck.apkg");
        let read_note = || {
            let mut archive = zip::ZipArchive::new(File::open(&package).unwrap()).unwrap();
            let mut media = String::new();
            archive
                .by_name("media")
                .unwrap()
                .read_to_string(&mut media)
                .unwrap();
            let map: serde_json::Value = serde_json::from_str(&media).unwrap();
            assert_eq!(map["0"], "apple.mp3");
            let mut actual = Vec::new();
            archive
                .by_name("0")
                .unwrap()
                .read_to_end(&mut actual)
                .unwrap();
            assert_eq!(actual, audio);
            let database = temp.path().join("collection.db");
            std::io::copy(
                &mut archive.by_name("collection.anki2").unwrap(),
                &mut File::create(&database).unwrap(),
            )
            .unwrap();
            let conn = rusqlite::Connection::open(database).unwrap();
            let count: i64 = conn
                .query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
                .unwrap();
            assert_eq!(count, 1);
            conn.query_row("SELECT guid FROM notes", [], |row| row.get::<_, String>(0))
                .unwrap()
        };
        let guid = read_note();
        let deck_id = fs::read_to_string(dir.join(".anki-deck-id")).unwrap();
        card["chn"] = serde_json::json!("新的释义");
        fs::write(&card_path, card.to_string()).unwrap();
        run();
        assert_eq!(read_note(), guid);
        assert_eq!(
            fs::read_to_string(dir.join(".anki-deck-id")).unwrap(),
            deck_id
        );
        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| e.phase == "media"
            && e.written_bytes == audio.len() as u64
            && e.total_bytes == audio.len() as u64));
        assert_eq!(events.last().unwrap().phase, "done");
    }
}
