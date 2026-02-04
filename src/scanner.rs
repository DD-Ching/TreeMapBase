use crate::model::Node;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub max_depth: usize,
    pub max_files: Option<usize>,
    pub progress_interval: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_files: Some(250_000),
            progress_interval: 400,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPhase {
    Counting,
    Scanning,
}

impl Default for ScanPhase {
    fn default() -> Self {
        Self::Counting
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub phase: ScanPhase,
    pub entries_scanned: u64,
    pub files_scanned: u64,
    pub directories_scanned: u64,
    pub warnings: u64,
    pub truncated: bool,
    pub current_path: Option<PathBuf>,
    pub total_estimated_entries: Option<u64>,
    pub remaining_estimated_entries: Option<u64>,
    pub progress_percent: Option<f32>,
    pub eta: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct ScanStats {
    pub entries_scanned: u64,
    pub files_scanned: u64,
    pub directories_scanned: u64,
    pub warnings: u64,
    pub truncated: bool,
    pub estimated_total_entries: Option<u64>,
    pub elapsed: Duration,
}

impl Default for ScanStats {
    fn default() -> Self {
        Self {
            entries_scanned: 0,
            files_scanned: 0,
            directories_scanned: 0,
            warnings: 0,
            truncated: false,
            estimated_total_entries: None,
            elapsed: Duration::ZERO,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub root: Node,
    pub stats: ScanStats,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum ScanMessage {
    Progress(ScanProgress),
    Finished(Result<ScanResult, String>),
}

pub fn spawn_scan(root_path: PathBuf, config: ScanConfig) -> Receiver<ScanMessage> {
    let (tx, rx) = mpsc::channel::<ScanMessage>();

    thread::spawn(move || {
        let started = Instant::now();
        let result = run_scan_pipeline(&root_path, &config, &tx).map(|mut result| {
            result.stats.elapsed = started.elapsed();
            result
        });

        let _ = tx.send(ScanMessage::Finished(result));
    });

    rx
}

fn run_scan_pipeline(
    root_path: &Path,
    config: &ScanConfig,
    tx: &Sender<ScanMessage>,
) -> Result<ScanResult, String> {
    if !root_path.exists() {
        return Err(format!("Directory does not exist: {}", root_path.display()));
    }

    if !root_path.is_dir() {
        return Err(format!("Path is not a directory: {}", root_path.display()));
    }

    let estimated_total_entries = estimate_total_entries(root_path, config, tx)?;
    scan_directory(root_path, config, tx, estimated_total_entries)
}

fn estimate_total_entries(
    root_path: &Path,
    config: &ScanConfig,
    tx: &Sender<ScanMessage>,
) -> Result<u64, String> {
    let mut progress = ScanProgress {
        phase: ScanPhase::Counting,
        ..Default::default()
    };

    let walker = WalkDir::new(root_path)
        .follow_links(false)
        .max_depth(config.max_depth.max(1));

    for entry_result in walker {
        match entry_result {
            Ok(entry) => {
                progress.entries_scanned = progress.entries_scanned.saturating_add(1);
                progress.current_path = Some(entry.path().to_path_buf());

                if entry.depth() == 0 {
                    continue;
                }

                if entry.file_type().is_dir() {
                    progress.directories_scanned = progress.directories_scanned.saturating_add(1);
                } else {
                    if let Some(max_files) = config.max_files {
                        if progress.files_scanned as usize >= max_files {
                            progress.truncated = true;
                            break;
                        }
                    }

                    progress.files_scanned = progress.files_scanned.saturating_add(1);
                }
            }
            Err(_) => {
                progress.warnings = progress.warnings.saturating_add(1);
            }
        }

        if progress.entries_scanned % config.progress_interval.max(1) as u64 == 0 {
            let _ = tx.send(ScanMessage::Progress(progress.clone()));
        }
    }

    let estimated_total_entries = progress.entries_scanned.max(1);
    progress.total_estimated_entries = Some(estimated_total_entries);

    let _ = tx.send(ScanMessage::Progress(progress));

    Ok(estimated_total_entries)
}

fn scan_directory(
    root_path: &Path,
    config: &ScanConfig,
    tx: &Sender<ScanMessage>,
    estimated_total_entries: u64,
) -> Result<ScanResult, String> {
    let root_name = root_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| root_path.display().to_string());

    let mut root = Node::new(root_name, root_path.to_path_buf(), 0);
    let mut warnings = Vec::new();
    let mut progress = ScanProgress {
        phase: ScanPhase::Scanning,
        total_estimated_entries: Some(estimated_total_entries.max(1)),
        progress_percent: Some(0.0),
        ..Default::default()
    };

    let phase_started = Instant::now();

    let walker = WalkDir::new(root_path)
        .follow_links(false)
        .max_depth(config.max_depth.max(1));

    for entry_result in walker {
        match entry_result {
            Ok(entry) => {
                progress.entries_scanned = progress.entries_scanned.saturating_add(1);
                progress.current_path = Some(entry.path().to_path_buf());

                if entry.depth() == 0 {
                    continue;
                }

                if entry.file_type().is_dir() {
                    progress.directories_scanned = progress.directories_scanned.saturating_add(1);
                } else {
                    if let Some(max_files) = config.max_files {
                        if progress.files_scanned as usize >= max_files {
                            progress.truncated = true;
                            break;
                        }
                    }

                    progress.files_scanned = progress.files_scanned.saturating_add(1);
                }

                let relative_path = match entry.path().strip_prefix(root_path) {
                    Ok(path) => path,
                    Err(_) => continue,
                };

                if relative_path.as_os_str().is_empty() {
                    continue;
                }

                let size = if entry.file_type().is_dir() {
                    0
                } else {
                    match fs::symlink_metadata(entry.path()) {
                        Ok(metadata) => metadata.len(),
                        Err(error) => {
                            progress.warnings = progress.warnings.saturating_add(1);
                            warnings.push(format!(
                                "Could not read metadata for {}: {}",
                                entry.path().display(),
                                error
                            ));
                            0
                        }
                    }
                };

                root.insert_relative(relative_path, size);
            }
            Err(error) => {
                progress.warnings = progress.warnings.saturating_add(1);
                warnings.push(format_walkdir_error(&error));
            }
        }

        if progress.entries_scanned % config.progress_interval.max(1) as u64 == 0 {
            update_scan_progress_metrics(&mut progress, phase_started, false);
            let _ = tx.send(ScanMessage::Progress(progress.clone()));
        }
    }

    root.compute_total_size();
    root.sort_children_by_size_desc();

    update_scan_progress_metrics(&mut progress, phase_started, true);
    let _ = tx.send(ScanMessage::Progress(progress.clone()));

    Ok(ScanResult {
        root,
        stats: ScanStats {
            entries_scanned: progress.entries_scanned,
            files_scanned: progress.files_scanned,
            directories_scanned: progress.directories_scanned,
            warnings: progress.warnings,
            truncated: progress.truncated,
            estimated_total_entries: progress.total_estimated_entries,
            elapsed: Duration::ZERO,
        },
        warnings,
    })
}

fn update_scan_progress_metrics(progress: &mut ScanProgress, started: Instant, finished: bool) {
    let total_estimated_entries = progress.total_estimated_entries.unwrap_or(1).max(1);

    let mut percent = progress.entries_scanned as f32 / total_estimated_entries as f32 * 100.0;

    if finished {
        percent = 100.0;
    } else {
        percent = percent.clamp(0.0, 99.9);
    }

    if let Some(previous) = progress.progress_percent {
        if !finished {
            percent = percent.max(previous);
        }
    }

    progress.progress_percent = Some(percent);

    let remaining_entries = if finished {
        0
    } else {
        total_estimated_entries.saturating_sub(progress.entries_scanned)
    };

    progress.remaining_estimated_entries = Some(remaining_entries);

    if finished {
        progress.eta = Some(Duration::ZERO);
        return;
    }

    if progress.entries_scanned == 0 {
        progress.eta = None;
        return;
    }

    let elapsed_seconds = started.elapsed().as_secs_f64();
    if elapsed_seconds <= 0.0 {
        progress.eta = None;
        return;
    }

    let entries_per_second = progress.entries_scanned as f64 / elapsed_seconds;
    if entries_per_second <= 0.0 {
        progress.eta = None;
        return;
    }

    let eta_seconds = remaining_entries as f64 / entries_per_second;
    progress.eta = Some(Duration::from_secs_f64(eta_seconds.max(0.0)));
}

fn format_walkdir_error(error: &walkdir::Error) -> String {
    if let Some(path) = error.path() {
        return format!("Could not access {}: {}", path.display(), error);
    }

    format!("Walkdir error: {error}")
}
