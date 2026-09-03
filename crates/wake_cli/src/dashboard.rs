//! Full-screen terminal dashboard for long-running Wake commands.

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Stderr, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::console::{CellPosition, ConsoleCommand, InputEditor, ScreenSelection, ScreenSnapshot};

const MAX_ACTIVITY: usize = 200;
const MAX_PROBLEMS: usize = 100;
const MAX_CHANGES: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Starting,
    Ready,
    Rebuilding,
    Error,
    Stopping,
    Stopped,
}

impl RunState {
    fn label(self) -> &'static str {
        match self {
            Self::Starting => "STARTING",
            Self::Ready => "READY",
            Self::Rebuilding => "REBUILDING",
            Self::Error => "ERROR",
            Self::Stopping => "STOPPING",
            Self::Stopped => "STOPPED",
        }
    }

    fn symbol(self, spinner: &str) -> &str {
        match self {
            Self::Starting | Self::Rebuilding => spinner,
            Self::Ready => "✓",
            Self::Error => "✗",
            Self::Stopping => "◌",
            Self::Stopped => "■",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BuildMetrics {
    pub modules: usize,
    pub updated_modules: usize,
    pub cached_modules: usize,
    pub chunks: usize,
    pub assets: usize,
    pub duration_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
struct Activity {
    elapsed: Duration,
    level: ActivityLevel,
    message: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DashboardView {
    #[default]
    Activity,
    Problems,
    Changes,
}

impl DashboardView {
    const ALL: [Self; 3] = [Self::Activity, Self::Problems, Self::Changes];

    fn index(self) -> usize {
        match self {
            Self::Activity => 0,
            Self::Problems => 1,
            Self::Changes => 2,
        }
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ViewScroll {
    from_bottom: usize,
    unread: usize,
}

#[derive(Clone, Debug)]
struct Problem {
    elapsed: Duration,
    diagnostic: wake_app::DiagnosticInfo,
    rendered: String,
}

#[derive(Clone, Debug)]
struct ChangeRecord {
    elapsed: Duration,
    changed_paths: Vec<String>,
    workspace: Option<String>,
    base_path: Option<String>,
    metrics: Option<BuildMetrics>,
}

#[derive(Clone, Debug)]
struct WorkspaceState {
    total: usize,
    loaded: usize,
    failed: usize,
    current: Option<String>,
    failed_names: Vec<String>,
}

#[derive(Debug)]
pub struct DashboardState {
    pub command: String,
    pub root: String,
    pub endpoint_label: String,
    pub endpoint: String,
    pub watch_label: String,
    pub state: RunState,
    pub metrics: Option<BuildMetrics>,
    pub rebuilds: usize,
    workspace_state: Option<WorkspaceState>,
    started: Instant,
    activity: VecDeque<Activity>,
    problems: VecDeque<Problem>,
    changes: VecDeque<ChangeRecord>,
    view: DashboardView,
    scroll: [ViewScroll; 3],
}

impl DashboardState {
    pub fn new(
        command: impl Into<String>,
        root: &Path,
        endpoint_label: impl Into<String>,
        watch_label: impl Into<String>,
    ) -> Self {
        let mut state = Self {
            command: command.into(),
            root: root.display().to_string(),
            endpoint_label: endpoint_label.into(),
            endpoint: String::new(),
            watch_label: watch_label.into(),
            state: RunState::Starting,
            metrics: None,
            rebuilds: 0,
            workspace_state: None,
            started: Instant::now(),
            activity: VecDeque::new(),
            problems: VecDeque::new(),
            changes: VecDeque::new(),
            view: DashboardView::Activity,
            scroll: [ViewScroll::default(); 3],
        };
        state.push(ActivityLevel::Info, "Starting Wake…");
        state
    }

    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    pub fn rebuilding(
        &mut self,
        changed_paths: Vec<String>,
        workspace: Option<String>,
        base_path: Option<String>,
    ) {
        self.state = RunState::Rebuilding;
        let changed = changed_paths.len();
        let message = if changed == 1 {
            "Rebuilding after 1 file change…".to_string()
        } else if changed > 1 {
            format!("Rebuilding after {changed} file changes…")
        } else {
            "Rebuilding…".to_string()
        };
        self.push(ActivityLevel::Warning, message);

        let before = self.change_row_count();
        if self.changes.len() == MAX_CHANGES {
            self.changes.pop_front();
        }
        self.changes.push_back(ChangeRecord {
            elapsed: self.runtime(),
            changed_paths,
            workspace,
            base_path,
            metrics: None,
        });
        self.note_view_update(DashboardView::Changes, before, self.change_row_count());
    }

    pub fn built(
        &mut self,
        metrics: BuildMetrics,
        initial: bool,
        workspace: Option<String>,
        base_path: Option<String>,
    ) {
        self.state = RunState::Ready;
        self.metrics = Some(metrics);
        if !initial {
            self.rebuilds += 1;
        }
        let message = if initial {
            format!(
                "Initial build completed: {} modules in {}",
                metrics.modules,
                human_duration(metrics.duration_ms)
            )
        } else {
            let updated = if metrics.updated_modules == 1 {
                "1 module".to_string()
            } else {
                format!("{} modules", metrics.updated_modules)
            };
            let cached = if metrics.cached_modules == 1 {
                "1 cache hit".to_string()
            } else {
                format!("{} cache hits", metrics.cached_modules)
            };
            format!(
                "Updated {updated} · {cached} in {}",
                human_duration(metrics.duration_ms)
            )
        };
        self.push(ActivityLevel::Success, message);

        if !initial {
            let before = self.change_row_count();
            if let Some(change) = self.changes.iter_mut().rev().find(|change| {
                change.metrics.is_none()
                    && change.workspace.as_ref() == workspace.as_ref()
                    && change.base_path.as_ref() == base_path.as_ref()
            }) {
                change.metrics = Some(metrics);
            } else {
                if self.changes.len() == MAX_CHANGES {
                    self.changes.pop_front();
                }
                self.changes.push_back(ChangeRecord {
                    elapsed: self.runtime(),
                    changed_paths: Vec::new(),
                    workspace,
                    base_path,
                    metrics: Some(metrics),
                });
            }
            self.note_view_update(DashboardView::Changes, before, self.change_row_count());
        }
    }

    pub fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.diagnostic(
            wake_app::DiagnosticInfo {
                severity: "error".to_string(),
                code: None,
                message: message.clone(),
                path: None,
                start: None,
                end: None,
                location: None,
                notes: Vec::new(),
            },
            message,
        );
    }

    pub fn diagnostic(
        &mut self,
        diagnostic: wake_app::DiagnosticInfo,
        rendered: impl Into<String>,
    ) {
        let rendered = rendered.into();
        let level = diagnostic_level(&diagnostic.severity);
        if level == ActivityLevel::Error {
            self.state = RunState::Error;
        }

        let before = self.problem_row_count();
        if self.problems.len() == MAX_PROBLEMS {
            self.problems.pop_front();
        }
        self.problems.push_back(Problem {
            elapsed: self.runtime(),
            diagnostic,
            rendered: rendered.clone(),
        });
        self.note_view_update(DashboardView::Problems, before, self.problem_row_count());
        self.push(level, rendered);
    }

    pub fn workspace_state(
        &mut self,
        total: usize,
        loaded: usize,
        failed: usize,
        current: Option<String>,
        failed_names: Vec<String>,
    ) {
        if let Some(workspace) = &current {
            self.info(format!("Loading workspace {workspace}…"));
        }
        self.workspace_state = Some(WorkspaceState {
            total,
            loaded,
            failed,
            current,
            failed_names,
        });
    }

    pub fn stopping(&mut self, reason: &str) {
        self.state = RunState::Stopping;
        self.push(ActivityLevel::Info, format!("Stopping ({reason})…"));
    }

    pub fn stopped(&mut self) {
        self.state = RunState::Stopped;
        self.push(ActivityLevel::Info, "Wake stopped");
    }

    pub fn runtime(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn clear_history(&mut self) {
        self.activity.clear();
        self.problems.clear();
        self.changes.clear();
        self.scroll = [ViewScroll::default(); 3];
    }

    fn info(&mut self, message: impl Into<String>) {
        self.push(ActivityLevel::Info, message);
    }

    fn warning(&mut self, message: impl Into<String>) {
        self.push(ActivityLevel::Warning, message);
    }

    fn push(&mut self, level: ActivityLevel, message: impl Into<String>) {
        let before = self.activity_row_count();
        if self.activity.len() == MAX_ACTIVITY {
            self.activity.pop_front();
        }
        self.activity.push_back(Activity {
            elapsed: self.started.elapsed(),
            level,
            message: message.into(),
        });
        self.note_view_update(DashboardView::Activity, before, self.activity_row_count());
    }

    fn scroll_up(&mut self, amount: usize) {
        let maximum = self.current_row_count().saturating_sub(1);
        let scroll = &mut self.scroll[self.view.index()];
        scroll.from_bottom = (scroll.from_bottom + amount).min(maximum);
    }

    fn scroll_down(&mut self, amount: usize) {
        let scroll = &mut self.scroll[self.view.index()];
        scroll.from_bottom = scroll.from_bottom.saturating_sub(amount);
        if scroll.from_bottom == 0 {
            scroll.unread = 0;
        }
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll[self.view.index()] = ViewScroll::default();
    }

    fn next_view(&mut self) {
        self.view = self.view.next();
    }

    fn previous_view(&mut self) {
        self.view = self.view.previous();
    }

    fn current_scroll(&self) -> ViewScroll {
        self.scroll[self.view.index()]
    }

    fn current_row_count(&self) -> usize {
        self.row_count(self.view)
    }

    fn row_count(&self, view: DashboardView) -> usize {
        match view {
            DashboardView::Activity => self.activity_row_count(),
            DashboardView::Problems => self.problem_row_count(),
            DashboardView::Changes => self.change_row_count(),
        }
    }

    fn activity_row_count(&self) -> usize {
        self.activity
            .iter()
            .map(|item| item.message.lines().count().max(1))
            .sum()
    }

    fn problem_row_count(&self) -> usize {
        self.problems
            .iter()
            .map(|problem| problem.rendered.lines().count().max(1))
            .sum()
    }

    fn change_row_count(&self) -> usize {
        self.changes
            .iter()
            .map(|change| change.changed_paths.len().max(1) + 2)
            .sum()
    }

    fn note_view_update(&mut self, view: DashboardView, before: usize, after: usize) {
        let scroll = &mut self.scroll[view.index()];
        if scroll.from_bottom == 0 {
            return;
        }
        if after >= before {
            scroll.from_bottom += after - before;
        } else {
            scroll.from_bottom = scroll.from_bottom.saturating_sub(before - after);
        }
        scroll.unread += 1;
    }
}

fn diagnostic_level(severity: &str) -> ActivityLevel {
    match severity.to_ascii_lowercase().as_str() {
        "error" => ActivityLevel::Error,
        "warning" => ActivityLevel::Warning,
        "note" | "help" => ActivityLevel::Info,
        _ => ActivityLevel::Info,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DashboardAction {
    Continue,
    Quit,
    Interrupt,
}

#[derive(Clone, Copy)]
enum ColorDepth {
    None,
    Indexed,
    TrueColor,
}

#[derive(Clone, Copy)]
struct Palette(ColorDepth);

impl Palette {
    fn detect(color: bool) -> Self {
        if !color {
            return Self(ColorDepth::None);
        }
        let true_color = std::env::var("COLORTERM").ok().is_some_and(|value| {
            value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
        });
        Self(if true_color {
            ColorDepth::TrueColor
        } else {
            ColorDepth::Indexed
        })
    }

    fn color(self, rgb: (u8, u8, u8), indexed: u8) -> Color {
        match self.0 {
            ColorDepth::None => Color::Reset,
            ColorDepth::Indexed => Color::Indexed(indexed),
            ColorDepth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
        }
    }

    fn brand(self) -> Style {
        Style::default()
            .fg(self.color((217, 70, 239), 213))
            .add_modifier(Modifier::BOLD)
    }

    fn accent(self) -> Style {
        Style::default().fg(self.color((34, 211, 238), 81))
    }

    fn warning(self) -> Style {
        Style::default().fg(self.color((251, 191, 36), 214))
    }

    fn success(self) -> Style {
        Style::default().fg(self.color((74, 222, 128), 114))
    }

    fn error(self) -> Style {
        Style::default().fg(self.color((251, 113, 133), 204))
    }

    fn muted(self) -> Style {
        Style::default().fg(self.color((148, 163, 184), 245))
    }

    fn status(self, state: RunState) -> Style {
        match state {
            RunState::Ready => self.success(),
            RunState::Error => self.error(),
            RunState::Starting | RunState::Rebuilding => self.warning(),
            RunState::Stopping | RunState::Stopped => self.muted(),
        }
        .add_modifier(Modifier::BOLD)
    }

    fn activity(self, level: ActivityLevel) -> Style {
        match level {
            ActivityLevel::Info => self.muted(),
            ActivityLevel::Success => self.success(),
            ActivityLevel::Warning => self.warning(),
            ActivityLevel::Error => self.error(),
        }
    }
}

pub struct Dashboard {
    terminal: Terminal<CrosstermBackend<Stderr>>,
    palette: Palette,
    editor: InputEditor,
    snapshot: ScreenSnapshot,
    selection: Option<ScreenSelection>,
    drag_start: Option<CellPosition>,
    last_selection: Option<String>,
    clipboard: Option<arboard::Clipboard>,
    notice: Option<(String, Instant, bool)>,
    restored: bool,
}

impl Dashboard {
    pub fn supported() -> bool {
        std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal()
            && std::env::var("TERM")
                .map(|term| !term.eq_ignore_ascii_case("dumb"))
                .unwrap_or(true)
    }

    pub fn new(color: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stderr = std::io::stderr();
        if let Err(error) = execute!(
            stderr,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(stderr);
        match Terminal::new(backend) {
            Ok(mut terminal) => {
                if let Err(error) = terminal.clear() {
                    let _ = disable_raw_mode();
                    let _ = execute!(
                        terminal.backend_mut(),
                        DisableBracketedPaste,
                        DisableMouseCapture,
                        Show,
                        LeaveAlternateScreen
                    );
                    return Err(error);
                }
                Ok(Self {
                    terminal,
                    palette: Palette::detect(color),
                    editor: InputEditor::default(),
                    snapshot: ScreenSnapshot::default(),
                    selection: None,
                    drag_start: None,
                    last_selection: None,
                    clipboard: arboard::Clipboard::new().ok(),
                    notice: None,
                    restored: false,
                })
            }
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stderr = std::io::stderr();
                let _ = execute!(
                    stderr,
                    DisableBracketedPaste,
                    DisableMouseCapture,
                    Show,
                    LeaveAlternateScreen
                );
                Err(error)
            }
        }
    }

    pub fn draw(&mut self, state: &DashboardState) -> io::Result<()> {
        let palette = self.palette;
        let input = self.editor.value().to_string();
        let cursor_cell = self.editor.cursor_cell();
        let selection = self.selection;
        let notice = self
            .active_notice()
            .map(|(text, error)| (text.to_string(), error));
        let completed = self.terminal.draw(|frame| {
            render_interactive(
                frame,
                state,
                palette,
                &input,
                cursor_cell,
                selection,
                notice.as_ref().map(|(text, error)| (text.as_str(), *error)),
            )
        })?;
        let buffer = completed.buffer.clone();
        let area = completed.area;
        let input_area = command_line_area(area);
        self.snapshot = ScreenSnapshot {
            width: area.width,
            height: area.height,
            rows: (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect()
                })
                .collect(),
            input_y: input_area.y,
            input_x: input_area.x.saturating_add(2),
        };
        Ok(())
    }

    pub fn read_action(
        &mut self,
        state: &mut DashboardState,
        timeout: Duration,
    ) -> io::Result<DashboardAction> {
        if !event::poll(timeout)? {
            return Ok(DashboardAction::Continue);
        }
        let input = event::read()?;
        if let Event::Paste(value) = input {
            self.editor.insert_paste(&value);
            self.selection = None;
            return Ok(DashboardAction::Continue);
        }
        if let Event::Mouse(mouse) = input {
            return self.handle_mouse(
                state,
                mouse.kind,
                CellPosition {
                    x: mouse.column,
                    y: mouse.row,
                },
            );
        }
        let Event::Key(key) = input else {
            return Ok(DashboardAction::Continue);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(DashboardAction::Continue);
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                Ok(DashboardAction::Interrupt)
            }
            (KeyCode::Char('y'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(value) = self.last_selection.clone() {
                    self.copy_text(&value);
                } else {
                    self.set_notice("No selected text to copy", true);
                }
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Char('v'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                self.paste_from_clipboard();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Enter, _) => self.submit_command(state),
            (KeyCode::Tab, _) => {
                state.next_view();
                self.selection = None;
                Ok(DashboardAction::Continue)
            }
            (KeyCode::BackTab, _) => {
                state.previous_view();
                self.selection = None;
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Esc, _) => {
                self.editor.clear();
                self.selection = None;
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Left, _) => {
                self.editor.move_left();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Right, _) => {
                self.editor.move_right();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Home, _) => {
                self.editor.move_home();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::End, modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                state.scroll_to_bottom();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::End, _) => {
                self.editor.move_end();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Backspace, _) => {
                self.editor.backspace();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Delete, _) => {
                self.editor.delete();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Up, _) => {
                self.editor.history_previous();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Down, _) => {
                self.editor.history_next();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::PageUp, _) => {
                state.scroll_up(10);
                Ok(DashboardAction::Continue)
            }
            (KeyCode::PageDown, _) => {
                state.scroll_down(10);
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Char(value), modifiers)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.editor.insert_char(value);
                self.selection = None;
                Ok(DashboardAction::Continue)
            }
            _ => Ok(DashboardAction::Continue),
        }
    }

    fn submit_command(&mut self, state: &mut DashboardState) -> io::Result<DashboardAction> {
        match self.editor.submit() {
            Ok(ConsoleCommand::Help) => {
                state.info("Commands: help · clear · open · quit (a leading / is optional)");
                Ok(DashboardAction::Continue)
            }
            Ok(ConsoleCommand::Clear) => {
                state.clear_history();
                self.set_notice("Dashboard history cleared", false);
                Ok(DashboardAction::Continue)
            }
            Ok(ConsoleCommand::Open) => {
                let endpoint = state.endpoint.trim();
                if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
                    state.warning("No development-server URL is available to open");
                } else {
                    match open::that(endpoint) {
                        Ok(()) => self.set_notice("Opened development server", false),
                        Err(error) => state.warning(format!("Failed to open {endpoint}: {error}")),
                    }
                }
                Ok(DashboardAction::Continue)
            }
            Ok(ConsoleCommand::Quit) => Ok(DashboardAction::Quit),
            Err(message) => {
                state.warning(message);
                Ok(DashboardAction::Continue)
            }
        }
    }

    fn handle_mouse(
        &mut self,
        state: &mut DashboardState,
        kind: MouseEventKind,
        position: CellPosition,
    ) -> io::Result<DashboardAction> {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.drag_start = Some(position);
                self.selection = Some(ScreenSelection {
                    start: position,
                    end: position,
                });
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(start) = self.drag_start {
                    self.selection = Some(ScreenSelection {
                        start,
                        end: position,
                    });
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(start) = self.drag_start.take() {
                    let selection = ScreenSelection {
                        start,
                        end: position,
                    };
                    let value = self.snapshot.extract(selection);
                    if value.is_empty() {
                        if position.y == self.snapshot.input_y {
                            self.editor.set_cursor_from_cell(
                                position.x.saturating_sub(self.snapshot.input_x) as usize,
                            );
                        }
                        self.selection = None;
                    } else {
                        self.selection = Some(selection);
                        self.last_selection = Some(value.clone());
                        self.copy_text(&value);
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) if position.y == self.snapshot.input_y => {
                self.paste_from_clipboard();
            }
            MouseEventKind::ScrollUp => state.scroll_up(3),
            MouseEventKind::ScrollDown => state.scroll_down(3),
            _ => {}
        }
        Ok(DashboardAction::Continue)
    }

    fn paste_from_clipboard(&mut self) {
        match self
            .clipboard
            .as_mut()
            .and_then(|clipboard| clipboard.get_text().ok())
        {
            Some(value) => {
                self.editor.insert_paste(&value);
                self.selection = None;
            }
            None => self.set_notice("Clipboard paste is unavailable; use terminal paste", true),
        }
    }

    fn copy_text(&mut self, value: &str) {
        let remote =
            std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        let copied = if remote {
            self.write_osc52(value).is_ok()
        } else {
            self.clipboard
                .as_mut()
                .is_some_and(|clipboard| clipboard.set_text(value.to_string()).is_ok())
                || self.write_osc52(value).is_ok()
        };
        if copied {
            self.set_notice("Copied to clipboard", false);
        } else {
            self.set_notice("Failed to copy; terminal clipboard is unavailable", true);
        }
    }

    fn write_osc52(&mut self, value: &str) -> io::Result<()> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(value);
        let sequence = format!("\x1b]52;c;{encoded}\x07");
        if std::env::var_os("TMUX").is_some() {
            write!(
                self.terminal.backend_mut(),
                "\x1bPtmux;{}\x1b\\",
                sequence.replace('\x1b', "\x1b\x1b")
            )?;
        } else {
            write!(self.terminal.backend_mut(), "{sequence}")?;
        }
        self.terminal.backend_mut().flush()
    }

    fn set_notice(&mut self, message: impl Into<String>, error: bool) {
        self.notice = Some((message.into(), Instant::now(), error));
    }

    fn active_notice(&self) -> Option<(&str, bool)> {
        self.notice
            .as_ref()
            .filter(|(_, started, _)| started.elapsed() < Duration::from_millis(1500))
            .map(|(message, _, error)| (message.as_str(), *error))
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            DisableMouseCapture,
            Show,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
        self.restored = true;
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(test)]
fn render(frame: &mut Frame<'_>, state: &DashboardState, palette: Palette) {
    render_interactive(frame, state, palette, "", 0, None, None);
}

fn render_interactive(
    frame: &mut Frame<'_>,
    state: &DashboardState,
    palette: Palette,
    input: &str,
    cursor_cell: usize,
    selection: Option<ScreenSelection>,
    notice: Option<(&str, bool)>,
) {
    let area = frame.area();
    if area.width < 60 || area.height < 14 {
        render_minimal(frame, area, state, palette);
    } else if area.width < 80 || area.height < 20 {
        render_compact(frame, area, state, palette);
    } else {
        render_full(frame, area, state, palette);
    }
    render_command_line(frame, area, input, cursor_cell, palette, notice);
    if let Some(selection) = selection {
        let buffer = frame.buffer_mut();
        for y in 0..area.height {
            for x in 0..area.width {
                if selection.contains(x, y) {
                    buffer[(x, y)].set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }
}

fn command_line_area(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(2),
        y: area.bottom().saturating_sub(2),
        width: area.width.saturating_sub(4),
        height: u16::from(area.height > 2),
    }
}

fn render_command_line(
    frame: &mut Frame<'_>,
    area: Rect,
    input: &str,
    cursor_cell: usize,
    palette: Palette,
    notice: Option<(&str, bool)>,
) {
    let input_area = command_line_area(area);
    if input_area.height == 0 || input_area.width < 3 {
        return;
    }
    let hint_area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(1),
        width: input_area.width,
        height: 1,
    };
    let hint = notice.map_or(
        "Tab views · PgUp/PgDn scroll · Enter command · drag copy · Ctrl-C quit",
        |(message, _)| message,
    );
    let hint_style = if notice.is_some_and(|(_, error)| error) {
        palette.error()
    } else {
        palette.muted()
    };
    frame.render_widget(Paragraph::new(hint).style(hint_style), hint_area);

    let available = input_area.width.saturating_sub(2) as usize;
    let (visible, visible_cursor) = visible_input(input, cursor_cell, available);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", palette.accent().add_modifier(Modifier::BOLD)),
            Span::raw(visible),
        ])),
        input_area,
    );
    frame.set_cursor_position(Position {
        x: input_area
            .x
            .saturating_add(2)
            .saturating_add(visible_cursor.min(available) as u16),
        y: input_area.y,
    });
}

fn visible_input(input: &str, cursor_cell: usize, width: usize) -> (String, usize) {
    if width == 0 {
        return (String::new(), 0);
    }
    let total_width = UnicodeWidthStr::width(input);
    let start_cell = if cursor_cell >= width {
        cursor_cell + 1 - width
    } else {
        0
    };
    if total_width <= width && start_cell == 0 {
        return (input.to_string(), cursor_cell);
    }
    let mut position = 0;
    let mut visible = String::new();
    for grapheme in input.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        let end = position + grapheme_width;
        if end > start_cell && position < start_cell + width {
            visible.push_str(grapheme);
        }
        position = end;
        if position >= start_cell + width {
            break;
        }
    }
    (visible, cursor_cell.saturating_sub(start_cell).min(width))
}

fn chrome<'a>(state: &DashboardState, palette: Palette) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(palette.brand())
        .title(Line::from(vec![
            Span::styled(" ⚡ WAKE ", palette.brand()),
            Span::styled(
                format!(
                    "/ {}  v{} ",
                    state.command.to_uppercase(),
                    wake_app::VERSION
                ),
                palette.muted(),
            ),
        ]))
}

fn header_line(state: &DashboardState, palette: Palette) -> Line<'static> {
    let spinner = spinner(state.runtime());
    Line::from(vec![
        Span::styled(
            format!("{} {}", state.state.symbol(spinner), state.state.label()),
            palette.status(state.state),
        ),
        Span::raw("   "),
        Span::styled(
            format!("uptime {}", human_runtime(state.runtime())),
            palette.muted(),
        ),
        Span::raw("   "),
        Span::styled(format!("{} rebuilds", state.rebuilds), palette.accent()),
    ])
}

fn render_full(frame: &mut Frame<'_>, area: Rect, state: &DashboardState, palette: Palette) {
    let block = chrome(state, palette);
    let inner = block.inner(area).inner(Margin::new(1, 0));
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(2),
        Constraint::Min(2),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    frame.render_widget(Paragraph::new(header_line(state, palette)), rows[0]);
    let endpoint = if state.endpoint.is_empty() {
        "waiting…"
    } else {
        &state.endpoint
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!("{:<7}", state.endpoint_label), palette.muted()),
                Span::styled(endpoint.to_string(), palette.accent()),
            ]),
            Line::from(vec![
                Span::styled("ROOT   ", palette.muted()),
                Span::raw(state.root.clone()),
            ]),
            Line::from(vec![
                Span::styled("MODE   ", palette.muted()),
                Span::raw(state.watch_label.clone()),
            ]),
        ])
        .wrap(Wrap { trim: true }),
        rows[1],
    );
    frame.render_widget(metrics_line(state, palette), rows[2]);
    render_view(frame, rows[3], state, palette);
}

