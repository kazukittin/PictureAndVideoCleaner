use image::{DynamicImage, GenericImageView, ImageReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use walkdir::WalkDir;

const SUPPORTED_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
const SIMILAR_DISTANCE: u32 = 10;
const BLUR_THRESHOLD: f64 = 80.0;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanOptions {
    include_subfolders: bool,
    detect_exact_duplicates: bool,
    detect_similar_images: bool,
    detect_blurry_images: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageItem {
    id: String,
    path: String,
    file_name: String,
    extension: String,
    size_bytes: u64,
    width: u32,
    height: u32,
    modified_at: String,
    blur_score: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageGroup {
    id: String,
    title: String,
    items: Vec<ImageItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanResult {
    scanned_count: usize,
    skipped_count: usize,
    exact_duplicate_groups: Vec<ImageGroup>,
    similar_image_groups: Vec<ImageGroup>,
    blurry_images: Vec<ImageItem>,
}

#[derive(Debug, Clone)]
struct AnalyzedImage {
    item: ImageItem,
    content_hash: String,
    perceptual_hash: u64,
}

#[tauri::command]
fn scan_images(root_path: String, options: ScanOptions) -> Result<ScanResult, String> {
    let root = PathBuf::from(root_path);
    if !root.exists() || !root.is_dir() {
        return Err("選択したフォルダが見つかりません。".to_string());
    }

    let paths = collect_image_paths(&root, options.include_subfolders)?;
    let mut analyzed = Vec::new();
    let mut skipped_count = 0;

    for path in paths {
        match analyze_image(&path, options.detect_blurry_images) {
            Ok(image) => analyzed.push(image),
            Err(_) => skipped_count += 1,
        }
    }

    let exact_duplicate_groups = if options.detect_exact_duplicates {
        build_exact_groups(&analyzed)
    } else {
        Vec::new()
    };
    let similar_image_groups = if options.detect_similar_images {
        build_similar_groups(&analyzed)
    } else {
        Vec::new()
    };
    let blurry_images = if options.detect_blurry_images {
        analyzed
            .iter()
            .filter_map(|image| {
                image
                    .item
                    .blur_score
                    .filter(|score| *score < BLUR_THRESHOLD)
                    .map(|_| image.item.clone())
            })
            .collect()
    } else {
        Vec::new()
    };

    Ok(ScanResult {
        scanned_count: analyzed.len(),
        skipped_count,
        exact_duplicate_groups,
        similar_image_groups,
        blurry_images,
    })
}

#[tauri::command]
fn move_to_trash(paths: Vec<String>) -> Result<Vec<String>, String> {
    let mut moved = Vec::new();
    for path in paths {
        trash::delete(&path).map_err(|error| {
            format!("ゴミ箱へ移動できませんでした: {} ({})", path, error)
        })?;
        moved.push(path);
    }
    Ok(moved)
}

fn collect_image_paths(root: &Path, include_subfolders: bool) -> Result<Vec<PathBuf>, String> {
    let walker = if include_subfolders {
        WalkDir::new(root)
    } else {
        WalkDir::new(root).max_depth(1)
    };

    let mut paths = Vec::new();
    for entry in walker.into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file() && is_supported_image(entry.path()) {
            paths.push(entry.path().to_path_buf());
        }
    }

    Ok(paths)
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| supported.eq_ignore_ascii_case(extension))
        })
        .unwrap_or(false)
}

fn analyze_image(path: &Path, include_blur_score: bool) -> Result<AnalyzedImage, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let content = fs::read(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    let content_hash = format!("{:x}", hasher.finalize());

    let image = ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?
        .decode()
        .map_err(|error| error.to_string())?;

    let (width, height) = image.dimensions();
    let blur_score = if include_blur_score {
        Some(calculate_blur_score(&image))
    } else {
        None
    };

    let item = ImageItem {
        id: stable_id(path),
        path: path.to_string_lossy().to_string(),
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("不明なファイル")
            .to_string(),
        extension: path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_lowercase(),
        size_bytes: metadata.len(),
        width,
        height,
        modified_at: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().to_string())
            .unwrap_or_default(),
        blur_score,
    };

    Ok(AnalyzedImage {
        item,
        content_hash,
        perceptual_hash: calculate_perceptual_hash(&image),
    })
}

