//! Breeze app entry point: winit event loop + a softbuffer window hosting tabs,
//! each a split workspace of throttled terminals. Background tabs are frozen.
//! No-GPU rendering; event-driven.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use breeze_core::geom::Rect;
use breeze_core::split_tree::SplitDirection;
use breeze_platform::managed::ManagedTerminal;
use breeze_ui::chrome;
use breeze_ui::confirm::ConfirmDialog;
use breeze_ui::input::encode_key;
use breeze_ui::palette::Palette;
use breeze_ui::panes::{pane_at, pane_rects};
use breeze_ui::render::{fill_rect, grid_size, Renderer};
use breeze_ui::workspace::Workspace;
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const BG: (u8, u8, u8) = (0x0b, 0x10, 0x16);
const FG: (u8, u8, u8) = (0xcf, 0xe3, 0xf2);
const FONT_SIZE: f32 = 11.0;
/// Padding (logical px) between a pane's edge and its terminal content.
const PANE_INSET: i32 = 2;
const FOCUS_RIM: (u8, u8, u8) = (0x7f, 0xc9, 0xff);
/// Muted text for secondary/footer/inactive UI (lower-contrast than `FG`).
const DIM: (u8, u8, u8) = (0x6e, 0x79, 0x88);
const SEL_BG: (u8, u8, u8) = (0x1d, 0x3a, 0x52);
const DIVIDER: (u8, u8, u8) = (0x3a, 0x3f, 0x4a);
const TAB_ACTIVE: (u8, u8, u8) = (0x20, 0x24, 0x2e);
/// Midpoint of FROST_BASE and TAB_ACTIVE — hovered inactive tab.
const TAB_HOVER: (u8, u8, u8) = (0x1a, 0x1d, 0x25);
const TAB_CLOSE: (u8, u8, u8) = (0x3a, 0x40, 0x4c);
/// Brighter close-circle fill while the cursor hovers it.
const TAB_CLOSE_HOVER: (u8, u8, u8) = (0x5a, 0x64, 0x74);
/// Danger rim drawn around panes pending deletion (only while the confirm is open).
const ERASE_RIM: (u8, u8, u8) = (0xe0, 0x55, 0x55);

/// A right-here context menu opened by double-clicking a pane: a small list of
/// actions anchored at the cursor.
struct PaneMenu {
    x: i32,
    y: i32,
    /// Panes the menu's delete action will remove (the marked set, or the
    /// double-clicked pane when nothing is marked).
    targets: Vec<i64>,
}

#[derive(Debug, Clone)]
enum UserEvent {
    Output,
    AttachTick,
    /// A window asked to open a new window (Cmd-N); the `App` router creates it.
    NewWindow,
    /// The launch update-check found a newer release; offer it to the user.
    UpdateAvailable { version: String, url: String },
}

/// One tab: a workspace of panes. Each leaf id is bound to a terminal in `panes`.
struct Tab {
    workspace: Workspace,
    panes: HashMap<i64, ManagedTerminal>,
}

impl Tab {
    fn new(proxy: &EventLoopProxy<UserEvent>) -> Tab {
        let workspace = Workspace::new();
        let mut panes = HashMap::new();
        if let Some(t) = spawn_terminal(proxy, None) {
            panes.insert(workspace.focused().id, t);
        }
        Tab { workspace, panes }
    }

    /// Rebuild a tab from a saved layout, spawning a terminal per saved pane.
    fn from_saved(proxy: &EventLoopProxy<UserEvent>, saved: &breeze_core::session::SavedGridspace) -> Tab {
        let tree = saved.split_tree.clone().unwrap_or_else(|| {
            breeze_core::split_tree::SplitNode::leaf(breeze_core::split_tree::PaneID::new(0))
        });
        let workspace = Workspace::from_tree(tree);
        let mut panes = HashMap::new();
        let dirs = saved.working_directories.clone().unwrap_or_default();
        for (i, id) in breeze_ui::panes::leaf_ids_in_order(workspace.tree()).iter().enumerate() {
            let dir = dirs.get(i).and_then(|d| d.clone());
            if let Some(t) = spawn_terminal(proxy, dir.as_deref()) {
                panes.insert(id.id, t);
            }
        }
        Tab { workspace, panes }
    }

    /// Build a tab from a saved layout preset (a bare split arrangement, no
    /// per-pane directories), spawning a fresh terminal per leaf.
    fn from_layout(proxy: &EventLoopProxy<UserEvent>, tree: breeze_core::split_tree::SplitNode) -> Tab {
        let workspace = Workspace::from_tree(tree);
        let mut panes = HashMap::new();
        for id in breeze_ui::panes::leaf_ids_in_order(workspace.tree()) {
            if let Some(t) = spawn_terminal(proxy, None) {
                panes.insert(id.id, t);
            }
        }
        Tab { workspace, panes }
    }

    /// Snapshot this tab's layout for persistence.
    fn to_saved(&self, name: String) -> breeze_core::session::SavedGridspace {
        let ids: Vec<i64> = breeze_ui::panes::leaf_ids_in_order(self.workspace.tree()).iter().map(|p| p.id).collect();
        breeze_core::session::SavedGridspace {
            name,
            pane_count: ids.len() as i64,
            working_directories: Some(ids.iter().map(|id| self.panes.get(id).and_then(|t| t.cwd())).collect()),
            split_tree: Some(self.workspace.tree().clone()),
            pane_ids: Some(ids),
            focused_pane_index: None,
        }
    }

    fn focused_terminal(&mut self) -> Option<&mut ManagedTerminal> {
        let id = self.workspace.focused().id;
        self.panes.get_mut(&id)
    }

    fn set_background(&self, bg: bool) {
        for t in self.panes.values() {
            if bg {
                t.enter_background();
            } else {
                t.enter_foreground();
            }
        }
    }
}

/// Active divider drag: linked vertical paths (moved by horizontal cursor
/// delta) and linked horizontal paths (moved by vertical delta). Both non-empty
/// means an intersection ("omni") drag that resizes along both axes at once.
#[derive(Clone, Default)]
struct DragDivider {
    v_paths: Vec<Vec<bool>>,
    h_paths: Vec<Vec<bool>>,
}

/// An in-progress scrollbar drag: which pane, and its track geometry (top + height
/// in physical px) captured at grab time so cursor-y maps to a scroll offset.
struct ScrollDrag {
    pane: i64,
    track_top: f64,
    track_h: f64,
}

/// One OS window: its surface, tab-set, and all per-window interaction state.
/// The `App` router owns a `Vec<WindowState>` and dispatches events by id.
struct WindowState {
    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    renderer: Renderer,
    modifiers: ModifiersState,
    bg: (u8, u8, u8),
    fg: (u8, u8, u8),
    sel_bg: (u8, u8, u8),
    font_size: f32,
    proxy: EventLoopProxy<UserEvent>,
    palette: Palette,
    confirm: ConfirmDialog,
    tabs: Vec<Tab>,
    active: usize,
    cursor: (f64, f64),
    selection: Option<((usize, usize), (usize, usize))>,
    selecting: bool,
    /// Dividers being dragged: linked vertical paths (moved by dx) and linked
    /// horizontal paths (moved by dy). Both populated = omni (intersection) drag.
    drag_divider: Option<DragDivider>,
    copy_on_select: bool,
    orphan_overlay: Option<Vec<i32>>,
    about_open: bool,
    low_power: bool,
    power_tick: u32,
    /// When `Some`, an inline text-input overlay is open, collecting a name for
    /// the current arrangement (Enter saves it as a layout, Esc cancels).
    name_prompt: Option<String>,
    /// When `Some`, the inline pane-count editor is open (digits; Enter sets the
    /// pane count via the keep-idle/skip-focused shrink rule, Esc cancels).
    count_prompt: Option<String>,
    /// The tab whose close "×" was clicked, awaiting confirm (Enter closes it).
    pending_close_tab: Option<usize>,
    /// The DMG URL to install if the user accepts the update prompt.
    pending_update: Option<String>,
    /// Set when this window should be closed (its X clicked or last tab gone);
    /// the `App` router removes it after dispatch and exits if none remain.
    wants_close: bool,
    /// The pane being drag-moved (grip handle held); released onto another pane
    /// swaps the two.
    pane_drag: Option<breeze_core::split_tree::PaneID>,
    scrollbar_drag: Option<ScrollDrag>,
    /// Signature of which unfocused panes show a "settled" badge; a change
    /// triggers exactly one redraw (no steady repaint).
    badge_sig: u64,
    /// Panes marked for deletion via Shift+click (shown with a subtle rim).
    marked_panes: Vec<i64>,
    /// Last left-press (time, x, y) — for double-click detection.
    last_click: Option<(std::time::Instant, f64, f64)>,
    /// Open double-click context menu, if any.
    pane_menu: Option<PaneMenu>,
    /// Pane ids awaiting the "Delete N pane(s)?" confirm.
    pending_erase: Option<Vec<i64>>,
    /// Off-UI-thread housekeeper: closed `ManagedTerminal`s and session-save
    /// snapshots are pushed here so kill/join/fsync never block the event loop.
    chore_tx: std::sync::mpsc::Sender<Chore>,
}

