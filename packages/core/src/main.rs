use anyhow::{Context, Result};
use fontcull::{decompress_font, subset_font_to_woff2, FontFormat};
use md5;
use rayon::prelude::*;
use serde_json;
use std::collections::BTreeMap;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use ttf_parser::name_id;
use ttf_parser::Face;
use walkdir::WalkDir;

const C: Colors = Colors {
    bold: "\x1b[1m",
    green: "\x1b[32m",
    yellow: "\x1b[33m",
    red: "\x1b[31m",
    blue: "\x1b[94m",
    end: "\x1b[0m",
};

struct Colors {
    bold: &'static str,
    green: &'static str,
    yellow: &'static str,
    red: &'static str,
    blue: &'static str,
    end: &'static str,
}

const CULL_VERSION: &str = "fontcull-2";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Cache {
    #[serde(default)]
    text_hash: String,
    #[serde(default = "current_version")]
    version: String,
    #[serde(default)]
    fonts: HashMap<String, String>,
}

fn current_version() -> String {
    CULL_VERSION.to_string()
}

fn main() {
    if let Err(e) = run_subset() {
        println!(" {}✗ Critical failure: {}{}", C.red, e, C.end);
    }
}

fn run_subset() -> Result<()> {
    println!(
        "\n{}🚀 Starting Font Subset Optimization...{}",
        C.bold, C.end
    );

    let project_root = env::var("PROJECT_ROOT")
        .ok()
        .and_then(|p| fs::canonicalize(p).ok())
        .unwrap_or_else(|| env::current_dir().expect("Failed to get current directory"));

    let src_dir = project_root.join("src");
    let font_dir = src_dir.join("assets/fonts");
    let subfont_dir = project_root.join(".subfont");
    let source_dir = subfont_dir.join("source");
    let cache_file = subfont_dir.join("cache.json");
    let manifest_path = subfont_dir.join("font-manifest.json");

    let (text, text_hash) = get_unique_chars(&src_dir)?;
    let chars: HashSet<char> = text.chars().collect();

    if chars.len() > 10000 {
        println!(
            " {}Large character set detected ({} unique chars) - subsetting may take a while{}",
            C.yellow,
            chars.len(),
            C.end
        );
    }

    if !font_dir.is_dir() {
        println!(" {}No public/fonts directory found.{}", C.yellow, C.end);
        return Ok(());
    }

    fs::create_dir_all(&subfont_dir)?;
    fs::create_dir_all(&source_dir)?;

    let cache_exists = cache_file.is_file();
    let mut cache: Cache = if cache_exists {
        let content = fs::read_to_string(&cache_file)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Cache::default()
    };

    if cache_exists && cache.version != CULL_VERSION {
        println!(
            " {}FontTools version change detected - reprocessing all fonts.{}",
            C.yellow, C.end
        );
        cache.fonts.clear();
    }

    let managed_bases: HashSet<String> = cache.fonts.keys().cloned().collect();

    for entry in fs::read_dir(&font_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        if file_name.starts_with('.') {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        let supported = matches!(ext.as_deref(), Some("ttf" | "otf" | "woff2"));
        if !supported {
            continue;
        }
        if ext.as_deref() == Some("woff") {
            println!(
                " {}- {}: {}WOFF format not supported by fontcull - skipping{}",
                C.blue, file_name, C.yellow, C.end
            );
            continue;
        }
        let base = path.file_stem().unwrap().to_string_lossy().to_string();
        let is_managed = ext.as_deref() == Some("woff2") && managed_bases.contains(&base);
        let dst_path = source_dir.join(&file_name);
        if is_managed {
            println!(
                " {}- {}: {}Managed subset (skipping copy){}",
                C.blue, file_name, C.yellow, C.end
            );
            continue;
        }
        let src_hash = get_file_hash(&path);
        let dst_hash = if dst_path.is_file() {
            get_file_hash(&dst_path)
        } else {
            "no_file".to_string()
        };
        if src_hash != dst_hash {
            fs::copy(&path, &dst_path)?;
        }
    }

    let mut groups: HashMap<String, Vec<(usize, String, PathBuf, bool, String, String)>> =
        HashMap::new();
    let mut seen_realpaths: HashSet<PathBuf> = HashSet::new();

    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        if file_name.starts_with('.') {
            continue;
        }
        let lower_ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_lowercase()));
        if !matches!(lower_ext.as_deref(), Some(".ttf" | ".otf" | ".woff2")) {
            continue;
        }
        let canon = path.canonicalize()?;
        if seen_realpaths.contains(&canon) {
            println!(
                " {}Skipping duplicate {} (symlink/realpath collision){}",
                C.yellow, file_name, C.end
            );
            continue;
        }
        seen_realpaths.insert(canon);
        if path.symlink_metadata()?.file_type().is_symlink() {
            println!(" {}Skipping symlink {}{}", C.yellow, file_name, C.end);
            continue;
        }
        let key_info = match get_font_key(&path) {
            Some(info) => info,
            None => continue,
        };
        let (_key, is_variable, family, display_style) = key_info;
        let base_name = path.file_stem().unwrap().to_string_lossy().to_string();
        let pri = match lower_ext.as_deref() {
            Some(".woff2") => 0,
            Some(".ttf") => 2,
            Some(".otf") => 3,
            _ => 10,
        } + if is_variable { 10 } else { 0 };
        groups.entry(base_name).or_default().push((
            pri,
            file_name,
            path,
            is_variable,
            family,
            display_style,
        ));
    }

    let mut new_cache = Cache {
        text_hash,
        version: CULL_VERSION.to_string(),
        fonts: HashMap::new(),
    };
    let mut manifest_mapping: HashMap<String, String> = HashMap::new();

    type Task = (String, PathBuf, String, String, PathBuf, PathBuf, PathBuf, String); 
    let mut to_process: Vec<Task> = Vec::new();

    for (base_name, mut candidates) in groups {
        candidates.sort_by_key(|c| c.0);
        let (_pri, _filename, input_path, _is_var, _family, _display_style) = candidates[0].clone();

        let input_hash = get_file_hash(&input_path);
        if input_hash == "hash_error" {
            continue;
        }

        let output_base = base_name.clone();
        let output_name = format!("{output_base}.woff2");
        let output_path = font_dir.join(&output_name);
        let temp_path = font_dir.join(format!("{output_name}.tmp"));
        let manifest_alias = base_name.to_lowercase().replace(' ', "");

        let cached_entry = cache.fonts.get(&output_base);
        if cached_entry.map(|s| s.as_str()) == Some(&input_hash)
            && cache.text_hash == new_cache.text_hash
            && output_path.is_file()
        {
            let size_kb = fs::metadata(&output_path)?.len() / 1024;
            println!(
                " {}- {}: {}Up to date ({}KB cached){}",
                C.blue, output_name, C.green, size_kb, C.end
            );
            new_cache.fonts.insert(output_base, input_hash);
            manifest_mapping.insert(manifest_alias, output_name);
            continue;
        }

        to_process.push((
            base_name,
            input_path,
            input_hash,
            output_name,
            output_path,
            temp_path,
            font_dir.clone(),
            manifest_alias,
        ));
    }

    let results: Vec<(String, String, String, String, usize, usize)> = to_process.into_par_iter().filter_map(
        |(base_name, input_path, input_hash, output_name, output_path, temp_path, font_dir, manifest_alias)| {
            let input_bytes = match fs::read(&input_path) {
                Ok(b) => b,
                Err(e) => {
                    println!(" {}Failed to read input for {}: {}{}", C.red, output_name, e, C.end);
                    return None;
                }
            };

            let orig_size = input_bytes.len() / 1024;

            let font_data = if input_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                == Some("woff2".to_string())
            {
                match decompress_font(&input_bytes) {
                    Ok(d) => d,
                    Err(e) => {
                        println!(" {}Decompress failed for {}: {}{}", C.yellow, output_name, e, C.end);
                        return None;
                    }
                }
            } else {
                input_bytes.clone()
            };

            let font_features: [[u8; 4]; 6] = [
    *b"ccmp",  
    *b"locl",
    *b"kern",
    *b"liga",
    *b"mark",
    *b"mkmk",
];

            let woff2_data = match subset_font_to_woff2(&font_data, &chars, &font_features) {
                Ok(data) => data,
                Err(e) => {
                    println!(
                        " {}Subset error for {}: {}{}",
                        C.yellow, output_name, e, C.end
                    );
                    println!(
                        " {}✗ Failed to process {} - keeping original formats{}",
                        C.red, output_name, C.end
                    );
                    let _ = fs::remove_file(&temp_path);
                    return None;
                }
            };

            let new_size = woff2_data.len() / 1024;

            if fs::write(&temp_path, &woff2_data).is_err() {
                return None;
            }
            if output_path.is_file() {
                let _ = fs::remove_file(&output_path);
            }
            if fs::rename(&temp_path, &output_path).is_err() {
                return None;
            }

            if let Ok(entries) = fs::read_dir(&font_dir) {
                for old_entry in entries.flatten() {
                    let old_path = old_entry.path();
                    if old_path.is_file() {
                        let old_name = old_path.file_name().unwrap().to_string_lossy();
                        if old_name == output_name {
                            continue;
                        }
                        let old_base = old_path.file_stem().unwrap().to_string_lossy().to_string();
                        if old_base == base_name {
                            if let Err(e) = fs::remove_file(&old_path) {
                                println!(
                                    " {}Could not remove old {}: {}{}",
                                    C.yellow, old_name, e, C.end
                                );
                            }
                        }
                    }
                }
            }

            println!(
                " {}{}✓{} {}{}{}: {}KB → {}{}KB{}",
                C.green, C.bold, C.end, C.bold, output_name, C.end, orig_size, C.green, new_size, C.end
            );

            Some((base_name, input_hash, manifest_alias, output_name, orig_size, new_size))
        },
    ).collect();

    for (base_name, input_hash, manifest_alias, output_name, _orig_size, _new_size) in results {
        new_cache.fonts.insert(base_name, input_hash);
        manifest_mapping.insert(manifest_alias, output_name);
    }

    let sorted_manifest: BTreeMap<_, _> = manifest_mapping.into_iter().collect();
    let manifest_json = serde_json::to_string_pretty(&sorted_manifest)?;
    fs::write(&manifest_path, manifest_json)?;

    println!(
        " {}{}✓{} Manifest generated at {}",
        C.green,
        C.bold,
        C.end,
        manifest_path.file_name().unwrap().to_string_lossy()
    );

    let cache_json = serde_json::to_string_pretty(&new_cache)?;
    fs::write(&cache_file, cache_json)?;

    println!(
        "{}{}✨ Finished! All fonts optimized.{}\n",
        C.bold, C.green, C.end
    );

    Ok(())
}

