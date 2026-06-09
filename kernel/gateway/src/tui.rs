#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Terminal UI for the aman gateway — two-column layout with real-time logs
//! on the left and interactive plugin capability approval prompts on the right.
//!
//! Built on [ratatui](https://crates.io/crates/ratatui) with a crossterm backend.
//! Activated via `aman --tui`.

use std::collections::VecDeque;
use std::io::{self, stdout};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tracing::Level;
use tracing_subscriber::Layer;

use crate::runtime::AgentRuntime;
use i18n::Translator;
use kernel::AmanResult;

// ── Log buffer ────────────────────────────────────────────────────────────

const LOG_CAPACITY: usize = 2048;

/// Thread-safe ring buffer for tracing log lines consumed by the TUI.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: StdMutex<VecDeque<LogLine>>,
}

/// A single formatted log line with its level for colouring.
#[derive(Debug, Clone)]
struct LogLine {
    level: Level,
    text: String,
}

impl LogBuffer {
    fn push(&self, level: Level, text: String) {
        let mut guard = self.lines.lock().unwrap();
        if guard.len() >= LOG_CAPACITY {
            guard.pop_front();
        }
        guard.push_back(LogLine { level, text });
    }

    fn snapshot(&self) -> Vec<LogLine> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }
}

// ── Tracing layer that feeds the log buffer ───────────────────────────────

/// A `tracing_subscriber::Layer` that duplicates formatted events into a
/// shared [`LogBuffer`] for the TUI, while still forwarding them downstream
/// for file / stdout output.
pub struct TuiLogLayer {
    buffer: Arc<LogBuffer>,
}

impl TuiLogLayer {
    pub fn new(buffer: Arc<LogBuffer>) -> Self {
        Self { buffer }
    }
}

impl<S> Layer<S> for TuiLogLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = TuiLogVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        let text = format!(
            "[{h:02}:{m:02}:{s:02}] {} {}",
            meta.target(),
            visitor.message,
        );
        self.buffer.push(*meta.level(), text);
    }
}

/// Minimal visitor that collects the `message` field from a tracing event.
#[derive(Default)]
struct TuiLogVisitor {
    message: String,
}

impl tracing::field::Visit for TuiLogVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        }
    }
}

// ── TUI application state ─────────────────────────────────────────────────

/// Which panel currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Logs,
    Approvals,
}

/// A single pending plugin approval shown in the right panel.
#[derive(Debug, Clone)]
struct PendingItem {
    plugin_name: String,
    version: String,
    summary: Vec<String>,
}

struct TuiState {
    log_buffer: Arc<LogBuffer>,
    runtime: Arc<AgentRuntime>,
    /// Translator for i18n.
    translator: Translator,
    /// Log lines captured since last render.
    log_lines: Vec<LogLine>,
    /// Pending approvals snapshot, refreshed periodically.
    pending: Vec<PendingItem>,
    /// Currently selected item in the approvals list (None if list empty).
    selected: Option<usize>,
    /// Which panel has keyboard focus.
    focus: Focus,
    /// Log scroll offset (0 = newest at bottom).
    log_scroll: usize,
    /// When we last refreshed the pending list.
    last_refresh: Instant,
}

impl TuiState {
    fn new(log_buffer: Arc<LogBuffer>, runtime: Arc<AgentRuntime>) -> Self {
        let locale = runtime.locale();
        let translator = Translator::new(locale);
        Self {
            log_buffer,
            runtime,
            translator,
            log_lines: Vec::new(),
            pending: Vec::new(),
            selected: None,
            focus: Focus::Logs,
            log_scroll: 0,
            last_refresh: Instant::now(),
        }
    }

    /// Pull fresh log lines from the buffer.
    fn refresh_logs(&mut self) {
        self.log_lines = self.log_buffer.snapshot();
    }

    /// Pull fresh pending approvals from the runtime.
    fn refresh_pending(&mut self) {
        let list = pollster::block_on(self.runtime.pending_plugin_approvals_list());
        self.pending = list
            .into_iter()
            .map(|p| PendingItem {
                plugin_name: p.plugin_name,
                version: p.version,
                summary: p.capabilities_summary,
            })
            .collect();
        // Keep selection within bounds.
        if self.pending.is_empty() {
            self.selected = None;
        } else if self.selected.unwrap_or(0) >= self.pending.len() {
            self.selected = Some(self.pending.len().saturating_sub(1));
        } else if self.selected.is_none() {
            self.selected = Some(0);
        }
    }

    fn select_next(&mut self) {
        if let Some(i) = self.selected {
            if i + 1 < self.pending.len() {
                self.selected = Some(i + 1);
            }
        }
    }

    fn select_prev(&mut self) {
        if let Some(i) = self.selected {
            self.selected = Some(i.saturating_sub(1));
        }
    }