fn command_list() -> Vec<String> {
    [
        "New Tab",
        "Close Pane",
        "Split Right",
        "Split Down",
        "Swap Panes",
        "Save Layout",
        "Open Layout",
        "Delete Layout",
        "Scan Orphans",
        "Settings",
        "About",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Scrollback depth applied to every spawned terminal. Resolved once at
/// startup from the loaded config (`config_store` stays the single source);
/// terminals are spawned after that, so the value is always set by then.
static SCROLLBACK: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn scrollback_lines() -> usize {
    *SCROLLBACK.get().unwrap_or(&breeze_vt::Vt::DEFAULT_HISTORY)
}

fn spawn_terminal(proxy: &EventLoopProxy<UserEvent>, dir: Option<&str>) -> Option<ManagedTerminal> {
    let p = proxy.clone();
    let notify = Box::new(move || {
        let _ = p.send_event(UserEvent::Output);
    });
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    ManagedTerminal::spawn_with_notify(
        &shell,
        &["-l"],
        &[("TERM", "xterm-256color"), ("TERM_PROGRAM", "Breeze")],
        dir,
        80,
        24,
        scrollback_lines(),
        Some(notify),
    )
    .ok()
}

/// Work funneled off the UI thread. Dropping a `ManagedTerminal` does a blocking
/// SIGKILL + pump-thread join (and a child that keeps the PTY open can block it
/// indefinitely); a session save does an fsync. Doing either inline would stall
/// the event loop — closing a many-pane tab would stutter, a stuck pane could
/// freeze the app — so both run on a background housekeeper instead.
enum Chore {
    Reap(ManagedTerminal),
    Save(Vec<breeze_core::session::SavedGridspace>),
}

/// Spawn the housekeeper thread and return its sender. It drains remaining
/// chores and exits once the last sender drops.
fn spawn_housekeeper() -> std::sync::mpsc::Sender<Chore> {
    let (tx, rx) = std::sync::mpsc::channel::<Chore>();
    std::thread::spawn(move || {
        for chore in rx {
            match chore {
                Chore::Reap(mt) => drop(mt),
                Chore::Save(saved) => {
                    let _ = breeze_platform::session_store::save(&saved);
                }
            }
        }
    });
    tx
}

impl WindowState {
    fn new(proxy: EventLoopProxy<UserEvent>) -> WindowState {
        let cfg = breeze_platform::config_store::load();
        let _ = SCROLLBACK.set(
            cfg.scrollback_limit.map(|n| n as usize).unwrap_or(breeze_vt::Vt::DEFAULT_HISTORY),
        );
        let to_rgb = |c: breeze_core::config::Rgba| (c.r, c.g, c.b);
        let mut renderer = Renderer::new();
        renderer.set_font_family(cfg.font_family.clone());
        WindowState {
            window: None,
            context: None,
            surface: None,
            renderer,
            modifiers: ModifiersState::empty(),
            bg: cfg.background.map(to_rgb).unwrap_or(BG),
            fg: cfg.foreground.map(to_rgb).unwrap_or(FG),
            sel_bg: cfg.selection_background.map(to_rgb).unwrap_or(SEL_BG),
            font_size: cfg.font_size.map(|s| s as f32).unwrap_or(FONT_SIZE),
            copy_on_select: cfg.copy_on_select,
            proxy,
            palette: Palette::new(command_list()),
            confirm: ConfirmDialog::new(),
            tabs: Vec::new(),
            active: 0,
            cursor: (0.0, 0.0),
            selection: None,
            selecting: false,
            drag_divider: None,
            orphan_overlay: None,
            about_open: false,
            low_power: false,
            power_tick: 0,
            name_prompt: None,
            count_prompt: None,
            pending_close_tab: None,
            pending_update: None,
            wants_close: false,
            pane_drag: None,
            scrollbar_drag: None,
            badge_sig: 0,
            marked_panes: Vec::new(),
            last_click: None,
            pane_menu: None,
            pending_erase: None,
            chore_tx: spawn_housekeeper(),
        }
    }


    /// The focused pane's on-screen rectangle.
    /// The visible palette-item index under (`cx`,`cy`), if the click lands on a
    /// row. Geometry mirrors the palette render block.
    fn palette_row_at(&self, cx: f64, cy: f64, pw: i32, ph: i32) -> Option<usize> {
        let bar_h = chrome::tab_bar_height(self.scale());
        let panel_w = (pw * 6 / 10).clamp(200, 760);
        let line_h = (self.fs() * 1.3).ceil() as i32;
        let px0 = (pw - panel_w) / 2;
        let py0 = (ph / 5).max(bar_h + 8);
        let pad = 8;
        let vis = self.palette.visible().len().min(10);
        for i in 0..vis {
            let iy = py0 + pad + line_h * (i as i32 + 1);
            if cx >= (px0 + 4) as f64
                && cx < (px0 + panel_w - 4) as f64
                && cy >= iy as f64
                && cy < (iy + line_h) as f64
            {
                return Some(i);
            }
        }
        None
    }

    /// The close-button square (x, y, w, h) at a pane's top-right corner.
    fn close_button_rect(&self, pane: Rect) -> (i32, i32, i32, i32) {
        let bw = (self.fs() * 1.3) as i32;
        let m = 4 * self.scale() as i32;
        (pane.x as i32 + pane.width as i32 - bw - m, pane.y as i32 + m, bw, bw)
    }

    /// The move-handle (drag-to-reorder grip) square at a pane's top-left.
    fn move_handle_rect(&self, pane: Rect) -> (i32, i32, i32, i32) {
        let bw = (self.fs() * 1.3) as i32;
        let m = 4 * self.scale() as i32;
        (pane.x as i32 + m, pane.y as i32 + m, bw, bw)
    }

    fn focused_pane_rect(&self) -> Option<Rect> {
        let window = self.window.as_ref()?;
        let size = window.inner_size();
        let content = self.content_rect(size.width, size.height);
        let focused = self.active_tab().workspace.focused();
        pane_rects(self.active_tab().workspace.tree(), content)
            .into_iter()
            .find(|(id, _)| *id == focused)
            .map(|(_, r)| r)
    }

    /// Map the cursor to a (row, col) cell within `rect`.
    fn cursor_cell(&self, rect: Rect) -> (usize, usize) {
        let (cw, ch) = breeze_ui::render::cell_size(self.fs());
        let inset = self.pane_inset() as f64;
        let col = ((self.cursor.0 - rect.x - inset).max(0.0) / cw as f64) as usize;
        let row = ((self.cursor.1 - rect.y - inset).max(0.0) / ch as f64) as usize;
        (row, col)
    }

    fn copy_selection(&self) {
        let Some((s, e)) = self.selection else { return };
        let Some(t) = self.tabs.get(self.active).and_then(|tab| {
            let id = tab.workspace.focused().id;
            tab.panes.get(&id)
        }) else {
            return;
        };
        let lines: Vec<String> = t.screen_text().split('\n').map(|s| s.to_string()).collect();
        let text = breeze_ui::select::selection_text(&lines, s, e);
        if text.is_empty() {
            return;
        }
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text);
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn scale(&self) -> u32 {
        self.window.as_ref().map(|w| w.scale_factor().round().max(1.0) as u32).unwrap_or(1)
    }

    /// Font size in physical pixels — the configured point size scaled for the
    /// display's DPI. The softbuffer surface and all input coords are physical,
    /// so every glyph/grid/overlay measurement uses this, not the raw point size.
    fn fs(&self) -> f32 {
        self.font_size * self.scale() as f32
    }

    /// Padding (physical px) between a pane's edge and its terminal content.
    fn pane_inset(&self) -> i32 {
        PANE_INSET * self.scale() as i32
    }

    fn content_rect(&self, pw: u32, ph: u32) -> Rect {
        let (x, y, w, h) = chrome::content_rect(pw as i32, ph as i32, self.scale());
        Rect::new(x as f64, y as f64, w as f64, h as f64)
    }

    /// Geometry `(x, y, w, h)` of the open double-click context menu's single row,
    /// in physical px. Shared by render and hit-testing. `None` when no menu open.
    fn pane_menu_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let m = self.pane_menu.as_ref()?;
        let si = self.scale() as i32;
        let line_h = (self.fs() * 1.3).ceil() as i32;
        let w = 200 * si;
        let h = line_h + 8 * si;
        Some((m.x, m.y, w, h))
    }

    /// Persist all tabs' layouts (debounce is unnecessary at our event rates).
    /// Serialization runs here on the UI thread (fast); the fsync write is sent
    /// to the housekeeper so it never stalls the loop.
    fn save_session(&self) {
        let saved: Vec<_> =
            self.tabs.iter().enumerate().map(|(i, t)| t.to_saved(format!("Tab {}", i + 1))).collect();
        let _ = self.chore_tx.send(Chore::Save(saved));
    }

    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }
    fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn relayout(&self) {
        let Some(window) = self.window.as_ref() else { return };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 || self.tabs.is_empty() {
            return;
        }
        let content = self.content_rect(size.width, size.height);
        let tab = &self.tabs[self.active];
        let inset2 = (self.pane_inset() * 2) as u32;
        for (id, r) in pane_rects(tab.workspace.tree(), content) {
            if let Some(t) = tab.panes.get(&id.id) {
                let w = (r.width as u32).saturating_sub(inset2);
                let h = (r.height as u32).saturating_sub(inset2);
                let (cols, rows) = grid_size(w, h, self.fs());
                let _ = t.resize(cols, rows);
            }
        }
    }

    /// Open the most-recently-saved layout preset in a new tab, if any exist.
    fn load_layout(&mut self) {
        let names = breeze_platform::layout_store::list();
        let pick = names.last().cloned();
        if let Some(tree) = pick.and_then(|n| breeze_platform::layout_store::load(&n)) {
            if let Some(t) = self.tabs.get(self.active) {
                t.set_background(true);
            }
            self.tabs.push(Tab::from_layout(&self.proxy, tree));
            self.active = self.tabs.len() - 1;
            self.relayout();
            self.save_session();
            self.request_redraw();
        }
    }

    /// Close any pane whose shell has exited; close a tab when its last pane
    /// goes. Returns whether anything changed.
    fn reap_exited_panes(&mut self) -> bool {
        use breeze_core::split_tree::PaneID;
        let mut changed = false;
        let mut empty_tabs: Vec<usize> = Vec::new();
        for (ti, tab) in self.tabs.iter_mut().enumerate() {
            let exited: Vec<i64> = tab
                .panes
                .iter()
                .filter(|(_, t)| t.has_exited())
                .map(|(id, _)| *id)
                .collect();
            for id in exited {
                tab.workspace.focus(PaneID::new(id));
                match tab.workspace.close_focused() {
                    Some(closed) => {
                        if let Some(mt) = tab.panes.remove(&closed.id) {
                            let _ = self.chore_tx.send(Chore::Reap(mt));
                        }
                        changed = true;
                    }
                    None => {
                        // Last pane in the tab exited — drop the whole tab.
                        if let Some(mt) = tab.panes.remove(&id) {
                            let _ = self.chore_tx.send(Chore::Reap(mt));
                        }
                        empty_tabs.push(ti);
                        changed = true;
                    }
                }
            }
        }
        // Remove emptied tabs high-index-first so earlier indices stay valid.
        empty_tabs.sort_unstable();
        empty_tabs.dedup();
        for ti in empty_tabs.into_iter().rev() {
            let mut t = self.tabs.remove(ti);
            // Reap any residual panes (defensive — the close branches above
            // should have drained them already).
            for (_, mt) in t.panes.drain() {
                let _ = self.chore_tx.send(Chore::Reap(mt));
            }
            if self.active >= ti && self.active > 0 {
                self.active -= 1;
            }
        }
        if changed {
            if self.tabs.is_empty() {
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            } else {
                if self.active >= self.tabs.len() {
                    self.active = self.tabs.len() - 1;
                }
                self.tabs[self.active].set_background(false);
                self.relayout();
                self.request_redraw();
            }
        }
        changed
    }

    /// Settings-palette entries: the current resolved config values (display)
    /// plus a live "Reload Config" action.
    fn settings_list(&self) -> Vec<String> {
        vec![
            format!("Font size: {}", self.font_size),
            format!("Foreground: #{:02x}{:02x}{:02x}", self.fg.0, self.fg.1, self.fg.2),
            format!("Background: #{:02x}{:02x}{:02x}", self.bg.0, self.bg.1, self.bg.2),
            format!("Selection: #{:02x}{:02x}{:02x}", self.sel_bg.0, self.sel_bg.1, self.sel_bg.2),
            "Reload Config".to_string(),
        ]
    }

    /// Map a cursor-y to an absolute scroll offset for the active scrollbar drag
    /// and apply it to that pane (centers the thumb on the cursor).
    fn scroll_drag_to(&mut self, my: f64) {
        let Some(drag) = self.scrollbar_drag.as_ref() else {
            return;
        };
        let (pane, track_top, track_h) = (drag.pane, drag.track_top, drag.track_h);
        let ch = breeze_ui::render::cell_size(self.fs()).1 as f64;
        let rows = (track_h / ch).floor().max(1.0) as usize;
        if let Some(t) = self.tabs[self.active].panes.get_mut(&pane) {
            let (cur, total) = t.scroll_position();
            let scrollable = total.saturating_sub(rows);
            if scrollable == 0 {
                return;
            }
            let min_h = 12.0_f64.min(track_h);
            let thumb_h = (track_h * rows as f64 / total as f64).max(min_h).min(track_h);
            let span = (track_h - thumb_h).max(1.0);
            // offset == scrollable → top of track; offset 0 → bottom.
            let frac = (((my - track_top) - thumb_h / 2.0) / span).clamp(0.0, 1.0);
            let target = ((scrollable as f64) * (1.0 - frac)).round() as i32;
            let delta = target - cur as i32;
            if delta != 0 {
                t.scroll(delta);
                self.request_redraw();
            }
        }
    }

    /// Re-read ~/.config/breeze/config and apply the colors/font to this window
    /// live (the parts that can change without respawning a terminal).
    fn apply_config(&mut self) {
        let cfg = breeze_platform::config_store::load();
        let to_rgb = |c: breeze_core::config::Rgba| (c.r, c.g, c.b);
        self.bg = cfg.background.map(to_rgb).unwrap_or(BG);
        self.fg = cfg.foreground.map(to_rgb).unwrap_or(FG);
        self.sel_bg = cfg.selection_background.map(to_rgb).unwrap_or(SEL_BG);
        self.font_size = cfg.font_size.map(|s| s as f32).unwrap_or(FONT_SIZE);
        self.renderer.set_font_family(cfg.font_family.clone());
        self.relayout();
        self.request_redraw();
    }

    /// Run a command-palette entry. Names mirror `command_list`.
    fn run_command(&mut self, name: &str) {
        match name {
            "New Tab" => self.new_tab(),
            "Close Pane" => self.do_close_pane(),
            "Split Right" => self.split_focused(SplitDirection::Vertical),
            "Split Down" => self.split_focused(SplitDirection::Horizontal),
            "Swap Panes" => {
                self.active_tab_mut().workspace.swap_focused_with_next();
                self.relayout();
                self.request_redraw();
            }
            "Save Layout" => {
                self.name_prompt = Some(String::new());
                self.request_redraw();
            }
            "Open Layout" => self.load_layout(),
            "Delete Layout" => {
                // Remove the most-recently-saved layout preset.
                if let Some(name) = breeze_platform::layout_store::list().last() {
                    let _ = breeze_platform::layout_store::delete(name);
                }
            }
            "Scan Orphans" => {
                self.orphan_overlay = Some(breeze_platform::proc::find_all_agent_processes());
                self.request_redraw();
            }
            "About" => {
                self.about_open = true;
                self.request_redraw();
            }
            "Reload Config" => self.apply_config(),
            "Settings" => {
                self.palette.set_items(self.settings_list());
                self.palette.open();
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn new_tab(&mut self) {
        if let Some(t) = self.tabs.get(self.active) {
            t.set_background(true);
        }
        self.tabs.push(Tab::new(&self.proxy));
        self.active = self.tabs.len() - 1;
        self.relayout();
        self.save_session();
        self.request_redraw();
    }

    fn switch_tab(&mut self, forward: bool) {
        if self.tabs.len() < 2 {
            return;
        }
        self.tabs[self.active].set_background(true);
        let n = self.tabs.len();
        self.active = if forward { (self.active + 1) % n } else { (self.active + n - 1) % n };
        self.tabs[self.active].set_background(false);
        self.relayout();
        self.request_redraw();
    }

    fn close_active_tab(&mut self) {
        self.close_tab(self.active);
    }

    /// Close the tab at index `i`. Adjusts the active index, foregrounds the new
    /// active tab, and persists; closing the last tab lets the router quit.
    /// Run the open confirm dialog's action (Enter key or the Close/Delete button):
    /// erase the marked panes, close the chosen tab, or close the focused pane.
    fn confirm_accept(&mut self) {
        if self.confirm.confirm() {
            // Update prompt takes priority: download + swap + relaunch (or exits).
            if let Some(url) = self.pending_update.take() {
                if let Err(e) = breeze_platform::update::apply(&url) {
                    eprintln!("breeze: update failed: {e}");
                }
                return;
            }
            if let Some(ids) = self.pending_erase.take() {
                self.erase_panes(&ids);
            } else {
                match self.pending_close_tab.take() {
                    Some(i) => self.close_tab(i),
                    None => self.do_close_pane(),
                }
            }
        }
        self.pending_erase = None;
        self.marked_panes.clear();
    }

    /// Dismiss the confirm dialog without acting (Esc or the Cancel button).
    fn confirm_dismiss(&mut self) {
        self.confirm.cancel();
        self.pending_close_tab = None;
        self.pending_erase = None;
        self.pending_update = None;
        self.marked_panes.clear();
    }

    /// Offer a newer release: arm the install URL and open the confirm prompt.
    fn offer_update(&mut self, version: String, url: String) {
        if self.confirm.is_open() {
            return; // don't stomp an existing prompt
        }
        self.pending_update = Some(url);
        self.confirm.ask_labeled(format!("Breeze {version} is available. Update now?"), "Update");
        self.request_redraw();
    }

    /// Rects of the confirm modal's `(Cancel, Confirm)` buttons in physical px —
    /// shared by render and the modal pointer hit-test. Must match the render
    /// geometry in `redraw`.
    fn confirm_button_rects(&self) -> Option<((i32, i32, i32, i32), (i32, i32, i32, i32))> {
        if !self.confirm.is_open() {
            return None;
        }
        let size = self.window.as_ref()?.inner_size();
        let (pw, ph) = (size.width as i32, size.height as i32);
        let font_size = self.fs();
        let line_h = (font_size * 1.3).ceil() as i32;
        let si = self.scale() as i32;
        let panel_w = (pw * 5 / 10).clamp(260, 560);
        let panel_h = line_h * 2 + 24 + line_h + 8 * si; // message + hint + button row
        let px0 = (pw - panel_w) / 2;
        let py0 = (ph - panel_h) / 2;
        let pad = 10;
        let btn_w = 92 * si;
        let btn_h = line_h + 6 * si;
        let by = py0 + panel_h - pad - btn_h;
        let confirm_x = px0 + panel_w - pad - btn_w;
        let cancel_x = confirm_x - 8 * si - btn_w;
        Some((
            (cancel_x, by, btn_w, btn_h),
            (confirm_x, by, btn_w, btn_h),
        ))
    }

    /// The tab's display label: the focused pane's program title, else `Tab N`.
    /// Shared by the tab-bar render and the close-confirm prompt.
    fn tab_label(&self, i: usize) -> String {
        self.tabs
            .get(i)
            .and_then(|t| t.panes.get(&t.workspace.focused().id).and_then(|p| p.title()))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| format!("Tab {}", i + 1))
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let mut tab = self.tabs.remove(i);
        for (_, mt) in tab.panes.drain() {
            let _ = self.chore_tx.send(Chore::Reap(mt));
        }
        if self.tabs.is_empty() {
            // Last tab closed → quit on next loop pass.
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if i < self.active {
            self.active -= 1;
        }
        self.tabs[self.active].set_background(false);
        self.relayout();
        self.save_session();
        self.request_redraw();
    }

    fn split_focused(&mut self, dir: SplitDirection) {
        // New pane inherits the focused pane's working directory.
        let cwd = {
            let t = self.active_tab();
            t.panes.get(&t.workspace.focused().id).and_then(|p| p.cwd())
        };
        let new_id = self.active_tab_mut().workspace.split(dir);
        if let Some(t) = spawn_terminal(&self.proxy, cwd.as_deref()) {
            self.active_tab_mut().panes.insert(new_id.id, t);
        }
        self.relayout();
        self.save_session();
        self.request_redraw();
    }

    /// Add a pane the balanced way (split the shallowest leaf, alternating
    /// direction by depth) — what the tool-strip "+" does.
    fn add_pane(&mut self) {
        let cwd = {
            let t = self.active_tab();
            t.panes.get(&t.workspace.focused().id).and_then(|p| p.cwd())
        };
        let new_id = self.active_tab_mut().workspace.add_pane_balanced();
        if let Some(t) = spawn_terminal(&self.proxy, cwd.as_deref()) {
            self.active_tab_mut().panes.insert(new_id.id, t);
        }
        self.relayout();
        self.save_session();
        self.request_redraw();
    }

    /// Set the active tab's pane count. Growing adds balanced panes; shrinking
    /// removes idle, non-focused panes from the end (never the focused pane or a
    /// pane with a running command), stopping when the target is met or no idle
    /// panes remain.
    fn set_pane_count(&mut self, target: usize) {
        let target = target.max(1);
        let current = self.active_tab().workspace.pane_count();
        if target > current {
            for _ in current..target {
                self.add_pane();
            }
            return;
        }
        if target < current {
            let focused = self.active_tab().workspace.focused();
            let order = breeze_ui::panes::leaf_ids_in_order(self.active_tab().workspace.tree());
            for id in order.into_iter().rev() {
                if self.active_tab().workspace.pane_count() <= target {
                    break;
                }
                if id == focused {
                    continue;
                }
                let idle = self.active_tab().panes.get(&id.id).map(|p| p.is_idle()).unwrap_or(true);
                if idle {
                    if let Some(closed) = self.active_tab_mut().workspace.close(id) {
                        if let Some(mt) = self.active_tab_mut().panes.remove(&closed.id) {
                            let _ = self.chore_tx.send(Chore::Reap(mt));
                        }
                    }
                }
            }
            self.relayout();
            self.save_session();
            self.request_redraw();
        }
    }

    /// Cmd-W: close the focused pane; if it's the tab's last pane, that closes
    /// the tab — so confirm first.
    fn request_close(&mut self) {
        if self.active_tab().workspace.pane_count() <= 1 {
            self.confirm.ask_labeled("Close this tab?", "Close");
            self.request_redraw();
        } else {
            self.do_close_pane();
        }
    }

    fn do_close_pane(&mut self) {
        if let Some(closed) = self.active_tab_mut().workspace.close_focused() {
            if let Some(mt) = self.active_tab_mut().panes.remove(&closed.id) {
                let _ = self.chore_tx.send(Chore::Reap(mt));
            }
            self.relayout();
            self.save_session();
            self.request_redraw();
        } else {
            // Was the last pane → close the whole tab.
            self.close_active_tab();
        }
    }

    /// Open the "Delete N pane(s)?" confirm for the given panes (the red rim
    /// highlight shows only while this confirm is open).
    fn ask_delete(&mut self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        let n = ids.len();
        let plural = if n == 1 { "pane" } else { "panes" };
        self.pending_erase = Some(ids);
        self.confirm.ask_labeled(format!("Delete {n} {plural}?"), "Delete");
    }

    /// Delete the given panes from the workspace. A tab emptied this way is
    /// itself closed.
    fn erase_panes(&mut self, ids: &[i64]) {
        for &id in ids {
            // Closing the tab's last pane closes the tab instead.
            if self.active_tab().workspace.pane_count() <= 1 {
                self.close_active_tab();
                continue;
            }
            let pid = breeze_core::split_tree::PaneID::new(id);
            if let Some(closed) = self.active_tab_mut().workspace.close(pid) {
                if let Some(mt) = self.active_tab_mut().panes.remove(&closed.id) {
                    let _ = self.chore_tx.send(Chore::Reap(mt));
                }
            }
        }
        self.relayout();
        self.save_session();
        self.request_redraw();
    }
}

impl WindowState {
    /// Create this window's OS window + surface. `restore` (the first window
    /// only) reopens the saved session; others get a single fresh tab.
    fn create(&mut self, event_loop: &ActiveEventLoop, restore: bool) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Breeze")
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 600.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("breeze: failed to create window: {e}");
                self.wants_close = true;
                return;
            }
        };
        let context = Context::new(window.clone()).expect("softbuffer context");
        let surface = Surface::new(&context, window.clone()).expect("softbuffer surface");

        // The first window restores a saved session if present; new windows (or a
        // first launch with no session) open a single fresh tab.
        {
            let saved = if restore { breeze_platform::session_store::load() } else { None };
            match saved {
                Some(saved) if !saved.is_empty() => {
                    for sg in &saved {
                        self.tabs.push(Tab::from_saved(&self.proxy, sg));
                    }
                    self.active = 0;
                    for (i, tab) in self.tabs.iter().enumerate() {
                        tab.set_background(i != self.active);
                    }
                }
                _ => {
                    self.tabs.push(Tab::new(&self.proxy));
                    self.active = 0;
                }
            }
        }

        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        self.relayout();
    }

    /// This window's OS id, once created.
    fn id(&self) -> Option<WindowId> {
        self.window.as_ref().map(|w| w.id())
    }

    /// Periodic per-window upkeep (driven by the global tick): attach engines,
    /// reap exited panes, track the OS power posture. Sets `wants_close` when no
    /// tabs remain.
    fn tick(&mut self) {
        if self.tabs.is_empty() {
            self.wants_close = true;
            return;
        }
        for tab in &self.tabs {
            for t in tab.panes.values() {
                t.poll_attach();
            }
        }
        if self.reap_exited_panes() && self.tabs.is_empty() {
            self.wants_close = true;
            return;
        }
        // Acknowledge the visible (focused) pane so it never carries a stale
        // badge, and recompute which background panes have settled. Repaint only
        // on a change — this rides the existing tick, adding no new wakeups.
        let sig = self.tabs.get(self.active).map(|tab| {
            let focused = tab.workspace.focused().id;
            if let Some(t) = tab.panes.get(&focused) {
                t.ack_activity();
            }
            let mut sig = 0u64;
            for (id, t) in &tab.panes {
                if *id != focused && t.is_settled() {
                    sig ^= (*id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                }
            }
            sig
        });
        if let Some(sig) = sig {
            if sig != self.badge_sig {
                self.badge_sig = sig;
                self.request_redraw();
            }
        }
        self.power_tick = self.power_tick.wrapping_add(1);
        if self.power_tick % 16 == 0 {
            let lp = breeze_platform::power::low_power_active();
            if lp != self.low_power {
                self.low_power = lp;
                for tab in &self.tabs {
                    for t in tab.panes.values() {
                        t.set_low_power(lp);
                    }
                }
            }
        }
    }

    fn handle_window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.wants_close = true,
            WindowEvent::Focused(focused) => {
                // When the whole app loses focus, the visible tab's agent is no
                // longer being watched — freeze it like a background tab to save
                // battery; thaw it when focus returns.
                if let Some(t) = self.tabs.get(self.active) {
                    t.set_background(!focused);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::DroppedFile(path) => {
                // Insert the dropped path (shell-escaped) into the focused pane.
                let escaped = breeze_core::shell::shell_escape(&path.to_string_lossy());
                if let Some(t) = self.active_tab_mut().focused_terminal() {
                    let _ = t.write(escaped.as_bytes());
                    let _ = t.write(b" ");
                }
                self.request_redraw();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 20.0) as i32,
                };
                if lines != 0 && !self.tabs.is_empty() {
                    if let Some(t) = self.active_tab_mut().focused_terminal() {
                        t.scroll(lines);
                    }
                    self.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (ox, oy) = self.cursor;
                self.cursor = (position.x, position.y);
                // Repaint the tab-bar hover affordances (× / + / pane-add) while the
                // cursor is over the bar (or just left it), so hover states update live.
                let bar_h_f = chrome::tab_bar_height(self.scale()) as f64;
                if position.y < bar_h_f || oy < bar_h_f {
                    self.request_redraw();
                }
                // A scrollbar drag tracks the cursor's y and beats everything else.
                if self.scrollbar_drag.is_some() {
                    self.scroll_drag_to(position.y);
                    return;
                }
                // Divider drag-to-resize takes precedence over text selection.
                if let Some(drag) = self.drag_divider.clone() {
                    if let Some(window) = self.window.as_ref() {
                        let size = window.inner_size();
                        let content = self.content_rect(size.width, size.height);
                        let (dx, dy) = (position.x - ox, position.y - oy);
                        // Vertical dividers track the horizontal delta; horizontal
                        // dividers track the vertical delta. With both populated
                        // this resizes along both axes (omni / intersection drag).
                        for path in &drag.v_paths {
                            self.active_tab_mut().workspace.resize_divider(path, dx, content.width);
                        }
                        for path in &drag.h_paths {
                            self.active_tab_mut().workspace.resize_divider(path, dy, content.height);
                        }
                        self.relayout();
                        self.request_redraw();
                    }
                    return;
                }
                if self.pane_drag.is_some() {
                    // Repaint so the drop-target highlight follows the cursor.
                    self.request_redraw();
                } else if self.selecting {
                    if let Some(rect) = self.focused_pane_rect() {
                        let cell = self.cursor_cell(rect);
                        if let Some((start, _)) = self.selection {
                            self.selection = Some((start, cell));
                            self.request_redraw();
                        }
                    }
                } else if let Some(window) = self.window.as_ref() {
                    // Show a resize cursor when hovering a divider, else default.
                    use winit::window::CursorIcon;
                    let size = window.inner_size();
                    let content = self.content_rect(size.width, size.height);
                    let tol = 4.0 * self.scale() as f64;
                    let icon = match breeze_ui::panes::divider_at(
                        self.active_tab().workspace.tree(), content, position.x, position.y, tol,
                    ) {
                        Some(d) => match d.direction {
                            breeze_core::split_tree::SplitDirection::Vertical => CursorIcon::ColResize,
                            breeze_core::split_tree::SplitDirection::Horizontal => CursorIcon::RowResize,
                        },
                        None => CursorIcon::Default,
                    };
                    window.set_cursor(icon);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // Right-click anywhere opens the command palette (context menu).
                if button == winit::event::MouseButton::Right {
                    if state == ElementState::Pressed && !self.tabs.is_empty() {
                        self.palette.set_items(command_list());
                        self.palette.open();
                        self.request_redraw();
                    }
                    return;
                }
                if button != winit::event::MouseButton::Left || self.tabs.is_empty() {
                    return;
                }
                // The confirm dialog is modal for the pointer: a click hits its
                // Cancel/Confirm buttons; any other click is swallowed.
                if self.confirm.is_open() {
                    if state == ElementState::Pressed {
                        if let Some((cancel, ok)) = self.confirm_button_rects() {
                            let (cx, cy) = self.cursor;
                            let inside = |r: (i32, i32, i32, i32)| {
                                cx >= r.0 as f64 && cx < (r.0 + r.2) as f64 && cy >= r.1 as f64 && cy < (r.1 + r.3) as f64
                            };
                            if inside(ok) {
                                self.confirm_accept();
                            } else if inside(cancel) {
                                self.confirm_dismiss();
                            }
                            self.request_redraw();
                        }
                    }
                    return;
                }
                // While the palette is open, a click selects/runs a row or
                // dismisses it.
                if self.palette.is_open() {
                    if state == ElementState::Pressed {
                        let (cx, cy) = self.cursor;
                        if let Some(size) = self.window.as_ref().map(|w| w.inner_size()) {
                            let name = self
                                .palette_row_at(cx, cy, size.width as i32, size.height as i32)
                                .and_then(|i| self.palette.visible().get(i).map(|s| s.to_string()));
                            self.palette.close();
                            if let Some(n) = name {
                                self.run_command(&n);
                            }
                            self.request_redraw();
                        }
                    }
                    return;
                }
                // While a double-click context menu is open, a click hits its
                // delete row or dismisses it (handled before the rest).
                if self.pane_menu.is_some() {
                    if state == ElementState::Pressed {
                        let (cx, cy) = self.cursor;
                        let hit = self
                            .pane_menu_rect()
                            .map(|(x, y, w, h)| {
                                cx >= x as f64 && cx < (x + w) as f64 && cy >= y as f64 && cy < (y + h) as f64
                            })
                            .unwrap_or(false);
                        if hit {
                            if let Some(menu) = self.pane_menu.take() {
                                self.ask_delete(menu.targets);
                            }
                        } else {
                            self.pane_menu = None;
                        }
                        self.request_redraw();
                    }
                    return;
                }
                match state {
                    ElementState::Pressed => {
                        let size = self.window.as_ref().map(|w| w.inner_size());
                        if let Some(size) = size {
                            let content = self.content_rect(size.width, size.height);
                            let (cx, cy) = self.cursor;
                            let bar_h = chrome::tab_bar_height(self.scale()) as f64;
                            // Tab bar: pane controls (right), new-tab "+", or a tab.
                            if cy < bar_h {
                                let si = self.scale() as i32;
                                let cb_r = 7 * si;
                                // Fixed right control cluster (matches the render layout):
                                // the "+" chip, then the split icon, then the pane count.
                                let plus_cx = size.width as i32 - 61 * si;
                                let plus_cy = chrome::tab_bar_height(self.scale()) / 2;
                                let pdx = cx - plus_cx as f64;
                                let pdy = cy - plus_cy as f64;
                                let phit = (cb_r + 3 * si) as f64;
                                if pdx * pdx + pdy * pdy <= phit * phit {
                                    self.new_tab();
                                    return;
                                }
                                if cx >= (size.width as i32 - 28 * si) as f64 {
                                    // The count number (rightmost) opens the count editor.
                                    let n = self.active_tab().workspace.pane_count();
                                    self.count_prompt = Some(n.to_string());
                                    self.request_redraw();
                                    return;
                                }
                                if cx >= (size.width as i32 - 50 * si) as f64 {
                                    // The split icon adds a pane.
                                    self.add_pane();
                                    return;
                                }
                                let tabs_avail = (size.width as i32 - 80 * si).max(60);
                                let tab_w = (tabs_avail / (self.tabs.len().max(1) as i32)).clamp(60 * si, 200 * si);
                                {
                                    let i = (cx as i32 / tab_w.max(1)) as usize;
                                    if i < self.tabs.len() {
                                        // Close circle hit-test (matches the render layout:
                                        // small circle at the tab's right edge).
                                        let bar_h_i = chrome::tab_bar_height(self.scale());
                                        let cb_r = 7 * si;
                                        let cb_cx = i as i32 * tab_w + tab_w - 6 * si - cb_r;
                                        let cb_cy = bar_h_i / 2;
                                        let dx = cx - cb_cx as f64;
                                        let dy = cy - cb_cy as f64;
                                        // Click hit area matches the hover highlight (`cb_r + 3*si`)
                                        // so clicking the lit-up "×" always closes — never falls
                                        // through to switching to that tab.
                                        let hit = (cb_r + 3 * si) as f64;
                                        if dx * dx + dy * dy <= hit * hit {
                                            // Clicked the close "×" → confirm closing that tab.
                                            self.pending_close_tab = Some(i);
                                            self.confirm.ask_labeled(
                                                format!("Close \"{}\"?", self.tab_label(i)),
                                                "Close",
                                            );
                                            self.request_redraw();
                                        } else if i != self.active {
                                            self.tabs[self.active].set_background(true);
                                            self.active = i;
                                            self.tabs[self.active].set_background(false);
                                            self.relayout();
                                            self.request_redraw();
                                        }
                                    }
                                }
                                return;
                            }
                            // Scrollbar grab: the right-edge track band of the pane
                            // under the cursor. Engages whenever there's scrollback —
                            // even at the live edge — so the bar is always grabbable.
                            {
                                let si = self.scale() as f64;
                                let bar_w = 7.0 * si;
                                let margin = 3.0 * si;
                                let hit = breeze_ui::panes::pane_rects(self.active_tab().workspace.tree(), content)
                                    .into_iter()
                                    .find(|(_, r)| {
                                        cx >= r.x + r.width - bar_w - margin
                                            && cx <= r.x + r.width
                                            && cy >= r.y
                                            && cy < r.y + r.height
                                    });
                                if let Some((id, r)) = hit {
                                    let ch = breeze_ui::render::cell_size(self.fs()).1 as f64;
                                    let rows = (r.height / ch).floor().max(1.0) as usize;
                                    let scrollable = self.tabs[self.active]
                                        .panes
                                        .get(&id.id)
                                        .map(|t| t.scroll_position().1.saturating_sub(rows))
                                        .unwrap_or(0);
                                    if scrollable > 0 {
                                        self.scrollbar_drag = Some(ScrollDrag {
                                            pane: id.id,
                                            track_top: r.y,
                                            track_h: r.height,
                                        });
                                        self.scroll_drag_to(cy);
                                        return;
                                    }
                                }
                            }
                            // Shift+click marks/unmarks a pane for deletion; a
                            // double-click opens a context menu with a delete action.
                            let now = std::time::Instant::now();
                            let is_double = self
                                .last_click
                                .map(|(t, lx, ly)| {
                                    now.duration_since(t).as_millis() < 400
                                        && (lx - cx).abs() < 6.0
                                        && (ly - cy).abs() < 6.0
                                })
                                .unwrap_or(false);
                            self.last_click = Some((now, cx, cy));
                            let pane_here =
                                pane_at(self.active_tab().workspace.tree(), content, cx, cy).map(|p| p.id);
                            if self.modifiers.shift_key() {
                                if let Some(id) = pane_here {
                                    match self.marked_panes.iter().position(|&m| m == id) {
                                        Some(pos) => {
                                            self.marked_panes.remove(pos);
                                        }
                                        None => self.marked_panes.push(id),
                                    }
                                    self.request_redraw();
                                }
                                return;
                            }
                            if is_double {
                                if let Some(id) = pane_here {
                                    let targets = if self.marked_panes.is_empty() {
                                        vec![id]
                                    } else {
                                        self.marked_panes.clone()
                                    };
                                    let si = self.scale() as i32;
                                    let mw = 200 * si;
                                    let mh = (self.fs() * 1.3).ceil() as i32 + 8 * si;
                                    let mx = (cx as i32).min(size.width as i32 - mw - 4).max(0);
                                    let my = (cy as i32).min(size.height as i32 - mh - 4).max(0);
                                    self.pane_menu = Some(PaneMenu { x: mx, y: my, targets });
                                    self.request_redraw();
                                }
                                return;
                            }
                            // Per-pane close button (×) / move handle on the focused pane.
                            if self.active_tab().workspace.pane_count() > 1 {
                                if let Some(pr) = self.focused_pane_rect() {
                                    let (bx, by, bw, bh) = self.close_button_rect(pr);
                                    if cx >= bx as f64 && cx < (bx + bw) as f64 && cy >= by as f64 && cy < (by + bh) as f64 {
                                        self.do_close_pane();
                                        return;
                                    }
                                    let (mx, my, mw, mh) = self.move_handle_rect(pr);
                                    if cx >= mx as f64 && cx < (mx + mw) as f64 && cy >= my as f64 && cy < (my + mh) as f64 {
                                        self.pane_drag = Some(self.active_tab().workspace.focused());
                                        return;
                                    }
                                }
                            }
                            // Grab a divider first (drag-to-resize).
                            let tol = 4.0 * self.scale() as f64;
                            let near = breeze_ui::panes::dividers_at(self.active_tab().workspace.tree(), content, cx, cy, tol);
                            if !near.is_empty() {
                                use breeze_core::split_tree::SplitDirection;
                                let all = breeze_ui::panes::all_dividers(self.active_tab().workspace.tree(), content);
                                // Link aligned same-axis dividers; if both a
                                // vertical and a horizontal are grabbed, this is
                                // an intersection (omni) drag over both axes.
                                let v_paths = near
                                    .iter()
                                    .find(|d| d.direction == SplitDirection::Vertical)
                                    .map(|d| breeze_ui::panes::find_linked(&all, d, tol))
                                    .unwrap_or_default();
                                let h_paths = near
                                    .iter()
                                    .find(|d| d.direction == SplitDirection::Horizontal)
                                    .map(|d| breeze_ui::panes::find_linked(&all, d, tol))
                                    .unwrap_or_default();
                                self.drag_divider = Some(DragDivider { v_paths, h_paths });
                                return;
                            }
                            if let Some(id) = pane_at(self.active_tab().workspace.tree(), content, cx, cy) {
                                self.active_tab_mut().workspace.focus(id);
                                if let Some(rect) = self.focused_pane_rect() {
                                    let cell = self.cursor_cell(rect);
                                    self.selection = Some((cell, cell));
                                    self.selecting = true;
                                }
                                self.request_redraw();
                            }
                        }
                    }
                    ElementState::Released => {
                        // End a scrollbar drag.
                        if self.scrollbar_drag.take().is_some() {
                            self.request_redraw();
                            return;
                        }
                        // Finish a pane drag-move: swap with the pane under the cursor.
                        if let Some(src) = self.pane_drag.take() {
                            if let Some(size) = self.window.as_ref().map(|w| w.inner_size()) {
                                let content = self.content_rect(size.width, size.height);
                                let (cx, cy) = self.cursor;
                                if let Some(dst) = pane_at(self.active_tab().workspace.tree(), content, cx, cy) {
                                    self.active_tab_mut().workspace.swap(src, dst);
                                    self.relayout();
                                    self.save_session();
                                }
                            }
                            self.request_redraw();
                            return;
                        }
                        if self.selecting && self.copy_on_select && self.selection.is_some() {
                            self.copy_selection();
                        }
                        self.selecting = false;
                        if self.drag_divider.take().is_some() {
                            self.save_session();
                        }
                    }
                }
            }
            WindowEvent::Resized(_) => {
                self.relayout();
                self.request_redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let cmd = self.modifiers.super_key();
                let shift = self.modifiers.shift_key();

                if self.confirm.is_open() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.confirm_dismiss(),
                        Key::Named(NamedKey::Enter) => self.confirm_accept(),
                        _ => {}
                    }
                    self.request_redraw();
                    return;
                }

                // Esc closes an open double-click context menu.
                if self.pane_menu.is_some() && matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                    self.pane_menu = None;
                    self.request_redraw();
                    return;
                }
                // Delete removes the marked panes — only when something is marked;
                // otherwise Delete passes through to the focused terminal.
                if matches!(event.logical_key, Key::Named(NamedKey::Delete)) && !self.marked_panes.is_empty() {
                    let ids = std::mem::take(&mut self.marked_panes);
                    self.ask_delete(ids);
                    self.request_redraw();
                    return;
                }

                // Inline name-input overlay captures keys while open.
                if let Some(buf) = self.name_prompt.as_mut() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.name_prompt = None,
                        Key::Named(NamedKey::Enter) => {
                            if let Some(name) = self.name_prompt.take() {
                                let name = name.trim();
                                if !name.is_empty() {
                                    let tree = self.active_tab().workspace.tree().clone();
                                    let _ = breeze_platform::layout_store::save(name, &tree);
                                }
                            }
                        }
                        Key::Named(NamedKey::Backspace) => {
                            buf.pop();
                        }
                        Key::Character(s) => {
                            for c in s.chars() {
                                if !c.is_control() {
                                    buf.push(c);
                                }
                            }
                        }
                        _ => {}
                    }
                    self.request_redraw();
                    return;
                }

                // Inline pane-count editor captures keys (digits only) while open.
                if let Some(buf) = self.count_prompt.as_mut() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.count_prompt = None,
                        Key::Named(NamedKey::Enter) => {
                            if let Some(text) = self.count_prompt.take() {
                                if let Ok(n) = text.trim().parse::<usize>() {
                                    self.set_pane_count(n);
                                }
                            }
                        }
                        Key::Named(NamedKey::Backspace) => {
                            buf.pop();
                        }
                        Key::Character(s) => {
                            for c in s.chars() {
                                if c.is_ascii_digit() {
                                    buf.push(c);
                                }
                            }
                        }
                        _ => {}
                    }
                    self.request_redraw();
                    return;
                }

                // About overlay closes on any key.
                if self.about_open {
                    self.about_open = false;
                    self.request_redraw();
                    return;
                }
                // Orphan-scan overlay captures keys while open.
                if self.orphan_overlay.is_some() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.orphan_overlay = None;
                        }
                        Key::Named(NamedKey::Enter) => {
                            if let Some(pids) = self.orphan_overlay.take() {
                                for p in pids {
                                    breeze_platform::suspend::terminate(p);
                                }
                            }
                        }
                        _ => {}
                    }
                    self.request_redraw();
                    return;
                }

                if cmd && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("k")) {
                    if self.palette.is_open() {
                        self.palette.close();
                    } else {
                        self.palette.set_items(command_list());
                        self.palette.open();
                    }
                    self.request_redraw();
                    return;
                }
                // Cmd-, opens the settings palette (config values + Reload).
                if cmd && matches!(&event.logical_key, Key::Character(s) if s == ",") {
                    self.palette.set_items(self.settings_list());
                    self.palette.open();
                    self.request_redraw();
                    return;
                }
                if self.palette.is_open() {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => self.palette.close(),
                        Key::Named(NamedKey::Enter) => {
                            let cmd = self.palette.selected_item().map(|s| s.to_string());
                            self.palette.close();
                            if let Some(c) = cmd {
                                self.run_command(&c);
                            }
                        }
                        Key::Named(NamedKey::Backspace) => self.palette.backspace(),
                        Key::Named(NamedKey::ArrowDown) => self.palette.move_selection(1),
                        Key::Named(NamedKey::ArrowUp) => self.palette.move_selection(-1),
                        Key::Character(s) => {
                            for c in s.chars() {
                                self.palette.push_char(c);
                            }
                        }
                        _ => {}
                    }
                    self.request_redraw();
                    return;
                }

                if cmd {
                    if let Key::Character(s) = &event.logical_key {
                        match s.to_lowercase().as_str() {
                            "t" => {
                                self.new_tab();
                                return;
                            }
                            "n" => {
                                // Open a new OS window (the router creates it).
                                let _ = self.proxy.send_event(UserEvent::NewWindow);
                                return;
                            }
                            "c" if self.selection.is_some() => {
                                self.copy_selection();
                                return;
                            }
                            "a" if shift => {
                                self.about_open = true;
                                self.request_redraw();
                                return;
                            }
                            "s" if shift => {
                                // Open the inline name input; Enter saves the
                                // current arrangement as a named layout.
                                self.name_prompt = Some(String::new());
                                self.request_redraw();
                                return;
                            }
                            "l" if shift => {
                                self.load_layout();
                                return;
                            }
                            "x" if shift => {
                                self.active_tab_mut().workspace.swap_focused_with_next();
                                self.relayout();
                                self.request_redraw();
                                return;
                            }
                            "o" if shift => {
                                // Scan for orphaned agent processes system-wide.
                                self.orphan_overlay =
                                    Some(breeze_platform::proc::find_all_agent_processes());
                                self.request_redraw();
                                return;
                            }
                            "v" => {
                                if let Ok(mut cb) = arboard::Clipboard::new() {
                                    if let Ok(text) = cb.get_text() {
                                        if let Some(t) = self.active_tab_mut().focused_terminal() {
                                            let _ = t.write(text.as_bytes());
                                        }
                                        self.request_redraw();
                                    }
                                }
                                return;
                            }
                            "d" => {
                                self.split_focused(if shift {
                                    SplitDirection::Horizontal
                                } else {
                                    SplitDirection::Vertical
                                });
                                return;
                            }
                            "w" => {
                                self.request_close();
                                return;
                            }
                            "]" => {
                                if shift {
                                    self.switch_tab(true);
                                } else {
                                    self.active_tab_mut().workspace.focus_cycle(true);
                                    self.request_redraw();
                                }
                                return;
                            }
                            "[" => {
                                if shift {
                                    self.switch_tab(false);
                                } else {
                                    self.active_tab_mut().workspace.focus_cycle(false);
                                    self.request_redraw();
                                }
                                return;
                            }
                            _ => {}
                        }
                    }
                }

                let ctrl = self.modifiers.control_key();
                let alt = self.modifiers.alt_key();
                if let Some(bytes) = encode_key(&event.logical_key, ctrl, alt) {
                    if let Some(t) = self.active_tab_mut().focused_terminal() {
                        let _ = t.write(&bytes);
                    }
                    self.request_redraw();
                }
            }
            _ => {}
        }
    }
}

