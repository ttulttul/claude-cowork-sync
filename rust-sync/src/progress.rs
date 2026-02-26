use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use log::debug;

const SPINNER_FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

type ValueFormatter = fn(u64) -> String;

#[derive(Debug, Clone, Copy)]
pub enum ProgressColor {
    Blue,
    Cyan,
    Green,
    Magenta,
    Red,
    Yellow,
}

pub fn progress_rendering_enabled() -> bool {
    io::stderr().is_terminal() && progress_enabled_by_env()
}

pub fn terminal_supports_color() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }
    let term = env::var("TERM").unwrap_or_default();
    if term.eq_ignore_ascii_case("dumb") {
        return false;
    }
    io::stderr().is_terminal()
}

pub fn colorize_text(text: &str, color: ProgressColor, enabled: bool) -> String {
    if !enabled {
        return text.to_string();
    }
    let code = match color {
        ProgressColor::Blue => "\u{1b}[34m",
        ProgressColor::Cyan => "\u{1b}[36m",
        ProgressColor::Green => "\u{1b}[32m",
        ProgressColor::Magenta => "\u{1b}[35m",
        ProgressColor::Red => "\u{1b}[31m",
        ProgressColor::Yellow => "\u{1b}[33m",
    };
    format!("{code}{text}\u{1b}[0m")
}

pub struct TerminalProgress {
    label: String,
    total: Option<u64>,
    unit: String,
    value_formatter: Option<ValueFormatter>,
    color: ProgressColor,
    min_interval: Duration,
    last_render: Instant,
    last_line_len: usize,
    spinner_index: usize,
    color_enabled: bool,
    active: bool,
    started_at: Instant,
}

impl TerminalProgress {
    pub fn new(label: &str, total: Option<u64>, unit: &str, color: ProgressColor) -> Self {
        let render_enabled = progress_rendering_enabled();
        let color_enabled = render_enabled && terminal_supports_color();
        let now = Instant::now();
        debug!(
            "Terminal progress initialized: label={} enabled={} total={:?}",
            label, render_enabled, total
        );
        Self {
            label: label.to_string(),
            total,
            unit: unit.to_string(),
            value_formatter: None,
            color,
            min_interval: Duration::from_millis(80),
            last_render: now,
            last_line_len: 0,
            spinner_index: 0,
            color_enabled,
            active: render_enabled,
            started_at: now,
        }
    }

    pub fn with_formatter(mut self, formatter: ValueFormatter) -> Self {
        self.value_formatter = Some(formatter);
        self
    }

    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    pub fn update(&mut self, completed: u64, detail: &str, force: bool) {
        if !self.active {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.last_render) < self.min_interval {
            return;
        }
        self.last_render = now;

        let line = self.build_line(completed, detail);
        let padded = self.pad_for_overwrite(&line);
        eprint!("\r{padded}");
        let _ = io::stderr().flush();
    }

    pub fn finish(&mut self, completed: u64, detail: &str, success: bool) {
        if !self.active {
            return;
        }
        let elapsed = Instant::now().duration_since(self.started_at).as_secs_f32();
        let status = if success { "OK" } else { "ERR" };
        let status_color = if success {
            ProgressColor::Green
        } else {
            ProgressColor::Red
        };
        let status_text = colorize_text(status, status_color, self.color_enabled);
        let suffix = format!("{status_text} {detail} ({elapsed:.1}s)");
        let line = self.build_line(completed, &suffix);
        let padded = self.pad_for_overwrite(&line);
        eprint!("\r{padded}\n");
        let _ = io::stderr().flush();
        self.active = false;
    }

