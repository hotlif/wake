//! Full-screen terminal dashboard for long-running Wake commands.

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Stderr};
use std::path::Path;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

const MAX_ACTIVITY: usize = 200;

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
    started: Instant,
    activity: VecDeque<Activity>,
    scroll_from_bottom: usize,
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
            started: Instant::now(),
            activity: VecDeque::new(),
            scroll_from_bottom: 0,
        };
        state.push(ActivityLevel::Info, "Starting Wake…");
        state
    }

    pub fn set_endpoint(&mut self, endpoint: impl Into<String>) {
        self.endpoint = endpoint.into();
    }

    pub fn rebuilding(&mut self, changed: usize) {
        self.state = RunState::Rebuilding;
        let message = if changed == 1 {
            "Rebuilding after 1 file change…".to_string()
        } else if changed > 1 {
            format!("Rebuilding after {changed} file changes…")
        } else {
            "Rebuilding…".to_string()
        };
        self.push(ActivityLevel::Warning, message);
    }

    pub fn built(&mut self, metrics: BuildMetrics, initial: bool) {
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
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.state = RunState::Error;
        self.push(ActivityLevel::Error, message);
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

    pub fn clear_activity(&mut self) {
        self.activity.clear();
        self.scroll_from_bottom = 0;
    }

    fn push(&mut self, level: ActivityLevel, message: impl Into<String>) {
        if self.activity.len() == MAX_ACTIVITY {
            self.activity.pop_front();
        }
        self.activity.push_back(Activity {
            elapsed: self.started.elapsed(),
            level,
            message: message.into(),
        });
        if self.scroll_from_bottom == 0 {
            self.scroll_from_bottom = 0;
        }
    }

    fn scroll_up(&mut self, amount: usize) {
        self.scroll_from_bottom =
            (self.scroll_from_bottom + amount).min(self.activity.len().saturating_sub(1));
    }

    fn scroll_down(&mut self, amount: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(amount);
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
        if let Err(error) = execute!(stderr, EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(error);
        }
        let backend = CrosstermBackend::new(stderr);
        match Terminal::new(backend) {
            Ok(mut terminal) => {
                if let Err(error) = terminal.clear() {
                    let _ = disable_raw_mode();
                    let _ = execute!(terminal.backend_mut(), Show, LeaveAlternateScreen);
                    return Err(error);
                }
                Ok(Self {
                    terminal,
                    palette: Palette::detect(color),
                    restored: false,
                })
            }
            Err(error) => {
                let _ = disable_raw_mode();
                let mut stderr = std::io::stderr();
                let _ = execute!(stderr, Show, LeaveAlternateScreen);
                Err(error)
            }
        }
    }

    pub fn draw(&mut self, state: &DashboardState) -> io::Result<()> {
        let palette = self.palette;
        self.terminal.draw(|frame| render(frame, state, palette))?;
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
        let Event::Key(key) = event::read()? else {
            return Ok(DashboardAction::Continue);
        };
        if key.kind != KeyEventKind::Press {
            return Ok(DashboardAction::Continue);
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
                Ok(DashboardAction::Interrupt)
            }
            (KeyCode::Char('q'), _) => Ok(DashboardAction::Quit),
            (KeyCode::Char('c'), _) => {
                state.clear_activity();
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Up, _) => {
                state.scroll_up(1);
                Ok(DashboardAction::Continue)
            }
            (KeyCode::Down, _) => {
                state.scroll_down(1);
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
            (KeyCode::End, _) => {
                state.scroll_from_bottom = 0;
                Ok(DashboardAction::Continue)
            }
            _ => Ok(DashboardAction::Continue),
        }
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
        self.restored = true;
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        self.restore();
    }
}

