use crate::format::human_size;
use crate::model::Node;
use crate::scanner::{spawn_scan, ScanConfig, ScanMessage, ScanPhase, ScanProgress, ScanResult};
use crate::treemap::{squarified_treemap, LayoutRect};
use eframe::egui::{self, Color32};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ACTION_LOG_CAPACITY: usize = 500;
const MAX_VISIBLE_LINES: usize = 30;
const LINE_LIFETIME_SECONDS: f32 = 5.0;
const MIN_ZOOM_FACTOR: f32 = 0.2;
const MAX_ZOOM_FACTOR: f32 = 10.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    AwaitingDirectory,
    Scanning,
    Ready,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
    English,
    Chinese,
}

impl Language {
    fn toggle(&mut self) {
        *self = match self {
            Self::English => Self::Chinese,
            Self::Chinese => Self::English,
        };
    }
}

#[derive(Debug, Clone)]
struct HoveredEntry {
    name: String,
    path: PathBuf,
    size: u64,
    is_dir: bool,
}

#[derive(Debug, Clone)]
struct CachedCell {
    rect: egui::Rect,
    name: String,
    path: PathBuf,
    size: u64,
    is_dir: bool,
    fill: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AliasKind {
    File,
    Folder,
}

#[derive(Debug, Clone)]
struct AliasEntry {
    code: String,
    kind: AliasKind,
}

#[derive(Debug, Clone)]
struct TypeStat {
    key: String,
    bytes: u64,
    files: u64,
    color: Color32,
}

#[derive(Debug, Clone)]
struct TreemapCache {
    scan_generation: u64,
    depth: usize,
    max_nodes: usize,
    min_cell_pixels: f32,
    canvas_min: egui::Pos2,
    width_px: u32,
    height_px: u32,
    cells: Vec<CachedCell>,
    cell_centers: HashMap<PathBuf, egui::Pos2>,
    cell_centers_by_key: HashMap<String, egui::Pos2>,
}

#[derive(Debug, Clone)]
struct ActionLogEntry {
    timestamp: SystemTime,
    target_path: PathBuf,
    action_type: String,
}

#[derive(Clone, Default)]
struct ActionLog {
    entries: Arc<Mutex<VecDeque<ActionLogEntry>>>,
}

impl ActionLog {
    fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(VecDeque::with_capacity(ACTION_LOG_CAPACITY))),
        }
    }

    fn push(&self, target_path: PathBuf, action_type: impl Into<String>) {
        let Ok(mut entries) = self.entries.try_lock() else {
            return;
        };

        if entries.len() >= ACTION_LOG_CAPACITY {
            entries.pop_front();
        }

        entries.push_back(ActionLogEntry {
            timestamp: SystemTime::now(),
            target_path,
            action_type: action_type.into(),
        });
    }

    fn latest(&self) -> Option<ActionLogEntry> {
        let Ok(entries) = self.entries.try_lock() else {
            return None;
        };

        entries.back().cloned()
    }

    fn len(&self) -> usize {
        let Ok(entries) = self.entries.try_lock() else {
            return 0;
        };

        entries.len()
    }

    fn clear(&self) {
        let Ok(mut entries) = self.entries.try_lock() else {
            return;
        };

        entries.clear();
    }
}

#[derive(Debug, Clone)]
struct VisualActionLine {
    timestamp: SystemTime,
    target_path: PathBuf,
    opacity: f32,
    age: f32,
}

pub struct TreeMapApp {
    mode: AppMode,
    language: Language,
    agent_path: Option<PathBuf>,
    root_path: Option<PathBuf>,
    scan_config: ScanConfig,
    scan_receiver: Option<Receiver<ScanMessage>>,
    scan_progress: ScanProgress,
    scan_result: Option<ScanResult>,
    error_message: Option<String>,
    treemap_depth: usize,
    max_render_nodes: usize,
    min_cell_pixels: f32,
    show_cell_labels: bool,
    demo_mode: bool,
    zoom_factor: f32,
    offset: egui::Vec2,
    startup_prompted: bool,
    scan_generation: u64,
    treemap_cache: Option<TreemapCache>,
    hovered_entry: Option<HoveredEntry>,
    type_stats: Vec<TypeStat>,
    total_file_bytes: u64,
    legend_top_n: usize,
    alias_map: HashMap<PathBuf, AliasEntry>,
    action_log: ActionLog,
    visual_lines: VecDeque<VisualActionLine>,
}