impl WindowState {
    fn redraw(&mut self) {
        // Physical-pixel font size: the softbuffer surface is physical pixels,
        // so all glyph/grid/overlay math uses this (not the raw point size).
        let (font_size, fg, bg) = (self.fs(), self.fg, self.bg);
        let scale = self.scale();
        let inset = self.pane_inset();
        if self.tabs.is_empty() {
            return;
        }
        let Some(window) = self.window.as_ref() else { return };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        let (pw, ph) = (w.get() as usize, h.get() as usize);

        let content = self.content_rect(size.width, size.height);
        let tab = &self.tabs[self.active];
        let focused = tab.workspace.focused();
        type PaneDraw = (Rect, Vec<Vec<breeze_vt::Cell>>, bool, (usize, usize), (usize, usize), bool);
        let panes: Vec<PaneDraw> = pane_rects(tab.workspace.tree(), content)
            .into_iter()
            .filter_map(|(id, r)| {
                tab.panes.get(&id.id).map(|t| {
                    (
                        r,
                        t.screen_cells(),
                        id == focused,
                        t.scroll_position(),
                        t.cursor_pos(),
                        id != focused && t.is_settled(),
                    )
                })
            })
            .collect();
        // Divider lines between panes (so splits read as separate regions).
        let dividers = breeze_ui::panes::all_dividers(tab.workspace.tree(), content);
        let tab_count = self.tabs.len();
        let active = self.active;
        // Per-tab label: the focused pane's program title, else "Tab N".
        let tab_labels: Vec<String> = (0..self.tabs.len()).map(|i| self.tab_label(i)).collect();
        let palette_open = self.palette.is_open();
        let bar_h = chrome::tab_bar_height(scale);
        let about_open = self.about_open;
        let pane_drag = self.pane_drag;
        let cursor = self.cursor;
        let mem_alert = self
            .tabs
            .get(self.active)
            .and_then(|t| t.panes.get(&t.workspace.focused().id))
            .and_then(|p| p.memory_alert_gb());
        let name_prompt = self.name_prompt.clone();
        // Pane-deletion highlight (captured before the frame buffer is borrowed):
        // red rims show ONLY while the confirm is open (pending_erase); otherwise
        // the Shift+click-marked set gets a subtle rim.
        let highlight_red = self.pending_erase.is_some();
        let highlight_ids: &[i64] =
            if let Some(p) = &self.pending_erase { p } else { &self.marked_panes };
        let highlight_rects: Vec<Rect> = if highlight_ids.is_empty() {
            Vec::new()
        } else {
            pane_rects(tab.workspace.tree(), content)
                .into_iter()
                .filter(|(id, _)| highlight_ids.contains(&id.id))
                .map(|(_, r)| r)
                .collect()
        };
        // The open double-click menu: position + how many panes its delete targets.
        let pane_menu_draw = self.pane_menu.as_ref().map(|m| (m.x, m.y, m.targets.len()));
        let count_prompt = self.count_prompt.clone();
        let orphans = self.orphan_overlay.clone();
        let orphan_perm = if orphans.is_some() {
            Some(breeze_platform::permissions::process_scan_access())
        } else {
            None
        };
        // Reflect the focused pane's program title in the window title.
        let win_title = self
            .tabs
            .get(self.active)
            .and_then(|t| t.panes.get(&t.workspace.focused().id))
            .and_then(|t| t.title())
            .map(|t| format!("{t} — Breeze"))
            .unwrap_or_else(|| "Breeze".to_string());
        window.set_title(&win_title);

        let Some(surface) = self.surface.as_mut() else { return };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buf) = surface.buffer_mut() else { return };