fn render_compact(frame: &mut Frame<'_>, area: Rect, state: &DashboardState, palette: Palette) {
    let block = chrome(state, palette);
    let inner = block.inner(area).inner(Margin::new(1, 0));
    frame.render_widget(block, area);
    let summary_height = if state.workspace_state.is_some() {
        3
    } else {
        2
    };
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(summary_height),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(Paragraph::new(header_line(state, palette)), rows[0]);
    let mut summary = vec![
        Line::from(vec![
            Span::styled(format!("{} ", state.endpoint_label), palette.muted()),
            Span::styled(
                if state.endpoint.is_empty() {
                    "waiting…".to_string()
                } else {
                    state.endpoint.clone()
                },
                palette.accent(),
            ),
        ]),
        metrics_spans(state, palette),
    ];
    if let Some(workspaces) = &state.workspace_state {
        summary.push(workspace_spans(workspaces, palette));
    }
    frame.render_widget(Paragraph::new(summary).wrap(Wrap { trim: true }), rows[1]);
    render_view(frame, rows[2], state, palette);
}

fn render_minimal(frame: &mut Frame<'_>, area: Rect, state: &DashboardState, palette: Palette) {
    let block = chrome(state, palette);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let last = state
        .problems
        .back()
        .map(|problem| problem.rendered.as_str())
        .or_else(|| state.activity.back().map(|item| item.message.as_str()))
        .unwrap_or("Starting Wake…");
    frame.render_widget(
        Paragraph::new(vec![
            header_line(state, palette),
            Line::raw(if state.endpoint.is_empty() {
                state.watch_label.clone()
            } else {
                state.endpoint.clone()
            }),
            Line::styled(last.to_string(), palette.muted()),
            Line::styled(
                "Resize for views · type help for commands",
                palette.accent(),
            ),
        ])
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn metrics_line(state: &DashboardState, palette: Palette) -> Paragraph<'static> {
    let mut lines = vec![metrics_spans(state, palette)];
    if let Some(workspaces) = &state.workspace_state {
        lines.push(workspace_spans(workspaces, palette));
    }
    Paragraph::new(lines)
}

fn workspace_spans(workspaces: &WorkspaceState, palette: Palette) -> Line<'static> {
    let mut spans = vec![
        Span::styled("WORKSPACES   ", palette.muted()),
        Span::styled(
            format!("{}/{} loaded", workspaces.loaded, workspaces.total),
            palette.accent(),
        ),
    ];
    if workspaces.failed > 0 {
        spans.push(Span::styled(" · ", palette.muted()));
        let names = if workspaces.failed_names.is_empty() {
            String::new()
        } else {
            format!(": {}", workspaces.failed_names.join(", "))
        };
        spans.push(Span::styled(
            format!("{} failed{names}", workspaces.failed),
            palette.error(),
        ));
    }
    if let Some(current) = &workspaces.current {
        spans.push(Span::styled(" · loading ", palette.muted()));
        spans.push(Span::raw(current.clone()));
    }
    Line::from(spans)
}