impl TreeMapApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        configure_fonts_for_cjk(&creation_context.egui_ctx);
        let scan_config = ScanConfig::default();

        Self {
            mode: AppMode::AwaitingDirectory,
            language: Language::English,
            agent_path: None,
            root_path: None,
            scan_config,
            scan_receiver: None,
            scan_progress: ScanProgress::default(),
            scan_result: None,
            error_message: None,
            treemap_depth: 8,
            max_render_nodes: 20_000,
            min_cell_pixels: 1.0,
            show_cell_labels: true,
            demo_mode: false,
            zoom_factor: 1.0,
            offset: egui::Vec2::ZERO,
            startup_prompted: false,
            scan_generation: 0,
            treemap_cache: None,
            hovered_entry: None,
            type_stats: Vec::new(),
            total_file_bytes: 0,
            legend_top_n: 12,
            alias_map: HashMap::new(),
            action_log: ActionLog::new(),
            visual_lines: VecDeque::with_capacity(MAX_VISIBLE_LINES),
        }
    }

    fn t<'a>(&self, english: &'a str, chinese: &'a str) -> &'a str {
        match self.language {
            Language::English => english,
            Language::Chinese => chinese,
        }
    }

    fn demo_name(&self, real_name: &str, path: &PathBuf, is_dir: bool) -> String {
        if !self.demo_mode {
            return real_name.to_string();
        }

        if let Some(alias) = self.alias_map.get(path) {
            return self.alias_display(alias);
        }

        if is_dir {
            self.t("Folder ?", "文件夹 ?").to_string()
        } else {
            self.t("File ?", "文件 ?").to_string()
        }
    }

    fn demo_path(&self, path: &PathBuf) -> String {
        if !self.demo_mode {
            return path.display().to_string();
        }

        self.alias_path(path)
    }

    fn alias_display(&self, alias: &AliasEntry) -> String {
        match alias.kind {
            AliasKind::File => format!("{}{}", self.t("File ", "文件 "), alias.code),
            AliasKind::Folder => format!("{}{}", self.t("Folder ", "文件夹 "), alias.code),
        }
    }

    fn alias_path(&self, path: &PathBuf) -> String {
        let Some(root_path) = &self.root_path else {
            return self.t("(hidden)", "（已隐藏）").to_string();
        };

        if !path.starts_with(root_path) {
            return self.t("(hidden)", "（已隐藏）").to_string();
        }

        let mut parts = Vec::new();
        if let Some(root_alias) = self.alias_map.get(root_path) {
            parts.push(self.alias_display(root_alias));
        }

        if let Ok(relative_path) = path.strip_prefix(root_path) {
            let mut current = root_path.clone();
            for component in relative_path.components() {
                current.push(component.as_os_str());
                if let Some(alias) = self.alias_map.get(&current) {
                    parts.push(self.alias_display(alias));
                }
            }
        }

        if parts.is_empty() {
            self.t("(hidden)", "（已隐藏）").to_string()
        } else {
            parts.join(" / ")
        }
    }

    fn world_to_screen(&self, position: egui::Pos2) -> egui::Pos2 {
        egui::pos2(
            position.x * self.zoom_factor + self.offset.x,
            position.y * self.zoom_factor + self.offset.y,
        )
    }

    fn screen_to_world(&self, position: egui::Pos2) -> egui::Pos2 {
        let zoom = self.zoom_factor.max(MIN_ZOOM_FACTOR);
        egui::pos2(
            (position.x - self.offset.x) / zoom,
            (position.y - self.offset.y) / zoom,
        )
    }

    fn transform_rect_for_view(&self, rect: egui::Rect) -> egui::Rect {
        egui::Rect::from_min_max(
            self.world_to_screen(rect.min),
            self.world_to_screen(rect.max),
        )
    }

    fn handle_pan_and_zoom(&mut self, ctx: &egui::Context, canvas_response: &egui::Response) {
        let middle_drag_delta = ctx.input(|input| {
            if input.pointer.button_down(egui::PointerButton::Middle) && canvas_response.hovered() {
                input.pointer.delta()
            } else {
                egui::Vec2::ZERO
            }
        });

        if middle_drag_delta != egui::Vec2::ZERO {
            self.offset += middle_drag_delta;
            ctx.request_repaint_after(Duration::from_millis(16));
        }

        if !canvas_response.hovered() {
            return;
        }

        let scroll_y = ctx.input(|input| input.raw_scroll_delta.y);
        if scroll_y.abs() <= f32::EPSILON {
            return;
        }

        let old_zoom = self.zoom_factor;
        let zoom_multiplier = (scroll_y * 0.0015).exp();
        let new_zoom = (old_zoom * zoom_multiplier).clamp(MIN_ZOOM_FACTOR, MAX_ZOOM_FACTOR);

        if (new_zoom - old_zoom).abs() <= f32::EPSILON {
            return;
        }

        if let Some(cursor_pos) = ctx.input(|input| input.pointer.hover_pos()) {
            // Keep the world point under the cursor fixed while zooming.
            let world_at_cursor = egui::pos2(
                (cursor_pos.x - self.offset.x) / old_zoom,
                (cursor_pos.y - self.offset.y) / old_zoom,
            );
            self.offset = egui::vec2(
                cursor_pos.x - world_at_cursor.x * new_zoom,
                cursor_pos.y - world_at_cursor.y * new_zoom,
            );
        }

        self.zoom_factor = new_zoom;
        ctx.request_repaint_after(Duration::from_millis(16));
    }

    fn log_action(&mut self, target_path: PathBuf, action_type: impl Into<String>) {
        let action_type = action_type.into();
        self.action_log.push(target_path.clone(), action_type);
        self.visual_lines.push_back(VisualActionLine {
            timestamp: SystemTime::now(),
            target_path,
            opacity: 1.0,
            age: 0.0,
        });
        while self.visual_lines.len() > MAX_VISIBLE_LINES {
            self.visual_lines.pop_front();
        }
    }

    fn simulate_agent_activity(&mut self) {
        let Some(cache) = self.treemap_cache.as_ref() else {
            return;
        };

        if cache.cells.is_empty() {
            return;
        }

        const ACTION_TYPES: [&str; 6] = [
            "inspect",
            "classify",
            "correlate",
            "trace",
            "verify",
            "highlight",
        ];

        let total = cache.cells.len();
        let event_count = total.min(6);
        let mut seed = time_seed();
        let mut selected = Vec::with_capacity(event_count);

        for offset in 0..event_count {
            seed = next_seed(seed ^ ((offset as u64 + 1) * 0x9E37_79B9));
            let index = (seed as usize) % total;
            let action_type = ACTION_TYPES[(seed as usize) % ACTION_TYPES.len()];
            if let Some(cell) = cache.cells.get(index) {
                selected.push((cell.path.clone(), action_type));
            }
        }

        for (path, action_type) in selected {
            self.log_action(path, action_type);
        }
    }

    fn update_visual_lines(&mut self, delta_seconds: f32) {
        let dt = delta_seconds.max(0.0);
        let now = SystemTime::now();

        for line in &mut self.visual_lines {
            let age_from_timestamp = now
                .duration_since(line.timestamp)
                .unwrap_or(Duration::ZERO)
                .as_secs_f32();
            line.age = (line.age + dt).max(age_from_timestamp);
        }

        while self
            .visual_lines
            .front()
            .map(|line| line.age > LINE_LIFETIME_SECONDS)
            .unwrap_or(false)
        {
            self.visual_lines.pop_front();
        }

        let total = self.visual_lines.len();
        for (idx, line) in self.visual_lines.iter_mut().enumerate() {
            let rank_from_newest = total.saturating_sub(idx + 1);
            let base_opacity = if rank_from_newest < 10 {
                1.0
            } else if rank_from_newest < 20 {
                0.5
            } else {
                0.2
            };
            let fade = (1.0 - line.age / LINE_LIFETIME_SECONDS).clamp(0.0, 1.0);
            line.opacity = base_opacity * fade;
        }
    }

    fn resolve_openclaw_world_pos(&self, cache: &TreemapCache) -> Option<egui::Pos2> {
        let agent_path = self.agent_path.as_ref()?;
        let agent_key = normalize_path_key(agent_path);

        if let Some(pos) = cache.cell_centers.get(agent_path) {
            return Some(*pos);
        }

        if let Some(pos) = cache.cell_centers_by_key.get(&agent_key) {
            return Some(*pos);
        }

        let mut candidate = agent_path.clone();
        while candidate.pop() {
            if let Some(root) = &self.root_path {
                if !path_within_root(&candidate, root) {
                    break;
                }
            }

            if let Some(pos) = cache.cell_centers.get(&candidate) {
                return Some(*pos);
            }

            let candidate_key = normalize_path_key(&candidate);
            if let Some(pos) = cache.cell_centers_by_key.get(&candidate_key) {
                return Some(*pos);
            }
        }

        None
    }

    fn render_openclaw_overlay(
        &self,
        painter: &egui::Painter,
        cache: &TreemapCache,
        canvas_rect: egui::Rect,
    ) -> bool {
        let Some(openclaw_world_pos) = self.resolve_openclaw_world_pos(cache) else {
            return false;
        };
        let openclaw_pos = self.world_to_screen(openclaw_world_pos);
        painter.circle_filled(openclaw_pos, 6.0, Color32::from_rgb(208, 58, 58));
        painter.text(
            openclaw_pos + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "OpenCLAW",
            egui::FontId::proportional(12.0),
            Color32::from_rgb(255, 210, 210),
        );

        let mut has_visible_line = false;
        for line in &self.visual_lines {
            if line.opacity <= 0.0 {
                continue;
            }

            let Some(target_world_pos) = cache.cell_centers.get(&line.target_path) else {
                continue;
            };
            let target_pos = self.world_to_screen(*target_world_pos);

            if !canvas_rect.expand(32.0).contains(target_pos) {
                continue;
            }

            let alpha = (line.opacity * 255.0).round().clamp(0.0, 255.0) as u8;
            let line_color = Color32::from_rgba_unmultiplied(255, 0, 0, alpha);
            painter.line_segment(
                [target_pos, openclaw_pos],
                egui::Stroke::new(1.0, line_color),
            );
            has_visible_line = true;
        }

        has_visible_line
    }

    fn pick_agent_path(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(self.t("Select OpenCLAW location", "选择 OpenCLAW 位置"))
            .pick_folder()
    }

    fn pick_and_scan(&mut self) {
        if let Some(directory) = rfd::FileDialog::new()
            .set_title(self.t("Select root directory", "选择根目录"))
            .pick_folder()
        {
            self.start_scan(directory);
        }
    }

    fn pick_startup_paths_and_scan(&mut self) {
        let Some(agent_path) = self.pick_agent_path() else {
            self.mode = AppMode::AwaitingDirectory;
            return;
        };

        let Some(root_path) = rfd::FileDialog::new()
            .set_title(self.t("Select root directory", "选择根目录"))
            .pick_folder()
        else {
            self.mode = AppMode::AwaitingDirectory;
            return;
        };

        self.agent_path = Some(agent_path);
        self.start_scan(root_path);
    }

    fn start_scan(&mut self, root_path: PathBuf) {
        self.scan_generation = self.scan_generation.wrapping_add(1);
        self.root_path = Some(root_path.clone());
        self.mode = AppMode::Scanning;
        self.error_message = None;
        self.scan_result = None;
        self.scan_progress = ScanProgress::default();
        self.hovered_entry = None;
        self.treemap_cache = None;
        self.type_stats.clear();
        self.total_file_bytes = 0;
        self.alias_map.clear();
        self.action_log.clear();
        self.visual_lines.clear();
        self.scan_receiver = Some(spawn_scan(root_path, self.scan_config.clone()));
    }

    fn poll_scan_messages(&mut self, ctx: &egui::Context) {
        if self.mode != AppMode::Scanning {
            return;
        }

        let mut final_result = None;

        if let Some(receiver) = &self.scan_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(ScanMessage::Progress(progress)) => {
                        self.scan_progress = progress;
                    }
                    Ok(ScanMessage::Finished(result)) => {
                        final_result = Some(result);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        final_result =
                            Some(Err("Scan worker disconnected unexpectedly".to_string()));
                        break;
                    }
                }
            }
        }

        if let Some(result) = final_result {
            self.scan_receiver = None;

            match result {
                Ok(result) => {
                    self.treemap_depth = self.treemap_depth.min(self.scan_config.max_depth.max(1));
                    let (type_stats, total_file_bytes) = compute_type_stats(&result.root);
                    self.alias_map = build_alias_map(&result.root);
                    self.scan_result = Some(result);
                    self.type_stats = type_stats;
                    self.total_file_bytes = total_file_bytes;
                    self.mode = AppMode::Ready;
                    self.treemap_cache = None;
                }
                Err(error) => {
                    self.error_message = Some(error);
                    self.mode = AppMode::Error;
                }
            }
        } else {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn render_top_bar(&mut self, ui: &mut egui::Ui) {
        let scanning = self.mode == AppMode::Scanning;

        ui.horizontal_wrapped(|ui| {
            if ui
                .button(self.t("Select OpenCLAW location...", "选择 OpenCLAW 位置..."))
                .clicked()
            {
                if let Some(path) = self.pick_agent_path() {
                    self.agent_path = Some(path);
                    self.visual_lines.clear();
                }
            }

            if let Some(agent) = &self.agent_path {
                let agent_text = self.demo_path(agent);
                ui.label(format!(
                    "{} {}",
                    self.t("OpenCLAW:", "OpenCLAW："),
                    agent_text
                ));
            } else {
                ui.label(self.t("OpenCLAW: (not selected)", "OpenCLAW：（未选择）"));
            }

            ui.separator();
            if ui
                .button(self.t("Select root directory...", "选择根目录..."))
                .clicked()
            {
                self.pick_and_scan();
            }

            if let Some(root) = &self.root_path {
                let root_text = self.demo_path(root);
                ui.label(format!("{} {}", self.t("Root:", "根目录："), root_text));
            } else {
                ui.label(self.t("Root: (not selected)", "根目录：（未选择）"));
            }

            if let (Some(agent), Some(root)) = (&self.agent_path, &self.root_path) {
                if !path_within_root(agent, root) {
                    ui.colored_label(
                        Color32::from_rgb(210, 70, 70),
                        self.t(
                            "OpenCLAW path is outside root; marker will not be shown.",
                            "OpenCLAW 路径不在根目录内，无法显示位置。",
                        ),
                    );
                }
            }

            ui.separator();
            ui.label(self.t("Max recursion depth:", "最大递归深度："));
            ui.add(egui::DragValue::new(&mut self.scan_config.max_depth).range(1..=256));

            let mut file_limit_enabled = self.scan_config.max_files.is_some();
            if ui
                .checkbox(
                    &mut file_limit_enabled,
                    self.t("File count limit", "文件数量上限"),
                )
                .changed()
            {
                if file_limit_enabled {
                    if self.scan_config.max_files.is_none() {
                        self.scan_config.max_files = Some(250_000);
                    }
                } else {
                    self.scan_config.max_files = None;
                }
            }

            if let Some(limit) = &mut self.scan_config.max_files {
                ui.add(
                    egui::DragValue::new(limit)
                        .range(1..=5_000_000)
                        .speed(250.0),
                );
            }

            let can_rescan = !scanning && self.root_path.is_some();
            if ui
                .add_enabled(can_rescan, egui::Button::new(self.t("Rescan", "重新扫描")))
                .clicked()
            {
                if let Some(root) = self.root_path.clone() {
                    self.start_scan(root);
                }
            }

            ui.separator();
            let show_labels_text = self.t("Show labels in cells", "在方块中显示名称");
            ui.checkbox(&mut self.show_cell_labels, show_labels_text);
            let demo_mode_text = self.t("Demo anonymous mode", "演示匿名模式");
            ui.checkbox(&mut self.demo_mode, demo_mode_text);
            let simulate_text = self.t("Simulate OpenCLAW", "模拟 OpenCLAW");
            if ui.button(simulate_text).clicked() {
                self.simulate_agent_activity();
            }

            let action_count = self.action_log.len();
            ui.small(format!(
                "{} {}",
                self.t("OpenCLAW actions:", "OpenCLAW 动作："),
                action_count
            ));

            if let Some(last_action) = self.action_log.latest() {
                let age_seconds = SystemTime::now()
                    .duration_since(last_action.timestamp)
                    .unwrap_or(Duration::ZERO)
                    .as_secs_f32();
                let target_text = self.demo_path(&last_action.target_path);
                ui.small(format!(
                    "{} {} ({:.1}s) | {}",
                    self.t("Last:", "最近："),
                    last_action.action_type,
                    age_seconds,
                    target_text
                ));
            }

            let language_button = match self.language {
                Language::English => "中文",
                Language::Chinese => "English",
            };
            if ui.button(language_button).clicked() {
                self.language.toggle();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(self.t("Reset View", "重置视图"))
                    .on_hover_text(self.t("Reset pan and zoom", "重置平移与缩放"))
                    .clicked()
                {
                    self.zoom_factor = 1.0;
                    self.offset = egui::Vec2::ZERO;
                }
            });
        });
    }

    fn render_status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.small(self.t(
                "Mode: read-only visualization (no file operations)",
                "模式：只读可视化（不进行文件操作）",
            ));

            if let Some(hovered) = &self.hovered_entry {
                let name_text = self.demo_name(&hovered.name, &hovered.path, hovered.is_dir);
                let path_text = self.demo_path(&hovered.path);
                ui.separator();
                ui.small(format!(
                    "{} | {} | {}",
                    name_text,
                    human_size(hovered.size),
                    path_text
                ));
            } else if let Some(root) = &self.root_path {
                let root_text = self.demo_path(root);
                ui.separator();
                ui.small(format!(
                    "{} {}",
                    self.t(
                        "Hover a rectangle to inspect full path. Root:",
                        "把鼠标停在方块上可查看完整路径。根目录："
                    ),
                    root_text
                ));
            }
        });
    }

    fn render_warning_banner(&self, ui: &mut egui::Ui, message: &str) {
        egui::Frame::none()
            .fill(Color32::from_rgb(255, 232, 147))
            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(177, 116, 0)))
            .inner_margin(egui::Margin::same(6.0))
            .show(ui, |ui| {
                ui.colored_label(Color32::from_rgb(74, 54, 0), message);
            });
    }

    fn render_type_legend(&mut self, ui: &mut egui::Ui) {
        if self.type_stats.is_empty() || self.total_file_bytes == 0 {
            return;
        }

        ui.horizontal(|ui| {
            ui.label(self.t("Top N types:", "前 N 个类型："));
            ui.add(
                egui::DragValue::new(&mut self.legend_top_n)
                    .range(3..=30)
                    .speed(0.2),
            );
        });

        egui::CollapsingHeader::new(self.t("Type Legend", "类型图例"))
            .default_open(true)
            .show(ui, |ui| {
                let count = self.legend_top_n.min(self.type_stats.len());
                for stat in self.type_stats.iter().take(count) {
                    let ratio = stat.bytes as f32 / self.total_file_bytes as f32;
                    let percent = ratio * 100.0;

                    ui.horizontal(|ui| {
                        let (swatch_rect, _) =
                            ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                        ui.painter().rect_filled(swatch_rect, 2.0, stat.color);

                        ui.label(format_type_key(&stat.key, self.language));
                        ui.add(
                            egui::ProgressBar::new(ratio.clamp(0.0, 1.0))
                                .desired_width(160.0)
                                .text(format!("{percent:.1}%")),
                        );
                        ui.label(human_size(stat.bytes));
                        ui.small(format!("{} {}", stat.files, self.t("files", "个文件")));
                    });
                }
            });
    }

    fn render_scanning_state(&self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.spinner();
            ui.heading(self.t("Scanning directory...", "正在扫描目录..."));
            ui.label(self.t(
                "Read-only scan in progress (no file operations are performed).",
                "正在进行只读扫描（不会执行任何文件操作）。",
            ));
            ui.add_space(12.0);

            let phase_text = match self.scan_progress.phase {
                ScanPhase::Counting => self.t(
                    "Phase 1/2: estimating total work...",
                    "阶段 1/2：正在估算总工作量...",
                ),
                ScanPhase::Scanning => self.t(
                    "Phase 2/2: building tree and sizes...",
                    "阶段 2/2：正在构建树结构与大小...",
                ),
            };
            ui.label(phase_text);

            if let Some(percent) = self.scan_progress.progress_percent {
                let ratio = (percent / 100.0).clamp(0.0, 1.0);
                ui.add(
                    egui::ProgressBar::new(ratio)
                        .desired_width(460.0)
                        .show_percentage()
                        .text(format!("{percent:.1}%")),
                );
            }

            if let Some(remaining_entries) = self.scan_progress.remaining_estimated_entries {
                if self.scan_progress.phase == ScanPhase::Scanning {
                    ui.small(format!(
                        "{} {}",
                        self.t("Estimated remaining entries:", "预计剩余条目："),
                        remaining_entries
                    ));
                }
            }

            if let Some(eta) = self.scan_progress.eta {
                if self.scan_progress.phase == ScanPhase::Scanning && eta > Duration::ZERO {
                    ui.small(format!(
                        "{} {}",
                        self.t("Estimated remaining time:", "预计剩余时间："),
                        format_duration_compact(eta)
                    ));
                }
            }

            ui.label(format!(
                "{} {} | {} {} | {} {} | {} {}",
                self.t("Entries:", "条目："),
                self.scan_progress.entries_scanned,
                self.t("Files:", "文件："),
                self.scan_progress.files_scanned,
                self.t("Directories:", "目录："),
                self.scan_progress.directories_scanned,
                self.t("Warnings:", "警告："),
                self.scan_progress.warnings
            ));

            if let Some(path) = &self.scan_progress.current_path {
                let current_path_text = if self.demo_mode {
                    self.t("(hidden during scan)", "（扫描中已隐藏）")
                        .to_string()
                } else {
                    path.display().to_string()
                };
                ui.small(format!(
                    "{} {}",
                    self.t("Current:", "当前："),
                    current_path_text
                ));
            }

            if self.scan_progress.truncated {
                self.render_warning_banner(
                    ui,
                    self.t(
                        "File limit reached. Increase the limit if you want a fuller scan.",
                        "已达到文件数量上限。若要更完整结果，请调高上限。",
                    ),
                );
            }
        });
    }

    fn render_error_state(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(50.0);
            ui.heading(self.t("Scan failed", "扫描失败"));

            if let Some(error) = &self.error_message {
                ui.colored_label(Color32::from_rgb(210, 70, 70), error);
            }

            if ui
                .button(self.t("Pick another directory", "选择其他目录"))
                .clicked()
            {
                self.pick_and_scan();
            }
        });
    }

    fn cache_needs_rebuild(&self, canvas_min: egui::Pos2, width_px: u32, height_px: u32) -> bool {
        match &self.treemap_cache {
            Some(cache) => {
                cache.scan_generation != self.scan_generation
                    || cache.depth != self.treemap_depth
                    || cache.max_nodes != self.max_render_nodes
                    || cache.canvas_min.distance(canvas_min) > f32::EPSILON
                    || cache.width_px != width_px
                    || cache.height_px != height_px
                    || (cache.min_cell_pixels - self.min_cell_pixels).abs() > f32::EPSILON
            }
            None => true,
        }
    }

    fn build_treemap_cache(
        scan_result: &ScanResult,
        canvas_rect: egui::Rect,
        scan_generation: u64,
        depth: usize,
        max_nodes: usize,
        min_cell_pixels: f32,
    ) -> TreemapCache {
        let bounds = LayoutRect::new(
            canvas_rect.min.x,
            canvas_rect.min.y,
            canvas_rect.width(),
            canvas_rect.height(),
        );

        let raw_cells = squarified_treemap(&scan_result.root, bounds, depth, max_nodes);

        let mut cells = Vec::with_capacity(raw_cells.len());
        let mut cell_centers = HashMap::with_capacity(raw_cells.len());
        let mut cell_centers_by_key = HashMap::with_capacity(raw_cells.len());

        for cell in raw_cells {
            let rect = egui::Rect::from_min_size(
                egui::pos2(cell.rect.x, cell.rect.y),
                egui::vec2(cell.rect.w, cell.rect.h),
            );

            let path = cell.node.path.clone();
            cell_centers.insert(path.clone(), rect.center());
            cell_centers_by_key.insert(normalize_path_key(&path), rect.center());

            if cell.depth == 0 {
                continue;
            }

            if rect.width() < min_cell_pixels || rect.height() < min_cell_pixels {
                continue;
            }

            cells.push(CachedCell {
                rect,
                name: cell.node.name.clone(),
                path,
                size: cell.node.size,
                is_dir: !cell.node.children.is_empty(),
                fill: color_for_node(cell.node, cell.depth),
            });
        }

        TreemapCache {
            scan_generation,
            depth,
            max_nodes,
            min_cell_pixels,
            canvas_min: canvas_rect.min,
            width_px: canvas_rect.width().round().max(1.0) as u32,
            height_px: canvas_rect.height().round().max(1.0) as u32,
            cells,
            cell_centers,
            cell_centers_by_key,
        }
    }

    fn render_ready_state(&mut self, ui: &mut egui::Ui) {
        let has_readable_files = {
            let Some(scan_result) = self.scan_result.as_ref() else {
                ui.label(self.t("No scan results yet.", "尚无扫描结果。"));
                return;
            };

            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{} {}",
                    self.t("Total size:", "总大小："),
                    human_size(scan_result.root.size)
                ));
                ui.separator();
                ui.label(format!(
                    "{} {}",
                    self.t("Entries:", "条目："),
                    scan_result.stats.entries_scanned
                ));
                if let Some(estimated_total_entries) = scan_result.stats.estimated_total_entries {
                    ui.label(format!(
                        "{} {}",
                        self.t("Estimated total entries:", "预计总条目："),
                        estimated_total_entries
                    ));
                }
                ui.label(format!(
                    "{} {}",
                    self.t("Files:", "文件："),
                    scan_result.stats.files_scanned
                ));
                ui.label(format!(
                    "{} {}",
                    self.t("Directories:", "目录："),
                    scan_result.stats.directories_scanned
                ));
                ui.label(format!(
                    "{} {:.2?}",
                    self.t("Elapsed:", "耗时："),
                    scan_result.stats.elapsed
                ));
                ui.label(format!(
                    "{} {}",
                    self.t("Warnings:", "警告："),
                    scan_result.stats.warnings
                ));
            });

            if scan_result.stats.truncated {
                self.render_warning_banner(
                    ui,
                    self.t(
                        "Result is partial because the file count limit was reached.",
                        "结果不完整：已达到文件数量上限。",
                    ),
                );
            }

            if !scan_result.warnings.is_empty() {
                egui::CollapsingHeader::new(format!(
                    "{} ({})",
                    self.t("Warnings", "警告"),
                    scan_result.warnings.len()
                ))
                .default_open(false)
                .show(ui, |ui| {
                    for warning in scan_result.warnings.iter().take(20) {
                        ui.small(warning);
                    }

                    if scan_result.warnings.len() > 20 {
                        ui.small(format!(
                            "{} {} {}",
                            self.t("... and", "... 还有"),
                            scan_result.warnings.len() - 20,
                            self.t("additional warnings", "条警告")
                        ));
                    }
                });
            }

            scan_result.root.size > 0
        };

        ui.separator();

        ui.horizontal(|ui| {
            ui.label(self.t("Treemap depth:", "Treemap 深度："));
            ui.add(
                egui::DragValue::new(&mut self.treemap_depth)
                    .range(1..=self.scan_config.max_depth.max(1)),
            );

            ui.label(self.t("Max rendered nodes:", "最大渲染节点："));
            ui.add(
                egui::DragValue::new(&mut self.max_render_nodes)
                    .range(1_000..=200_000)
                    .speed(500.0),
            );

            ui.label(self.t("Min cell px:", "最小方块像素："));
            ui.add(
                egui::DragValue::new(&mut self.min_cell_pixels)
                    .range(0.5..=8.0)
                    .speed(0.1),
            );
        });

        self.render_type_legend(ui);

        ui.add_space(4.0);

        if !has_readable_files {
            ui.label(self.t(
                "No readable files were found in this directory.",
                "此目录中没有可读取的文件。",
            ));
            return;
        }

        let available = ui.available_size();
        if available.x < 40.0 || available.y < 40.0 {
            return;
        }

        let (canvas_rect, canvas_response) =
            ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        self.handle_pan_and_zoom(ui.ctx(), &canvas_response);
        let width_px = canvas_rect.width().round().max(1.0) as u32;
        let height_px = canvas_rect.height().round().max(1.0) as u32;

        if self.cache_needs_rebuild(canvas_rect.min, width_px, height_px) {
            let Some(scan_result) = self.scan_result.as_ref() else {
                return;
            };

            let rebuilt = Self::build_treemap_cache(
                scan_result,
                canvas_rect,
                self.scan_generation,
                self.treemap_depth,
                self.max_render_nodes,
                self.min_cell_pixels,
            );

            self.treemap_cache = Some(rebuilt);
        }

        let Some(cache) = self.treemap_cache.as_ref() else {
            return;
        };

        let painter = ui.painter_at(canvas_rect);
        painter.rect_filled(canvas_rect, 0.0, Color32::from_rgb(26, 30, 34));

        for cell in &cache.cells {
            let transformed_rect = self.transform_rect_for_view(cell.rect);
            if !transformed_rect.intersects(canvas_rect) {
                continue;
            }

            painter.rect_filled(transformed_rect, 0.0, cell.fill);
            painter.rect_stroke(
                transformed_rect,
                0.0,
                egui::Stroke::new(1.0, Color32::from_black_alpha(45)),
            );

            if self.show_cell_labels
                && transformed_rect.width() > 95.0
                && transformed_rect.height() > 20.0
            {
                let label_name = self.demo_name(&cell.name, &cell.path, cell.is_dir);
                let label = format!("{} ({})", label_name, human_size(cell.size));
                let max_chars = (transformed_rect.width() / 7.0).floor().max(6.0) as usize;
                let text = truncate_label(&label, max_chars);

                painter.text(
                    transformed_rect.left_top() + egui::vec2(4.0, 4.0),
                    egui::Align2::LEFT_TOP,
                    text,
                    egui::TextStyle::Small.resolve(ui.style()),
                    Color32::WHITE,
                );
            }
        }

        let has_active_lines = self.render_openclaw_overlay(&painter, cache, canvas_rect);
        if has_active_lines {
            ui.ctx().request_repaint_after(Duration::from_millis(33));
        }

        let hovered_snapshot = if canvas_response.hovered() {
            let pointer_pos = ui.ctx().input(|input| input.pointer.hover_pos());

            pointer_pos.and_then(|pos| {
                let world_pos = self.screen_to_world(pos);
                cache
                    .cells
                    .iter()
                    .rev()
                    .find(|cell| cell.rect.contains(world_pos))
                    .map(|cell| HoveredEntry {
                        name: cell.name.clone(),
                        path: cell.path.clone(),
                        size: cell.size,
                        is_dir: cell.is_dir,
                    })
            })
        } else {
            None
        };

        self.hovered_entry = hovered_snapshot.clone();

        if let Some(hovered) = hovered_snapshot {
            #[allow(deprecated)]
            let _ = egui::show_tooltip_at_pointer(
                ui.ctx(),
                ui.layer_id(),
                egui::Id::new("treemap_hover"),
                |ui| {
                    ui.set_min_width(420.0);
                    let type_text = if hovered.is_dir {
                        self.t("Folder", "文件夹").to_string()
                    } else {
                        let type_key = file_type_key(&hovered.path);
                        format_type_key(&type_key, self.language)
                    };
                    let name_text = self.demo_name(&hovered.name, &hovered.path, hovered.is_dir);
                    let path_text = self.demo_path(&hovered.path);
                    ui.label(format!("{} {}", self.t("Name:", "名称："), name_text));
                    ui.label(format!("{} {}", self.t("Type:", "类型："), type_text));
                    ui.label(format!(
                        "{} {}",
                        self.t("Size:", "大小："),
                        human_size(hovered.size)
                    ));
                    ui.label(format!("{} {}", self.t("Path:", "路径："), path_text));
                },
            );
        }
    }
}