        fill_rect(&mut buf, pw, ph, 0, 0, pw as i32, ph as i32, bg);
        let (cw, ch) = breeze_ui::render::cell_size(font_size);
        for (r, cells, is_focused, scroll_pos, cursor_rc, settled) in &panes {
            // Content origin sits `inset` px inside the pane edge.
            let ox = r.x as i32 + inset;
            let oy = r.y as i32 + inset;
            // Full-color cell grid (per-cell bg + glyphs).
            self.renderer.draw_cells(&mut buf, pw, ph, ox, oy, cells, font_size, cw, ch, bg);
            // Steady block cursor on the focused pane, only at the live edge.
            if *is_focused && scroll_pos.0 == 0 {
                let (cr, cc) = *cursor_rc;
                if let Some(cell) = cells.get(cr).and_then(|row| row.get(cc)) {
                    let cx = ox + (cc as f32 * cw) as i32;
                    let cy = oy + (cr as f32 * ch) as i32;
                    fill_rect(&mut buf, pw, ph, cx, cy, cw.ceil() as i32, ch as i32, FOCUS_RIM);
                    if cell.ch != ' ' && cell.ch != '\0' {
                        // Redraw the glyph in the pane bg so it shows through.
                        self.renderer.draw_run(
                            &mut buf, pw, ph, cx, cy, cw + cw, ch, &cell.ch.to_string(), font_size, bg, cell.bold,
                        );
                    }
                }
            }
            // Selection highlight (focused pane), tinted over the text. Only a
            // real (non-empty) selection is shown — a bare click doesn't.
            if *is_focused {
                if let Some((s, e)) = self.selection {
                    if s != e {
                        let cols = ((r.width - (inset * 2) as f64) / cw as f64).floor() as usize;
                        for (row, c0, c1) in breeze_ui::select::selection_spans(s, e, cols) {
                            let x = ox + (c0 as f32 * cw) as i32;
                            let y = oy + (row as f32 * ch) as i32;
                            let w = ((c1 - c0 + 1) as f32 * cw) as i32;
                            breeze_ui::render::blend_rect(&mut buf, pw, ph, x, y, w, ch as i32, self.sel_bg, 150);
                        }
                    }
                }
            }
            if *is_focused && panes.len() > 1 {
                let (x, y, ww, hh) = (r.x as i32, r.y as i32, r.width as i32, r.height as i32);
                fill_rect(&mut buf, pw, ph, x, y, ww, scale as i32, FOCUS_RIM);
                fill_rect(&mut buf, pw, ph, x, y + hh - scale as i32, ww, scale as i32, FOCUS_RIM);
                fill_rect(&mut buf, pw, ph, x, y, scale as i32, hh, FOCUS_RIM);
                fill_rect(&mut buf, pw, ph, x + ww - scale as i32, y, scale as i32, hh, FOCUS_RIM);
                // Per-pane close button (×) at the focused pane's top-right.
                let bw_close = (font_size * 1.3) as i32;
                let m_close = 4 * scale as i32;
                let cbx = r.x as i32 + r.width as i32 - bw_close - m_close;
                let cby = r.y as i32 + m_close;
                self.renderer.draw_text_at(&mut buf, pw, ph, cbx, cby, bw_close as f32, bw_close as f32, "×", font_size, FOCUS_RIM);
                // Move handle (grip) at the focused pane's top-left.
                self.renderer.draw_text_at(&mut buf, pw, ph, r.x as i32 + m_close, r.y as i32 + m_close, bw_close as f32, bw_close as f32, "⠿", font_size, FOCUS_RIM);
            }
            // Settled badge: an unfocused pane whose output went quiet on its own
            // (a finished/paused process), so the user knows it's worth a look.
            if *settled {
                let bw = (font_size * 1.3) as i32;
                let m = 4 * scale as i32;
                let bx = r.x as i32 + r.width as i32 - bw - m;
                let by = r.y as i32 + m;
                self.renderer.draw_text_at(&mut buf, pw, ph, bx, by, bw as f32, bw as f32, "✓", font_size, FOCUS_RIM);
            }
            // Drop-target highlight while drag-moving a pane onto another.
            if pane_drag.is_some()
                && cursor.0 >= r.x && cursor.0 < r.x + r.width
                && cursor.1 >= r.y && cursor.1 < r.y + r.height
            {
                let (x, y, ww, hh) = (r.x as i32, r.y as i32, r.width as i32, r.height as i32);
                let t = 2 * scale as i32;
                fill_rect(&mut buf, pw, ph, x, y, ww, t, SEL_BG);
                fill_rect(&mut buf, pw, ph, x, y + hh - t, ww, t, SEL_BG);
                fill_rect(&mut buf, pw, ph, x, y, t, hh, SEL_BG);
                fill_rect(&mut buf, pw, ph, x + ww - t, y, t, hh, SEL_BG);
            }
            // Scrollback indicator at the pane's right edge — auto-hides at the
            // live bottom edge (only drawn while scrolled back).
            let rows = (r.height / ch as f64).floor() as usize;
            if let Some((thumb_y, thumb_h)) =
                breeze_ui::render::scroll_thumb(r.height as i32, rows, scroll_pos.1, scroll_pos.0)
            {
                let bar_w = 7 * scale as i32;
                let margin = 3 * scale as i32;
                let x = r.x as i32 + r.width as i32 - bar_w - margin;
                let (mx, my) = cursor;
                let in_band = mx >= x as f64
                    && mx <= (x + bar_w) as f64
                    && my >= r.y
                    && my < r.y + r.height;
                let dragging = self
                    .scrollbar_drag
                    .as_ref()
                    .is_some_and(|d| (d.track_top - r.y).abs() < 1.0);
                let col = if in_band || dragging { (0xc4, 0xd4, 0xe6) } else { FOCUS_RIM };
                breeze_ui::render::fill_capsule(&mut buf, pw, ph, x, r.y as i32 + thumb_y, bar_w, thumb_h, col);
            }
        }