    fn build_line(&mut self, completed: u64, detail: &str) -> String {
        let label = colorize_text(&self.label, self.color, self.color_enabled);
        let completed_text = self.format_value(completed);

        let body = if let Some(total) = self.total {
            if total > 0 && completed <= total {
                let ratio = completed as f64 / total as f64;
                let clamped = ratio.clamp(0.0, 1.0);
                let bar = progress_bar(clamped, 24);
                let percent = (clamped * 100.0).round() as u64;
                let total_text = self.format_value(total);
                format!(
                    "{bar} {percent:3}% {completed_text}/{total_text} {}",
                    self.unit
                )
            } else {
                let spinner = SPINNER_FRAMES[self.spinner_index % SPINNER_FRAMES.len()];
                self.spinner_index += 1;
                let total_text = self.format_value(total);
                format!(
                    "{spinner} {completed_text} {} (est {total_text})",
                    self.unit
                )
            }
        } else {
            let spinner = SPINNER_FRAMES[self.spinner_index % SPINNER_FRAMES.len()];
            self.spinner_index += 1;
            format!("{spinner} {completed_text} {}", self.unit)
        };

        if detail.is_empty() {
            format!("{label}: {body}")
        } else {
            format!("{label}: {body}  {detail}")
        }
    }

    fn pad_for_overwrite(&mut self, line: &str) -> String {
        let extra = self.last_line_len.saturating_sub(line.len());
        self.last_line_len = line.len();
        if extra == 0 {
            line.to_string()
        } else {
            format!("{line}{}", " ".repeat(extra))
        }
    }

    fn format_value(&self, value: u64) -> String {
        match self.value_formatter {
            Some(formatter) => formatter(value),
            None => value.to_string(),
        }
    }
}

pub fn run_with_spinner_result<T, E, F>(
    label: &str,
    detail: &str,
    color: ProgressColor,
    success_detail: &str,
    action: F,
) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
{
    if !progress_rendering_enabled() {
        return action();
    }

    let progress = Arc::new(Mutex::new(
        TerminalProgress::new(label, None, "ticks", color)
            .with_min_interval(Duration::from_millis(120)),
    ));

    {
        if let Ok(mut guard) = progress.lock() {
            guard.update(0, detail, true);
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let progress_clone = Arc::clone(&progress);
    let stop_clone = Arc::clone(&stop);
    let detail_owned = detail.to_string();

    let spinner_handle = thread::spawn(move || {
        let mut ticks = 0_u64;
        while !stop_clone.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(120));
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            ticks += 1;
            if let Ok(mut guard) = progress_clone.lock() {
                guard.update(ticks, &detail_owned, true);
            }
        }
        ticks
    });

    let result = action();

    stop.store(true, Ordering::Relaxed);
    let ticks = spinner_handle.join().unwrap_or(0);

    if let Ok(mut guard) = progress.lock() {
        match result {
            Ok(_) => guard.finish(ticks, success_detail, true),
            Err(_) => guard.finish(ticks, "failed", false),
        }
    }

    result
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_index = 0_usize;
    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }
    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{value:.1} {}", UNITS[unit_index])
    }
}

fn progress_enabled_by_env() -> bool {
    let raw = env::var("COWORK_MERGE_PROGRESS")
        .unwrap_or_else(|_| "1".to_string())
        .trim()
        .to_ascii_lowercase();
    !matches!(raw.as_str(), "0" | "false" | "off" | "no")
}

fn progress_bar(ratio: f64, width: usize) -> String {
    let filled = ((width as f64) * ratio).round() as usize;
    let clamped_filled = filled.min(width);
    format!(
        "[{}{}]",
        "#".repeat(clamped_filled),
        "-".repeat(width.saturating_sub(clamped_filled))
    )
}

#[cfg(test)]
mod tests {
    use super::{format_bytes, progress_bar};

    #[test]
    fn format_bytes_formats_expected_units() {
        assert_eq!(format_bytes(1), "1 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn progress_bar_clamps_ratio() {
        assert_eq!(progress_bar(-1.0, 4), "[----]");
        assert_eq!(progress_bar(0.5, 4), "[##--]");
        assert_eq!(progress_bar(2.0, 4), "[####]");
    }
}
