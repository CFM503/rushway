//! GoWay-parity UX surfaces: TUI dashboard, `-log-file` ring, and the
//! tracing bridge that feeds both.
//!
//! Layout and cadence mirror `goway.go` `drawTUI` / `tuiRefreshLoop` /
//! `addLogFileEntry` so operators can switch tools without relearning the
//! dashboard.

use crate::runtime::RuntimeConfig;
use crate::stats;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

const TUI_RING_CAP: usize = 100;
const LOG_FILE_RING_CAP: usize = 10;

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_MAGENTA: &str = "\x1b[35m";
const ANSI_GREY: &str = "\x1b[90m";

// ---------------------------------------------------------------------------
// TUI ring + dirty flag
// ---------------------------------------------------------------------------

static ENABLED: AtomicBool = AtomicBool::new(false);

struct TuiState {
    dirty: AtomicBool,
    ring: Mutex<VecDeque<String>>,
}

fn tui_state() -> &'static TuiState {
    static STATE: OnceLock<TuiState> = OnceLock::new();
    STATE.get_or_init(|| TuiState {
        dirty: AtomicBool::new(false),
        ring: Mutex::new(VecDeque::with_capacity(TUI_RING_CAP)),
    })
}

/// Enables TUI bookkeeping. Must be called before the refresh loop starts.
pub fn enable_tui() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn tui_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn trigger_refresh() {
    if tui_enabled() {
        tui_state().dirty.store(true, Ordering::Relaxed);
    }
}

fn add_tui_log(line: &str) {
    if !tui_enabled() {
        return;
    }
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let mut ring = tui_state().ring.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() == TUI_RING_CAP {
        ring.pop_front();
    }
    ring.push_back(line.to_string());
    tui_state().dirty.store(true, Ordering::Relaxed);
}

fn tui_log_slice(n: usize) -> Vec<String> {
    let ring = tui_state().ring.lock().unwrap_or_else(|e| e.into_inner());
    let take = n.min(ring.len());
    ring.iter().rev().take(take).rev().cloned().collect()
}

// ---------------------------------------------------------------------------
// `-log-file` ring (last 10 WARN/ERROR lines, rewritten on each append)
// ---------------------------------------------------------------------------

struct LogFileState {
    path: Mutex<Option<PathBuf>>,
    ring: Mutex<VecDeque<String>>,
}

fn log_file_state() -> &'static LogFileState {
    static STATE: OnceLock<LogFileState> = OnceLock::new();
    STATE.get_or_init(|| LogFileState {
        path: Mutex::new(None),
        ring: Mutex::new(VecDeque::with_capacity(LOG_FILE_RING_CAP)),
    })
}

/// Configures the `-log-file` destination (call once during startup).
pub fn configure_log_file(path: PathBuf) {
    let state = log_file_state();
    *state.path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

fn log_file_add(entry: &str) {
    let state = log_file_state();
    let path = state.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(path) = path else { return };
    let stamp = stamp_ymd_hms();
    let mut ring = state.ring.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() == LOG_FILE_RING_CAP {
        ring.pop_front();
    }
    ring.push_back(format!("{stamp} {entry}"));
    let content = render_log_file(&ring);
    drop(ring);
    let _ = std::fs::write(&path, content);
}

fn render_log_file(ring: &VecDeque<String>) -> String {
    let mut out = String::new();
    for line in ring {
        let _ = writeln!(out, "{line}");
    }
    out
}

/// Final flush on shutdown (GoWay `saveLogFile` parity).
pub fn save_log_file() {
    let state = log_file_state();
    let path = state.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(path) = path else { return };
    let ring = state.ring.lock().unwrap_or_else(|e| e.into_inner());
    if ring.is_empty() {
        return;
    }
    let content = render_log_file(&ring);
    let count = ring.len();
    drop(ring);
    match std::fs::write(&path, content) {
        Ok(()) => tracing::info!(path = %path.display(), entries = count, "log file saved"),
        Err(error) => tracing::warn!(path = %path.display(), %error, "failed to save log file"),
    }
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

fn clock_hms() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn stamp_ymd_hms() -> String {
    // Civil date from Unix seconds (Howard Hinnant's algorithm) — avoids a
    // formatting dependency and matches GoWay's local-looking wall stamp shape.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = now.div_euclid(86_400);
    let secs_of_day = now.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// ANSI helpers
// ---------------------------------------------------------------------------

fn visible_len(s: &str) -> usize {
    let mut len = 0;
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == 0x1b {
            for b in bytes.by_ref() {
                if b == b'm' {
                    break;
                }
            }
            continue;
        }
        len += 1;
    }
    len
}

fn pad_visible(s: &str, width: usize) -> String {
    let vl = visible_len(s);
    if vl >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - vl))
}