fn metrics_spans(state: &DashboardState, palette: Palette) -> Line<'static> {
    let Some(metrics) = state.metrics else {
        return Line::from(Span::styled(
            "BUILD   waiting for metrics…",
            palette.muted(),
        ));
    };
    Line::from(vec![
        Span::styled("BUILD   ", palette.muted()),
        Span::styled(format!("{} modules", metrics.modules), palette.accent()),
        Span::styled(" · ", palette.muted()),
        Span::raw(format!("{} chunks", metrics.chunks)),
        Span::styled(" · ", palette.muted()),
        Span::raw(format!("{} assets", metrics.assets)),
        Span::styled(" · ", palette.muted()),
        Span::styled(human_duration(metrics.duration_ms), palette.success()),
    ])
}

fn render_view(frame: &mut Frame<'_>, area: Rect, state: &DashboardState, palette: Palette) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(palette.muted())
        .title(view_title(state, palette));
    let height = block.inner(area).height as usize;
    let mut rows = view_lines(state, palette);
    if rows.is_empty() {
        rows = empty_view_lines(state.view, palette);
    }
    let end = rows
        .len()
        .saturating_sub(state.current_scroll().from_bottom);
    let start = end.saturating_sub(height.max(1));
    let items = rows
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .cloned()
        .map(ListItem::new)
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items).block(block), area);
}

