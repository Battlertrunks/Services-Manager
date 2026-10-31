//! The live log viewer modal.

use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::{App, Overlay, log_top_index};
use crate::service::Service;
use crate::ui::{popup_percent, status_color};

pub fn area(frame_area: Rect) -> Rect {
    popup_percent(frame_area, 90, 84)
}

pub fn render(frame: &mut Frame, app: &mut App, index: usize) {
    let Some(service) = app.services.get(index).cloned() else {
        app.overlay = Overlay::None;
        return;
    };
    let status = service.status();
    let color = status_color(status);
    let meta = service.logs_meta();
    let total = meta.len;

    let area = area(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title_top(Line::from(vec![
            Span::styled(
                " Logs ",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("· {} ", service.spec.name),
                Style::new().fg(Color::White),
            ),
        ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let [info_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let viewport = body_area.height as usize;
    app.logs.viewport = body_area.height;
    let start = log_top_index(&meta, body_area.height, app.logs.follow, app.logs.top_seq);
    if !app.logs.follow {
        app.logs.top_seq = meta.first_seq.saturating_add(start as u64);
    }
    let end = start.saturating_add(viewport).min(total);
    let snapshot = service.logs_snapshot_range(start, end.saturating_sub(start));

    let mut info_spans = vec![
        Span::styled("●", Style::new().fg(color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(status.label().to_string(), Style::new().fg(color)),
        Span::styled(
            format!("  ·  {}", service.spec.relative_path()),
            Style::new().fg(Color::DarkGray),
        ),
    ];
    if let Some(port) = service.port() {
        info_spans.push(Span::styled(
            format!("  ·  :{port}"),
            Style::new().fg(Color::Cyan),
        ));
    }
    if let Some(started) = service.runtime.started_at() {
        info_spans.push(Span::styled(
            format!("  ·  up {}", format_uptime(started.elapsed().as_secs())),
            Style::new().fg(Color::DarkGray),
        ));
    }
    info_spans.push(Span::styled(
        format!("  ·  {total} line{}", if total == 1 { "" } else { "s" }),
        Style::new().fg(Color::DarkGray),
    ));
    if meta.dropped > 0 {
        info_spans.push(Span::styled(
            format!(" ({} older dropped)", meta.dropped),
            Style::new().fg(Color::DarkGray),
        ));
    }
    info_spans.push(Span::styled(
        if app.logs.follow {
            "  ·  following"
        } else {
            "  ·  paused"
        },
        Style::new()
            .fg(if app.logs.follow {
                Color::Green
            } else {
                Color::Yellow
            })
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(Line::from(info_spans)), info_area);

    if total == 0 {
        let help = if service.is_running() {
            "Waiting for output…"
        } else {
            "No logs yet. Start the service to see its output."
        };
        frame.render_widget(
            Paragraph::new(help)
                .style(Style::new().fg(Color::DarkGray))
                .alignment(ratatui::layout::Alignment::Center),
            body_area,
        );
    } else if viewport > 0 {
        let lines: Vec<Line<'static>> = snapshot
            .lines
            .iter()
            .map(|log| {
                let style = log_style(&service, &log.text);
                Line::from(vec![
                    Span::styled(
                        format!("{} ", log.at.format("%H:%M:%S")),
                        Style::new().fg(Color::DarkGray),
                    ),
                    Span::styled(log.text.clone(), style),
                ])
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).scroll((0, app.logs.h_scroll)),
            body_area,
        );
    }

    let footer = "↑↓/PgUp PgDn scroll · g/G top/tail · f follow · ←→ pan · c clear · Esc close";
    frame.render_widget(
        Paragraph::new(footer).style(Style::new().fg(Color::DarkGray)),
        footer_area,
    );
}

fn format_uptime(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h{}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

fn log_style(service: &Arc<Service>, text: &str) -> Style {
    let lower = text.to_lowercase();
    if text.starts_with("──") {
        return Style::new().fg(Color::DarkGray);
    }
    if text.starts_with('✖') || service.spec.line_is_error(&lower) {
        return Style::new().fg(Color::Red);
    }
    if lower.contains("warn") {
        return Style::new().fg(Color::Yellow);
    }
    if service.spec.line_is_ready(&lower)
        || lower.contains("ready in")
        || lower.contains("listening")
        || lower.contains("compiled successfully")
    {
        return Style::new().fg(Color::Green);
    }
    Style::new().fg(Color::White)
}