        // Divider lines between panes (muted, ~1.2px), so splits read as
        // separate regions like the original.
        let dline = (scale as i32).max(1);
        for d in &dividers {
            let pos = d.position as i32;
            match d.direction {
                breeze_core::split_tree::SplitDirection::Vertical => {
                    fill_rect(&mut buf, pw, ph, pos - dline / 2, d.rect.y as i32, dline, d.rect.height as i32, DIVIDER);
                }
                breeze_core::split_tree::SplitDirection::Horizontal => {
                    fill_rect(&mut buf, pw, ph, d.rect.x as i32, pos - dline / 2, d.rect.width as i32, dline, DIVIDER);
                }
            }
        }

        // Pane-deletion highlight: red rims while the "Delete?" confirm is open;
        // a subtle (ice) rim for Shift+click-marked panes the rest of the time.
        let rim_color = if highlight_red { ERASE_RIM } else { FOCUS_RIM };
        for hr in &highlight_rects {
            let (x, y, ww, hh) = (hr.x as i32, hr.y as i32, hr.width as i32, hr.height as i32);
            let t = 2 * scale as i32;
            fill_rect(&mut buf, pw, ph, x, y, ww, t, rim_color);
            fill_rect(&mut buf, pw, ph, x, y + hh - t, ww, t, rim_color);
            fill_rect(&mut buf, pw, ph, x, y, t, hh, rim_color);
            fill_rect(&mut buf, pw, ph, x + ww - t, y, t, hh, rim_color);
        }

