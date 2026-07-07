use std::sync::Arc;
use std::path::PathBuf;
use crossbeam_channel::{unbounded, Receiver};
use egui::{
    Color32, FontId, Pos2, Rect, RichText, Rounding, Stroke, Vec2, Align2,
};

use crate::colors::{get_category, FileCategory};
use crate::filter::Filter;
use crate::scanner::{
    get_disk_info, remove_from_tree, start_scan, CancelToken,
    FileEntry, ScanMessage, find_descendant,
};
use crate::treemap::{build_layout, LayoutNode, HEADER_H};

const BORDER_WIDTH: f32 = 1.5;
const MIN_CELL_SIZE: f32 = 5.0;
const LABEL_MIN_WIDTH: f32 = 34.0;
const LABEL_MIN_HEIGHT: f32 = 16.0;
const DEFAULT_DETAIL: u32 = 3;
const MAX_DETAIL: u32 = 8;
const REFRESH_INTERVAL_SECS: f64 = 2.0;

// Sentinel path component used to identify the injected free-space entry.
const FREE_SPACE_NAME: &str = "\u{0}__free_space__";

#[derive(Debug, Clone, PartialEq)]
enum ScanState {
    Idle,
    Scanning { path: PathBuf, bytes: u64, files: u64 },
    Done,
    Error(String),
}

/// Which entry is awaiting a delete confirmation.
struct DeleteConfirm {
    entry: Arc<FileEntry>,
    error: Option<String>,
}

pub struct AtlasApp {
    // --- Scan ---
    scan_root: Option<Arc<FileEntry>>,
    scan_state: ScanState,
    scan_rx: Option<Receiver<ScanMessage>>,
    cancel_token: Option<CancelToken>,

    // --- Disk info (for free-space display) ---
    disk_total: u64,
    disk_available: u64,

    // --- Navigation ---
    zoom_path: Vec<String>,
    current_node: Option<Arc<FileEntry>>,

    // --- Layout cache ---
    cached_layout: Vec<LayoutNode>,
    last_layout_rect: Option<Rect>,
    last_layout_node_path: Option<Vec<String>>,
    last_layout_depth: Option<u32>,

    // --- UI state ---
    path_input: String,
    filter_text: String,
    hovered_path: Option<PathBuf>,
    dark_mode: bool,
    show_legend: bool,
    show_files: bool,
    /// Whether the remaining free-space block is shown at the root level.
    show_free_space: bool,
    /// How many levels of nesting the treemap draws (SpaceSniffer "detail").
    detail_depth: u32,

    // --- Auto-refresh ---
    /// Periodically re-scan the current folder to pick up file changes.
    auto_refresh: bool,
    /// Time (egui seconds) the last auto-refresh was kicked off.
    last_refresh: f64,
    /// True while a *silent* background refresh is in flight (keeps the old
    /// view on screen until fresh data is ready, instead of blanking it).
    refreshing: bool,

    // --- Delete confirmation ---
    delete_confirm: Option<DeleteConfirm>,

    /// Set when launched with a CLI path; triggers a scan on the first frame.
    pending_scan: bool,
}