impl eframe::App for TreeMapApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let delta_seconds = ctx.input(|input| input.stable_dt);
        self.update_visual_lines(delta_seconds);
        if !self.visual_lines.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        if !self.startup_prompted {
            self.startup_prompted = true;
            self.pick_startup_paths_and_scan();
        }

        self.poll_scan_messages(ctx);

        egui::TopBottomPanel::top("top_controls").show(ctx, |ui| {
            self.render_top_bar(ui);
        });

        egui::TopBottomPanel::bottom("status_bar")
            .resizable(false)
            .show(ctx, |ui| {
                self.render_status_bar(ui);
            });

        egui::CentralPanel::default().show(ctx, |ui| match self.mode {
            AppMode::AwaitingDirectory => {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.heading("tree-map-base");
                    ui.label(self.t(
                        "Select a directory to build a read-only size treemap.",
                        "请选择一个目录来生成只读大小 Treemap。",
                    ));
                    if ui.button(self.t("Choose directory", "选择目录")).clicked() {
                        self.pick_startup_paths_and_scan();
                    }
                });
            }
            AppMode::Scanning => self.render_scanning_state(ui),
            AppMode::Ready => self.render_ready_state(ui),
            AppMode::Error => self.render_error_state(ui),
        });
    }
}

fn format_duration_compact(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        return format!("{hours}h {minutes:02}m {seconds:02}s");
    }

    if minutes > 0 {
        return format!("{minutes}m {seconds:02}s");
    }

    format!("{seconds}s")
}

