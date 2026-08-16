//! Terminal live-progress rendering for check/update runs.
//!
//! When stdout is a terminal and the command is not `--json`, a live panel
//! shows one row per plugin (spinner while checking, ✓/✗ when done) plus an
//! overall progress bar, so long network phases read as activity instead of
//! silence. The panel is cleared before the final line-oriented report, so
//! piped output (herdr logs, tests, scripts) stays exactly as before.
//! Everything here is best-effort: rendering failures never change the
//! command's result or exit code. VT sequences need a VT-capable terminal
//! (Windows Terminal, modern conhost, xterm-class); otherwise the panel is
//! simply not rendered.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const BAR_WIDTH: usize = 20;
const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";

pub struct Progress {
    enabled: bool,
    /// One slot per plugin in registry order; `None` = no row (skipped kind,
    /// e.g. local links or `--only` filters).
    rows: Vec<Option<Row>>,
    done: usize,
    visible: usize,
    frame: usize,
    rendered: bool,
}

struct Row {
    plugin_id: String,
    state: RowState,
}

enum RowState {
    Checking,
    Done { ok: bool, label: String },
}

impl Progress {
    /// Create a panel sized for `count` plugins. Disabled (and a no-op)
    /// when stdout is not a terminal or `--json` is active.
    pub fn new(count: usize, json: bool) -> Progress {
        Progress {
            enabled: !json && std::io::stdout().is_terminal(),
            rows: (0..count).map(|_| None).collect(),
            done: 0,
            visible: 0,
            frame: 0,
            rendered: false,
        }
    }

    /// Register a plugin row at `index` (registry order). Called for every
    /// plugin that will produce a status.
    pub fn row(&mut self, index: usize, plugin_id: &str) {
        if !self.enabled {
            return;
        }
        self.rows[index] = Some(Row {
            plugin_id: plugin_id.to_string(),
            state: RowState::Checking,
        });
        self.visible += 1;
    }

    /// Mark a plugin's row done; `ok` selects ✓/✗, `label` is the same text
    /// the final report will print for that plugin.
    pub fn done(&mut self, index: usize, ok: bool, label: String) {
        if !self.enabled {
            return;
        }
        if let Some(Some(row)) = self.rows.get_mut(index) {
            if matches!(row.state, RowState::Done { .. }) {
                return;
            }
            row.state = RowState::Done { ok, label };
            self.done += 1;
        }
        self.render();
    }

    /// Advance the spinner frame (call while waiting for in-flight checks).
    pub fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        self.frame = self.frame.wrapping_add(1);
        self.render();
    }

    /// Hide the panel. The final line-oriented report is printed afterwards,
    /// starting where the panel used to begin.
    pub fn finish(&mut self) {
        if !self.enabled || !self.rendered {
            return;
        }
        let mut out = String::new();
        out.push('\r');
        out.push_str(&format!("\x1b[{}A", self.visible + 1));
        out.push_str("\x1b[J");
        let _ = std::io::stdout().write_all(out.as_bytes());
        let _ = std::io::stdout().flush();
        self.rendered = false;
    }

    fn render(&mut self) {
        let mut out = String::new();
        if self.rendered {
            // Rewind to the panel top (rows + bar line).
            out.push_str(&format!("\x1b[{}A", self.visible + 1));
        }
        out.push('\r');
        let spinner = SPINNER[self.frame % SPINNER.len()];
        for row in self.rows.iter().flatten() {
            match &row.state {
                RowState::Checking => {
                    out.push_str(&format!(
                        "  {DIM}{spinner} {}{RESET}\x1b[K\n",
                        row.plugin_id
                    ));
                }
                RowState::Done { ok, label } => {
                    let mark = if *ok {
                        format!("{GREEN}✓{RESET}")
                    } else {
                        format!("{RED}✗{RESET}")
                    };
                    out.push_str(&format!("  {mark} {label}\x1b[K\n"));
                }
            }
        }
        let total = self.visible;
        let filled = (self.done * BAR_WIDTH).checked_div(total).unwrap_or(0);
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled));
        out.push_str(&format!("  {bar} {}/{} checked\x1b[K", self.done, total));
        let _ = std::io::stdout().write_all(out.as_bytes());
        let _ = std::io::stdout().flush();
        self.rendered = true;
    }
}

/// Run `f` while animating `label` on a single line (TTY only); returns
/// `f`'s result and clears the line. Non-TTY: runs `f` without decoration.
pub fn with_activity(label: &str, f: impl FnOnce() -> bool + Send + 'static) -> bool {
    if !std::io::stdout().is_terminal() {
        return f();
    }
    let handle = std::thread::spawn(f);
    let mut frame = 0usize;
    let mut stdout = std::io::stdout();
    let started = Instant::now();
    while !handle.is_finished() {
        let _ = write!(
            stdout,
            "\r  {}{} {}{RESET}\x1b[K",
            DIM,
            SPINNER[frame % SPINNER.len()],
            label
        );
        let _ = stdout.flush();
        std::thread::sleep(Duration::from_millis(80));
        frame += 1;
        if started.elapsed() > Duration::from_secs(60) {
            // Paranoia: never spin forever on a wedged child; the caller's
            // own timeout governs the install, this is only the paint loop.
            break;
        }
    }
    let ok = handle.join().unwrap_or(false);
    let _ = write!(stdout, "\r\x1b[K");
    let _ = stdout.flush();
    ok
}
