//! Project-aware terminal dashboard.
//!
//! The dashboard owns no trading logic. It starts a bot runtime, reads the
//! operator snapshot, and sends only explicit stop requests back to the
//! runtime. Paper/live actions remain configuration-controlled.

use crate::bot::{self, BotKind, BotRuntime};
use crate::config::AppConfig;
use crate::operator::{
    ActivityItem, ActivityLevel, ConnectionStatus, OperatorSnapshot, PositionView, TradeView,
    TradingMode,
};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
    },
    Terminal,
};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::io;
use std::time::Duration;

const BOTS: &[BotKind] = &[BotKind::CopyTrading];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Launcher,
    Overview,
    Positions,
    Activity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PositionSort {
    Contract,
    Pnl,
    Exposure,
    Freshness,
}

impl PositionSort {
    fn label(self) -> &'static str {
        match self {
            Self::Contract => "CONTRACT",
            Self::Pnl => "P&L",
            Self::Exposure => "EXPOSURE",
            Self::Freshness => "FRESHNESS",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Contract => Self::Pnl,
            Self::Pnl => Self::Exposure,
            Self::Exposure => Self::Freshness,
            Self::Freshness => Self::Contract,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Theme {
    dark: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self { dark: true }
    }
}

impl Theme {
    fn background(self) -> Color {
        if self.dark {
            Color::Rgb(12, 16, 22)
        } else {
            Color::Rgb(244, 241, 234)
        }
    }

    fn foreground(self) -> Color {
        if self.dark {
            Color::White
        } else {
            Color::Rgb(30, 34, 39)
        }
    }

    fn muted(self) -> Color {
        if self.dark {
            Color::DarkGray
        } else {
            Color::Rgb(100, 100, 95)
        }
    }

    fn panel(self) -> Color {
        if self.dark {
            Color::Rgb(20, 27, 35)
        } else {
            Color::Rgb(232, 228, 218)
        }
    }