fn get_unique_chars(src_dir: &Path) -> Result<(String, String)> {
    let default_str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
    let mut chars: HashSet<char> = default_str.chars().collect();

    let extensions = [
        ".astro", ".md", ".mdx", ".ts", ".tsx", ".js", ".jsx", ".json", ".html", ".vue", ".svelte"
    ];
    let ext_set: HashSet<&str> = extensions.iter().copied().collect();

    let mut paths: Vec<PathBuf> = Vec::new();

    if src_dir.is_dir() {
        for entry in WalkDir::new(src_dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|os| os.to_str()) {
                    let full_ext = format!(".{}", ext);
                    if ext_set.contains(full_ext.as_str()) {
                        paths.push(path.to_path_buf());
                    }
                }
            }
        }
    }

    if !paths.is_empty() {
        println!(
            " {}Scanning {} source files for unique characters...{}",
            C.blue, paths.len(), C.end
        );
    }

    let additional_chars: HashSet<char> = paths.into_par_iter().fold(
        || HashSet::new(),
        |mut acc: HashSet<char>, path: PathBuf| {
            if let Ok(bytes) = fs::read(&path) {
                let text: String = if std::str::from_utf8(&bytes).is_ok() {
                    let mut s = String::from_utf8(bytes.clone()).unwrap();
                    if s.starts_with('\u{feff}') {
                        if bytes.len() >= 3 {
                            s = String::from_utf8_lossy(&bytes[3..]).to_string();
                        }
                    }
                    s
                } else {
                    bytes
                        .into_iter()
                        .map(|b| char::from_u32(u32::from(b)).unwrap_or('\u{fffd}'))
                        .collect()
                };
                acc.extend(text.chars());
            }
            acc
        },
    ).reduce(
        || HashSet::new(),
        |mut acc, part| {
            acc.extend(part);
            acc
        },
    );

    chars.extend(additional_chars);

    let mut sorted: Vec<char> = chars.into_iter().collect();
    sorted.sort_unstable();
    let result_text = sorted.into_iter().collect::<String>();

    let mut context = md5::Context::new();
    context.consume(result_text.as_bytes());
    let text_hash = format!("{:x}", context.compute());

    Ok((result_text, text_hash))
}