fn truncate_visible(s: &str, max_len: usize) -> String {
    if visible_len(s) <= max_len {
        return s.to_string();
    }
    let limit = max_len.saturating_sub(3);
    let mut out = String::new();
    let mut count = 0;
    let mut in_esc = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_esc = true;
            out.push(ch);
            continue;
        }
        if in_esc {
            out.push(ch);
            if ch == 'm' {
                in_esc = false;
            }
            continue;
        }
        if count < limit {
            out.push(ch);
            count += 1;
        } else {
            break;
        }
    }
    out.push_str("...");
    out.push_str(ANSI_RESET);
    out
}

fn format_bytes(bytes: i64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    const UNITS: [f64; 5] = [
        1024.0,
        1_048_576.0,
        1_073_741_824.0,
        1_099_511_627_776.0,
        1_125_899_906_842_624.0,
    ];
    const LABELS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    let abs = bytes.unsigned_abs() as f64;
    let neg = bytes < 0;
    if abs < 1024.0 {
        return format!("{bytes} B");
    }
    for (i, div) in UNITS.iter().enumerate() {
        if abs < *div * 1024.0 || i == UNITS.len() - 1 {
            let v = abs / div;
            return format!("{}{v:.2} {}", if neg { "-" } else { "" }, LABELS[i]);
        }
    }
    format!("{bytes} B")
}

fn two_col(
    left_label: &str,
    left_val: &str,
    right_label: &str,
    right_val: &str,
    col: usize,
) -> String {
    let left = pad_visible(&format!("{left_label}{left_val}"), col);
    format!("{left}{right_label}{right_val}")
}

// ---------------------------------------------------------------------------
// Dashboard render + refresh loop
// ---------------------------------------------------------------------------

/// Immutable snapshot of everything the dashboard shows (built once in main).
#[derive(Clone)]
pub struct TuiConfig {
    pub version: String,
    pub mode: String,
    pub listen: String,
    pub upstream: String,
    pub mux: String,
    pub obfs: String,
    pub dns: String,
    pub auth: String,
    pub buffer: String,
    pub max_conns: usize,
    pub block_local: bool,
    pub verify_ssl: bool,
    pub log_level: String,
    pub fakehost: Option<String>,
    pub tcp_nodelay: bool,
    pub tcp_keepalive: bool,
    pub connection_timeout: u64,
}