    fn selection(self) -> Color {
        if self.dark {
            Color::Rgb(25, 38, 50)
        } else {
            Color::Rgb(210, 223, 228)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Positions,
    Activity,
}

struct UiState {
    view: View,
    focus: Focus,
    bot_list: ListState,
    position_table: TableState,
    position_selected: usize,
    activity_scroll: usize,
    activity_paused: bool,
    paused_activity: Option<VecDeque<ActivityItem>>,
    paused_trades: Option<VecDeque<TradeView>>,
    search: String,
    searching: bool,
    position_sort: PositionSort,
    theme: Theme,
    quitting: bool,
    runtime: Option<BotRuntime>,
    snapshot: OperatorSnapshot,
    notice: Option<String>,
}

impl UiState {
    fn new() -> Self {
        let mut bot_list = ListState::default();
        bot_list.select(Some(0));
        Self {
            view: View::Launcher,
            focus: Focus::Positions,
            bot_list,
            position_table: TableState::default(),
            position_selected: 0,
            activity_scroll: 0,
            activity_paused: false,
            paused_activity: None,
            paused_trades: None,
            search: String::new(),
            searching: false,
            position_sort: PositionSort::Contract,
            theme: Theme::default(),
            quitting: false,
            runtime: None,
            snapshot: OperatorSnapshot::default(),
            notice: None,
        }
    }
}

pub async fn run(cfg: AppConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = ui_loop(&mut terminal, cfg).await;
    let cleanup = cleanup_terminal(&mut terminal);
    result.and(cleanup)
}

fn cleanup_terminal<B: Backend + io::Write>(terminal: &mut Terminal<B>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn ui_loop<B: Backend + io::Write>(terminal: &mut Terminal<B>, cfg: AppConfig) -> Result<()> {
    let mut app = UiState::new();

    loop {
        refresh_runtime(&mut app).await;
        terminal.draw(|frame| draw(frame, &cfg, &mut app))?;
        if app.quitting
            && app
                .runtime
                .as_ref()
                .map(|runtime| !runtime.is_running())
                .unwrap_or(true)
        {
            return Ok(());
        }

        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if handle_key(&mut app, &cfg, key).await? {
            return Ok(());
        }
    }
}

async fn refresh_runtime(app: &mut UiState) {
    let finished = app
        .runtime
        .as_ref()
        .map(BotRuntime::is_finished)
        .unwrap_or(false);
    if finished {
        if let Some(runtime) = app.runtime.as_mut() {
            let result = runtime.wait().await;
            app.snapshot = runtime.bus.snapshot();
            if let Err(error) = result {
                app.notice = Some(format!("bot stopped with error: {error:#}"));
            } else if !app.quitting {
                app.notice = Some("stopped · press r to resume".into());
            }
        }
    }
    if let Some(runtime) = app.runtime.as_ref() {
        app.snapshot = runtime.bus.snapshot();
    }
    let visible_len = visible_positions(app).len();
    if app.position_selected >= visible_len {
        app.position_selected = visible_len.saturating_sub(1);
    }
    app.position_table.select(
        (visible_len > 0).then_some(app.position_selected.min(visible_len.saturating_sub(1))),
    );
}

async fn handle_key(app: &mut UiState, cfg: &AppConfig, key: KeyEvent) -> Result<bool> {
    let ctrl_c = key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);

    if app.searching {
        match key.code {
            KeyCode::Esc => {
                app.search.clear();
                app.searching = false;
            }
            KeyCode::Enter => app.searching = false,
            KeyCode::Backspace => {
                app.search.pop();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.search.push(ch);
            }
            _ => {}
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') => {
            request_stop(app, true);
            if app.runtime.is_none() {
                return Ok(true);
            }
        }
        KeyCode::Char('c') if ctrl_c => {
            request_stop(app, true);
            if app.runtime.is_none() {
                return Ok(true);
            }
        }
        KeyCode::Char('1') if app.runtime.is_some() => app.view = View::Overview,
        KeyCode::Char('2') if app.runtime.is_some() => app.view = View::Activity,
        KeyCode::Char('3') if app.runtime.is_some() => app.view = View::Positions,
        KeyCode::Char('s') if app.runtime.is_some() => {
            request_stop(app, false);
        }
        KeyCode::Char('r') if app.runtime.is_some() => {
            let can_resume = app
                .runtime
                .as_ref()
                .map(|runtime| !runtime.is_running() && runtime.is_finished())
                .unwrap_or(false);
            if can_resume {
                if let Some(runtime) = app.runtime.as_mut() {
                    if runtime.resume() {
                        app.quitting = false;
                        app.notice = Some("resuming with preserved session state".into());
                    }
                }
            } else {
                app.notice = Some("resume is available after the worker stops".into());
            }
        }
        KeyCode::Char('p') if app.runtime.is_some() => toggle_activity_pause(app),
        KeyCode::Char('/') if matches!(app.view, View::Overview | View::Positions) => {
            app.searching = true;
            app.notice = Some("search contract: type text, Enter accepts, Esc clears input".into());
        }
        KeyCode::Char('o') if matches!(app.view, View::Overview | View::Positions) => {
            app.position_sort = app.position_sort.next();
            app.position_selected = 0;
        }
        KeyCode::Char('t') => app.theme.dark = !app.theme.dark,
        KeyCode::Tab if app.view == View::Overview => {
            app.focus = match app.focus {
                Focus::Positions => Focus::Activity,
                Focus::Activity => Focus::Positions,
            };
        }
        KeyCode::Enter if app.view == View::Launcher && app.runtime.is_none() => {
            let selected = app.bot_list.selected().unwrap_or(0);
            app.runtime = Some(bot::spawn(BOTS[selected], cfg.clone()));
            app.view = View::Overview;
            app.notice = Some(format!("starting {}", BOTS[selected].label()));
        }
        KeyCode::Esc if app.runtime.is_none() => app.view = View::Launcher,
        KeyCode::Up | KeyCode::Char('k') => move_selection(app, -1),
        KeyCode::Down | KeyCode::Char('j') => move_selection(app, 1),
        _ => {}
    }
    Ok(false)
}

fn request_stop(app: &mut UiState, quitting: bool) {
    app.quitting |= quitting;
    if let Some(runtime) = app.runtime.as_ref() {
        runtime.request_stop();
        app.notice = Some(if quitting {
            "quit requested; waiting for workers without blocking the UI".into()
        } else {
            "stop requested; dashboard remains available".into()
        });
    }
}

fn toggle_activity_pause(app: &mut UiState) {
    if app.activity_paused {
        app.activity_paused = false;
        app.paused_activity = None;
        app.paused_trades = None;
    } else {
        app.activity_paused = true;
        app.paused_activity = Some(app.snapshot.activity.clone());
        app.paused_trades = Some(app.snapshot.recent_trades.clone());
    }
}

fn move_selection(app: &mut UiState, delta: i32) {
    match app.view {
        View::Launcher => {
            let current = app.bot_list.selected().unwrap_or(0) as i32;
            let next = (current + delta).clamp(0, BOTS.len().saturating_sub(1) as i32) as usize;
            app.bot_list.select(Some(next));
        }
        View::Overview if app.focus == Focus::Positions => {
            let max = visible_positions(app).len().saturating_sub(1);
            let current = app.position_selected as i32;
            app.position_selected = (current + delta).clamp(0, max as i32) as usize;
            let has_positions = !visible_positions(app).is_empty();
            app.position_table
                .select(has_positions.then_some(app.position_selected));
        }
        View::Positions => {
            let max = visible_positions(app).len().saturating_sub(1);
            let current = app.position_selected as i32;
            app.position_selected = (current + delta).clamp(0, max as i32) as usize;
            let has_positions = !visible_positions(app).is_empty();
            app.position_table
                .select(has_positions.then_some(app.position_selected));
        }
        View::Overview | View::Activity => {
            let max = activity_max_scroll(app);
            let current = app.activity_scroll as i32;
            app.activity_scroll = (current - delta).clamp(0, max as i32) as usize;
        }
    }
}

fn activity_max_scroll(app: &UiState) -> usize {
    let count = activity_items(app).len();
    count.saturating_sub(8)
}

fn activity_items(app: &UiState) -> Vec<ActivityItem> {
    app.paused_activity
        .as_ref()
        .unwrap_or(&app.snapshot.activity)
        .iter()
        .cloned()
        .collect()
}

fn draw(frame: &mut Frame, cfg: &AppConfig, app: &mut UiState) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(app.theme.background())),
        area,
    );
    match app.view {
        View::Launcher => draw_launcher(frame, area, cfg, app),
        View::Overview => draw_dashboard(frame, area, cfg, app, false),
        View::Positions => draw_dashboard(frame, area, cfg, app, false),
        View::Activity => draw_dashboard(frame, area, cfg, app, true),
    }
}