fn get_file_hash(path: &Path) -> String {
    let file_name = path.file_name().unwrap().to_string_lossy();
    match fs::read(path) {
        Ok(bytes) => {
            let mut context = md5::Context::new();
            context.consume(&bytes);
            format!("{:x}", context.compute())
        }
        Err(e) => {
            println!(" {}Failed to hash {}: {}{}", C.red, file_name, e, C.end);
            "hash_error".to_string()
        }
    }
}

fn get_font_key(path: &Path) -> Option<((String, String), bool, String, String)> {
    let file_name = path.file_name().unwrap().to_string_lossy();
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => {
            return None;
        }
    };
    let data = if FontFormat::detect(&bytes) == FontFormat::Woff2 {
        match decompress_font(&bytes) {
            Ok(d) => d,
            Err(_) => {
                println!(
                    " {}✗ Skipping invalid/corrupted font {}: decompress failed{}",
                    C.red, file_name, C.end
                );
                return None;
            }
        }
    } else {
        bytes
    };
    let face = match Face::parse(&data, 0) {
        Ok(f) => f,
        Err(_) => {
            println!(
                " {}✗ Skipping invalid/corrupted font {}{}",
                C.red, file_name, C.end
            );
            return None;
        }
    };
    let names = face.names();
    let family = names
        .into_iter()
        .find(|name| name.name_id == name_id::TYPOGRAPHIC_FAMILY)
        .or_else(|| {
            names
                .into_iter()
                .find(|name| name.name_id == name_id::FAMILY)
        })
        .and_then(|name| name.to_string())
        .unwrap_or_else(|| "UnknownFamily".to_string());
    let style = names
        .into_iter()
        .find(|name| name.name_id == name_id::TYPOGRAPHIC_SUBFAMILY)
        .or_else(|| {
            names
                .into_iter()
                .find(|name| name.name_id == name_id::SUBFAMILY)
        })
        .and_then(|name| name.to_string())
        .unwrap_or_else(|| "Regular".to_string());
    let is_variable = face.is_variable();
    let display_style = if style != "Regular" {
        style.clone()
    } else {
        String::new()
    };
    Some(((family.clone(), style), is_variable, family, display_style))
}