    fn scroll_logs_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_add(1);
    }

    fn scroll_logs_down(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(1);
    }

    /// Approve the currently selected pending plugin.
    fn approve_selected(&mut self) -> AmanResult<()> {
        let err_msg = self.translator.translate("tui.error.no_plugin_selected").to_owned();
        let idx = self.selected.ok_or_else(|| kernel::Error::Unrecoverable {
            message: err_msg.clone(),
        })?;
        let item = &self.pending[idx];
        self.runtime.resolve_plugin_approval_sync(&item.plugin_name, true)?;
        self.refresh_pending();
        Ok(())
    }

    /// Deny the currently selected pending plugin.
    fn deny_selected(&mut self) -> AmanResult<()> {
        let err_msg = self.translator.translate("tui.error.no_plugin_selected").to_owned();
        let idx = self.selected.ok_or_else(|| kernel::Error::Unrecoverable {
            message: err_msg.clone(),
        })?;
        let item = &self.pending[idx];
        self.runtime.resolve_plugin_approval_sync(&item.plugin_name, false)?;
        self.refresh_pending();
        Ok(())
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────

fn render(terminal: &mut DefaultTerminal, state: &TuiState) -> io::Result<()> {
    terminal.draw(|frame: &mut Frame| {
        // Split into two columns: 70% logs | 30% approvals
        let main = frame.area();
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(main);

        render_log_panel(frame, columns[0], state);
        render_approval_panel(frame, columns[1], state);
        render_footer(frame, main, state);
    })?;
    Ok(())
}

fn render_log_panel(frame: &mut Frame, area: Rect, state: &TuiState) {
    let is_focused = state.focus == Focus::Logs;
    let border_style = if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::Gray)
    };

    let title = format!(
        " {} ({}) ",
        state.translator.translate("tui.logs.title"),
        state.translator.translate("tui.logs.switch_hint"),
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    // Show the most recent lines that fit in the area, respecting scroll.
    let visible_height = area.height.saturating_sub(2) as usize; // minus borders
    let total = state.log_lines.len();
    let skip = if total > visible_height {
        total.saturating_sub(visible_height).saturating_sub(state.log_scroll)
    } else {
        0
    };

    let lines: Vec<Line<'_>> = state
        .log_lines
        .iter()
        .skip(skip)
        .take(visible_height)
        .map(|ll| {
            let color = level_color(ll.level);
            // Truncate long lines to fit; but use wrap for readability.
            let text = &ll.text;
            Line::from(Span::styled(text, Style::default().fg(color)))
        })
        .collect();

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

fn render_approval_panel(frame: &mut Frame, area: Rect, state: &TuiState) {
    let is_focused = state.focus == Focus::Approvals;
    let border_style = if is_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Gray)
    };

    let title = state.translator.translate("tui.approvals.title");
    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(border_style);

    if state.pending.is_empty() {
        let no_pending = state.translator.translate("tui.approvals.no_pending");
        let text = Text::from(vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {no_pending}"),
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        let p = Paragraph::new(text).block(block);
        frame.render_widget(p, area);
        return;
    }

    // Build list items.
    let items: Vec<ListItem<'_>> = state
        .pending
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let is_selected = state.selected == Some(i);
            let prefix = if is_selected { "▶ " } else { "  " };
            let line = format!(
                "{}{} (v{})",
                prefix, item.plugin_name, item.version
            );
            let style = if is_selected && is_focused {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if is_selected {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::from(Line::from(Span::styled(line, style)))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    // Track selection for ratatui.
    let mut list_state = ListState::default().with_selected(state.selected);

    frame.render_stateful_widget(list, area, &mut list_state);

    // Render detail for selected item below the list.
    if let Some(idx) = state.selected {
        if let Some(item) = state.pending.get(idx) {
            // Calculate detail area below the list
            let detail_y = area.y + 2 + state.pending.len().min(20) as u16;
            if detail_y < area.bottom() {
                let detail_area = Rect {
                    x: area.x + 1,
                    y: detail_y,
                    width: area.width.saturating_sub(2),
                    height: (area.bottom() - detail_y).min(8),
                };
                let caps_label = state.translator.translate("tui.approvals.capabilities_for");
                let hint = state.translator.translate("tui.approvals.approve_deny_hint");
                let mut detail_lines: Vec<Line<'_>> = vec![
                    Line::from(Span::styled(
                        format!("{caps_label} {}:", item.plugin_name),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    )),
                ];
                for cap in &item.summary {
                    detail_lines.push(Line::from(Span::styled(
                        cap.clone(),
                        Style::default().fg(Color::White),
                    )));
                }
                detail_lines.push(Line::from(""));
                detail_lines.push(Line::from(Span::styled(
                    hint,
                    Style::default().fg(Color::DarkGray),
                )));
                let detail = Paragraph::new(Text::from(detail_lines));
                frame.render_widget(detail, detail_area);
            }
        }
    }
}

fn render_footer(frame: &mut Frame, area: Rect, state: &TuiState) {
    let footer_y = area.bottom().saturating_sub(1);
    let footer_area = Rect {
        x: area.x,
        y: footer_y,
        width: area.width,
        height: 1,
    };

    let t = &state.translator;

    let mut spans = vec![
        Span::styled(
            format!(" {} ", t.translate("tui.footer.tab")),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ),
        Span::styled(
            format!(" {} ", t.translate("tui.footer.switch_focus")),
            Style::default(),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
    ];

    if state.focus == Focus::Approvals && !state.pending.is_empty() {
        spans.extend(vec![
            Span::styled(
                " ↑↓ ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ),
            Span::styled(
                format!(" {} ", t.translate("tui.footer.navigate")),
                Style::default(),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(" {} ", t.translate("tui.footer.enter")),
                Style::default().fg(Color::Black).bg(Color::Green),
            ),
            Span::styled(
                format!(" {} ", t.translate("tui.footer.approve")),
                Style::default(),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!(" {} ", t.translate("tui.footer.d_key")),
                Style::default().fg(Color::Black).bg(Color::Red),
            ),
            Span::styled(
                format!(" {} ", t.translate("tui.footer.deny")),
                Style::default(),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        ]);
    } else if state.focus == Focus::Logs {
        spans.extend(vec![
            Span::styled(
                " ↑↓ ",
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
            Span::styled(
                format!(" {} ", t.translate("tui.footer.scroll")),
                Style::default(),
            ),
            Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        ]);
    }

    spans.push(Span::styled(
        format!(" {} ", t.translate("tui.footer.q_key")),
        Style::default().fg(Color::Black).bg(Color::Red),
    ));
    spans.push(Span::styled(
        format!(" {} ", t.translate("tui.footer.quit")),
        Style::default(),
    ));

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), footer_area);
}

fn level_color(level: Level) -> Color {
    match level {
        Level::ERROR => Color::Red,
        Level::WARN => Color::Yellow,
        Level::INFO => Color::Green,
        Level::DEBUG => Color::Blue,
        Level::TRACE => Color::DarkGray,
    }
}

// ── Event loop ────────────────────────────────────────────────────────────

/// Run the TUI on the current thread. Blocks until the user presses `q`.
///
/// The TUI renders a two-column layout:
///  - Left (70%): scrolling real-time logs captured from the tracing layer.
///  - Right (30%): list of plugins awaiting capability approval.
///
/// `runtime` must already be built and started (server running in background).
pub fn run_tui(
    log_buffer: Arc<LogBuffer>,
    runtime: Arc<AgentRuntime>,
) -> io::Result<()> {
    // Set up the terminal.
    let mut stdout = stdout();
    terminal::enable_raw_mode()?;
    stdout.execute(EnterAlternateScreen)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut state = TuiState::new(log_buffer, runtime);
    state.refresh_logs();
    state.refresh_pending();

    let result = event_loop(&mut terminal, &mut state);

    // Restore terminal.
    terminal::disable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(LeaveAlternateScreen)?;

    result
}

fn event_loop(terminal: &mut DefaultTerminal, state: &mut TuiState) -> io::Result<()> {
    let refresh_interval = Duration::from_millis(200);
    let pending_refresh_interval = Duration::from_secs(2);

    loop {
        // Draw current frame.
        render(terminal, state)?;

        // Poll for input with timeout (non-blocking refresh).
        if event::poll(refresh_interval)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        KeyCode::Char('q')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            return Ok(());
                        }
                        KeyCode::Char('c')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            return Ok(());
                        }
                        KeyCode::Tab => {
                            state.focus = match state.focus {
                                Focus::Logs => Focus::Approvals,
                                Focus::Approvals => Focus::Logs,
                            };
                        }
                        KeyCode::Up => match state.focus {
                            Focus::Logs => state.scroll_logs_up(),
                            Focus::Approvals => state.select_prev(),
                        },
                        KeyCode::Down => match state.focus {
                            Focus::Logs => state.scroll_logs_down(),
                            Focus::Approvals => state.select_next(),
                        },
                        KeyCode::Enter if state.focus == Focus::Approvals => {
                            if let Err(e) = state.approve_selected() {
                                let err_msg = state.translator.translate("tui.error.approve_failed");
                                state.log_buffer.push(
                                    Level::ERROR,
                                    format!("{err_msg}: {e}"),
                                );
                            }
                        }
                        KeyCode::Char('d') if state.focus == Focus::Approvals => {
                            if let Err(e) = state.deny_selected() {
                                let err_msg = state.translator.translate("tui.error.deny_failed");
                                state.log_buffer.push(
                                    Level::ERROR,
                                    format!("{err_msg}: {e}"),
                                );
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Periodic refresh.
        state.refresh_logs();
        let now = Instant::now();
        if now.duration_since(state.last_refresh) >= pending_refresh_interval {
            state.refresh_pending();
            state.last_refresh = now;
        }
    }
}