fn view_title(state: &DashboardState, palette: Palette) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, view) in DashboardView::ALL.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", palette.muted()));
        }
        let total = match view {
            DashboardView::Activity => None,
            DashboardView::Problems => Some(state.problems.len()),
            DashboardView::Changes => Some(state.changes.len()),
        };
        let unread = state.scroll[view.index()].unread;
        let label = match view {
            DashboardView::Activity => "ACTIVITY",
            DashboardView::Problems => "RECENT PROBLEMS",
            DashboardView::Changes => "CHANGES",
        };
        let total = total.map_or_else(String::new, |value| format!(" {value}"));
        let unread = if unread > 0 {
            format!(" +{unread}")
        } else {
            String::new()
        };
        let text = if state.view == view {
            format!(" [{label}{total}{unread}] ")
        } else {
            format!(" {label}{total}{unread} ")
        };
        spans.push(Span::styled(
            text,
            if state.view == view {
                palette.brand()
            } else {
                palette.muted()
            },
        ));
    }
    Line::from(spans)
}

fn view_lines(state: &DashboardState, palette: Palette) -> Vec<Line<'static>> {
    match state.view {
        DashboardView::Activity => state
            .activity
            .iter()
            .flat_map(|item| event_lines(item.elapsed, item.level, &item.message, palette))
            .collect(),
        DashboardView::Problems => state
            .problems
            .iter()
            .flat_map(|problem| {
                event_lines(
                    problem.elapsed,
                    diagnostic_level(&problem.diagnostic.severity),
                    &problem.rendered,
                    palette,
                )
            })
            .collect(),
        DashboardView::Changes => state
            .changes
            .iter()
            .flat_map(|change| change_lines(change, palette))
            .collect(),
    }
}