impl TuiConfig {
    pub fn from_runtime(
        cfg: &RuntimeConfig,
        mux_sessions: usize,
        dns_display: &str,
        verify_ssl: bool,
        log_level: &str,
    ) -> Self {
        let mode = match cfg.upstream.as_deref() {
            None => "Server".to_string(),
            Some(u) if u.starts_with("wss://") => "Client (WSS + SOCKS5/HTTP)".to_string(),
            Some(u) if u.starts_with("quic") => "Client (QUIC + SOCKS5/HTTP)".to_string(),
            Some(_) => "Client (WS + SOCKS5/HTTP)".to_string(),
        };
        let mux = if cfg.mux {
            format!("Enabled (sessions: {mux_sessions})")
        } else {
            "Disabled (1:1)".to_string()
        };
        let auth = if cfg.key.is_some() {
            "Enabled (XOR)".to_string()
        } else if cfg.upstream.is_none() {
            "DISABLED (Open Proxy!)".to_string()
        } else {
            "Disabled".to_string()
        };
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            mode,
            listen: format!("{}:{}", cfg.proxy_host, cfg.proxy_port),
            upstream: cfg.upstream.clone().unwrap_or_else(|| "N/A".to_string()),
            mux,
            obfs: if cfg.obfs { "Enabled" } else { "Disabled" }.to_string(),
            dns: dns_display.to_string(),
            auth,
            buffer: {
                let mut s = format!("{} KB", cfg.buffer_size / 1024);
                if cfg.socket_buffer > 0 {
                    let _ = write!(s, " (Socket: {} KB)", cfg.socket_buffer);
                }
                s
            },
            max_conns: cfg.max_connections,
            block_local: cfg.block_local,
            verify_ssl,
            log_level: log_level.trim().to_ascii_uppercase(),
            fakehost: cfg.fakehost.clone(),
            tcp_nodelay: cfg.tcp_nodelay,
            tcp_keepalive: cfg.tcp_keepalive,
            connection_timeout: cfg.connection_timeout,
        }
    }
}