fn draw_launcher(frame: &mut Frame, area: Rect, cfg: &AppConfig, app: &mut UiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(
        frame,
        chunks[0],
        "POLYCRAB · POLYMARKET",
        cfg,
        None,
        None,
        None,
    );

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);
    let items = BOTS.iter().map(|kind| {
        ListItem::new(Line::from(vec![
            Span::styled("READY ", Style::default().fg(Color::Green)),
            Span::raw(kind.label()),
        ]))
    });
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Strategies "))
        .highlight_style(
            Style::default()
                .bg(app.theme.selection())
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, body[0], &mut app.bot_list);

    let selected = BOTS[app.bot_list.selected().unwrap_or(0)];
    let detail = launcher_detail(selected, cfg);
    frame.render_widget(
        Paragraph::new(detail)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Run configuration "),
            )
            .wrap(Wrap { trim: true }),
        body[1],
    );
    render_footer(
        frame,
        chunks[2],
        "↑/↓ select   Enter start   q quit",
        app.notice.as_deref(),
    );
}

fn launcher_detail(kind: BotKind, cfg: &AppConfig) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            kind.label(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled("READY", Style::default().fg(Color::Green))),
        Line::from(""),
    ];
    lines.extend([
        Line::from(format!("mode             {}", mode_label(cfg))),
        Line::from(format!(
            "wallets          {} configured",
            cfg.bot.wallets_to_track.len()
        )),
        Line::from(format!(
            "active source     {}",
            cfg.bot
                .wallets_to_track
                .first()
                .map(|wallet| short_address(wallet))
                .unwrap_or_else(|| "none".into())
        )),
        Line::from(format!(
            "sizing            {:?} · ${:.2}–${:.2}",
            cfg.strategy.copy_strategy,
            cfg.strategy.min_order_size_usd,
            cfg.strategy.max_order_size_usd
        )),
        Line::from(format!(
            "risk              {:.0} shares / {} trigger",
            cfg.risk.large_trade_shares, cfg.risk.consecutive_trigger
        )),
        Line::from(format!(
            "TP/SL             {}",
            if cfg.tp_sl.enabled {
                "enabled"
            } else {
                "disabled"
            }
        )),
        Line::from("depth guard       unavailable"),
    ]);
    lines
}