        // Frost tab bar + tab labels.
        fill_rect(&mut buf, pw, ph, 0, 0, pw as i32, bar_h, chrome::FROST_BASE);
        fill_rect(&mut buf, pw, ph, 0, bar_h - scale as i32, pw as i32, scale as i32, chrome::FROST_RIM);
        let si = scale as i32;
        // The right edge holds a FIXED control cluster: the new-tab "+", the pane
        // count, and the split icon. Tabs get everything to the left of it, so the
        // controls never shift as tabs are added/closed.
        let tabs_avail = (pw as i32 - 80 * si).max(60);
        let tab_w = (tabs_avail / tab_count.max(1) as i32).clamp(60 * si, 200 * si);
        // Per-tab close button: a small circle with a centered "×" at the tab's
        // RIGHT edge (layout shared with the click hit-test).
        let cb_r = 7 * si; // small circle radius
        let (glyph_w, _) = breeze_ui::render::cell_size(font_size);
        for i in 0..tab_count {
            let tx = i as i32 * tab_w;
            if i == active {
                fill_rect(&mut buf, pw, ph, tx + 2, 3, tab_w - 4, bar_h - 6, TAB_ACTIVE);
            } else if cursor.0 >= tx as f64 && cursor.0 < (tx + tab_w) as f64
                && cursor.1 >= 0.0 && cursor.1 < bar_h as f64
            {
                fill_rect(&mut buf, pw, ph, tx + 2, 3, tab_w - 4, bar_h - 6, TAB_HOVER);
            }
            // Label, centered in the tab but clamped clear of the close circle.
            let cb_cx = tx + tab_w - 6 * si - cb_r;
            let cb_cy = bar_h / 2;
            let area_l = tx + 8 * si;
            let area_r = cb_cx - cb_r;
            let text_w = (tab_labels[i].chars().count() as f32 * glyph_w) as i32;
            let centered = tx + (tab_w - text_w) / 2;
            let label_x = centered.clamp(area_l, (area_r - text_w).max(area_l));
            let label_w = (area_r - label_x).max(8);
            self.renderer.draw_text_at(
                &mut buf, pw, ph, label_x, cb_cy - (font_size * 0.65) as i32,
                label_w as f32, font_size * 1.3, &tab_labels[i], font_size, fg,
            );
            // Close affordance: the circle is always drawn around the "×";
            // hovering just brightens it.
            let (mx, my) = cursor;
            let hit = (cb_r + 3 * si) as f64;
            let (dxh, dyh) = (mx - cb_cx as f64, my - cb_cy as f64);
            let hovered = dxh * dxh + dyh * dyh <= hit * hit;
            let circle = if hovered { TAB_CLOSE_HOVER } else { TAB_CLOSE };
            breeze_ui::render::fill_circle(&mut buf, pw, ph, cb_cx, cb_cy, cb_r, circle);
            // The "×" itself brightens to white on hover so it clearly lights up.
            let xcol = if hovered { (0xff, 0xff, 0xff) } else { fg };
            self.renderer.draw_text_at(
                &mut buf, pw, ph,
                cb_cx - (glyph_w / 2.0) as i32, cb_cy - (font_size * 0.65) as i32,
                glyph_w * 2.0, font_size * 1.3, "×", font_size, xcol,
            );
        }
        // New-tab "+" in a circle chip matching the close button (hover-highlights).
        // Pinned to a fixed slot just left of the pane count + split icon.
        let (mx, my) = cursor;
        let plus_cx = pw as i32 - 61 * si;
        let plus_cy = bar_h / 2;
        let phit = (cb_r + 3 * si) as f64;
        let (pdx, pdy) = (mx - plus_cx as f64, my - plus_cy as f64);
        let plus_hover = pdx * pdx + pdy * pdy <= phit * phit;
        breeze_ui::render::fill_circle(
            &mut buf, pw, ph, plus_cx, plus_cy, cb_r,
            if plus_hover { TAB_CLOSE_HOVER } else { TAB_CLOSE },
        );
        self.renderer.draw_text_at(
            &mut buf, pw, ph,
            plus_cx - (glyph_w / 2.0) as i32, plus_cy - (font_size * 0.65) as i32,
            glyph_w * 2.0, font_size * 1.3, "+", font_size,
            if plus_hover { (0xff, 0xff, 0xff) } else { fg },
        );