fn configure_fonts_for_cjk(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    let loaded_fonts = load_system_cjk_fonts();
    let mut loaded_font_names = Vec::with_capacity(loaded_fonts.len());

    for (font_name, font_data) in loaded_fonts {
        fonts.font_data.insert(
            font_name.clone(),
            egui::FontData::from_owned(font_data).into(),
        );
        loaded_font_names.push(font_name);
    }

    if !loaded_font_names.is_empty() {
        // Insert in reverse so the first candidate keeps highest priority.
        if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            for font_name in loaded_font_names.iter().rev() {
                proportional.insert(0, font_name.clone());
            }
        }

        if let Some(monospace) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            for font_name in loaded_font_names.iter().rev() {
                monospace.insert(0, font_name.clone());
            }
        }
    }

    ctx.set_fonts(fonts);
}

fn load_system_cjk_fonts() -> Vec<(String, Vec<u8>)> {
    let mut loaded = Vec::new();
    let candidates = [
        // Prefer plain TTF fonts for maximum compatibility in egui.
        ("NotoSansTC", "C:\\Windows\\Fonts\\NotoSansTC-VF.ttf"),
        ("NotoSansHK", "C:\\Windows\\Fonts\\NotoSansHK-VF.ttf"),
        ("SimSunExtG", "C:\\Windows\\Fonts\\SimsunExtG.ttf"),
        ("SimSunBold", "C:\\Windows\\Fonts\\simsunb.ttf"),
        ("KaiU", "C:\\Windows\\Fonts\\kaiu.ttf"),
    ];

    for (name, path) in candidates {
        if let Ok(bytes) = fs::read(path) {
            loaded.push((name.to_string(), bytes));
        }
    }

    loaded
}