fn event_lines(
    elapsed: Duration,
    level: ActivityLevel,
    message: &str,
    palette: Palette,
) -> Vec<Line<'static>> {
    let symbol = match level {
        ActivityLevel::Info => "·",
        ActivityLevel::Success => "✓",
        ActivityLevel::Warning => "⚠",
        ActivityLevel::Error => "✗",
    };
    message
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let prefix = if index == 0 {
                vec![
                    Span::styled(format!("{}  ", elapsed_stamp(elapsed)), palette.muted()),
                    Span::styled(format!("{symbol} "), palette.activity(level)),
                ]
            } else {
                vec![Span::raw("          ")]
            };
            Line::from(
                prefix
                    .into_iter()
                    .chain(std::iter::once(Span::raw(line.to_string())))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn change_lines(change: &ChangeRecord, palette: Palette) -> Vec<Line<'static>> {
    let target = match (&change.workspace, &change.base_path) {
        (Some(workspace), Some(base_path)) => format!("{workspace} · {base_path}"),
        (Some(workspace), None) => workspace.clone(),
        (None, Some(base_path)) => base_path.clone(),
        (None, None) => "project".to_string(),
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("{}  ", elapsed_stamp(change.elapsed)),
            palette.muted(),
        ),
        Span::styled("↻ ", palette.warning()),
        Span::raw(target),
    ])];
    if change.changed_paths.is_empty() {
        lines.push(Line::styled(
            "          Changed files unavailable",
            palette.muted(),
        ));
    } else {
        lines.extend(change.changed_paths.iter().map(|path| {
            Line::from(vec![
                Span::raw("          "),
                Span::styled(path.clone(), palette.accent()),
            ])
        }));
    }
    lines.push(if let Some(metrics) = change.metrics {
        Line::from(vec![
            Span::raw("          "),
            Span::styled("✓ ", palette.success()),
            Span::raw(format!(
                "{} updated · {} cache hits · {}",
                metrics.updated_modules,
                metrics.cached_modules,
                human_duration(metrics.duration_ms)
            )),
        ])
    } else {
        Line::from(vec![
            Span::raw("          "),
            Span::styled("… rebuilding…", palette.warning()),
        ])
    });
    lines
}