        // Pane controls at the top-right: a split-pane icon, then the pane count
        // to its RIGHT (rightmost element).
        let pane_count = panes.len();
        let icon_sz = 16 * si;
        let icon_x = pw as i32 - 46 * si;
        let icon_y = (bar_h - icon_sz) / 2;
        // New-panel button highlights on hover (chip behind + brighter icon).
        let pane_hover = mx >= (icon_x - 4 * si) as f64
            && mx < (icon_x + icon_sz + 4 * si) as f64
            && my < bar_h as f64;
        if pane_hover {
            breeze_ui::render::fill_circle(&mut buf, pw, ph, icon_x + icon_sz / 2, bar_h / 2, icon_sz * 3 / 4, TAB_CLOSE_HOVER);
        }
        let pane_icon_col = if pane_hover { (0xff, 0xff, 0xff) } else { fg };
        breeze_ui::render::draw_pane_icon(&mut buf, pw, ph, icon_x, icon_y, icon_sz, pane_icon_col);
        self.renderer.draw_text_at(
            &mut buf, pw, ph, pw as i32 - 28 * si, 5, (20 * si) as f32, (bar_h - 8) as f32,
            &pane_count.to_string(), font_size, fg,
        );

        // Double-click context menu: a small frost panel with a single delete row.
        if let Some((mx, my, n)) = pane_menu_draw {
            let line_h = (font_size * 1.3).ceil() as i32;
            let mw = 200 * si;
            let mh = line_h + 8 * si;
            fill_rect(&mut buf, pw, ph, mx, my, mw, mh, chrome::FROST_BASE);
            breeze_ui::render::stroke_rect(&mut buf, pw, ph, mx, my, mw, mh, scale as i32, DIVIDER);
            let plural = if n == 1 { "pane" } else { "panes" };
            let label = format!("Delete {n} {plural}");
            self.renderer.draw_text_at(
                &mut buf, pw, ph, mx + 8 * si, my + 4 * si, (mw - 12 * si) as f32, line_h as f32,
                &label, font_size, ERASE_RIM,
            );
        }

