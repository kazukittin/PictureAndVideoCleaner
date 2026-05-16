use image::{DynamicImage, GenericImageView, ImageReader};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        OnceLock,
    },
    thread,
    time::UNIX_EPOCH,
};
use tauri::Emitter;
use walkdir::WalkDir;

const SUPPORTED_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];
const SIMILAR_DISTANCE: u32 = 10;
const BLUR_THRESHOLD: f64 = 80.0;

static CANCEL_SCAN: OnceLock<AtomicBool> = OnceLock::new();

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
    cache_hit_count: usize,
    exact_duplicate_groups: Vec<ImageGroup>,
    similar_image_groups: Vec<ImageGroup>,
    blurry_images: Vec<ImageItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    message: String,
    current: usize,
    total: usize,
}

#[derive(Debug, Clone)]
struct CacheRecord {
    path: String,
    size_bytes: u64,
    modified_at: u64,
    content_hash: String,
    width: u32,
    height: u32,
    blur_score: Option<f64>,
    perceptual_hash: Option<u64>,
}

impl CacheRecord {
    fn basic(path: &Path, metadata: &fs::Metadata) -> CacheRecord {
        CacheRecord {
            path: path_to_string(path),
            size_bytes: metadata.len(),
            modified_at: modified_secs(metadata),
            content_hash: String::new(),
            width: 0,
            height: 0,
            blur_score: None,
            perceptual_hash: None,
        }
    }

    fn has_details(&self) -> bool {
        self.width > 0 && self.height > 0 && self.perceptual_hash.is_some()
    }

    fn item(&self) -> ImageItem {
        let path = Path::new(&self.path);
        ImageItem {
            id: stable_id(path),
            path: self.path.clone(),
            file_name: display_file_name(path),
            extension: path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_lowercase(),
            size_bytes: self.size_bytes,
            width: self.width,
            height: self.height,
            modified_at: self.modified_at.to_string(),
            blur_score: self.blur_score,
        }
    }

    fn from_cache_line(line: &str) -> Option<CacheRecord> {
        let parts = line.split('\t').collect::<Vec<_>>();
        if parts.len() != 8 {
            return None;
        }
        Some(CacheRecord {
            path: unescape_field(parts[0]),
            size_bytes: parts[1].parse().ok()?,
            modified_at: parts[2].parse().ok()?,
            content_hash: parts[3].to_string(),
            width: parts[4].parse().ok()?,
            height: parts[5].parse().ok()?,
            blur_score: if parts[6].is_empty() { None } else { parts[6].parse().ok() },
            perceptual_hash: if parts[7].is_empty() { None } else { parts[7].parse().ok() },
        })
    }

    fn to_cache_line(&self) -> String {
        [
            escape_field(&self.path),
            self.size_bytes.to_string(),
            self.modified_at.to_string(),
            self.content_hash.clone(),
            self.width.to_string(),
            self.height.to_string(),
            self.blur_score.map(|score| score.to_string()).unwrap_or_default(),
            self.perceptual_hash.map(|hash| hash.to_string()).unwrap_or_default(),
        ]
        .join("\t")
    }
}

#[derive(Debug, Clone)]
struct AnalyzedImage {
    record: CacheRecord,
}

impl AnalyzedImage {
    fn item(&self) -> ImageItem {
        self.record.item()
    }
}

#[tauri::command]
fn request_cancel_scan() {
    cancel_flag().store(true, Ordering::Relaxed);
}

#[tauri::command]
async fn scan_images(
    app: tauri::AppHandle,
    root_path: String,
    options: ScanOptions,
) -> Result<ScanResult, String> {
    tauri::async_runtime::spawn_blocking(move || scan_images_inner(app, root_path, options))
        .await
        .map_err(|error| format!("スキャン処理を開始できませんでした: {}", error))?
}