fn draw_tui(cfg: &TuiConfig) {
    let (term_w, term_h) = terminal_size();
    let mut box_w = term_w.saturating_sub(2);
    box_w = box_w.clamp(50, 110);
    let inner = box_w.saturating_sub(4);
    let col = inner / 2;

    let h = "─".repeat(box_w.saturating_sub(2));
    let top = format!("┌{h}┐\n");
    let mid = format!("├{h}┤\n");
    let bot = format!("└{h}┘\n");

    let mut buf = String::with_capacity(box_w * (term_h + 4));
    buf.push_str("\x1b[H\x1b[J");
    buf.push_str(&top);

    let title = format!("RUSHWAY Proxy Dashboard (v{})", cfg.version);
    let pad = (box_w.saturating_sub(2).saturating_sub(title.len())) / 2;
    let rpad = box_w
        .saturating_sub(2)
        .saturating_sub(pad)
        .saturating_sub(title.len());
    let _ = writeln!(
        buf,
        "│{}{ANSI_CYAN}{title}{ANSI_RESET}{}│",
        " ".repeat(pad),
        " ".repeat(rpad)
    );
    buf.push_str(&mid);

    let mode_col = if cfg.upstream == "N/A" {
        format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.mode)
    } else {
        format!("{ANSI_GREEN}{}{ANSI_RESET}", cfg.mode)
    };
    row(
        &mut buf,
        &two_col(
            "Mode:      ",
            &mode_col,
            "Max Conns:   ",
            &cfg.max_conns.to_string(),
            col,
        ),
        box_w,
    );
    row(
        &mut buf,
        &two_col(
            "Listen:    ",
            &format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.listen),
            "Timeout:     ",
            &format!("{ANSI_YELLOW}{}s{ANSI_RESET}", cfg.connection_timeout),
            col,
        ),
        box_w,
    );
    let block_col = if cfg.block_local {
        format!("{ANSI_GREEN}Enabled{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Disabled{ANSI_RESET}")
    };
    row(
        &mut buf,
        &two_col(
            "Upstream:  ",
            &format!("{ANSI_MAGENTA}{}{ANSI_RESET}", cfg.upstream),
            "Block Local: ",
            &block_col,
            col,
        ),
        box_w,
    );
    let ssl_col = if cfg.upstream == "N/A" {
        format!("{ANSI_GREY}N/A{ANSI_RESET}")
    } else if cfg.verify_ssl {
        format!("{ANSI_GREEN}Enabled{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Disabled (Insecure){ANSI_RESET}")
    };
    let auth_col = if cfg.auth.starts_with("Enabled") {
        format!("{ANSI_GREEN}{}{ANSI_RESET}", cfg.auth)
    } else if cfg.auth.contains("DISABLED") {
        format!("{ANSI_RED}{}{ANSI_RESET}", cfg.auth)
    } else {
        format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.auth)
    };
    row(
        &mut buf,
        &two_col("Auth:      ", &auth_col, "SSL Verify:  ", &ssl_col, col),
        box_w,
    );
    let dns_col = if cfg.dns == "System default" {
        format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.dns)
    } else {
        format!("{ANSI_GREEN}Remote: {}{ANSI_RESET}", cfg.dns)
    };
    let level_col = match cfg.log_level.as_str() {
        "DEBUG" => format!("{ANSI_CYAN}DEBUG{ANSI_RESET}"),
        "WARN" | "WARNING" => format!("{ANSI_YELLOW}WARN{ANSI_RESET}"),
        "ERROR" => format!("{ANSI_RED}ERROR{ANSI_RESET}"),
        "OFF" => format!("{ANSI_GREY}OFF{ANSI_RESET}"),
        _ => format!("{ANSI_GREEN}INFO{ANSI_RESET}"),
    };
    row(
        &mut buf,
        &two_col("DNS:       ", &dns_col, "Log Level:   ", &level_col, col),
        box_w,
    );
    let nd = if cfg.tcp_nodelay {
        format!("{ANSI_GREEN}On{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Off{ANSI_RESET}")
    };
    let ka = if cfg.tcp_keepalive {
        format!("{ANSI_GREEN}On{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Off{ANSI_RESET}")
    };
    row(
        &mut buf,
        &two_col(
            "Buffer:    ",
            &cfg.buffer,
            "TCP Settings:",
            &format!("NoDelay:{nd} KeepAlive:{ka}"),
            col,
        ),
        box_w,
    );
    let mut config_lines = 6u32;
    if let Some(fh) = cfg.fakehost.as_deref().filter(|s| !s.is_empty()) {
        row(
            &mut buf,
            &two_col(
                "Fake Host: ",
                &format!("{ANSI_MAGENTA}{fh}{ANSI_RESET}"),
                "",
                "",
                col,
            ),
            box_w,
        );
        config_lines = 7;
    }
    row(
        &mut buf,
        &two_col("Mux:       ", &cfg.mux, "Obfs:        ", &cfg.obfs, col),
        box_w,
    );
    config_lines += 1;

    buf.push_str(&mid);

    let (active, (up_total, down_total)) = (stats::active_conns(), stats::totals());
    let (speed_up, speed_down) = stats::speeds();
    row(
        &mut buf,
        &two_col(
            "Conns:     ",
            &format!("{active} / {}", cfg.max_conns),
            "",
            "",
            col,
        ),
        box_w,
    );
    row(
        &mut buf,
        &two_col(
            "Download:  ",
            &format!("{speed_down:.2} MB/s"),
            "Total Down:  ",
            &format_bytes(down_total),
            col,
        ),
        box_w,
    );
    row(
        &mut buf,
        &two_col(
            "Upload:    ",
            &format!("{speed_up:.2} MB/s"),
            "Total Up:    ",
            &format_bytes(up_total),
            col,
        ),
        box_w,
    );

    buf.push_str(&mid);

    // 3 borders + title lines + config rows + mid + 3 stats + mid + logs + bot
    let used = 3 + 2 + config_lines + 1 + 3 + 1 + 1 + 1;
    let mut log_lines = (term_h as i32 - used as i32).max(5) as usize;
    if log_lines > 40 {
        log_lines = 40;
    }
    row(
        &mut buf,
        &format!("{ANSI_CYAN}Recent Logs:{ANSI_RESET}"),
        box_w,
    );
    let logs = tui_log_slice(log_lines);
    for line in &logs {
        row(&mut buf, &truncate_visible(line, inner), box_w);
    }
    for _ in logs.len()..log_lines {
        row(&mut buf, "", box_w);
    }

    buf.push_str(&bot);
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

fn row(buf: &mut String, content: &str, width: usize) {
    buf.push_str("│ ");
    buf.push_str(content);
    let pad = width.saturating_sub(4).saturating_sub(visible_len(content));
    if pad > 0 {
        buf.push_str(&" ".repeat(pad));
    }
    buf.push_str(" │\n");
}