impl AtlasApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        initial_path: Option<String>,
        free_space_override: Option<bool>,
    ) -> Self {
        let mut dark_mode = true;
        let mut path_input = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .to_string_lossy()
            .into_owned();

        let mut detail_depth = DEFAULT_DETAIL;
        let mut show_free_space = true;
        let mut auto_refresh = true;

        if let Some(storage) = cc.storage {
            if let Some(val) = eframe::get_value::<bool>(storage, "atlas_dark_mode") {
                dark_mode = val;
            }
            if let Some(val) = eframe::get_value::<String>(storage, "atlas_last_path") {
                path_input = val;
            }
            if let Some(val) = eframe::get_value::<u32>(storage, "atlas_detail_depth") {
                detail_depth = val.clamp(1, MAX_DETAIL);
            }
            if let Some(val) = eframe::get_value::<bool>(storage, "atlas_show_free_space") {
                show_free_space = val;
            }
            if let Some(val) = eframe::get_value::<bool>(storage, "atlas_auto_refresh") {
                auto_refresh = val;
            }
        }

        if let Some(v) = free_space_override {
            show_free_space = v;
        }

        // A CLI path overrides the remembered path and auto-scans.
        let pending_scan = match initial_path {
            Some(p) if !p.trim().is_empty() => {
                path_input = p;
                true
            }
            _ => false,
        };

        let app = AtlasApp {
            scan_root: None,
            scan_state: ScanState::Idle,
            scan_rx: None,
            cancel_token: None,

            disk_total: 0,
            disk_available: 0,

            zoom_path: vec![],
            current_node: None,

            cached_layout: vec![],
            last_layout_rect: None,
            last_layout_node_path: None,
            last_layout_depth: None,

            path_input,
            filter_text: String::new(),
            hovered_path: None,
            dark_mode,
            show_legend: true,
            show_files: true,
            show_free_space,
            detail_depth,

            auto_refresh,
            last_refresh: 0.0,
            refreshing: false,

            delete_confirm: None,
            pending_scan,
        };

        if dark_mode {
            cc.egui_ctx.set_visuals(dark_visuals());
        } else {
            cc.egui_ctx.set_visuals(light_visuals());
        }

        app
    }

    // -----------------------------------------------------------------------
    // Scan management
    // -----------------------------------------------------------------------

    fn start_scan(&mut self) {
        if let Some(ct) = self.cancel_token.take() {
            ct.cancel();
        }

        let path = PathBuf::from(self.path_input.trim());
        if !path.exists() {
            self.scan_state = ScanState::Error(format!("Path does not exist: {}", path.display()));
            return;
        }

        // Fetch disk info upfront so we can use it for placeholder sizing
        if let Some((total, avail)) = get_disk_info(&path) {
            self.disk_total = total;
            self.disk_available = avail;
        }

        let (tx, rx) = unbounded();
        let ct = CancelToken::new();
        self.cancel_token = Some(ct.clone());
        self.scan_rx = Some(rx);
        self.scan_state = ScanState::Scanning { path: path.clone(), bytes: 0, files: 0 };
        self.scan_root = None;
        self.current_node = None;
        self.zoom_path.clear();
        self.invalidate_layout();

        start_scan(path, tx, ct);
    }

    /// Silently re-scan the current root to pick up file changes, keeping the
    /// existing view (zoom path, current node) on screen until the fresh tree
    /// is ready. Intermediate placeholder updates are ignored so the map does
    /// not flicker.
    fn refresh_scan(&mut self) {
        let path = match &self.scan_root {
            Some(root) => root.path.clone(),
            None => return,
        };
        if !path.exists() {
            return;
        }

        if let Some(ct) = self.cancel_token.take() {
            ct.cancel();
        }

        if let Some((total, avail)) = get_disk_info(&path) {
            self.disk_total = total;
            self.disk_available = avail;
        }

        let (tx, rx) = unbounded();
        let ct = CancelToken::new();
        self.cancel_token = Some(ct.clone());
        self.scan_rx = Some(rx);
        self.scan_state = ScanState::Scanning { path: path.clone(), bytes: 0, files: 0 };
        self.refreshing = true;
        // Deliberately keep scan_root / current_node / zoom_path intact.

        start_scan(path, tx, ct);
    }

    fn poll_scan(&mut self) {
        // Clone the receiver so we can hold a mutable borrow on self while processing messages
        let rx = match self.scan_rx.as_ref() {
            Some(rx) => rx.clone(),
            None => return,
        };

        loop {
            match rx.try_recv() {
                Ok(ScanMessage::Progress { path, bytes, files }) => {
                    self.scan_state = ScanState::Scanning { path, bytes, files };
                }
                Ok(ScanMessage::Update(partial)) => {
                    // During a silent refresh, keep the old tree visible and
                    // only swap in the final result — avoids placeholder flicker.
                    if !self.refreshing {
                        self.apply_tree_update(partial);
                    }
                }
                Ok(ScanMessage::Done(root)) => {
                    // Refresh disk info now that scan is complete
                    if let Some((total, avail)) = get_disk_info(&root.path) {
                        self.disk_total = total;
                        self.disk_available = avail;
                    }
                    self.apply_tree_update(root);
                    self.scan_state = ScanState::Done;
                    self.scan_rx = None;
                    self.cancel_token = None;
                    self.refreshing = false;
                    break;
                }
                Ok(ScanMessage::Error(e)) => {
                    self.scan_state = ScanState::Error(e);
                    self.scan_rx = None;
                    self.cancel_token = None;
                    self.refreshing = false;
                    break;
                }
                Err(_) => break,
            }
        }
    }

    fn apply_tree_update(&mut self, root: Arc<FileEntry>) {
        // Update current node following the zoom path into the new tree
        let new_node = if self.zoom_path.is_empty() {
            Some(Arc::clone(&root))
        } else {
            find_descendant(&root, &self.zoom_path).or_else(|| Some(Arc::clone(&root)))
        };

        self.scan_root = Some(root);
        self.current_node = new_node;
        self.invalidate_layout();
    }

    fn invalidate_layout(&mut self) {
        self.cached_layout.clear();
        self.last_layout_rect = None;
        self.last_layout_node_path = None;
        self.last_layout_depth = None;
    }

    /// Change the nesting detail level, clamped to a sane range.
    fn set_detail(&mut self, depth: u32) {
        let clamped = depth.clamp(1, MAX_DETAIL);
        if clamped != self.detail_depth {
            self.detail_depth = clamped;
            self.invalidate_layout();
        }
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    fn navigate_to(&mut self, entry: &Arc<FileEntry>) {
        if !entry.is_dir || entry.is_unscanned {
            return;
        }
        if let Some(root) = &self.scan_root {
            let root_path = root.path.clone();
            if let Ok(rel) = entry.path.strip_prefix(&root_path) {
                self.zoom_path = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                self.current_node = Some(Arc::clone(entry));
                self.invalidate_layout();
            }
        }
    }

    fn navigate_up(&mut self) {
        if self.zoom_path.is_empty() {
            return;
        }
        self.zoom_path.pop();
        if let Some(root) = &self.scan_root {
            self.current_node = if self.zoom_path.is_empty() {
                Some(Arc::clone(root))
            } else {
                find_descendant(root, &self.zoom_path)
            };
        }
        self.invalidate_layout();
    }

    fn navigate_to_breadcrumb(&mut self, idx: usize) {
        self.zoom_path.truncate(idx);
        if let Some(root) = &self.scan_root {
            self.current_node = if self.zoom_path.is_empty() {
                Some(Arc::clone(root))
            } else {
                find_descendant(root, &self.zoom_path)
            };
        }
        self.invalidate_layout();
    }

    /// Global keyboard shortcuts, ignored while a text field has focus.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (up, home, more, less, rescan) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Backspace),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                i.key_pressed(egui::Key::Minus),
                i.key_pressed(egui::Key::F5),
            )
        });
        if up {
            self.navigate_up();
        }
        if home {
            self.navigate_to_breadcrumb(0);
        }
        if more {
            self.set_detail(self.detail_depth + 1);
        }
        if less {
            self.set_detail(self.detail_depth.saturating_sub(1));
        }
        if rescan {
            self.start_scan();
        }
    }

    // -----------------------------------------------------------------------
    // Layout
    // -----------------------------------------------------------------------

    /// Build a display node for the current view.
    /// When at the root level and disk info is available, injects a free-space
    /// synthetic entry so it appears proportionally in the treemap.
    fn display_node(&self) -> Option<Arc<FileEntry>> {
        let node = self.current_node.as_ref()?;

        // Inject free-space only at the root level, once a full tree exists
        // (scan done, or an old tree still shown during a silent refresh).
        if self.show_free_space
            && self.zoom_path.is_empty()
            && (matches!(self.scan_state, ScanState::Done) || self.refreshing)
            && self.disk_available > 0
        {
            let free_entry = Arc::new(FileEntry {
                path: node.path.join(FREE_SPACE_NAME),
                name: format!(
                    "Free Space  {}",
                    humansize::format_size(self.disk_available, humansize::BINARY)
                ),
                size: self.disk_available,
                is_dir: false,
                is_unscanned: false,
                children: vec![],
                file_count: 0,
                modified: None,
            });

            let mut children = node.children.clone();
            children.push(free_entry);
            // Don't re-sort: keep free space after the real entries

            return Some(Arc::new(FileEntry {
                size: node.size + self.disk_available,
                children,
                ..(**node).clone()
            }));
        }

        Some(Arc::clone(node))
    }

    fn get_or_build_layout(&mut self, rect: Rect) -> &[LayoutNode] {
        let needs_rebuild = self.cached_layout.is_empty()
            || self.last_layout_rect != Some(rect)
            || self.last_layout_node_path.as_deref() != Some(&self.zoom_path)
            || self.last_layout_depth != Some(self.detail_depth);

        if needs_rebuild {
            if let Some(dn) = self.display_node() {
                self.cached_layout = build_layout(&dn, rect, MIN_CELL_SIZE, self.detail_depth);
            } else {
                self.cached_layout.clear();
            }
            self.last_layout_rect = Some(rect);
            self.last_layout_node_path = Some(self.zoom_path.clone());
            self.last_layout_depth = Some(self.detail_depth);
        }
        &self.cached_layout
    }

    // -----------------------------------------------------------------------
    // Delete
    // -----------------------------------------------------------------------

    fn perform_delete(&mut self, entry: Arc<FileEntry>) -> Result<(), std::io::Error> {
        if entry.is_dir {
            std::fs::remove_dir_all(&entry.path)?;
        } else {
            std::fs::remove_file(&entry.path)?;
        }

        // Update in-memory tree
        if let Some(root) = &self.scan_root {
            let new_root = remove_from_tree(root, &entry.path)
                .unwrap_or_else(|| Arc::clone(root));
            // Refresh available space
            if let Some((_total, avail)) = get_disk_info(&new_root.path) {
                self.disk_available = avail;
            }
            self.apply_tree_update(new_root);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// eframe::App
// ---------------------------------------------------------------------------

impl eframe::App for AtlasApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "atlas_dark_mode", &self.dark_mode);
        eframe::set_value(storage, "atlas_last_path", &self.path_input);
        eframe::set_value(storage, "atlas_detail_depth", &self.detail_depth);
        eframe::set_value(storage, "atlas_show_free_space", &self.show_free_space);
        eframe::set_value(storage, "atlas_auto_refresh", &self.auto_refresh);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.pending_scan {
            self.pending_scan = false;
            self.start_scan();
        }

        self.poll_scan();
        self.handle_shortcuts(ctx);

        // Keep repainting while scanning so the pulsing animation plays
        if matches!(self.scan_state, ScanState::Scanning { .. }) {
            ctx.request_repaint_after(std::time::Duration::from_millis(40));
        }

        // Auto-refresh: periodically re-scan the current folder so on-disk
        // changes show up without the user clicking Scan again.
        if self.auto_refresh
            && !self.refreshing
            && matches!(self.scan_state, ScanState::Done)
            && self.scan_root.is_some()
        {
            let now = ctx.input(|i| i.time);
            if self.last_refresh <= 0.0 {
                // Baseline the timer on the first completed scan.
                self.last_refresh = now;
            } else if now - self.last_refresh >= REFRESH_INTERVAL_SECS {
                self.last_refresh = now;
                self.refresh_scan();
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        // Delete confirmation dialog (modal window)
        self.draw_delete_dialog(ctx);

        egui::TopBottomPanel::top("toolbar")
            .min_height(48.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| self.draw_toolbar(ui, ctx));
                ui.add_space(4.0);
            });

        egui::TopBottomPanel::bottom("statusbar")
            .min_height(24.0)
            .show(ctx, |ui| self.draw_statusbar(ui));

        egui::CentralPanel::default()
            .show(ctx, |ui| self.draw_treemap(ui, ctx));
    }
}