fn render(frame: &mut Frame<'_>, state: &DashboardState, palette: Palette) {
    let area = frame.area();
    if area.width < 60 || area.height < 14 {
        render_minimal(frame, area, state, palette);
    } else if area.width < 80 || area.height < 20 {
        render_compact(frame, area, state, palette);
    } else {
        render_full(frame, area, state, palette);
    }
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
        Constraint::Min(4),
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
    render_activity(frame, rows[3], state, palette);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("↑↓/PgUp/PgDn", palette.accent()),
            Span::styled(" scroll   ", palette.muted()),
            Span::styled("End", palette.accent()),
            Span::styled(" follow   ", palette.muted()),
            Span::styled("c", palette.accent()),
            Span::styled(" clear   ", palette.muted()),
            Span::styled("q/Ctrl-C", palette.accent()),
            Span::styled(" quit", palette.muted()),
        ])),
        rows[4],
    );
}

fn render_compact(frame: &mut Frame<'_>, area: Rect, state: &DashboardState, palette: Palette) {
    let block = chrome(state, palette);
    let inner = block.inner(area).inner(Margin::new(1, 0));
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(inner);
    frame.render_widget(Paragraph::new(header_line(state, palette)), rows[0]);
    frame.render_widget(
        Paragraph::new(vec![
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
        ])
        .wrap(Wrap { trim: true }),
        rows[1],
    );
    render_activity(frame, rows[2], state, palette);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "↑↓ scroll · End follow · c clear · q/Ctrl-C quit",
            palette.muted(),
        )),
        rows[3],
    );
}

fn render_minimal(frame: &mut Frame<'_>, area: Rect, state: &DashboardState, palette: Palette) {
    let block = chrome(state, palette);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let last = state
        .activity
        .back()
        .map(|item| item.message.as_str())
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
            Line::styled("Resize for details · q/Ctrl-C quit", palette.accent()),
        ])
        .wrap(Wrap { trim: true }),
        inner,
    );
}

fn metrics_line(state: &DashboardState, palette: Palette) -> Paragraph<'static> {
    Paragraph::new(metrics_spans(state, palette))
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

fn render_activity(frame: &mut Frame<'_>, area: Rect, state: &DashboardState, palette: Palette) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(palette.muted())
        .title(Span::styled(" ACTIVITY ", palette.brand()));
    let height = block.inner(area).height as usize;
    let end = state
        .activity
        .len()
        .saturating_sub(state.scroll_from_bottom);
    let start = end.saturating_sub(height.max(1));
    let items = state
        .activity
        .iter()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|item| {
            let symbol = match item.level {
                ActivityLevel::Info => "·",
                ActivityLevel::Success => "✓",
                ActivityLevel::Warning => "↻",
                ActivityLevel::Error => "✗",
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{}  ", elapsed_stamp(item.elapsed)),
                    palette.muted(),
                ),
                Span::styled(format!("{symbol} "), palette.activity(item.level)),
                Span::raw(item.message.clone()),
            ]))
        })
        .collect::<Vec<_>>();
    frame.render_widget(List::new(items).block(block), area);
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
                    .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn full_dashboard_contains_brand_metrics_and_controls() {
        let mut state = DashboardState::new("dev", Path::new("demo"), "LOCAL", "HMR · watching");
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
        );
        let text = render_text(90, 24, &state);
        assert!(text.contains("WAKE"), "{text}");
        assert!(text.contains("128 modules"), "{text}");
        assert!(text.contains("ACTIVITY"), "{text}");
        assert!(text.contains("q/Ctrl-C"), "{text}");

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
        );
        let activity = &state.activity.back().unwrap().message;
        assert!(activity.contains("Updated 1 module"), "{activity}");
        assert!(activity.contains("127 cache hits"), "{activity}");
    }

    #[test]
    fn compact_and_minimal_layouts_keep_status_visible() {
        let state = DashboardState::new("build --watch", Path::new("demo"), "WATCH", "src");
        let compact = render_text(70, 18, &state);
        let minimal = render_text(50, 10, &state);
        assert!(compact.contains("STARTING"), "{compact}");
        assert!(minimal.contains("Resize for details"), "{minimal}");
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
}