fn build_exact_groups(images: &[AnalyzedImage]) -> Vec<ImageGroup> {
    let mut by_hash: HashMap<&str, Vec<ImageItem>> = HashMap::new();
    for image in images {
        by_hash
            .entry(&image.content_hash)
            .or_default()
            .push(image.item.clone());
    }

    by_hash
        .into_iter()
        .filter_map(|(hash, items)| {
            if items.len() < 2 {
                return None;
            }
            Some(ImageGroup {
                id: format!("exact-{}", &hash[0..12]),
                title: "同じ内容の画像".to_string(),
                items,
            })
        })
        .collect()
}

fn build_similar_groups(images: &[AnalyzedImage]) -> Vec<ImageGroup> {
    let mut used_indexes = HashSet::new();
    let mut groups = Vec::new();

    for (index, image) in images.iter().enumerate() {
        if used_indexes.contains(&index) {
            continue;
        }

        let mut group = vec![image.item.clone()];
        for (other_index, other) in images.iter().enumerate().skip(index + 1) {
            if used_indexes.contains(&other_index) {
                continue;
            }

            let distance = (image.perceptual_hash ^ other.perceptual_hash).count_ones();
            if distance <= SIMILAR_DISTANCE {
                used_indexes.insert(other_index);
                group.push(other.item.clone());
            }
        }

        if group.len() > 1 {
            used_indexes.insert(index);
            groups.push(ImageGroup {
                id: format!("similar-{}", index),
                title: "見た目が近い画像".to_string(),
                items: group,
            });
        }
    }

    groups
}

fn calculate_perceptual_hash(image: &DynamicImage) -> u64 {
    let gray = image
        .resize_exact(32, 32, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut dct = [[0.0; 32]; 32];

    for u in 0..32 {
        for v in 0..32 {
            let mut sum = 0.0;
            for x in 0..32 {
                for y in 0..32 {
                    let pixel = gray.get_pixel(x, y)[0] as f64;
                    let cos_x = (((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI) / 64.0).cos();
                    let cos_y = (((2 * y + 1) as f64 * v as f64 * std::f64::consts::PI) / 64.0).cos();
                    sum += pixel * cos_x * cos_y;
                }
            }
            dct[u as usize][v as usize] = sum;
        }
    }

    let mut values = Vec::with_capacity(64);
    for u in 0..8 {
        for v in 0..8 {
            if u != 0 || v != 0 {
                values.push(dct[u][v]);
            }
        }
    }

    let mut sorted = values.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let median = sorted[sorted.len() / 2];

    values
        .iter()
        .enumerate()
        .fold(0u64, |hash, (index, value)| {
            if *value > median {
                hash | (1u64 << index)
            } else {
                hash
            }
        })
}

fn calculate_blur_score(image: &DynamicImage) -> f64 {
    let gray = image.to_luma8();
    let width = gray.width();
    let height = gray.height();

    if width < 3 || height < 3 {
        return 0.0;
    }

    let mut values = Vec::with_capacity(((width - 2) * (height - 2)) as usize);
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let center = gray.get_pixel(x, y)[0] as f64;
            let left = gray.get_pixel(x - 1, y)[0] as f64;
            let right = gray.get_pixel(x + 1, y)[0] as f64;
            let top = gray.get_pixel(x, y - 1)[0] as f64;
            let bottom = gray.get_pixel(x, y + 1)[0] as f64;
            values.push((left + right + top + bottom - 4.0 * center).abs());
        }
    }

    let mean = values.iter().sum::<f64>() / values.len() as f64;
    values
        .iter()
        .map(|value| {
            let difference = value - mean;
            difference * difference
        })
        .sum::<f64>()
        / values.len() as f64
}

fn stable_id(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![scan_images, move_to_trash])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
