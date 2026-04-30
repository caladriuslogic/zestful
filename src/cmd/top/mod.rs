//! `zestful top` — live agent TUI. Network-only client of the daemon's
//! HTTP+SSE API. See spec: docs/superpowers/specs/2026-04-29-zestful-top-tui-design.md.

mod app;
mod client;
mod colors;
mod keys;
mod ui;

use anyhow::Result;
use crossterm::{event::{Event as CtEvent, EventStream}, execute, terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen}};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::{Duration, Instant}};
use tokio::time::interval;

use app::{AppState, Connection, SideEffect};
use client::{Client, StreamEvent};
use keys::key_to_action;

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

    // Debounce timer for SSE-driven refetches.
    let mut dirty_at: Option<Instant> = None;
    const DEBOUNCE: Duration = Duration::from_millis(100);

    term.draw(|f| ui::draw(f, &state))?;

    loop {
        if state.should_quit { break; }

        // Compute the soonest debounce deadline.
        let debounce_sleep = dirty_at.map(|t| {
            let elapsed = t.elapsed();
            if elapsed >= DEBOUNCE { Duration::from_millis(0) } else { DEBOUNCE - elapsed }
        });

        tokio::select! {
            biased;

            Some(Ok(ev)) = keys.next() => {
                if let CtEvent::Key(k) = ev {
                    if let Some(action) = key_to_action(k, state.input_mode) {
                        let fx = state.apply(action);
                        if let Some(c) = client.as_ref() { run_side_effects(c, &mut state, fx).await; }
                    }
                }
            }

            // SSE frames (if connected).
            maybe = async {
                match sse_stream.as_mut() { Some(s) => s.next().await, None => None }
            }, if sse_stream.is_some() => {
                match maybe {
                    Some(StreamEvent::Connected) => {
                        state.connection = Connection::Live;
                    }
                    Some(StreamEvent::ProjectionChanged(_frame)) => {
                        if state.connection != Connection::Live {
                            state.connection = Connection::Live;
                        }
                        dirty_at = Some(Instant::now());
                    }
                    Some(StreamEvent::Disconnected(reason)) => {
                        state.connection = Connection::Reconnecting;
                        let _ = reason; // reqwest-eventsource will reconnect itself.
                    }
                    None => {
                        sse_stream = None;
                        state.connection = Connection::Offline("stream ended".to_string());
                    }
                }
            }

            // Debounce-driven refetch.
            _ = async {
                if let Some(d) = debounce_sleep { tokio::time::sleep(d).await; }
                else { futures::future::pending::<()>().await; }
            }, if dirty_at.is_some() => {
                dirty_at = None;
                if let Some(c) = client.as_ref() {
                    refetch_after_signal(c, &mut state).await;
                }
            }

            _ = tick.tick() => {
                // Local clock work only — toast expiry, relative-time advancement.
                if let Some((_, when)) = &state.toast {
                    if when.elapsed() > Duration::from_secs(3) {
                        state.toast = None;
                    }
                }
            }
        }

        term.draw(|f| ui::draw(f, &state))?;
    }
    Ok(())
}

async fn kickoff_initial_fetch(c: &Client, state: &mut AppState) {
    let since = since_24h();
    match c.tiles(since).await {
        Ok(t) => state.tiles = t,
        Err(e) => state.connection = Connection::Offline(format!("{}", e)),
    }
    if let Ok(n) = c.notifications(since).await {
        state.notifications = n;
    }
}

async fn refetch_after_signal(c: &Client, state: &mut AppState) {
    let since = since_24h();
    let (t, n) = tokio::join!(c.tiles(since), c.notifications(since));
    if let Ok(t) = t { state.tiles = t; }
    if let Ok(n) = n { state.notifications = n; }
    if let Some(sel) = state.selected_tile() {
        let agent = sel.agent.clone();
        if let Ok(evs) = c.events_for_agent(&agent, since_1h(), 60).await {
            state.recent_events = evs;
        }
    }
}

async fn run_side_effects(c: &Client, state: &mut AppState, fx: Vec<SideEffect>) {
    for f in fx {
        match f {
            SideEffect::RefetchTiles => {
                if let Ok(t) = c.tiles(since_24h()).await { state.tiles = t; }
            }
            SideEffect::RefetchNotifications => {
                if let Ok(n) = c.notifications(since_24h()).await { state.notifications = n; }
            }
            SideEffect::RefetchEventsForSelected => {
                if let Some(sel) = state.selected_tile() {
                    let agent = sel.agent.clone();
                    if let Ok(evs) = c.events_for_agent(&agent, since_1h(), 60).await {
                        state.recent_events = evs;
                    }
                }
            }
            SideEffect::PostFocus { terminal_uri, .. } => {
                if let Err(e) = c.post_focus(&terminal_uri).await {
                    state.toast = Some((format!("focus failed: {}", e), Instant::now()));
                }
            }
        }
    }
}

fn since_24h() -> i64 { now_ms() - 24 * 3_600_000 }
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