fn terminal_size() -> (usize, usize) {
    let mut w = std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok());
    let mut h = std::env::var("LINES").ok().and_then(|v| v.parse().ok());
    if w.is_none() || h.is_none() {
        // GoWay's single-file build hardcodes 80×24; keep that as the floor and
        // allow COLUMNS/LINES overrides for better UX on real terminals.
        if w.is_none() {
            w = Some(80);
        }
        if h.is_none() {
            h = Some(24);
        }
    }
    (w.unwrap_or(80), h.unwrap_or(24))
}

/// GoWay `tuiRefreshLoop`: initial draw, then redraw at 10 Hz only when dirty.
pub fn spawn_refresh_loop(cfg: TuiConfig) {
    tokio::spawn(async move {
        draw_tui(&cfg);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if tui_state().dirty.swap(false, Ordering::Relaxed) {
                draw_tui(&cfg);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// tracing bridge
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct UxLayer {
    tui: bool,
    log_file: bool,
}

impl UxLayer {
    pub fn new(tui: bool, log_file: bool) -> Self {
        Self { tui, log_file }
    }
}

#[derive(Default)]
struct EventVisitor {
    message: String,
    extras: Vec<String>,
}

impl tracing::field::Visit for EventVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // `format_args!` Debug forwards to Display → no surrounding quotes.
            self.message = format!("{value:?}");
        } else {
            self.extras.push(format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }
}

impl<S> Layer<S> for UxLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        let mut text = visitor.message;
        if !visitor.extras.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&visitor.extras.join(" "));
        }
        if text.is_empty() {
            return;
        }

        if self.tui {
            let (color, tag) = match level {
                tracing::Level::TRACE => (ANSI_GREY, "TRACE"),
                tracing::Level::DEBUG => (ANSI_CYAN, "DEBUG"),
                tracing::Level::INFO => (ANSI_GREEN, "INFO "),
                tracing::Level::WARN => (ANSI_YELLOW, "WARN"),
                tracing::Level::ERROR => (ANSI_RED, "ERROR"),
            };
            add_tui_log(&format!(
                "{} {color}[{tag}]{ANSI_RESET} {text}",
                clock_hms()
            ));
        }

        if self.log_file && (level == tracing::Level::WARN || level == tracing::Level::ERROR) {
            log_file_add(&format!("[{level}] {text}"));
        }
    }
}

/// Returns true when stdout is a character device (enables terminal control
/// sequences without pulling in a TTY crate).
pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_len_skips_ansi() {
        assert_eq!(visible_len("\x1b[32mHi\x1b[0m"), 2);
    }

    #[test]
    fn civil_from_days_known_epochs() {
        // 1970-01-01 is day 0.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-03-01 (leap year check): days = 11017
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
    }

    #[test]
    fn format_bytes_rounds() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(2048), "2.00 KB");
    }

    #[test]
    fn log_file_ring_keeps_last_entries() {
        let dir = std::env::temp_dir().join(format!("rushway-logfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.log");
        configure_log_file(path.clone());
        for i in 0..15 {
            log_file_add(&format!("[ERROR] entry-{i}"));
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), LOG_FILE_RING_CAP);
        assert!(lines[0].contains("entry-5"));
        assert!(lines[9].contains("entry-14"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_conn_guard_tracks_active() {
        let before = crate::stats::active_conns();
        {
            let _g = crate::stats::ConnGuard::new();
            assert_eq!(crate::stats::active_conns(), before + 1);
        }
        assert_eq!(crate::stats::active_conns(), before);
    }

    #[test]
    fn stats_add_bytes_accumulates() {
        let (u0, d0) = crate::stats::totals();
        crate::stats::add_bytes(100, 200);
        let (u1, d1) = crate::stats::totals();
        assert_eq!(u1 - u0, 100);
        assert_eq!(d1 - d0, 200);
    }
}