fn compute_type_stats(root: &Node) -> (Vec<TypeStat>, u64) {
    let mut map: HashMap<String, (u64, u64)> = HashMap::new();
    let mut total_file_bytes = 0_u64;
    collect_type_stats(root, &mut map, &mut total_file_bytes);

    let mut stats: Vec<TypeStat> = map
        .into_iter()
        .map(|(key, (bytes, files))| TypeStat {
            color: color_for_type_key(&key),
            key,
            bytes,
            files,
        })
        .collect();

    stats.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.key.cmp(&b.key)));
    (stats, total_file_bytes)
}

fn collect_type_stats(
    node: &Node,
    map: &mut HashMap<String, (u64, u64)>,
    total_file_bytes: &mut u64,
) {
    if node.children.is_empty() {
        let key = file_type_key(&node.path);
        let entry = map.entry(key).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(node.size);
        entry.1 = entry.1.saturating_add(1);
        *total_file_bytes = total_file_bytes.saturating_add(node.size);
        return;
    }

    for child in &node.children {
        collect_type_stats(child, map, total_file_bytes);
    }
}

fn format_type_key(key: &str, language: Language) -> String {
    if key == "(no_ext)" {
        return match language {
            Language::English => "(no extension)".to_string(),
            Language::Chinese => "（无扩展名）".to_string(),
        };
    }

    format!(".{key}")
}