fn empty_view_lines(view: DashboardView, palette: Palette) -> Vec<Line<'static>> {
    let (title, detail) = match view {
        DashboardView::Activity => ("No activity yet", "Wake events will appear here."),
        DashboardView::Problems => (
            "No recent problems",
            "Warnings and errors will appear here with source context.",
        ),
        DashboardView::Changes => ("No rebuilds yet", "Edit a file to trigger a rebuild."),
    };
    vec![
        Line::styled(title.to_string(), palette.accent()),
        Line::styled(detail.to_string(), palette.muted()),
    ]
}

pub fn human_duration(duration_ms: f64) -> String {
    let milliseconds = duration_ms.max(1.0);
    if milliseconds < 1_000.0 {
        format!("{milliseconds:.0}ms")
    } else if milliseconds < 60_000.0 {
        format!("{:.2}s", milliseconds / 1_000.0)
    } else {
        let seconds = milliseconds / 1_000.0;
        format!("{}m{:.1}s", (seconds / 60.0) as u64, seconds % 60.0)
    }
}

pub fn human_runtime(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h{:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

fn elapsed_stamp(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("+{:02}:{:02}", (seconds / 60) % 100, seconds % 60)
}

fn spinner(duration: Duration) -> &'static str {
    const FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    FRAMES[(duration.as_millis() as usize / 100) % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn render_text(width: u16, height: u16, state: &DashboardState) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render(frame, state, Palette(ColorDepth::None)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<Vec<_>>()
                    .concat()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn diagnostic(severity: &str, message: &str) -> wake_app::DiagnosticInfo {
        wake_app::DiagnosticInfo {
            severity: severity.to_string(),
            code: None,
            message: message.to_string(),
            path: None,
            start: None,
            end: None,
            location: None,
            notes: Vec::new(),
        }
    }

    fn contract_diagnostic(value: &serde_json::Value) -> wake_app::DiagnosticInfo {
        let location = value
            .get("location")
            .map(|location| wake_app::DiagnosticLocation {
                line: location["line"].as_u64().unwrap() as u32,
                column: location["column"].as_u64().unwrap() as u32,
                end_line: location["endLine"].as_u64().unwrap() as u32,
                end_column: location["endColumn"].as_u64().unwrap() as u32,
                line_text: location["lineText"].as_str().unwrap().to_string(),
                label: location
                    .get("label")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            });
        wake_app::DiagnosticInfo {
            severity: value["severity"].as_str().unwrap().to_string(),
            code: value
                .get("code")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            message: value["message"].as_str().unwrap().to_string(),
            path: value
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            start: None,
            end: None,
            location,
            notes: value
                .get("notes")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect(),
        }
    }

    fn contract_diagnostic_text(diagnostic: &wake_app::DiagnosticInfo) -> String {
        let code = diagnostic
            .code
            .as_ref()
            .map_or_else(String::new, |code| format!(" [{code}]"));
        let mut text = format!(
            "{}{code}: {}",
            diagnostic.severity.to_ascii_uppercase(),
            diagnostic.message
        );
        if let (Some(path), Some(location)) = (&diagnostic.path, &diagnostic.location) {
            text.push_str(&format!(
                "\n --> {path}:{}:{}\n{} | {}",
                location.line, location.column, location.line, location.line_text
            ));
        }
        for note in &diagnostic.notes {
            text.push_str(&format!("\n  = note: {note}"));
        }
        text
    }

    #[test]
    fn full_dashboard_contains_brand_metrics_and_controls() {
        let mut state =
            DashboardState::new("dev", Path::new("demo"), "LOCAL", "Live reload · watching");
        state.set_endpoint("http://127.0.0.1:5173/");
        state.built(
            BuildMetrics {
                modules: 128,
                updated_modules: 128,
                cached_modules: 0,
                chunks: 3,
                assets: 4,
                duration_ms: 42.0,
            },
            true,
            None,
            None,
        );
        let text = render_text(90, 24, &state);
        assert!(text.contains("WAKE"), "{text}");
        assert!(text.contains("128 modules"), "{text}");
        assert!(text.contains("ACTIVITY"), "{text}");
        assert!(text.contains("drag copy"), "{text}");
        assert!(text.contains("›"), "{text}");

        state.built(
            BuildMetrics {
                modules: 128,
                updated_modules: 1,
                cached_modules: 127,
                chunks: 3,
                assets: 4,
                duration_ms: 13.0,
            },
            false,
            None,
            None,
        );
        let activity = &state.activity.back().unwrap().message;
        assert!(activity.contains("Updated 1 module"), "{activity}");
        assert!(activity.contains("127 cache hits"), "{activity}");
    }

    #[test]
    fn compact_and_minimal_layouts_keep_status_visible() {
        let mut state = DashboardState::new("build --watch", Path::new("demo"), "WATCH", "src");
        let compact = render_text(70, 18, &state);
        let minimal = render_text(50, 10, &state);
        assert!(compact.contains("STARTING"), "{compact}");
        assert!(minimal.contains("Resize for views"), "{minimal}");

        state.view = DashboardView::Problems;
        let empty_problems = render_text(90, 24, &state);
        assert!(
            empty_problems.contains("No recent problems"),
            "{empty_problems}"
        );
        state.view = DashboardView::Changes;
        let empty_changes = render_text(90, 24, &state);
        assert!(
            empty_changes.contains("Edit a file to trigger a rebuild"),
            "{empty_changes}"
        );
    }

    #[test]
    fn activity_history_is_bounded() {
        let mut state = DashboardState::new("dev", Path::new("demo"), "LOCAL", "watching");
        for index in 0..250 {
            state.error(format!("error {index}"));
        }
        assert_eq!(state.activity.len(), MAX_ACTIVITY);
        assert!(state.activity.front().unwrap().message.contains("error 50"));
    }

    #[test]
    fn multiline_diagnostics_expand_into_scrollable_activity_rows() {
        let mut state = DashboardState::new("dev", Path::new("demo"), "LOCAL", "watching");
        state.error(
            "ERROR [WAKE_PARSE]: Unexpected token\n --> src/App.tsx:12:8\n   |\n12 | const value = ;\n   |               ^",
        );
        assert_eq!(state.activity_row_count(), 6);
        state.scroll_up(3);
        assert_eq!(state.current_scroll().from_bottom, 3);
        state.scroll_down(3);
        let text = render_text(90, 24, &state);
        assert!(text.contains("src/App.tsx:12:8"), "{text}");
        assert!(text.contains("12 | const value = ;"), "{text}");
    }

    #[test]
    fn shared_dashboard_contract_preserves_problems_changes_and_workspaces() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/terminal-dashboard-contract.json"
        ))
        .unwrap();
        let mut state = DashboardState::new(
            "docs dev",
            Path::new("demo"),
            "LOCAL",
            "MDX · Live reload · watching",
        );
        for event in contract["events"].as_array().unwrap() {
            match event["type"].as_str().unwrap() {
                "rebuildStart" => state.rebuilding(
                    event["changedPaths"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|path| path.as_str().unwrap().to_string())
                        .collect(),
                    event
                        .get("workspace")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    event
                        .get("basePath")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                ),
                "rebuilt" => state.built(
                    BuildMetrics {
                        modules: event["modules"].as_u64().unwrap() as usize,
                        updated_modules: event["updatedModules"].as_u64().unwrap() as usize,
                        cached_modules: event["cachedModules"].as_u64().unwrap() as usize,
                        chunks: event["chunks"].as_u64().unwrap() as usize,
                        assets: event["assets"].as_u64().unwrap() as usize,
                        duration_ms: event["durationMs"].as_f64().unwrap(),
                    },
                    event["initial"].as_bool().unwrap(),
                    event
                        .get("workspace")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    event
                        .get("basePath")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                ),
                "diagnostic" => {
                    let diagnostic = contract_diagnostic(&event["diagnostic"]);
                    let rendered = contract_diagnostic_text(&diagnostic);
                    state.diagnostic(diagnostic, rendered);
                }
                "workspaceState" => state.workspace_state(
                    event["total"].as_u64().unwrap() as usize,
                    event["loaded"].as_u64().unwrap() as usize,
                    event["failed"].as_u64().unwrap() as usize,
                    event
                        .get("current")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    event["failedNames"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|name| name.as_str().unwrap().to_string())
                        .collect(),
                ),
                other => panic!("unsupported dashboard contract event: {other}"),
            }
        }

        let expected = &contract["expected"];
        assert_eq!(state.state.label().to_ascii_lowercase(), expected["status"]);
        assert_eq!(state.activity.len(), expected["activityCount"]);
        assert_eq!(state.problems.len(), expected["problemCount"]);
        assert_eq!(
            state
                .problems
                .iter()
                .map(|problem| problem.diagnostic.severity.as_str())
                .collect::<Vec<_>>(),
            vec!["warning", "note", "error"]
        );
        assert_eq!(state.changes.len(), expected["changeCount"]);
        assert_eq!(
            state.changes[0].changed_paths.len(),
            expected["changePathCount"]
        );
        assert_eq!(state.changes[0].metrics.unwrap().duration_ms, 18.0);
        assert_eq!(
            state.workspace_state.as_ref().unwrap().failed_names,
            vec!["rc-beta"]
        );

        state.view = DashboardView::Problems;
        let problems = render_text(120, 26, &state);
        assert!(problems.contains("[RECENT PROBLEMS 3]"), "{problems}");
        assert!(
            problems.contains("WARNING [WAKE_DOCS]: Missing summary"),
            "{problems}"
        );
        assert!(problems.contains(":12:8"), "{problems}");
        assert_eq!(
            state.problems.back().unwrap().diagnostic.path.as_deref(),
            Some("src/你好.tsx")
        );

        state.view = DashboardView::Changes;
        let changes = render_text(120, 26, &state);
        assert!(changes.contains("[CHANGES 1]"), "{changes}");
        assert!(
            changes.contains("rc-alpha · /components/rc-alpha/"),
            "{changes}"
        );
        assert_eq!(state.changes[0].changed_paths[0], "src/你好.tsx");
        assert!(
            changes.contains("2 updated · 22 cache hits · 18ms"),
            "{changes}"
        );
        assert!(changes.contains("1 failed: rc-beta"), "{changes}");
    }

    #[test]
    fn only_error_diagnostics_change_run_state_to_error() {
        let mut state = DashboardState::new("dev", Path::new("demo"), "LOCAL", "watching");
        state.built(BuildMetrics::default(), true, None, None);
        for severity in ["warning", "note", "help"] {
            state.diagnostic(diagnostic(severity, severity), severity);
            assert_eq!(state.state, RunState::Ready);
        }
        assert_eq!(
            state
                .activity
                .iter()
                .rev()
                .take(3)
                .map(|item| item.level)
                .collect::<Vec<_>>(),
            vec![
                ActivityLevel::Info,
                ActivityLevel::Info,
                ActivityLevel::Warning
            ]
        );
        state.diagnostic(diagnostic("error", "broken"), "ERROR: broken");
        assert_eq!(state.state, RunState::Error);
    }

    #[test]
    fn view_scroll_and_clear_history_are_independent_and_bounded() {
        let mut state = DashboardState::new("dev", Path::new("demo"), "LOCAL", "watching");
        state.set_endpoint("http://127.0.0.1:5173/");
        state.metrics = Some(BuildMetrics {
            modules: 4,
            ..BuildMetrics::default()
        });
        state.scroll[DashboardView::Activity.index()].from_bottom = 1;
        state.diagnostic(diagnostic("warning", "warning"), "WARNING: warning");
        assert_eq!(state.scroll[DashboardView::Activity.index()].unread, 1);
        assert!(state.scroll[DashboardView::Activity.index()].from_bottom > 1);
        assert_eq!(state.scroll[DashboardView::Problems.index()].unread, 0);

        state.view = DashboardView::Problems;
        state.scroll[DashboardView::Problems.index()].from_bottom = 1;
        state.diagnostic(diagnostic("note", "note"), "NOTE: note");
        assert_eq!(state.scroll[DashboardView::Problems.index()].unread, 1);
        state.scroll_to_bottom();
        assert_eq!(state.current_scroll(), ViewScroll::default());

        state.clear_history();
        assert!(state.activity.is_empty());
        assert!(state.problems.is_empty());
        assert!(state.changes.is_empty());
        assert_eq!(state.endpoint, "http://127.0.0.1:5173/");
        assert_eq!(state.metrics.unwrap().modules, 4);
    }
}