fn draw_dashboard(
    frame: &mut Frame,
    area: Rect,
    cfg: &AppConfig,
    app: &mut UiState,
    activity_view: bool,
) {
    let header_status = Some(app.snapshot.connection);
    let risk = Some((app.snapshot.risk_tripped, risk_remaining(&app.snapshot)));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(
        frame,
        chunks[0],
        &format!("POLYCRAB · {}", app.snapshot.bot_name.to_uppercase()),
        cfg,
        header_status,
        risk,
        app.snapshot.started_at,
    );
    render_metrics(frame, chunks[1], &app.snapshot, app.theme);
    if activity_view {
        render_activity(frame, chunks[2], app);
    } else if app.view == View::Positions {
        render_positions(frame, chunks[2], app);
    } else {
        render_overview(frame, chunks[2], cfg, app);
    }
    let footer = if app.snapshot.bot_running {
        "1 overview   2 activity   3 positions   / search   o sort   p pause   s stop   t theme   q quit"
    } else {
        "1 overview   2 activity   3 positions   r resume   t theme   q quit"
    };
    render_footer(frame, chunks[3], footer, app.notice.as_deref());
}

fn render_header(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    cfg: &AppConfig,
    connection: Option<ConnectionStatus>,
    risk: Option<(bool, Option<u64>)>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let mode = mode_label(cfg);
    let mode_color = if mode == "LIVE" {
        Color::Red
    } else {
        Color::Yellow
    };
    let elapsed = started_at
        .map(|started| format!("UP {}", format_duration(chrono::Utc::now() - started)))
        .unwrap_or_else(|| "UP —".into());
    let mut spans = vec![
        Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            mode,
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(status) = connection {
        spans.extend([
            Span::raw("  WS "),
            Span::styled(
                status.label(),
                Style::default().fg(connection_color(status)),
            ),
        ]);
    }
    spans.push(Span::styled(
        format!("  {elapsed}"),
        Style::default().fg(Color::DarkGray),
    ));
    if let Some((tripped, remaining)) = risk {
        spans.push(Span::raw("  RISK "));
        spans.push(Span::styled(
            if tripped {
                format!(
                    "TRIPPED {}",
                    remaining.map(|s| format!("{s}s")).unwrap_or_default()
                )
            } else {
                "CLEAR".into()
            },
            Style::default().fg(if tripped { Color::Red } else { Color::Green }),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::BOTTOM)),
        area,
    );
}

fn render_metrics(frame: &mut Frame, area: Rect, snapshot: &OperatorSnapshot, theme: Theme) {
    let deployed: f64 = snapshot
        .positions
        .iter()
        .map(|position| position.usd_notional)
        .sum();
    let market_value: f64 = snapshot
        .positions
        .iter()
        .filter_map(|position| position.market_value_usd)
        .sum();
    let unrealized: f64 = snapshot
        .positions
        .iter()
        .filter_map(|position| position.pnl_usd)
        .sum();
    let c = &snapshot.counters;
    let first = metric_line(
        [
            ("OPEN", snapshot.positions.len().to_string()),
            ("DEPLOYED", format_money(deployed)),
            (
                "VALUE",
                if snapshot.positions.iter().any(|p| p.midpoint.is_some()) {
                    format_money(market_value)
                } else {
                    "—".into()
                },
            ),
            (
                "UNREAL",
                if snapshot.positions.iter().any(|p| p.pnl_usd.is_some()) {
                    format_signed_money(unrealized)
                } else {
                    "—".into()
                },
            ),
            ("OBSERVED", c.observed.to_string()),
        ],
        theme,
    );
    let second = metric_line(
        [
            ("SKIPPED", c.skipped.to_string()),
            (
                "REALIZED",
                if c.exits > 0 {
                    format_signed_money(snapshot.realized_pnl_usd)
                } else {
                    "—".into()
                },
            ),
            ("W/L", format!("{}/{}", c.wins, c.losses)),
            (
                "LATENCY",
                c.latency_total_ms
                    .checked_div(c.latency_samples)
                    .map(|value| format!("{value}ms"))
                    .unwrap_or_else(|| "—".into()),
            ),
            ("ERRORS", c.errors.to_string()),
            ("POST", c.post_responses.to_string()),
            (
                "BLOCK",
                snapshot
                    .last_block
                    .map(|block| block.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
        ],
        theme,
    );
    frame.render_widget(
        Paragraph::new(vec![first, second]).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Session metrics ")
                .style(Style::default().bg(theme.panel()).fg(theme.foreground())),
        ),
        area,
    );
}

fn metric_line<const N: usize>(metrics: [(&str, String); N], theme: Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (label, value)) in metrics.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            format!("{label:<9}"),
            Style::default().fg(theme.muted()),
        ));
        spans.push(Span::styled(value, Style::default().fg(theme.foreground())));
    }
    Line::from(spans)
}