// ---------------------------------------------------------------------------
// UI drawing methods
// ---------------------------------------------------------------------------

impl AtlasApp {
    fn draw_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(
            RichText::new("◈ Atlas")
                .font(FontId::proportional(18.0))
                .strong()
                .color(accent_color(self.dark_mode)),
        );
        ui.separator();

        ui.label("Path:");
        let path_resp = ui.add(
            egui::TextEdit::singleline(&mut self.path_input)
                .desired_width(280.0)
                .hint_text("/home/user"),
        );
        if path_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.start_scan();
        }

        // A silent auto-refresh is "scanning" internally but should not turn
        // the Scan button into a Stop button on every tick.
        let scanning = matches!(self.scan_state, ScanState::Scanning { .. }) && !self.refreshing;
        if scanning {
            if ui
                .button(RichText::new("⏹ Stop").color(Color32::from_rgb(220, 80, 80)))
                .clicked()
            {
                if let Some(ct) = self.cancel_token.take() {
                    ct.cancel();
                }
                self.scan_state = ScanState::Idle;
                self.scan_rx = None;
            }
        } else {
            if ui
                .button(RichText::new("▶ Scan").color(accent_color(self.dark_mode)))
                .clicked()
            {
                self.start_scan();
            }
        }

        ui.separator();

        let can_go_up = !self.zoom_path.is_empty();
        if ui.add_enabled(can_go_up, egui::Button::new("⬆ Up")).clicked() {
            self.navigate_up();
        }
        if ui.add_enabled(can_go_up, egui::Button::new("⌂ Root")).clicked() {
            self.navigate_to_breadcrumb(0);
        }

        ui.separator();

        ui.label("Filter:");
        let filter_resp = ui.add(
            egui::TextEdit::singleline(&mut self.filter_text)
                .desired_width(170.0)
                .hint_text("e.g. *.jpg  >10mb  type:video"),
        );
        filter_resp.on_hover_text(
            "Filter tokens (ANDed together):\n\
             • text — name contains text\n\
             • *.ext — file extension\n\
             • type:image|video|audio|archive|document|code|exe|dir…\n\
             • >10mb  <1gb  >=500k — size (b/k/m/g/t)",
        );
        if !self.filter_text.is_empty() {
            if ui.small_button("✕").clicked() {
                self.filter_text.clear();
            }
        }

        ui.separator();

        // Nesting detail control (SpaceSniffer-style "more/less detail").
        ui.label("Detail:");
        if ui
            .add_enabled(self.detail_depth > 1, egui::Button::new("−"))
            .on_hover_text("Less nesting  (−)")
            .clicked()
        {
            self.set_detail(self.detail_depth - 1);
        }
        ui.label(RichText::new(self.detail_depth.to_string()).strong());
        if ui
            .add_enabled(self.detail_depth < MAX_DETAIL, egui::Button::new("+"))
            .on_hover_text("More nesting  (+)")
            .clicked()
        {
            self.set_detail(self.detail_depth + 1);
        }

        ui.separator();
        ui.checkbox(&mut self.show_files, "Files");
        ui.checkbox(&mut self.show_legend, "Legend");
        if ui
            .checkbox(&mut self.show_free_space, "Free")
            .on_hover_text("Show the disk's remaining free space as a block")
            .changed()
        {
            self.invalidate_layout();
        }
        if ui
            .checkbox(&mut self.auto_refresh, "Auto")
            .on_hover_text("Auto-refresh the current folder every 2 seconds")
            .changed()
        {
            // Re-baseline the timer so toggling on doesn't refresh instantly.
            self.last_refresh = 0.0;
        }

        ui.separator();
        let moon = if self.dark_mode { "☀ Light" } else { "☾ Dark" };
        if ui.button(moon).clicked() {
            self.dark_mode = !self.dark_mode;
            ctx.set_visuals(if self.dark_mode { dark_visuals() } else { light_visuals() });
        }
    }

    /// Render the "current folder | N files | Used …" summary line.
    fn status_summary(&self, ui: &mut egui::Ui) {
        if let Some(node) = &self.current_node {
            let disk_str = if self.disk_total > 0 {
                format!(
                    "  |  Disk: {}  Free: {}",
                    humansize::format_size(self.disk_total, humansize::BINARY),
                    humansize::format_size(self.disk_available, humansize::BINARY),
                )
            } else {
                String::new()
            };
            ui.label(format!(
                "{}  |  {} files  |  Used: {}{}",
                node.path.display(),
                node.file_count,
                humansize::format_size(node.size, humansize::BINARY),
                disk_str,
            ));
        }
    }

    fn draw_statusbar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match &self.scan_state {
                // A silent auto-refresh: keep showing the last-known summary
                // with a subtle live indicator instead of "Scanning…".
                ScanState::Scanning { .. } if self.refreshing => {
                    ui.spinner();
                    ui.label(RichText::new("⟳ Live").color(accent_color(self.dark_mode)));
                    self.status_summary(ui);
                }
                ScanState::Idle => {
                    ui.label(RichText::new("Ready").weak());
                }
                ScanState::Scanning { path, bytes, files } => {
                    ui.spinner();
                    ui.label(format!(
                        "Scanning {}  |  {} files  |  {}",
                        path.display(),
                        files,
                        humansize::format_size(*bytes, humansize::BINARY)
                    ));
                }
                ScanState::Done => {
                    self.status_summary(ui);
                }
                ScanState::Error(e) => {
                    ui.label(
                        RichText::new(format!("Error: {}", e))
                            .color(Color32::from_rgb(220, 80, 80)),
                    );
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(hp) = &self.hovered_path {
                    if let Some(root) = &self.scan_root {
                        if let Some(entry) = find_entry_by_path(root, hp) {
                            let cat = get_category(&entry);
                            ui.label(format!(
                                "{}  {}  [{}]",
                                entry.name,
                                humansize::format_size(entry.size, humansize::BINARY),
                                cat.label()
                            ));
                        }
                    }
                }
            });
        });
    }

    fn draw_treemap(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let available = ui.available_rect_before_wrap();
        let breadcrumb_h = 28.0;
        let treemap_rect = Rect::from_min_max(
            available.min + Vec2::new(0.0, breadcrumb_h),
            available.max,
        );

        // --- Breadcrumbs ---
        let bc_rect = Rect::from_min_size(available.min, Vec2::new(available.width(), breadcrumb_h));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bc_rect), |ui| {
            ui.horizontal(|ui| {
                let root_name = self.scan_root.as_ref().map(|r| r.name.as_str()).unwrap_or("(root)");
                if ui
                    .selectable_label(self.zoom_path.is_empty(), RichText::new(root_name).strong())
                    .clicked()
                    && !self.zoom_path.is_empty()
                {
                    self.navigate_to_breadcrumb(0);
                }
                let segments: Vec<String> = self.zoom_path.clone();
                for (i, seg) in segments.iter().enumerate() {
                    ui.label("›");
                    let is_last = i + 1 == segments.len();
                    if ui
                        .selectable_label(is_last, RichText::new(seg).strong())
                        .clicked()
                        && !is_last
                    {
                        self.navigate_to_breadcrumb(i + 1);
                    }
                }
            });
        });

        // --- Empty state ---
        if self.current_node.is_none() {
            ui.allocate_rect(treemap_rect, egui::Sense::hover());
            let painter = ui.painter().clone();
            painter.rect_filled(treemap_rect, Rounding::ZERO, bg_color(self.dark_mode));
            let msg = match &self.scan_state {
                ScanState::Idle => "Enter a path above and click ▶ Scan",
                ScanState::Scanning { .. } => "Scanning…",
                ScanState::Error(_) => "Scan failed – check the path",
                ScanState::Done => "No data",
            };
            painter.text(
                treemap_rect.center(),
                Align2::CENTER_CENTER,
                msg,
                FontId::proportional(18.0),
                dim_color(self.dark_mode),
            );
            return;
        }

        // --- Build layout ---
        let layout_nodes: Vec<LayoutNode> = {
            let nodes = self.get_or_build_layout(treemap_rect);
            nodes.to_vec()
        };

        let response = ui.allocate_rect(treemap_rect, egui::Sense::click());
        let mouse_pos = ui.input(|i| i.pointer.hover_pos());
        let now = ctx.input(|i| i.time) as f32;

        // Mouse wheel over the map adjusts nesting detail (SpaceSniffer-style).
        if response.hovered() {
            let scroll_y = ui.input(|i| i.raw_scroll_delta.y);
            if scroll_y > 1.0 {
                self.set_detail(self.detail_depth + 1);
                ctx.request_repaint();
            } else if scroll_y < -1.0 {
                self.set_detail(self.detail_depth.saturating_sub(1));
                ctx.request_repaint();
            }
        }

        let painter = ui.painter().clone();
        painter.rect_filled(treemap_rect, Rounding::ZERO, bg_color(self.dark_mode));

        let filter = Filter::parse(&self.filter_text);
        let show_files = self.show_files;
        let is_special = |n: &LayoutNode| {
            n.entry.is_dir || n.entry.is_unscanned || n.entry.name.starts_with("Free Space")
        };

        // Deepest (last-drawn) node under the cursor wins hit-testing, so
        // clicking a nested child selects the child, not its container.
        let hovered_idx = mouse_pos.and_then(|p| {
            layout_nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| (show_files || is_special(n)) && n.rect.contains(p))
                .map(|(i, _)| i)
                .last()
        });

        let dir_base = if self.dark_mode {
            FileCategory::Directory.dark_color()
        } else {
            FileCategory::Directory.light_color()
        };

        // --- Draw each cell (parent-before-child, so children paint on top) ---
        for (idx, node) in layout_nodes.iter().enumerate() {
            let entry = &node.entry;
            let is_free_space = entry.name.starts_with("Free Space");
            let is_unscanned = entry.is_unscanned;

            if !show_files && !entry.is_dir && !is_free_space && !is_unscanned {
                continue;
            }

            let is_hovered = Some(idx) == hovered_idx;
            let matched = filter.is_empty() || is_free_space || is_unscanned || filter.matches(entry);
            // Never dim a nesting container, or its children would be hidden.
            let dimmed = !matched && !node.is_container;

            // --- Choose fill color ---
            let fill = if is_free_space {
                if is_hovered {
                    Color32::from_rgb(55, 55, 65)
                } else {
                    Color32::from_rgb(35, 35, 42)
                }
            } else if is_unscanned {
                let pulse = ((now * 1.5).sin() * 0.5 + 0.5) as f32; // 0..1
                let base = if self.dark_mode { 55u8 } else { 160u8 };
                let vary = (pulse * 25.0) as u8;
                let v = base + vary;
                Color32::from_rgb(v, v, v + 8)
            } else if node.is_container {
                // Directory frame: a shade darker than a leaf so nesting reads,
                // getting progressively darker with depth to separate levels.
                if is_hovered {
                    dir_base
                } else {
                    let step = 18 + (node.depth.min(6) as u8) * 6;
                    darken_color(dir_base, step)
                }
            } else {
                let category = get_category(entry);
                let base = if self.dark_mode { category.dark_color() } else { category.light_color() };
                if dimmed {
                    darken_color(base, 60)
                } else if is_hovered {
                    if self.dark_mode { category.dark_color_hover() } else { category.light_color_hover() }
                } else {
                    base
                }
            };

            painter.rect_filled(node.rect, Rounding::ZERO, fill);

            // Border
            let border_color = if is_hovered {
                Color32::WHITE
            } else if node.is_container {
                if self.dark_mode {
                    Color32::from_rgba_unmultiplied(180, 200, 235, 70)
                } else {
                    Color32::from_rgba_unmultiplied(30, 60, 110, 90)
                }
            } else if is_unscanned {
                Color32::from_rgba_unmultiplied(150, 150, 180, 60)
            } else if self.dark_mode {
                Color32::from_rgba_unmultiplied(255, 255, 255, 25)
            } else {
                Color32::from_rgba_unmultiplied(0, 0, 0, 40)
            };
            let border_w = if node.is_container { BORDER_WIDTH + 0.5 } else { BORDER_WIDTH };
            painter.rect_stroke(node.rect, Rounding::ZERO, Stroke::new(border_w, border_color));

            // Clip text to the cell so labels never bleed into siblings.
            let tp = painter.with_clip_rect(node.rect);

            if node.is_container {
                // --- Container header label (name + size in the top strip) ---
                if node.rect.width() >= LABEL_MIN_WIDTH {
                    let text_color = if self.dark_mode {
                        Color32::from_rgb(235, 240, 250)
                    } else {
                        Color32::from_rgb(15, 25, 45)
                    };
                    let size_str = humansize::format_size(entry.size, humansize::BINARY);
                    let label = format!("{}  ·  {}", entry.name, size_str);
                    tp.text(
                        Pos2::new(node.rect.left() + 4.0, node.rect.top() + HEADER_H * 0.5),
                        Align2::LEFT_CENTER,
                        &label,
                        FontId::proportional(10.0),
                        text_color,
                    );
                }
            } else {
                // --- Leaf label (name, size, dir arrow) ---
                let inner = node.rect.shrink(4.0);
                if inner.width() >= LABEL_MIN_WIDTH && inner.height() >= LABEL_MIN_HEIGHT {
                    let text_color = if is_free_space {
                        Color32::from_rgb(140, 140, 160)
                    } else if is_unscanned {
                        Color32::from_rgb(160, 160, 180)
                    } else if self.dark_mode {
                        Color32::from_rgb(230, 230, 230)
                    } else {
                        Color32::from_rgb(20, 20, 20)
                    };

                    let font_size = if inner.height() > 40.0 { 13.0 } else { 10.0 };
                    let label = if is_unscanned {
                        format!("⌛ {}", entry.name)
                    } else {
                        entry.name.clone()
                    };

                    tp.text(
                        Pos2::new(inner.left() + 2.0, inner.top() + 2.0),
                        Align2::LEFT_TOP,
                        &label,
                        FontId::proportional(font_size),
                        text_color,
                    );

                    if inner.height() > 32.0 && !is_unscanned {
                        let size_str = humansize::format_size(entry.size, humansize::BINARY);
                        tp.text(
                            Pos2::new(inner.left() + 2.0, inner.top() + font_size + 4.0),
                            Align2::LEFT_TOP,
                            size_str,
                            FontId::proportional(font_size - 1.0),
                            dim_color_for(text_color),
                        );
                    }

                    // A collapsed directory (shown as a leaf) still zooms in.
                    if entry.is_dir && !is_unscanned && inner.width() > 20.0 {
                        tp.text(
                            Pos2::new(inner.right() - 2.0, inner.top() + 2.0),
                            Align2::RIGHT_TOP,
                            "▸",
                            FontId::proportional(font_size),
                            text_color.gamma_multiply(0.6),
                        );
                    }
                }
            }
        }

        // --- Resolve hover / click from the deepest node under the cursor ---
        let mut hovered_entry: Option<Arc<FileEntry>> = None;
        let mut clicked_entry: Option<Arc<FileEntry>> = None;
        let mut double_clicked: Option<Arc<FileEntry>> = None;
        if let Some(i) = hovered_idx {
            let entry = Arc::clone(&layout_nodes[i].entry);
            hovered_entry = Some(Arc::clone(&entry));
            if response.clicked() {
                clicked_entry = Some(Arc::clone(&entry));
            }
            if response.double_clicked() {
                double_clicked = Some(entry);
            }
        }

        // --- Tooltip ---
        if let Some(entry) = &hovered_entry {
            self.hovered_path = Some(entry.path.clone());
            let tooltip_text = build_tooltip(entry, self.disk_total, self.disk_available);
            egui::show_tooltip(
                ui.ctx(),
                ui.layer_id(),
                egui::Id::new("treemap_tooltip"),
                |ui| {
                    ui.label(tooltip_text);
                },
            );
        } else {
            self.hovered_path = None;
        }

        // --- Context menu ---
        let hovered_for_menu = hovered_entry.clone();
        response.context_menu(|ui| {
            if let Some(entry) = &hovered_for_menu {
                let is_free_space = entry.name.starts_with("Free Space");
                let is_unscanned = entry.is_unscanned;

                ui.label(RichText::new(&entry.name).strong());
                if !is_unscanned {
                    ui.label(humansize::format_size(entry.size, humansize::BINARY));
                }
                ui.separator();

                if entry.is_dir && !is_unscanned {
                    if ui.button("📂 Open in file manager").clicked() {
                        let _ = std::process::Command::new("xdg-open").arg(&entry.path).spawn();
                        ui.close_menu();
                    }
                }
                if !is_free_space && !is_unscanned {
                    if ui.button("📋 Copy path").clicked() {
                        ui.output_mut(|o| {
                            o.copied_text = entry.path.to_string_lossy().into_owned();
                        });
                        ui.close_menu();
                    }
                }

                // Delete — only for real (non-synthetic) entries
                if !is_free_space && !is_unscanned {
                    ui.separator();
                    if ui
                        .button(
                            RichText::new("🗑 Delete…")
                                .color(Color32::from_rgb(220, 80, 80)),
                        )
                        .clicked()
                    {
                        self.delete_confirm = Some(DeleteConfirm {
                            entry: Arc::clone(entry),
                            error: None,
                        });
                        ui.close_menu();
                    }
                }
            } else {
                ui.label(RichText::new("(no selection)").weak());
                if ui.button("⟳ Re-scan").clicked() {
                    self.start_scan();
                    ui.close_menu();
                }
            }
        });

        // --- Navigation ---
        let zoom_target = double_clicked.or_else(|| {
            clicked_entry
                .as_ref()
                .filter(|e| e.is_dir && !e.is_unscanned)
                .map(Arc::clone)
        });
        if let Some(entry) = zoom_target {
            if entry.is_dir && !entry.children.is_empty() && !entry.is_unscanned {
                self.navigate_to(&entry);
            }
        }

        // --- Legend ---
        if self.show_legend {
            self.draw_legend(ui, treemap_rect);
        }
    }

    fn draw_delete_dialog(&mut self, ctx: &egui::Context) {
        // We need to take the state out temporarily to avoid borrow issues
        let confirm = match self.delete_confirm.take() {
            Some(c) => c,
            None => return,
        };

        // Use local enums to track the desired action without holding borrows
        #[derive(PartialEq)]
        enum Action { Keep, Cancel, Delete }
        let mut action = Action::Keep;

        egui::Window::new("Confirm Delete")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.add_space(8.0);

                ui.label(
                    RichText::new(format!("\"{}\"", confirm.entry.name))
                        .font(FontId::proportional(15.0))
                        .strong(),
                );
                ui.label(format!(
                    "  {}",
                    humansize::format_size(confirm.entry.size, humansize::BINARY)
                ));
                ui.add_space(6.0);

                if confirm.entry.is_dir {
                    ui.label(
                        RichText::new(format!(
                            "⚠  This directory contains {} files and will be permanently deleted.",
                            confirm.entry.file_count
                        ))
                        .color(Color32::from_rgb(230, 160, 40)),
                    );
                } else {
                    ui.label(
                        RichText::new("This file will be permanently deleted.")
                            .color(Color32::from_rgb(200, 200, 200)),
                    );
                }

                if let Some(ref err) = confirm.error {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(format!("Error: {}", err))
                            .color(Color32::from_rgb(220, 80, 80)),
                    );
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(
                            RichText::new("🗑  Delete permanently")
                                .color(Color32::from_rgb(220, 80, 80)),
                        )
                        .clicked()
                    {
                        action = Action::Delete;
                    }
                    if ui.button("Cancel").clicked() {
                        action = Action::Cancel;
                    }
                });
                ui.add_space(4.0);
            });

        match action {
            Action::Delete => {
                match self.perform_delete(Arc::clone(&confirm.entry)) {
                    Ok(()) => {} // dialog stays closed; tree already updated
                    Err(e) => {
                        self.delete_confirm = Some(DeleteConfirm {
                            entry: confirm.entry,
                            error: Some(e.to_string()),
                        });
                    }
                }
            }
            Action::Cancel => {} // leave delete_confirm = None (already taken)
            Action::Keep => {
                self.delete_confirm = Some(DeleteConfirm {
                    entry: confirm.entry,
                    error: confirm.error,
                });
            }
        }
    }

    fn draw_legend(&self, ui: &mut egui::Ui, treemap_rect: Rect) {
        let categories = [
            FileCategory::Directory,
            FileCategory::Image,
            FileCategory::Video,
            FileCategory::Audio,
            FileCategory::Archive,
            FileCategory::Document,
            FileCategory::Code,
            FileCategory::Executable,
            FileCategory::Data,
            FileCategory::Other,
        ];

        let swatch_size = 12.0;
        let row_h = 18.0;
        let col_w = 100.0;
        let padding = 8.0;
        let cols = 2usize;
        let rows = (categories.len() + cols - 1) / cols;

        // Extra rows for free-space and unscanned if scan is done or in progress
        let extra_rows = match &self.scan_state {
            ScanState::Done => 1,
            ScanState::Scanning { .. } => 2,
            _ => 0,
        };
        let total_rows = rows + extra_rows;

        let panel_w = cols as f32 * col_w + padding * 2.0;
        let panel_h = total_rows as f32 * row_h + padding * 2.0;

        let panel_rect = Rect::from_min_size(
            Pos2::new(treemap_rect.right() - panel_w - 8.0, treemap_rect.top() + 8.0),
            Vec2::new(panel_w, panel_h),
        );

        let painter = ui.painter().clone();
        let bg = if self.dark_mode {
            Color32::from_rgba_unmultiplied(20, 20, 30, 210)
        } else {
            Color32::from_rgba_unmultiplied(250, 250, 255, 210)
        };
        painter.rect_filled(panel_rect, Rounding::same(4.0), bg);
        painter.rect_stroke(
            panel_rect,
            Rounding::same(4.0),
            Stroke::new(1.0, dim_color(self.dark_mode)),
        );

        let text_color = if self.dark_mode {
            Color32::from_rgb(200, 200, 200)
        } else {
            Color32::from_rgb(40, 40, 40)
        };

        let draw_row = |row: usize, col: usize, color: Color32, label: &str| {
            let x = panel_rect.left() + padding + col as f32 * col_w;
            let y = panel_rect.top() + padding + row as f32 * row_h;
            let swatch = Rect::from_min_size(
                Pos2::new(x, y + (row_h - swatch_size) / 2.0),
                Vec2::splat(swatch_size),
            );
            painter.rect_filled(swatch, Rounding::same(2.0), color);
            painter.text(
                Pos2::new(x + swatch_size + 4.0, y + row_h / 2.0),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                text_color,
            );
        };

        for (i, cat) in categories.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let color = if self.dark_mode { cat.dark_color() } else { cat.light_color() };
            draw_row(row, col, color, cat.label());
        }

        // Free space / unscanned rows
        let mut extra = rows;
        if matches!(&self.scan_state, ScanState::Done | ScanState::Scanning { .. }) {
            draw_row(extra, 0, Color32::from_rgb(35, 35, 42), "Free Space");
            extra += 1;
        }
        if matches!(&self.scan_state, ScanState::Scanning { .. }) {
            draw_row(extra, 0, Color32::from_rgb(70, 70, 80), "Scanning…");
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_tooltip(entry: &FileEntry, disk_total: u64, disk_avail: u64) -> String {
    if entry.name.starts_with("Free Space") {
        let pct = if disk_total > 0 {
            disk_avail as f64 / disk_total as f64 * 100.0
        } else {
            0.0
        };
        return format!(
            "Free Space\nAvailable: {}  ({:.1}% of disk)",
            humansize::format_size(disk_avail, humansize::BINARY),
            pct
        );
    }
    if entry.is_unscanned {
        return format!("Scanning: {}\n(size not yet known)", entry.path.display());
    }

    let size = humansize::format_size(entry.size, humansize::BINARY);
    let cat = get_category(entry);
    let mut lines = vec![
        format!("Name: {}", entry.name),
        format!("Type: {}", cat.label()),
        format!("Size: {}", size),
    ];
    if entry.is_dir {
        lines.push(format!("Files: {}", entry.file_count));
        lines.push(format!("Items: {}", entry.children.len()));
    }
    lines.push(format!("Path: {}", entry.path.display()));
    if let Some(mtime) = entry.modified {
        if let Ok(elapsed) = mtime.elapsed() {
            let secs = elapsed.as_secs();
            let age = if secs < 60 {
                format!("{secs} seconds ago")
            } else if secs < 3600 {
                format!("{} minutes ago", secs / 60)
            } else if secs < 86400 {
                format!("{} hours ago", secs / 3600)
            } else {
                format!("{} days ago", secs / 86400)
            };
            lines.push(format!("Modified: {age}"));
        }
    }
    lines.join("\n")
}

fn find_entry_by_path(root: &Arc<FileEntry>, path: &PathBuf) -> Option<Arc<FileEntry>> {
    if &root.path == path {
        return Some(Arc::clone(root));
    }
    for child in &root.children {
        if let Some(found) = find_entry_by_path(child, path) {
            return Some(found);
        }
    }
    None
}

fn darken_color(c: Color32, amount: u8) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_sub(amount),
        c.g().saturating_sub(amount),
        c.b().saturating_sub(amount),
    )
}

