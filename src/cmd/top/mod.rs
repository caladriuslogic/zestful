//! `zestful top` — live agent TUI. Network-only client of the daemon's
//! HTTP+SSE API. See spec: docs/superpowers/specs/2026-04-29-zestful-top-tui-design.md.

mod app;
mod client;
mod colors;
mod keys;
mod ui;

use anyhow::Result;
use crossterm::{event::{Event as CtEvent, EventStream, KeyEventKind}, execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::{Duration, Instant}};
use tokio::time::interval;

use app::{AppState, Connection, SideEffect};
use client::{Client, StreamEvent};
use keys::key_to_action;

use crate::events::tiles::tile::Tile;
use crate::events::notifications::notification::Notification;
use crate::events::store::query::EventRow;

pub fn run(modal: bool) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(run_async(modal))
}

async fn run_async(modal: bool) -> Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut term = Terminal::new(backend)?;

    let result = main_loop(&mut term, modal).await;

    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    term.show_cursor().ok();
    result
}

async fn main_loop(term: &mut Terminal<CrosstermBackend<io::Stdout>>, modal: bool) -> Result<()> {
    let mut state = AppState::new();
    state.fullscreen = !modal;

    // Build client; if config is missing we still run (offline mode).
    let client_res = Client::from_config();
    let client = match client_res {
        Ok(c) => Some(c),
        Err(e) => {
            state.connection = Connection::Offline(format!("{}", e));
            None
        }
    };

    // Initial fetch (if we have a client).
    if let Some(c) = client.as_ref() { kickoff_initial_fetch(c, &mut state).await; }

    // SSE stream.
    let mut sse_stream: Option<std::pin::Pin<Box<dyn futures::Stream<Item = StreamEvent> + Send>>> =
        client.as_ref().map(|c| Box::pin(c.stream()) as _);

    let mut keys = EventStream::new();
    let mut tick = interval(Duration::from_secs(1));

    // Debounce timer for SSE-driven refetches. 1 s avoids hammering tiles::compute
    // (a full 24 h event rescan) on every hook event during active Claude sessions.
    let mut dirty_at: Option<Instant> = None;
    const DEBOUNCE: Duration = Duration::from_millis(1000);

    // Channel for background refetch results. The spawned task sends (tiles,
    // notifications, events); the result arm in select! applies them to state.
    type FetchResult = (Vec<Tile>, Vec<Notification>, Vec<EventRow>, Option<crate::events::summary::Summary>);
    let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::channel::<FetchResult>(1);
    let mut fetch_pending = false;

    // Guard against unnecessary redraws. VS Code's terminal (xterm.js) sends
    // focus and cursor-position-report events that wake the select loop without
    // changing any state; redrawing on every wakeup creates a feedback loop that
    // pins CPU. We only redraw when an arm actually mutates state or the terminal
    // is resized.
    let mut needs_redraw = true; // always draw the first frame

    loop {
        if state.should_quit { break; }

        if needs_redraw {
            term.draw(|f| ui::draw(f, &state))?;
            needs_redraw = false;
        }

        // Compute the soonest debounce deadline.
        let debounce_sleep = dirty_at.map(|t| {
            let elapsed = t.elapsed();
            if elapsed >= DEBOUNCE { Duration::from_millis(0) } else { DEBOUNCE - elapsed }
        });

        tokio::select! {
            biased;

            Some(Ok(ev)) = keys.next() => {
                match ev {
                    CtEvent::Key(k) if k.kind == KeyEventKind::Press => {
                        if let Some(action) = key_to_action(k, state.input_mode) {
                            let fx = state.apply(action);
                            if let Some(c) = client.as_ref() { run_side_effects(c, &mut state, fx).await; }
                            needs_redraw = true;
                        }
                    }
                    // Redraw to fill new dimensions; ignore focus/mouse/CPR events.
                    CtEvent::Resize(_, _) => { needs_redraw = true; }
                    _ => {}
                }
            }

            // SSE frames (if connected).
            maybe = async {
                match sse_stream.as_mut() { Some(s) => s.next().await, None => None }
            }, if sse_stream.is_some() => {
                match maybe {
                    Some(StreamEvent::Connected) => {
                        state.connection = Connection::Live;
                        needs_redraw = true;
                    }
                    Some(StreamEvent::ProjectionChanged(_frame)) => {
                        if state.connection != Connection::Live {
                            state.connection = Connection::Live;
                        }
                        dirty_at = Some(Instant::now());
                        needs_redraw = true;
                    }
                    Some(StreamEvent::Disconnected(reason)) => {
                        state.connection = Connection::Reconnecting;
                        let _ = reason; // reqwest-eventsource will reconnect itself.
                        needs_redraw = true;
                    }
                    None => {
                        sse_stream = None;
                        state.connection = Connection::Offline("stream ended".to_string());
                        needs_redraw = true;
                    }
                }
            }

            // Debounce-driven refetch — spawned as a background task so keyboard
            // input is never blocked while HTTP round-trips are in flight.
            _ = async {
                if let Some(d) = debounce_sleep { tokio::time::sleep(d).await; }
                else { futures::future::pending::<()>().await; }
            }, if dirty_at.is_some() && !fetch_pending => {
                dirty_at = None;
                if let Some(c) = client.as_ref() {
                    let c2 = c.clone();
                    let tx = fetch_tx.clone();
                    let agent = state.selected_tile().map(|t| t.agent.clone());
                    let surface = state.selected_tile().and_then(|t| tile_surface_token(t));
                    fetch_pending = true;
                    tokio::spawn(async move {
                        let result = fetch_projection(&c2, agent, surface).await;
                        let _ = tx.send(result).await;
                    });
                }
            }

            // Background refetch completed — apply results to state.
            Some((tiles, notifs, events, summary)) = fetch_rx.recv() => {
                fetch_pending = false;
                state.update_tiles_and_emit_toasts(tiles);
                state.notifications = notifs;
                state.recent_events = events;
                if summary.is_some() { state.summary = summary; }
                needs_redraw = true;
            }

            _ = tick.tick() => {
                // Local clock work only — toast expiry, relative-time advancement.
                if let Some(t) = &state.toast {
                    if t.since.elapsed() > t.lifetime {
                        state.toast = None;
                        needs_redraw = true;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn kickoff_initial_fetch(c: &Client, state: &mut AppState) {
    let since = since_1h();
    match c.tiles(since).await {
        Ok(t) => state.update_tiles_and_emit_toasts(t),
        Err(e) => state.connection = Connection::Offline(format!("{}", e)),
    }
    if let Ok(n) = c.notifications(since).await {
        state.notifications = n;
    }
    if let Ok(s) = c.summary().await {
        state.summary = Some(s);
    }
}

/// Fetch tiles, notifications, and optionally events for the selected agent.
/// Pure I/O — returns data without mutating AppState, so it can run in a
/// spawned task without holding any state borrows.
async fn fetch_projection(
    c: &Client,
    agent: Option<String>,
    surface: Option<String>,
) -> (Vec<Tile>, Vec<Notification>, Vec<EventRow>, Option<crate::events::summary::Summary>) {
    let since = since_1h();
    let (t, n, s) = tokio::join!(c.tiles(since), c.notifications(since), c.summary());
    let tiles = t.unwrap_or_default();
    let notifs = n.unwrap_or_default();
    let summary = s.ok();
    let events = match agent {
        Some(a) => c.events_for_agent(&a, surface.as_deref(), since_1h(), 60).await.unwrap_or_default(),
        None => vec![],
    };
    (tiles, notifs, events, summary)
}

async fn run_side_effects(c: &Client, state: &mut AppState, fx: Vec<SideEffect>) {
    for f in fx {
        match f {
            SideEffect::RefetchTiles => {
                if let Ok(t) = c.tiles(since_1h()).await {
                    state.update_tiles_and_emit_toasts(t);
                }
                if let Ok(s) = c.summary().await { state.summary = Some(s); }
            }
            SideEffect::RefetchNotifications => {
                if let Ok(n) = c.notifications(since_1h()).await { state.notifications = n; }
            }
            SideEffect::RefetchEventsForSelected => {
                if let Some(sel) = state.selected_tile() {
                    let agent = sel.agent.clone();
                    let surface = tile_surface_token(sel);
                    if let Ok(evs) = c.events_for_agent(&agent, surface.as_deref(), since_1h(), 60).await {
                        state.recent_events = evs;
                    }
                }
            }
            SideEffect::PostFocus { terminal_uri, .. } => {
                let c2 = c.clone();
                tokio::spawn(async move { let _ = c2.post_focus(&terminal_uri).await; });
            }
        }
    }
}

/// Return the surface_token to pass to /events for a tile, or None for
/// browser tiles (which collapse all conversations into one tile and
/// have no per-event surface field to filter on).
fn tile_surface_token(tile: &crate::events::tiles::tile::Tile) -> Option<String> {
    if tile.surface_kind == "browser" { None } else { Some(tile.surface_token.clone()) }
}

fn since_1h() -> i64 { now_ms() - 3_600_000 }
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        prev(info);
    }));
}