fn color_for_node(node: &Node, depth: usize) -> Color32 {
    if !node.children.is_empty() {
        return folder_color(depth);
    }

    let key = file_type_key(&node.path);
    let base = color_for_type_key(&key);
    shade_color(base, depth)
}

fn folder_color(depth: usize) -> Color32 {
    shade_color(Color32::from_rgb(72, 78, 86), depth)
}

fn file_type_key(path: &std::path::Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "(no_ext)".to_string())
}

fn color_for_type_key(key: &str) -> Color32 {
    if key == "(no_ext)" {
        return Color32::from_rgb(122, 128, 136);
    }

    const PALETTE: [Color32; 24] = [
        Color32::from_rgb(210, 96, 96),
        Color32::from_rgb(214, 127, 78),
        Color32::from_rgb(196, 151, 72),
        Color32::from_rgb(153, 171, 72),
        Color32::from_rgb(106, 175, 87),
        Color32::from_rgb(79, 177, 120),
        Color32::from_rgb(74, 173, 153),
        Color32::from_rgb(73, 166, 179),
        Color32::from_rgb(76, 152, 194),
        Color32::from_rgb(88, 137, 204),
        Color32::from_rgb(109, 124, 209),
        Color32::from_rgb(128, 112, 207),
        Color32::from_rgb(149, 104, 197),
        Color32::from_rgb(173, 98, 185),
        Color32::from_rgb(191, 95, 166),
        Color32::from_rgb(201, 96, 143),
        Color32::from_rgb(210, 106, 124),
        Color32::from_rgb(171, 126, 98),
        Color32::from_rgb(144, 140, 101),
        Color32::from_rgb(111, 146, 114),
        Color32::from_rgb(95, 147, 133),
        Color32::from_rgb(101, 142, 152),
        Color32::from_rgb(112, 132, 165),
        Color32::from_rgb(130, 121, 167),
    ];

    let index = (stable_hash(&key) % PALETTE.len() as u64) as usize;
    PALETTE[index]
}