fn scan_images_inner(
    app: tauri::AppHandle,
    root_path: String,
    options: ScanOptions,
) -> Result<ScanResult, String> {
    cancel_flag().store(false, Ordering::Relaxed);

    let root = PathBuf::from(root_path);
    if !root.exists() || !root.is_dir() {
        return Err("選択したフォルダが見つかりません。".to_string());
    }

    emit_progress(&app, "画像ファイルを探しています", 0, 0);
    let paths = collect_image_paths(&root, options.include_subfolders)?;
    let total = paths.len();
    let cache_path = local_cache_path(&root)?;
    let cache = load_cache(&cache_path);

    emit_progress(&app, "ローカルキャッシュを確認しています", 0, total);
    let mut records = Vec::with_capacity(total);
    let mut cache_hit_count = 0;
    let mut skipped_count = 0;

    for path in paths {
        check_cancelled()?;
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                skipped_count += 1;
                continue;
            }
        };

        let path_string = path_to_string(&path);
        let modified_at = modified_secs(&metadata);
        if let Some(record) = cache.get(&path_string) {
            if record.size_bytes == metadata.len() && record.modified_at == modified_at {
                cache_hit_count += 1;
                records.push(record.clone());
                continue;
            }
        }

        records.push(CacheRecord::basic(&path, &metadata));
    }

    if options.detect_exact_duplicates {
        hash_duplicate_candidates(&app, &mut records, &cache_path)?;
    }

    let exact_duplicate_groups = if options.detect_exact_duplicates {
        emit_progress(&app, "完全重複を確認しています", records.len(), records.len());
        build_exact_groups(&records)
    } else {
        Vec::new()
    };

    let exact_duplicate_paths = collect_group_paths(&exact_duplicate_groups);
    let mut cache_records = Vec::new();
    let mut analyzed = Vec::new();
    let mut detail_targets = Vec::new();

    for record in records {
        if exact_duplicate_paths.contains(&record.path) {
            cache_records.push(record);
            continue;
        }
        if record.has_details() {
            cache_records.push(record.clone());
            analyzed.push(AnalyzedImage { record });
        } else {
            detail_targets.push(record.clone());
            cache_records.push(record);
        }
    }

    emit_progress(
        &app,
        "未解析画像のブレ値と類似判定データを作成しています",
        0,
        detail_targets.len(),
    );
    let analyze_counter = AtomicUsize::new(0);
    let analyzed_results = run_parallel_records(&detail_targets, |record| {
        check_cancelled()?;
        let current = analyze_counter.fetch_add(1, Ordering::Relaxed) + 1;
        analyze_image(
            &app,
            Path::new(&record.path),
            &record.content_hash,
            options.detect_blurry_images,
            current,
            detail_targets.len(),
        )
    });

    let mut cancelled = false;
    let analyzed_by_path = analyzed_results
        .into_iter()
        .filter_map(|result| match result {
            Ok(image) => Some((image.record.path.clone(), image)),
            Err(error) if error == "cancelled" => {
                cancelled = true;
                None
            }
            Err(_) => {
                skipped_count += 1;
                None
            }
        })
        .collect::<HashMap<_, _>>();

    cache_records = cache_records
        .into_iter()
        .map(|record| analyzed_by_path.get(&record.path).map(|image| image.record.clone()).unwrap_or(record))
        .collect();
    analyzed.extend(analyzed_by_path.into_values());

    if cancelled {
        save_cache(&cache_path, &cache_records);
        return Err("スキャンをキャンセルしました。次回は保存済みの確認結果を使って再開します。".to_string());
    }

    let similar_image_groups = if options.detect_similar_images {
        emit_progress(&app, "類似画像をバケット方式で確認しています", analyzed.len(), analyzed.len());
        build_similar_groups_bucketed(&analyzed)
    } else {
        Vec::new()
    };

    let blurry_images = if options.detect_blurry_images {
        emit_progress(&app, "ブレの可能性がある画像をまとめています", analyzed.len(), analyzed.len());
        analyzed
            .iter()
            .filter_map(|image| {
                image
                    .record
                    .blur_score
                    .filter(|score| *score < BLUR_THRESHOLD)
                    .map(|_| image.item())
            })
            .collect()
    } else {
        Vec::new()
    };

    save_cache(&cache_path, &cache_records);

    emit_progress(&app, "スキャン結果を表示しています", total, total);
    Ok(ScanResult {
        scanned_count: total,
        skipped_count,
        cache_hit_count,
        exact_duplicate_groups,
        similar_image_groups,
        blurry_images,
    })
}

#[tauri::command]
fn move_to_trash(paths: Vec<String>) -> Result<Vec<String>, String> {
    let mut moved = Vec::new();
    for path in paths {
        trash::delete(&path)
            .map_err(|error| format!("ゴミ箱へ移動できませんでした: {} ({})", path, error))?;
        moved.push(path);
    }
    Ok(moved)
}

