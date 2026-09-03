//! Wake terminal presentation primitives shared by static and full-screen modes.

use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use clap::ValueEnum;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::dashboard::{BuildMetrics, human_duration, human_runtime};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum UiMode {
    #[default]
    Auto,
    Tui,
    Plain,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Auto,
    Human,
    Json,
}

impl OutputFormat {
    pub fn resolve(self) -> Self {
        match self {
            Self::Auto if std::io::stdout().is_terminal() => Self::Human,
            Self::Auto => Self::Json,
            explicit => explicit,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Ui {
    pub color: bool,
    true_color: bool,
}

impl Ui {
    pub fn detect(no_color: bool) -> Self {
        let color =
            !no_color && std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal();
        Self::new(color)
    }

    pub fn new(color: bool) -> Self {
        let true_color = color
            && std::env::var("COLORTERM").ok().is_some_and(|value| {
                value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
            });
        Self { color, true_color }
    }

    fn wrap(&self, indexed: u8, rgb: (u8, u8, u8), s: &str) -> String {
        if !self.color {
            return s.to_string();
        }
        let code = if self.true_color {
            format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
        } else {
            format!("\x1b[38;5;{indexed}m")
        };
        format!("{code}{s}{RESET}")
    }

    pub fn brand(&self, s: &str) -> String {
        let value = self.wrap(213, (217, 70, 239), s);
        if self.color {
            format!("{BOLD}{value}{RESET}")
        } else {
            value
        }
    }

    pub fn ok(&self, s: &str) -> String {
        self.wrap(114, (74, 222, 128), s)
    }

    pub fn err(&self, s: &str) -> String {
        self.wrap(204, (251, 113, 133), s)
    }

    pub fn warn(&self, s: &str) -> String {
        self.wrap(214, (251, 191, 36), s)
    }

    pub fn dim(&self, s: &str) -> String {
        if self.color {
            format!("{DIM}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    pub fn accent(&self, s: &str) -> String {
        self.wrap(81, (34, 211, 238), s)
    }

    pub fn bold(&self, s: &str) -> String {
        if self.color {
            format!("{BOLD}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    pub fn header(&self, command: &str) {
        eprintln!();
        eprintln!(
            "  {} {} {} {}  {}",
            self.warn("⚡"),
            self.brand("WAKE"),
            self.dim("/"),
            self.bold(&command.to_uppercase()),
            self.dim(&format!("v{}", wake_app::VERSION)),
        );
        eprintln!();
    }

    pub fn build_result(&self, label: &str, result: &wake_app::BuildResult, extra: Option<&str>) {
        let bytes = result.files.iter().map(|file| file.bytes).sum::<usize>();
        eprintln!(
            "  {}  {} {}",
            self.ok("✓"),
            self.bold(label),
            self.accent(&format!("in {}", human_duration(result.duration_ms))),
        );
        eprintln!(
            "     {} {} {} {} {} {} {}",
            self.accent(&format!("{} modules", result.module_count)),
            self.dim("·"),
            result.files.len(),
            self.dim("files"),
            self.dim("·"),
            self.accent(&human_bytes(bytes)),
            extra.unwrap_or_default(),
        );
        if let Some(output_dir) = &result.output_dir {
            eprintln!("     {}  {}", self.dim("Output"), self.accent(output_dir));
        }
        for diagnostic in &result.diagnostics {
            self.write_diagnostic(diagnostic, "     ");
        }
        eprintln!();
        let _ = io::stderr().flush();
    }

    pub fn bundle_result(&self, label: &str, result: &wake_app::BundleResult, extra: Option<&str>) {
        let bytes = result.files.iter().map(|file| file.bytes).sum::<usize>();
        eprintln!(
            "  {}  {} {}",
            self.ok("✓"),
            self.bold(label),
            self.accent(&format!("in {}", human_duration(result.duration_ms))),
        );
        eprintln!(
            "     {} {} {} {} {} {} {}",
            self.accent(&format!("{} modules", result.module_count)),
            self.dim("·"),
            result.files.len(),
            self.dim("files"),
            self.dim("·"),
            self.accent(&human_bytes(bytes)),
            extra.unwrap_or_default(),
        );
        if let Some(output_file) = &result.output_file {
            eprintln!("     {}  {}", self.dim("Output"), self.accent(output_file));
        }
        for diagnostic in &result.diagnostics {
            self.write_diagnostic(diagnostic, "     ");
        }
        eprintln!();
        let _ = io::stderr().flush();
    }

    pub fn app_error(&self, error: &wake_app::WakeError) {
        eprintln!(
            "  {}  {} {}",
            self.err("✗"),
            self.bold("Wake failed"),
            self.err(&format!("[{}]", error.code)),
        );
        eprintln!("     {}", error.message);
        if let Some(path) = &error.path {
            eprintln!("     {}  {}", self.dim("Path"), self.accent(path));
        }
        for diagnostic in &error.diagnostics {
            self.write_diagnostic(diagnostic, "     ");
        }
        eprintln!();
    }

    pub fn server_ready(&self, endpoint: &str, metrics: Option<BuildMetrics>) {
        eprintln!(
            "  {}  {}",
            self.ok("✓"),
            self.bold("Development server ready")
        );
        eprintln!("     {}  {}", self.dim("Local"), self.accent(endpoint));
        if let Some(metrics) = metrics {
            eprintln!(
                "     {} {} {} {} {} {} {}",
                self.accent(&format!("{} modules", metrics.modules)),
                self.dim("·"),
                metrics.chunks,
                self.dim("chunks"),
                self.dim("·"),
                metrics.assets,
                self.dim("assets"),
            );
        }
        eprintln!("     {}", self.dim("Press Ctrl-C to stop"));
        eprintln!();
    }

    pub fn rebuild_start(&self, changed: usize) {
        let detail = if changed == 1 {
            "Rebuilding after 1 file change…".to_string()
        } else if changed > 1 {
            format!("Rebuilding after {changed} file changes…")
        } else {
            "Rebuilding…".to_string()
        };
        eprintln!("  {}  {}", self.warn("↻"), self.dim(&detail));
    }

    pub fn rebuilt(&self, metrics: BuildMetrics, initial: bool) {
        if initial {
            return;
        }
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
        eprintln!(
            "  {}  {}  {}  {}  {}  {}  {}",
            self.ok("✓"),
            self.bold("Updated"),
            self.dim("·"),
            self.accent(&updated),
            self.dim("·"),
            self.accent(&cached),
            self.accent(&human_duration(metrics.duration_ms)),
        );
    }

    pub fn diagnostic(&self, diagnostic: &wake_app::DiagnosticInfo) {
        let marker = match diagnostic.severity.as_str() {
            "error" => self.err("✗"),
            "warning" => self.warn("!"),
            _ => self.accent("·"),
        };
        for (index, line) in self.format_diagnostic(diagnostic).lines().enumerate() {
            if index == 0 {
                eprintln!("  {marker}  {}", self.bold(line));
            } else {
                eprintln!("     {line}");
            }
        }
    }

    pub fn format_diagnostic(&self, diagnostic: &wake_app::DiagnosticInfo) -> String {
        let mut lines = Vec::new();
        let severity = diagnostic.severity.to_uppercase();
        let heading = match &diagnostic.code {
            Some(code) => format!("{severity} [{code}]: {}", diagnostic.message),
            None => format!("{severity}: {}", diagnostic.message),
        };
        lines.push(match diagnostic.severity.as_str() {
            "error" => self.err(&heading),
            "warning" => self.warn(&heading),
            _ => self.accent(&heading),
        });

        match (&diagnostic.path, &diagnostic.location) {
            (Some(path), Some(location)) => lines.push(format!(
                " {} {}:{}:{}",
                self.dim("-->"),
                self.accent(path),
                location.line,
                location.column
            )),
            (Some(path), None) => {
                lines.push(format!(" {} {}", self.dim("-->"), self.accent(path)));
            }
            _ => {}
        }

        if let Some(location) = &diagnostic.location {
            let line_text = expand_tabs(&location.line_text);
            let gutter = location.line.to_string().len();
            let start = display_column(&location.line_text, location.column);
            let end = if location.end_line == location.line {
                display_column(&location.line_text, location.end_column)
            } else {
                UnicodeWidthStr::width(line_text.as_str())
            };
            let width = end.saturating_sub(start).max(1);
            lines.push(format!("{} {}", " ".repeat(gutter), self.dim("|")));
            lines.push(format!(
                "{:>gutter$} {} {line_text}",
                location.line,
                self.dim("|")
            ));
            let marker = format!(
                "{}{}{}",
                " ".repeat(start),
                "^".repeat(width),
                location
                    .label
                    .as_deref()
                    .map(|label| format!(" {label}"))
                    .unwrap_or_default()
            );
            lines.push(format!(
                "{} {} {}",
                " ".repeat(gutter),
                self.dim("|"),
                self.err(&marker)
            ));
        }
        for note in &diagnostic.notes {
            lines.push(format!("  {} note: {note}", self.dim("=")));
        }
        lines.join("\n")
    }

    fn write_diagnostic(&self, diagnostic: &wake_app::DiagnosticInfo, indent: &str) {
        for line in self.format_diagnostic(diagnostic).lines() {
            eprintln!("{indent}{line}");
        }
    }

    pub fn final_summary(
        &self,
        state: &str,
        target_label: &str,
        target: &str,
        rebuilds: usize,
        runtime: Duration,
        reason: &str,
    ) {
        eprintln!();
        eprintln!(
            "  {}  {} {}",
            self.dim("■"),
            self.bold(state),
            self.dim(&format!("({reason})")),
        );
        if !target.is_empty() {
            eprintln!("     {}  {}", self.dim(target_label), self.accent(target));
        }
        eprintln!(
            "     {} rebuilds {} runtime {}",
            rebuilds,
            self.dim("·"),
            self.accent(&human_runtime(runtime)),
        );
        eprintln!();
    }
}

pub fn format_diagnostic_plain(diagnostic: &wake_app::DiagnosticInfo) -> String {
    Ui::new(false).format_diagnostic(diagnostic)
}

fn display_column(line: &str, one_based_column: u32) -> usize {
    let prefix = line
        .chars()
        .take(one_based_column.saturating_sub(1) as usize)
        .collect::<String>();
    UnicodeWidthStr::width(expand_tabs(&prefix).as_str())
}

fn expand_tabs(line: &str) -> String {
    const TAB_STOP: usize = 4;
    let mut output = String::new();
    let mut width = 0;
    for character in line.chars() {
        if character == '\t' {
            let spaces = TAB_STOP - width % TAB_STOP;
            output.push_str(&" ".repeat(spaces));
            width += spaces;
        } else {
            output.push(character);
            width += UnicodeWidthChar::width(character).unwrap_or(0);
        }
    }
    output
}

pub fn human_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let value = bytes as f64;
    if value >= MB {
        format!("{:.2} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_human() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(1_657_958), "1.58 MB");
    }

    #[test]
    fn duration_is_human() {
        assert_eq!(human_duration(24.0), "24ms");
        assert_eq!(human_duration(3430.0), "3.43s");
    }

    #[test]
    fn plain_ui_has_no_escape_codes() {
        let ui = Ui::new(false);
        assert_eq!(ui.ok("x"), "x");
        assert!(!ui.brand("WAKE").contains('\x1b'));
    }

    #[test]
    fn output_format_uses_explicit_value() {
        assert_eq!(OutputFormat::Human.resolve(), OutputFormat::Human);
        assert_eq!(OutputFormat::Json.resolve(), OutputFormat::Json);
    }

    #[test]
    fn shared_diagnostic_contract_renders_exact_code_frame() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/terminal-diagnostic-contract.json"
        ))
        .unwrap();
        let value = &contract["diagnostic"];
        let location = &value["location"];
        let diagnostic = wake_app::DiagnosticInfo {
            severity: value["severity"].as_str().unwrap().to_string(),
            code: value["code"].as_str().map(str::to_string),
            message: value["message"].as_str().unwrap().to_string(),
            path: value["path"].as_str().map(str::to_string),
            start: value["start"].as_u64().map(|value| value as u32),
            end: value["end"].as_u64().map(|value| value as u32),
            location: Some(wake_app::DiagnosticLocation {
                line: location["line"].as_u64().unwrap() as u32,
                column: location["column"].as_u64().unwrap() as u32,
                end_line: location["endLine"].as_u64().unwrap() as u32,
                end_column: location["endColumn"].as_u64().unwrap() as u32,
                line_text: location["lineText"].as_str().unwrap().to_string(),
                label: location["label"].as_str().map(str::to_string),
            }),
            notes: value["notes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|note| note.as_str().unwrap().to_string())
                .collect(),
        };
        let expected = contract["plainLines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            format_diagnostic_plain(&diagnostic)
                .lines()
                .collect::<Vec<_>>(),
            expected
        );
    }
}