fn shade_color(base: Color32, depth: usize) -> Color32 {
    let factor = (1.0 - depth as f32 * 0.03).clamp(0.58, 1.0);
    let [r, g, b, _] = base.to_array();

    let scaled_r = (r as f32 * factor).round().clamp(0.0, 255.0) as u8;
    let scaled_g = (g as f32 * factor).round().clamp(0.0, 255.0) as u8;
    let scaled_b = (b as f32 * factor).round().clamp(0.0, 255.0) as u8;

    Color32::from_rgb(scaled_r, scaled_g, scaled_b)
}

fn stable_hash<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn time_seed() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as u64,
        Err(_) => 0xA5A5_5A5A_1234_5678,
    }
}

fn next_seed(seed: u64) -> u64 {
    seed.wrapping_mul(6364136223846793005).wrapping_add(1)
}

fn normalize_path_key(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn path_within_root(path: &std::path::Path, root: &std::path::Path) -> bool {
    let path_key = normalize_path_key(path);
    let root_key = normalize_path_key(root);

    if path_key == root_key {
        return true;
    }

    let mut root_prefix = root_key;
    if !root_prefix.ends_with('/') {
        root_prefix.push('/');
    }

    path_key.starts_with(&root_prefix)
}

fn build_alias_map(root: &Node) -> HashMap<PathBuf, AliasEntry> {
    let mut alias_map = HashMap::new();
    let mut file_counter = 0_usize;
    let mut folder_counter = 0_usize;
    assign_alias(
        root,
        true,
        &mut alias_map,
        &mut file_counter,
        &mut folder_counter,
    );
    alias_map
}

fn assign_alias(
    node: &Node,
    is_root: bool,
    alias_map: &mut HashMap<PathBuf, AliasEntry>,
    file_counter: &mut usize,
    folder_counter: &mut usize,
) {
    let is_dir = is_root || !node.children.is_empty();
    let (kind, code) = if is_dir {
        let index = *folder_counter;
        *folder_counter = folder_counter.saturating_add(1);
        (AliasKind::Folder, alphabet_code(index))
    } else {
        let index = *file_counter;
        *file_counter = file_counter.saturating_add(1);
        (AliasKind::File, alphabet_code(index))
    };

    alias_map.insert(node.path.clone(), AliasEntry { code, kind });

    for child in &node.children {
        assign_alias(child, false, alias_map, file_counter, folder_counter);
    }
}

fn alphabet_code(mut index: usize) -> String {
    // 0 -> A, 25 -> Z, 26 -> AA
    let mut chars = Vec::new();
    loop {
        let rem = (index % 26) as u8;
        chars.push((b'A' + rem) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    chars.iter().rev().collect()
}

fn truncate_label(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    if max_chars <= 3 {
        return "...".to_string();
    }

    let mut truncated = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index + 3 >= max_chars {
            break;
        }
        truncated.push(ch);
    }

    truncated.push_str("...");
    truncated
}