fn dim_color_for(text: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(text.r(), text.g(), text.b(), 120)
}

fn accent_color(dark_mode: bool) -> Color32 {
    if dark_mode {
        Color32::from_rgb(100, 180, 255)
    } else {
        Color32::from_rgb(30, 100, 200)
    }
}

fn bg_color(dark_mode: bool) -> Color32 {
    if dark_mode {
        Color32::from_rgb(18, 18, 24)
    } else {
        Color32::from_rgb(240, 240, 245)
    }
}

fn dim_color(dark_mode: bool) -> Color32 {
    if dark_mode {
        Color32::from_rgb(100, 100, 120)
    } else {
        Color32::from_rgb(150, 150, 160)
    }
}

fn dark_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(22, 22, 30);
    v.window_fill = Color32::from_rgb(25, 25, 35);
    v.faint_bg_color = Color32::from_rgb(30, 30, 40);
    v.extreme_bg_color = Color32::from_rgb(15, 15, 20);
    v.code_bg_color = Color32::from_rgb(30, 30, 40);
    v.widgets.noninteractive.bg_fill = Color32::from_rgb(35, 35, 48);
    v.widgets.inactive.bg_fill = Color32::from_rgb(42, 42, 58);
    v.widgets.hovered.bg_fill = Color32::from_rgb(55, 55, 75);
    v.widgets.active.bg_fill = Color32::from_rgb(70, 70, 100);
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(70, 120, 200, 120);
    v
}

fn light_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::light();
    v.panel_fill = Color32::from_rgb(248, 248, 252);
    v.window_fill = Color32::from_rgb(255, 255, 255);
    v
}
