//! 终端美化输出：ANSI 着色（遵循 `--no-color`/`NO_COLOR`/tty）+ 人类可读的体积/耗时。
//!
//! 输出风格对标 Vite/esbuild：留白、品牌行、`✓`/`✗` 状态符、次要信息用暗色。

use std::time::Duration;

// —— ANSI 代码 ——
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const MAGENTA_BOLD: &str = "\x1b[1;35m";

/// 着色器。`color=false` 时所有方法原样返回文本。
#[derive(Clone, Copy)]
pub struct Ui {
    pub color: bool,
}

impl Ui {
    pub fn new(color: bool) -> Ui {
        Ui { color }
    }

    fn wrap(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }

    pub fn brand(&self, s: &str) -> String {
        self.wrap(MAGENTA_BOLD, s)
    }
    pub fn ok(&self, s: &str) -> String {
        self.wrap(GREEN, s)
    }
    pub fn err(&self, s: &str) -> String {
        self.wrap(RED, s)
    }
    pub fn warn(&self, s: &str) -> String {
        self.wrap(YELLOW, s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.wrap(DIM, s)
    }
    pub fn accent(&self, s: &str) -> String {
        self.wrap(CYAN, s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.wrap(BOLD, s)
    }
}

/// 人类可读的字节数：`1.58 MB` / `842 B` / `12.3 KB`。
pub fn human_bytes(n: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let f = n as f64;
    if f >= MB {
        format!("{:.2} MB", f / MB)
    } else if f >= KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

/// 人类可读的耗时：`24ms` / `3.43s` / `1m3.2s`。
pub fn human_dur(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{:.0}ms", ms.max(1.0))
    } else if ms < 60_000.0 {
        format!("{:.2}s", ms / 1000.0)
    } else {
        let secs = d.as_secs_f64();
        format!("{}m{:.1}s", (secs / 60.0) as u64, secs % 60.0)
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
    fn dur_is_human() {
        assert_eq!(human_dur(Duration::from_millis(24)), "24ms");
        assert_eq!(human_dur(Duration::from_millis(3430)), "3.43s");
    }

    #[test]
    fn plain_ui_has_no_codes() {
        let u = Ui::new(false);
        assert_eq!(u.ok("x"), "x");
        assert!(!u.brand("wake").contains('\x1b'));
    }
}