fn render_overview(frame: &mut Frame, area: Rect, cfg: &AppConfig, app: &mut UiState) {
    let columns = if area.width >= 100 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area)
    };
    render_positions(frame, columns[0], app);
    render_health(frame, columns[1], cfg, app);
}

fn render_positions(frame: &mut Frame, area: Rect, app: &mut UiState) {
    let positions = visible_positions(app);
    let compact = area.width < 100;
    let header = if compact {
        Row::new(["CONTRACT", "OUTCOME", "MID", "P&L"])
    } else {
        Row::new([
            "CONTRACT", "OUTCOME", "EXPIRY", "ENTRY", "MID", "VALUE", "P&L",
        ])
    };
    let rows = positions.iter().map(|position| {
        if compact {
            let contract = format!("{} · {}", position.question, format_expiry(position.expiry));
            Row::new([
                Cell::from(truncate(&contract, 22)),
                Cell::from(position.outcome.as_deref().unwrap_or("—")),
                Cell::from(format_price(position.midpoint)),
                pnl_cell(position.pnl_usd, position.pnl_pct),
            ])
        } else {
            Row::new([
                Cell::from(truncate(&position.question, 28)),
                Cell::from(position.outcome.as_deref().unwrap_or("—")),
                Cell::from(format_expiry(position.expiry)),
                Cell::from(format_price(Some(position.entry_price))),
                Cell::from(format_price(position.midpoint)),
                Cell::from(
                    position
                        .market_value_usd
                        .map(format_money)
                        .unwrap_or_else(|| "—".into()),
                ),
                pnl_cell(position.pnl_usd, position.pnl_pct),
            ])
        }
    });
    let widths = if compact {
        vec![
            Constraint::Min(14),
            Constraint::Length(6),
            Constraint::Length(8),
            Constraint::Length(10),
        ]
    } else {
        vec![
            Constraint::Min(24),
            Constraint::Length(6),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(11),
            Constraint::Length(17),
        ]
    };
    let mode = if app.snapshot.mode == TradingMode::Simulated {
        "simulated / not account positions"
    } else {
        "session state"
    };
    let title = format!(
        " Positions · {} · sort {}{} ",
        mode,
        app.position_sort.label(),
        if app.search.is_empty() {
            String::new()
        } else {
            format!(" · filter {}", app.search)
        }
    );
    let table = Table::new(rows, widths)
        .header(
            header.style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
        .highlight_style(Style::default().bg(app.theme.selection()))
        .block(
            Block::default().borders(Borders::ALL).title(title).style(
                Style::default()
                    .bg(app.theme.panel())
                    .fg(app.theme.foreground()),
            ),
        );
    frame.render_stateful_widget(table, area, &mut app.position_table);
}

fn visible_positions(app: &UiState) -> Vec<PositionView> {
    let query = app.search.to_ascii_lowercase();
    let mut positions: Vec<_> = app
        .snapshot
        .positions
        .iter()
        .filter(|position| {
            query.is_empty()
                || position.question.to_ascii_lowercase().contains(&query)
                || position.slug.to_ascii_lowercase().contains(&query)
                || position.token_id.to_ascii_lowercase().contains(&query)
                || position
                    .outcome
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains(&query)
                || position
                    .category
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .contains(&query)
                || position
                    .tags
                    .iter()
                    .any(|tag| tag.to_ascii_lowercase().contains(&query))
        })
        .cloned()
        .collect();
    match app.position_sort {
        PositionSort::Contract => positions.sort_by(|a, b| a.question.cmp(&b.question)),
        PositionSort::Pnl => positions.sort_by(|a, b| {
            b.pnl_usd
                .unwrap_or(f64::NEG_INFINITY)
                .partial_cmp(&a.pnl_usd.unwrap_or(f64::NEG_INFINITY))
                .unwrap_or(Ordering::Equal)
        }),
        PositionSort::Exposure => positions.sort_by(|a, b| {
            b.usd_notional
                .partial_cmp(&a.usd_notional)
                .unwrap_or(Ordering::Equal)
        }),
        PositionSort::Freshness => {
            positions.sort_by_key(|position| std::cmp::Reverse(position.quoted_at))
        }
    }
    positions
}

fn render_health(frame: &mut Frame, area: Rect, cfg: &AppConfig, app: &UiState) {
    let active_wallet = app
        .snapshot
        .active_wallet
        .as_deref()
        .map(short_address)
        .unwrap_or_else(|| "—".into());
    let connection = app.snapshot.connection;
    let risk = if app.snapshot.risk_tripped {
        format!(
            "TRIPPED {}",
            risk_remaining(&app.snapshot)
                .map(|secs| format!("({secs}s)"))
                .unwrap_or_default()
        )
    } else {
        "CLEAR".into()
    };
    let cap_util = cap_utilization(cfg, app);
    let lines = vec![
        Line::from(vec![
            Span::styled("MODE       ", Style::default().fg(Color::DarkGray)),
            Span::styled(mode_label(cfg), Style::default().fg(mode_color(cfg))),
        ]),
        Line::from(vec![
            Span::styled("POLYGON WS  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                connection.label(),
                Style::default().fg(connection_color(connection)),
            ),
        ]),
        Line::from(format!("SOURCE      {active_wallet}")),
        Line::from(format!(
            "WALLETS     {} configured",
            app.snapshot.configured_wallets
        )),
        Line::from(format!("RISK        {risk}")),
        Line::from(format!(
            "LIMIT       {:.0} shares / {} large trades",
            cfg.risk.large_trade_shares, cfg.risk.consecutive_trigger
        )),
        Line::from(format!("CAP UTIL    {cap_util}")),
        Line::from(format!(
            "FEES        {} bps configured / actual unavailable",
            cfg.trading.fee_rate_bps
        )),
        Line::from("DEPTH       UNAVAILABLE"),
        Line::from("ALLOCATED   UNAVAILABLE (no account balance)"),
        Line::from("AVAILABLE   UNAVAILABLE (no account balance)"),
        Line::from(if app.snapshot.counters.exits > 0 {
            format!(
                "REALIZED    {}",
                format_signed_money(app.snapshot.realized_pnl_usd)
            )
        } else {
            "REALIZED    UNAVAILABLE (no closed fills)".into()
        }),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Health & limits ")
                    .style(
                        Style::default()
                            .bg(app.theme.panel())
                            .fg(app.theme.foreground()),
                    ),
            )
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn cap_utilization(cfg: &AppConfig, app: &UiState) -> String {
    let mut highest: Option<(f64, String)> = None;
    for (category, cap) in &cfg.filters.per_category_max_open_usd {
        if *cap <= 0.0 {
            continue;
        }
        let current: f64 = app
            .snapshot
            .positions
            .iter()
            .filter(|position| {
                position
                    .category
                    .as_deref()
                    .map(|value| value.eq_ignore_ascii_case(category))
                    .unwrap_or(false)
            })
            .map(|position| position.usd_notional)
            .sum();
        let candidate = (current / cap, category.clone());
        if highest
            .as_ref()
            .map(|(ratio, _)| candidate.0 > *ratio)
            .unwrap_or(true)
        {
            highest = Some(candidate);
        }
    }
    for (tag, cap) in &cfg.filters.per_tag_max_open_usd {
        if *cap <= 0.0 {
            continue;
        }
        let current: f64 = app
            .snapshot
            .positions
            .iter()
            .filter(|position| {
                position
                    .tags
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(tag))
            })
            .map(|position| position.usd_notional)
            .sum();
        let candidate = (current / cap, format!("tag:{tag}"));
        if highest
            .as_ref()
            .map(|(ratio, _)| candidate.0 > *ratio)
            .unwrap_or(true)
        {
            highest = Some(candidate);
        }
    }
    highest
        .map(|(ratio, name)| format!("{:.0}% ({name})", ratio * 100.0))
        .unwrap_or_else(|| "UNAVAILABLE (no configured cap)".into())
}

fn render_activity(frame: &mut Frame, area: Rect, app: &UiState) {
    if area.height >= 12 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area);
        render_trade_feed(frame, chunks[0], app);
        render_activity_log(frame, chunks[1], app);
    } else {
        render_trade_feed(frame, area, app);
    }
}