fn hash_duplicate_candidates(
    app: &tauri::AppHandle,
    records: &mut [CacheRecord],
    cache_path: &Path,
) -> Result<(), String> {
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (index, record) in records.iter().enumerate() {
        by_size.entry(record.size_bytes).or_default().push(index);
    }

    let hash_targets = by_size
        .values()
        .filter(|indexes| indexes.len() > 1)
        .flat_map(|indexes| {
            indexes.iter().filter_map(|index| {
                if records[*index].content_hash.is_empty() {
                    Some(PathBuf::from(&records[*index].path))
                } else {
                    None
                }
            })
        })
        .collect::<Vec<_>>();

    emit_progress(
        app,
        "同じサイズの画像だけ内容を確認しています",
        0,
        hash_targets.len(),
    );
    let hash_counter = AtomicUsize::new(0);
    let hashed_results = run_parallel(&hash_targets, |path| {
        check_cancelled()?;
        let current = hash_counter.fetch_add(1, Ordering::Relaxed) + 1;
        emit_progress(
            app,
            &format!("「{}」の内容を確認中", display_file_name(path)),
            current,
            hash_targets.len(),
        );
        hash_file(path).map(|hash| (path_to_string(path), hash))
    });

    let mut hash_by_path = HashMap::new();
    let mut cancelled = false;
    for result in hashed_results {
        match result {
            Ok((path, hash)) => {
                hash_by_path.insert(path, hash);
            }
            Err(error) if error == "cancelled" => cancelled = true,
            Err(_) => {}
        }
    }

    for record in records.iter_mut() {
        if let Some(hash) = hash_by_path.get(&record.path) {
            record.content_hash = hash.clone();
        }
    }

    save_cache(cache_path, records);
    if cancelled {
        return Err("スキャンをキャンセルしました。次回は保存済みの確認結果を使って再開します。".to_string());
    }

    Ok(())
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

fn hash_file(path: &Path) -> Result<String, String> {
    let content = fs::read(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(&content);
    Ok(format!("{:x}", hasher.finalize()))
}

fn analyze_image(
    app: &tauri::AppHandle,
    path: &Path,
    content_hash: &str,
    include_blur_score: bool,
    current: usize,
    total: usize,
) -> Result<AnalyzedImage, String> {
    emit_progress(app, &format!("「{}」を読み込んでいます", display_file_name(path)), current, total);

    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let image = ImageReader::open(path)
        .map_err(|error| error.to_string())?
        .with_guessed_format()
        .map_err(|error| error.to_string())?
        .decode()
        .map_err(|error| error.to_string())?;
    let (width, height) = image.dimensions();

    let blur_score = if include_blur_score {
        emit_progress(app, &format!("「{}」のブレ値を計算中", display_file_name(path)), current, total);
        Some(calculate_blur_score(&image))
    } else {
        None
    };

    emit_progress(
        app,
        &format!("「{}」の類似判定用データを作成中", display_file_name(path)),
        current,
        total,
    );

    let mut record = CacheRecord::basic(path, &metadata);
    record.content_hash = content_hash.to_string();
    record.width = width;
    record.height = height;
    record.blur_score = blur_score;
    record.perceptual_hash = Some(calculate_perceptual_hash(&image));

    Ok(AnalyzedImage { record })
}

fn build_exact_groups(records: &[CacheRecord]) -> Vec<ImageGroup> {
    let mut by_hash: HashMap<&str, Vec<&CacheRecord>> = HashMap::new();
    for record in records {
        if !record.content_hash.is_empty() {
            by_hash.entry(&record.content_hash).or_default().push(record);
        }
    }

    by_hash
        .into_iter()
        .filter_map(|(hash, records)| {
            if records.len() < 2 {
                return None;
            }

            Some(ImageGroup {
                id: format!("exact-{}", &hash[0..12]),
                title: "同じ内容の画像".to_string(),
                items: records.iter().map(|record| record.item()).collect(),
            })
        })
        .collect()
}

fn collect_group_paths(groups: &[ImageGroup]) -> HashSet<String> {
    groups
        .iter()
        .flat_map(|group| group.items.iter().map(|item| item.path.clone()))
        .collect()
}

fn build_similar_groups_bucketed(images: &[AnalyzedImage]) -> Vec<ImageGroup> {
    let mut buckets: HashMap<(usize, u16), Vec<usize>> = HashMap::new();
    for (index, image) in images.iter().enumerate() {
        if let Some(hash) = image.record.perceptual_hash {
            for band in 0..4 {
                let key = ((hash >> (band * 16)) & 0xffff) as u16;
                buckets.entry((band, key)).or_default().push(index);
            }
        }
    }

    let mut candidate_pairs = HashSet::new();
    for indexes in buckets.values() {
        if indexes.len() > 500 {
            continue;
        }
        for (left_pos, left) in indexes.iter().enumerate() {
            for right in indexes.iter().skip(left_pos + 1) {
                candidate_pairs.insert((*left.min(right), *left.max(right)));
            }
        }
    }

    let mut parent = (0..images.len()).collect::<Vec<_>>();
    for (left, right) in candidate_pairs {
        let Some(left_hash) = images[left].record.perceptual_hash else { continue };
        let Some(right_hash) = images[right].record.perceptual_hash else { continue };
        if (left_hash ^ right_hash).count_ones() <= SIMILAR_DISTANCE {
            union(&mut parent, left, right);
        }
    }

    let mut grouped: HashMap<usize, Vec<ImageItem>> = HashMap::new();
    for index in 0..images.len() {
        let root = find(&mut parent, index);
        grouped.entry(root).or_default().push(images[index].item());
    }

    grouped
        .into_iter()
        .filter_map(|(root, items)| {
            if items.len() < 2 {
                return None;
            }
            Some(ImageGroup {
                id: format!("similar-{}", root),
                title: "見た目が近い画像".to_string(),
                items,
            })
        })
        .collect()
}

fn find(parent: &mut [usize], index: usize) -> usize {
    if parent[index] != index {
        parent[index] = find(parent, parent[index]);
    }
    parent[index]
}

fn union(parent: &mut [usize], left: usize, right: usize) {
    let left_root = find(parent, left);
    let right_root = find(parent, right);
    if left_root != right_root {
        parent[right_root] = left_root;
    }
}

fn calculate_perceptual_hash(image: &DynamicImage) -> u64 {
    let gray = image
        .resize_exact(9, 8, image::imageops::FilterType::Triangle)
        .to_luma8();
    let mut hash = 0u64;

    for y in 0..8 {
        for x in 0..8 {
            let left = gray.get_pixel(x, y)[0];
            let right = gray.get_pixel(x + 1, y)[0];
            if left > right {
                hash |= 1u64 << (y * 8 + x);
            }
        }
    }

    hash
}

fn calculate_blur_score(image: &DynamicImage) -> f64 {
    let gray = image
        .resize(640, 640, image::imageops::FilterType::Triangle)
        .to_luma8();
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

fn run_parallel<T, F>(paths: &[PathBuf], worker: F) -> Vec<Result<T, String>>
where
    T: Send,
    F: Fn(&PathBuf) -> Result<T, String> + Sync,
{
    if paths.is_empty() {
        return Vec::new();
    }

    thread::scope(|scope| {
        let mut handles = Vec::new();
        for chunk in paths.chunks(chunk_size(paths.len())) {
            let worker = &worker;
            handles.push(scope.spawn(move || chunk.iter().map(worker).collect::<Vec<_>>()));
        }

        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    })
}

fn run_parallel_records<T, F>(records: &[CacheRecord], worker: F) -> Vec<Result<T, String>>
where
    T: Send,
    F: Fn(&CacheRecord) -> Result<T, String> + Sync,
{
    if records.is_empty() {
        return Vec::new();
    }

    thread::scope(|scope| {
        let mut handles = Vec::new();
        for chunk in records.chunks(chunk_size(records.len())) {
            let worker = &worker;
            handles.push(scope.spawn(move || chunk.iter().map(worker).collect::<Vec<_>>()));
        }

        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    })
}

fn chunk_size(item_count: usize) -> usize {
    let available = thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4);
    let worker_count = available.saturating_sub(1).clamp(1, 4);
    item_count.div_ceil(worker_count).max(1)
}

fn local_cache_path(root: &Path) -> Result<PathBuf, String> {
    let base = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cache_dir = base.join("PictureCleaner").join("scan-cache");
    fs::create_dir_all(&cache_dir).map_err(|error| error.to_string())?;
    Ok(cache_dir.join(format!("{}.tsv", stable_id(root))))
}

fn load_cache(cache_path: &Path) -> HashMap<String, CacheRecord> {
    let Ok(content) = fs::read_to_string(cache_path) else {
        return HashMap::new();
    };

    content
        .lines()
        .filter_map(CacheRecord::from_cache_line)
        .map(|record| (record.path.clone(), record))
        .collect()
}

fn save_cache(cache_path: &Path, records: &[CacheRecord]) {
    let mut lines = String::new();
    for record in records {
        lines.push_str(&record.to_cache_line());
        lines.push('\n');
    }
    let _ = fs::write(cache_path, lines);
}

fn escape_field(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn unescape_field(value: &str) -> String {
    let mut output = String::new();
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            output.push(match ch {
                't' => '\t',
                'n' => '\n',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            output.push(ch);
        }
    }
    output
}

fn emit_progress(app: &tauri::AppHandle, message: &str, current: usize, total: usize) {
    let _ = app.emit(
        "scan-progress",
        ScanProgress {
            message: message.to_string(),
            current,
            total,
        },
    );
}

fn cancel_flag() -> &'static AtomicBool {
    CANCEL_SCAN.get_or_init(|| AtomicBool::new(false))
}

fn check_cancelled() -> Result<(), String> {
    if cancel_flag().load(Ordering::Relaxed) {
        Err("cancelled".to_string())
    } else {
        Ok(())
    }
}

fn modified_secs(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn display_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("不明なファイル")
        .to_string()
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn stable_id(path: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    path.to_string_lossy().hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            scan_images,
            move_to_trash,
            request_cancel_scan
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