        // High-memory banner for the focused pane's agent (top of content).
        if let Some(gb) = mem_alert {
            let line_h = (font_size * 1.3).ceil() as i32;
            let top = chrome::top_chrome_height(scale);
            fill_rect(&mut buf, pw, ph, 0, top, pw as i32, line_h + 6, (0x5a, 0x1e, 0x1e));
            let msg = format!("⚠ Agent using {gb:.1} GB — consider restarting (Cmd-W)");
            self.renderer.draw_text_at(&mut buf, pw, ph, 8, top + 3, (pw - 16) as f32, line_h as f32, &msg, font_size, fg);
        }

        if palette_open {
            let panel_w = (pw as i32 * 6 / 10).clamp(200, 760);
            let line_h = (font_size * 1.3).ceil() as i32;
            let rows = self.palette.visible().len().min(10) as i32;
            let panel_h = line_h * (rows + 1) + 16;
            let px0 = (pw as i32 - panel_w) / 2;
            let py0 = (ph as i32 / 5).max(bar_h + 8);
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, panel_h, chrome::FROST_BASE);
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, scale as i32, chrome::FROST_RIM);
            let pad = 8;
            let q = self.palette.query();
            let query_line = if q.is_empty() {
                "> Type a command…".to_string()
            } else {
                format!("> {}", q)
            };
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad, (panel_w - pad * 2) as f32, line_h as f32, &query_line, font_size, fg);
            for (i, item) in self.palette.visible().iter().take(10).enumerate() {
                let iy = py0 + pad + line_h * (i as i32 + 1);
                if i == self.palette.selected_index() {
                    fill_rect(&mut buf, pw, ph, px0 + 4, iy, panel_w - 8, line_h, chrome::FROST_RIM);
                }
                self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, iy, (panel_w - pad * 2) as f32, line_h as f32, item, font_size, fg);
            }
        }

        if self.confirm.is_open() {
            let line_h = (font_size * 1.3).ceil() as i32;
            let si = scale as i32;
            let panel_w = (pw as i32 * 5 / 10).clamp(260, 560);
            let panel_h = line_h * 2 + 24 + line_h + 8 * si; // message + hint + button row
            let px0 = (pw as i32 - panel_w) / 2;
            let py0 = (ph as i32 - panel_h) / 2;
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, panel_h, chrome::FROST_BASE);
            breeze_ui::render::stroke_rect(&mut buf, pw, ph, px0, py0, panel_w, panel_h, si, chrome::FROST_RIM);
            let pad = 10;
            // Question (names the tab) + a dim keyboard hint.
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad, (panel_w - pad * 2) as f32, line_h as f32, self.confirm.message(), font_size, fg);
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad + line_h, (panel_w - pad * 2) as f32, line_h as f32, "Enter / Esc", font_size, DIM);
            // Clickable buttons (geometry matches `confirm_button_rects`).
            let (gw, _) = breeze_ui::render::cell_size(font_size);
            let btn_w = 92 * si;
            let btn_h = line_h + 6 * si;
            let by = py0 + panel_h - pad - btn_h;
            let confirm_x = px0 + panel_w - pad - btn_w;
            let cancel_x = confirm_x - 8 * si - btn_w;
            let ty = by + (btn_h - line_h) / 2;
            let confirm_label = self.confirm.confirm_label().to_string();
            // Cancel button (neutral).
            fill_rect(&mut buf, pw, ph, cancel_x, by, btn_w, btn_h, TAB_CLOSE);
            let cw = ("Cancel".len() as f32 * gw) as i32;
            self.renderer.draw_text_at(&mut buf, pw, ph, cancel_x + (btn_w - cw) / 2, ty, cw as f32 + gw, line_h as f32, "Cancel", font_size, fg);
            // Confirm button (primary, ice accent with dark label).
            fill_rect(&mut buf, pw, ph, confirm_x, by, btn_w, btn_h, FOCUS_RIM);
            let kw = (confirm_label.chars().count() as f32 * gw) as i32;
            self.renderer.draw_text_at(&mut buf, pw, ph, confirm_x + (btn_w - kw) / 2, ty, kw as f32 + gw, line_h as f32, &confirm_label, font_size, bg);
        }

        // Orphan-scan overlay.
        if let Some(pids) = &orphans {
            use breeze_platform::permissions::AccessState;
            let line_h = (font_size * 1.3).ceil() as i32;
            let shown = pids.len().min(12);
            let panel_w = (pw as i32 * 6 / 10).clamp(300, 640);
            let denied = matches!(orphan_perm, Some(AccessState::Denied));
            let panel_h = line_h * (shown as i32 + if denied { 3 } else { 2 }) + 16;
            let px0 = (pw as i32 - panel_w) / 2;
            let py0 = (ph as i32 / 6).max(bar_h + 8);
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, panel_h, chrome::FROST_BASE);
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, scale as i32, chrome::FROST_RIM);
            let pad = 10;
            let mut y = py0 + pad;
            let header = format!("{} orphaned agent process(es)", pids.len());
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, y, (panel_w - pad * 2) as f32, line_h as f32, &header, font_size, fg);
            y += line_h;
            if denied {
                self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, y, (panel_w - pad * 2) as f32, line_h as f32, breeze_platform::permissions::guidance(), font_size, fg);
                y += line_h;
            }
            for pid in pids.iter().take(shown) {
                self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, y, (panel_w - pad * 2) as f32, line_h as f32, &format!("  pid {pid}"), font_size, fg);
                y += line_h;
            }
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, y, (panel_w - pad * 2) as f32, line_h as f32, "Esc: close    Enter: terminate all", font_size, fg);
        }

        // Inline name-input overlay.
        if let Some(text) = name_prompt {
            let line_h = (font_size * 1.3).ceil() as i32;
            let panel_w = (pw as i32 * 5 / 10).clamp(280, 480);
            let panel_h = line_h * 2 + 20;
            let px0 = (pw as i32 - panel_w) / 2;
            let py0 = (ph as i32 - panel_h) / 2;
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, panel_h, chrome::FROST_BASE);
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, scale as i32, chrome::FROST_RIM);
            let pad = 12;
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad, (panel_w - pad * 2) as f32, line_h as f32, "Save layout as:", font_size, fg);
            let entry = format!("{}\u{2588}", text);
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad + line_h, (panel_w - pad * 2) as f32, line_h as f32, &entry, font_size, fg);
        }


        // Inline pane-count editor.
        if let Some(text) = count_prompt {
            let line_h = (font_size * 1.3).ceil() as i32;
            let panel_w = (pw as i32 * 5 / 10).clamp(280, 480);
            let panel_h = line_h * 2 + 20;
            let px0 = (pw as i32 - panel_w) / 2;
            let py0 = (ph as i32 - panel_h) / 2;
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, panel_h, chrome::FROST_BASE);
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, scale as i32, chrome::FROST_RIM);
            let pad = 12;
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad, (panel_w - pad * 2) as f32, line_h as f32, "Set pane count:", font_size, fg);
            let entry = format!("{}\u{2588}", text);
            self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad + line_h, (panel_w - pad * 2) as f32, line_h as f32, &entry, font_size, fg);
        }

        // About overlay.
        if about_open {
            let line_h = (font_size * 1.3).ceil() as i32;
            let panel_w = (pw as i32 * 5 / 10).clamp(300, 520);
            let lines = [
                "Breeze",
                "The battery-efficient AI terminal.",
                "Trims wasted agent CPU; freezes idle tabs.",
                "",
                "Press any key to close.",
            ];
            let panel_h = line_h * lines.len() as i32 + 20;
            let px0 = (pw as i32 - panel_w) / 2;
            let py0 = (ph as i32 - panel_h) / 2;
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, panel_h, chrome::FROST_BASE);
            fill_rect(&mut buf, pw, ph, px0, py0, panel_w, scale as i32, chrome::FROST_RIM);
            let pad = 12;
            for (i, t) in lines.iter().enumerate() {
                self.renderer.draw_text_at(&mut buf, pw, ph, px0 + pad, py0 + pad + line_h * i as i32, (panel_w - pad * 2) as f32, line_h as f32, t, font_size, fg);
            }
        }

        let _ = buf.present();
    }
}

/// Top-level router: owns every open window and dispatches winit events to the
/// right one by `WindowId`. Closing the last window exits the app.
struct App {
    windows: Vec<WindowState>,
    proxy: EventLoopProxy<UserEvent>,
    started: bool,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> App {
        App { windows: Vec::new(), proxy, started: false }
    }

    /// Drop any window that asked to close; exit when none remain.
    fn reap_windows(&mut self, event_loop: &ActiveEventLoop) {
        self.windows.retain(|w| !w.wants_close);
        if self.started && self.windows.is_empty() {
            event_loop.exit();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        // First window restores the saved session.
        let mut first = WindowState::new(self.proxy.clone());
        first.create(event_loop, true);
        self.windows.push(first);
        // One global 500ms tick drives upkeep across all windows.
        let tick_proxy = self.proxy.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if tick_proxy.send_event(UserEvent::AttachTick).is_err() {
                break;
            }
        });
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Output => {
                for w in &self.windows {
                    w.request_redraw();
                }
            }
            UserEvent::AttachTick => {
                for w in &mut self.windows {
                    w.tick();
                }
                self.reap_windows(event_loop);
            }
            UserEvent::NewWindow => {
                let mut w = WindowState::new(self.proxy.clone());
                w.create(event_loop, false);
                self.windows.push(w);
            }
            UserEvent::UpdateAvailable { version, url } => {
                if let Some(w) = self.windows.first_mut() {
                    w.offer_update(version, url);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id() == Some(id)) {
            w.handle_window_event(event);
        }
        self.reap_windows(event_loop);
    }
}

fn main() {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    // One-shot, best-effort update check on launch. Off the UI thread; if a newer
    // release exists it posts back and the window offers a prompt.
    {
        let up = proxy.clone();
        std::thread::spawn(move || {
            if let Some(a) = breeze_platform::update::check() {
                let _ = up.send_event(UserEvent::UpdateAvailable { version: a.version, url: a.dmg_url });
            }
        });
    }
    let mut app = App::new(proxy);
    event_loop.run_app(&mut app).expect("run app");
}