fn render_trade_feed(frame: &mut Frame, area: Rect, app: &UiState) {
    let trades = app
        .paused_trades
        .as_ref()
        .unwrap_or(&app.snapshot.recent_trades);
    let visible = area.height.saturating_sub(3) as usize;
    let start = trades.len().saturating_sub(visible);
    let rows = trades.iter().skip(start).map(|trade| {
        let copy = match (
            trade.copied_side,
            trade.copied_shares,
            trade.copied_usd_notional,
            trade.copy_ratio,
        ) {
            (Some(side), Some(shares), Some(usd), Some(ratio)) => {
                format!("{} {shares:.1}sh ${usd:.2} ({ratio:.0}%)", side_text(side))
            }
            (Some(side), Some(shares), Some(usd), None) => {
                format!("{} {shares:.1}sh ${usd:.2}", side_text(side))
            }
            (None, Some(shares), Some(usd), _) => format!("{shares:.1}sh ${usd:.2}"),
            (_, None, Some(usd), _) => format!("${usd:.2}"),
            _ => "—".into(),
        };
        let contract = trade
            .outcome
            .as_ref()
            .map(|outcome| format!("{} [{outcome}]", trade.market))
            .unwrap_or_else(|| trade.market.clone());
        let contract = match trade.expiry {
            Some(expiry) => format!("{contract} · {}", format_expiry(Some(expiry))),
            None => contract,
        };
        let source = format!(
            "{} {} {:.1}sh ${:.2}",
            side_text(trade.source_side),
            short_address(&trade.source_wallet),
            trade.source_shares,
            trade.source_usd_notional
        );
        let result = trade
            .reason
            .as_ref()
            .map(|reason| format!("{} · {reason}", trade.result))
            .unwrap_or_else(|| trade.result.clone());
        Row::new([
            Cell::from(trade.at.format("%H:%M:%S").to_string()),
            Cell::from(truncate(&contract, 32)),
            Cell::from(truncate(&source, 22)),
            Cell::from(copy),
            Cell::from(format!("{result} · {}ms", trade.latency_ms)),
        ])
    });
    let title = if app.activity_paused {
        " Trades · PAUSED (engine continues) "
    } else {
        " Trades · source → copy · execution reason "
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Min(20),
            Constraint::Length(22),
            Constraint::Length(18),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(["TIME", "CONTRACT", "SOURCE", "COPY", "RESULT"]).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(
        Block::default().borders(Borders::ALL).title(title).style(
            Style::default()
                .bg(app.theme.panel())
                .fg(app.theme.foreground()),
        ),
    );
    frame.render_widget(table, area);
}

fn render_activity_log(frame: &mut Frame, area: Rect, app: &UiState) {
    let items = activity_items(app);
    let visible = area.height.saturating_sub(3) as usize;
    let end = items
        .len()
        .saturating_sub(app.activity_scroll)
        .min(items.len());
    let start = end.saturating_sub(visible);
    let rows = items[start..end].iter().map(|item| {
        Row::new([
            Cell::from(item.at.format("%H:%M:%S").to_string()),
            Cell::from(Span::styled(item.kind.clone(), activity_style(item.level))),
            Cell::from(item.detail.clone()),
        ])
    });
    let title = if app.activity_paused {
        " Activity · PAUSED (engine continues) "
    } else {
        " Activity "
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(["TIME", "TYPE", "DETAIL"]).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
    .block(
        Block::default().borders(Borders::ALL).title(title).style(
            Style::default()
                .bg(app.theme.panel())
                .fg(app.theme.foreground()),
        ),
    );
    frame.render_widget(table, area);
}

fn render_footer(frame: &mut Frame, area: Rect, text: &str, notice: Option<&str>) {
    let content = notice
        .map(|notice| vec![Line::from(text), Line::from(notice)])
        .unwrap_or_else(|| vec![Line::from(text)]);
    frame.render_widget(
        Paragraph::new(content)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP)),
        area,
    );
}

