// src/cli/tui.rs
use std::{io, sync::Arc, time::Duration};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{error::Result, run::RunEngine};

#[derive(Clone, Copy, Debug)]
enum Who {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug)]
struct ChatLine {
    who: Who,
    text: String,
}

/// Blocking TUI runner.
/// Call this from `tokio::task::spawn_blocking`.
pub fn run_blocking(
    engine: Arc<RunEngine>,
    session_key: String,
    cancel: CancellationToken,
) -> Result<()> {
    // Bridge between blocking UI thread and async engine task.
    let (tx_msg, mut rx_msg) = mpsc::unbounded_channel::<String>();
    let (tx_rep, mut rx_rep) = mpsc::unbounded_channel::<Result<String>>();

    // Async worker: consumes messages, calls engine, emits replies.
    tokio::spawn({
        let engine = engine.clone();
        let session_key = session_key.clone();
        let cancel = cancel.clone();
        async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    Some(msg) = rx_msg.recv() => {
                        let out = engine
                            .handle_message(session_key.clone(), msg)
                            .await
                            .map(|r| r.response);
                        let _ = tx_rep.send(out);
                    }
                }
            }
        }
    });

    enable_raw_mode().map_err(str_err)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(str_err)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(str_err)?;
    terminal.clear().map_err(str_err)?;
    terminal.hide_cursor().map_err(str_err)?;

    let mut chat: Vec<ChatLine> = vec![ChatLine {
        who: Who::System,
        text: "Enter=send | Esc/Ctrl+C=quit | Up/Down=scroll".to_string(),
    }];
    let mut input = String::new();
    let mut scroll: u16 = 0;

    // One inflight request at a time. (tracked in the UI thread)
    let mut inflight: bool = false;

    loop {
        // Drain replies from async worker.
        while let Ok(r) = rx_rep.try_recv() {
            inflight = false;
            match r {
                Ok(txt) => chat.push(ChatLine {
                    who: Who::Assistant,
                    text: txt,
                }),
                Err(e) => chat.push(ChatLine {
                    who: Who::System,
                    text: format!("error: {e:?}"),
                }),
            }
            scroll = scroll_to_bottom(&chat);
        }

        terminal
            .draw(|f| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(3), Constraint::Length(3)])
                    .split(f.size());

                let chat_text = render_chat(&chat);
                let chat_box = Paragraph::new(chat_text)
                    .block(Block::default().borders(Borders::ALL).title("Chat"))
                    .wrap(Wrap { trim: false })
                    .scroll((scroll, 0));
                f.render_widget(chat_box, chunks[0]);

                let title = if inflight { "You (waiting…)" } else { "You" };
                let input_box = Paragraph::new(input.as_str())
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .wrap(Wrap { trim: false });
                f.render_widget(input_box, chunks[1]);
            })
            .map_err(str_err)?;

        if cancel.is_cancelled() {
            break;
        }

        // Input handling (blocking, but we're on a blocking thread).
        if event::poll(Duration::from_millis(30)).map_err(str_err)? {
            match event::read().map_err(str_err)? {
                Event::Key(KeyEvent {
                    code, modifiers, ..
                }) => {
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(code, KeyCode::Char('c'))
                    {
                        cancel.cancel();
                        break;
                    }

                    match code {
                        KeyCode::Esc => {
                            cancel.cancel();
                            break;
                        }
                        KeyCode::Enter => {
                            let msg = input.trim().to_string();
                            input.clear();
                            if msg.is_empty() {
                                continue;
                            }

                            chat.push(ChatLine {
                                who: Who::User,
                                text: msg.clone(),
                            });
                            scroll = scroll_to_bottom(&chat);

                            if !inflight {
                                inflight = true;
                                if tx_msg.send(msg).is_err() {
                                    inflight = false;
                                    chat.push(ChatLine {
                                        who: Who::System,
                                        text: "error: message channel closed".to_string(),
                                    });
                                    scroll = scroll_to_bottom(&chat);
                                }
                            } else {
                                chat.push(ChatLine {
                                    who: Who::System,
                                    text: "busy: wait for reply".to_string(),
                                });
                                scroll = scroll_to_bottom(&chat);
                            }
                        }
                        KeyCode::Backspace => {
                            input.pop();
                        }
                        KeyCode::Char(c) => {
                            if !modifiers.contains(KeyModifiers::CONTROL) {
                                input.push(c);
                            }
                        }
                        KeyCode::Up => scroll = scroll.saturating_add(1),
                        KeyCode::Down => scroll = scroll.saturating_sub(1),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    // restore terminal
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    Ok(())
}

fn render_chat(lines: &[ChatLine]) -> Text<'static> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len() * 2);
    for l in lines {
        let (prefix, style) = match l.who {
            Who::User => ("you: ", Style::default().add_modifier(Modifier::BOLD)),
            Who::Assistant => ("assistant: ", Style::default()),
            Who::System => ("system: ", Style::default().add_modifier(Modifier::DIM)),
        };
        out.push(Line::from(vec![
            Span::styled(prefix.to_string(), style),
            Span::raw(l.text.clone()),
        ]));
        out.push(Line::raw(""));
    }
    Text::from(out)
}

fn scroll_to_bottom(lines: &[ChatLine]) -> u16 {
    (lines.len() as u16).saturating_sub(1)
}

fn str_err<E: std::fmt::Display>(e: E) -> crate::error::Error {
    crate::error::Error::other(e.to_string())
}