fn pnl_cell(usd: Option<f64>, pct: Option<f64>) -> Cell<'static> {
    match (usd, pct) {
        (Some(usd), Some(pct)) => Cell::from(Span::styled(
            format!("{usd:+.2} / {pct:+.1}%"),
            Style::default().fg(if usd >= 0.0 { Color::Green } else { Color::Red }),
        )),
        _ => Cell::from(Span::styled("—", Style::default().fg(Color::DarkGray))),
    }
}

fn side_text(side: crate::models::Side) -> &'static str {
    match side {
        crate::models::Side::Buy => "BUY",
        crate::models::Side::Sell => "SELL",
    }
}

fn mode_label(cfg: &AppConfig) -> &'static str {
    if cfg.live_trading_allowed() {
        "LIVE"
    } else {
        "SIMULATED"
    }
}

fn mode_color(cfg: &AppConfig) -> Color {
    if cfg.live_trading_allowed() {
        Color::Red
    } else {
        Color::Yellow
    }
}

fn connection_color(status: ConnectionStatus) -> Color {
    match status {
        ConnectionStatus::Subscribed | ConnectionStatus::Connected => Color::Green,
        ConnectionStatus::Error => Color::Red,
        ConnectionStatus::Reconnecting => Color::Yellow,
        ConnectionStatus::Connecting => Color::Yellow,
        ConnectionStatus::Stopped => Color::DarkGray,
    }
}

fn activity_style(level: ActivityLevel) -> Style {
    Style::default().fg(match level {
        ActivityLevel::Info => Color::Gray,
        ActivityLevel::Success => Color::Green,
        ActivityLevel::Warning => Color::Yellow,
        ActivityLevel::Error => Color::Red,
    })
}

fn risk_remaining(snapshot: &OperatorSnapshot) -> Option<u64> {
    snapshot
        .risk_until
        .and_then(|until| until.checked_duration_since(std::time::Instant::now()))
        .map(|duration| duration.as_secs())
}

fn format_price(price: Option<f64>) -> String {
    price
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "—".into())
}

fn format_expiry(expiry: Option<chrono::DateTime<chrono::Utc>>) -> String {
    expiry
        .map(|date| date.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

fn format_money(amount: f64) -> String {
    format!("${amount:.2}")
}

fn format_signed_money(amount: f64) -> String {
    format!("${amount:+.2}")
}

fn format_duration(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().max(0);
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, (seconds / 60) % 60)
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn short_address(address: &str) -> String {
    if address.len() > 12 {
        format!("{}…{}", &address[..6], &address[address.len() - 4..])
    } else {
        address.to_string()
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>()
        + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn config() -> AppConfig {
        serde_json::from_str(include_str!("../../config.json")).unwrap()
    }

    fn position(question: &str, pnl_usd: f64) -> PositionView {
        PositionView {
            token_id: question.into(),
            slug: question.to_ascii_lowercase().replace(' ', "-"),
            question: question.into(),
            outcome: Some("YES".into()),
            expiry: None,
            category: Some("Crypto".into()),
            tags: vec!["solana".into()],
            side: crate::models::Side::Buy,
            entry_price: 0.5,
            midpoint: Some(0.6),
            market_value_usd: Some(12.0),
            shares: 20.0,
            usd_notional: 10.0,
            pnl_pct: Some(pnl_usd * 10.0),
            pnl_usd: Some(pnl_usd),
            quoted_at: Some(chrono::Utc::now()),
            opened_at: chrono::Utc::now(),
            take_profit_pct: 50.0,
            stop_loss_pct: 30.0,
            source_maker: None,
            source_trade_hash: None,
            source_shares: None,
            source_usd_notional: None,
        }
    }

    #[test]
    fn missing_values_are_explicit() {
        assert_eq!(format_price(None), "—");
        assert_eq!(format_duration(chrono::Duration::seconds(3661)), "1h 01m");
        assert_eq!(truncate("abcdefgh", 5), "abcd…");
    }

    #[test]
    fn launcher_and_dashboard_render_in_small_terminal() {
        let cfg = config();
        let mut terminal = Terminal::new(TestBackend::new(64, 20)).unwrap();
        let mut app = UiState::new();
        terminal.draw(|frame| draw(frame, &cfg, &mut app)).unwrap();
        app.view = View::Overview;
        terminal.draw(|frame| draw(frame, &cfg, &mut app)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(content.contains("SIMULATED"));
    }

    #[test]
    fn position_search_and_sort_use_real_position_fields() {
        let mut app = UiState::new();
        app.snapshot.positions = vec![
            position("Bitcoin Up or Down", -2.0),
            position("Solana Up or Down", 4.0),
        ];
        app.snapshot.positions[0].tags.clear();
        app.search = "solana".into();
        app.position_sort = PositionSort::Pnl;
        let positions = visible_positions(&app);
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].question, "Solana Up or Down");

        app.search.clear();
        let positions = visible_positions(&app);
        assert_eq!(positions[0].question, "Solana Up or Down");
    }
}
