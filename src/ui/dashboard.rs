//! The service card grid.

use std::sync::Arc;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::{App, CardHitArea};
use crate::service::{Service, ServiceStatus};
use crate::ui::{format_memory, kind_color, status_color};

const CARD_HEIGHT: u16 = 8;
const GAP: u16 = 1;
const MIN_CARD_WIDTH: u16 = 30;
const MAX_CARD_WIDTH: u16 = 54;

pub fn render(frame: &mut Frame, app: &mut App, area: Rect) {
    if area.width == 0 || area.height == 0 || app.services.is_empty() {
        return;
    }

    let max_columns = ((area.width + GAP) / (MIN_CARD_WIDTH + GAP)).max(1) as usize;
    let columns = max_columns.clamp(1, app.services.len());
    let gaps = GAP * (columns as u16).saturating_sub(1);
    let card_width = (area.width.saturating_sub(gaps) / columns as u16).clamp(1, MAX_CARD_WIDTH);
    let grid_width = card_width
        .saturating_mul(columns as u16)
        .saturating_add(gaps);
    let start_x = area.x + area.width.saturating_sub(grid_width) / 2;

    let visible_rows = (((area.height + GAP) / (CARD_HEIGHT + GAP)).max(1)) as usize;
    app.prepare_grid(columns, visible_rows);

    for visible_row in 0..visible_rows {
        let row = app.grid_offset_rows + visible_row;
        let y = area.y + (visible_row as u16) * (CARD_HEIGHT + GAP);
        if y >= area.y + area.height {
            break;
        }
        let available_height = area.y + area.height - y;
        let height = CARD_HEIGHT.min(available_height);
        if height < 3 {
            break;
        }

        for column in 0..columns {
            let index = row * columns + column;
            if index >= app.services.len() {
                break;
            }
            let x = start_x + (column as u16) * (card_width + GAP);
            let rect = Rect::new(x, y, card_width, height);
            render_card(frame, app, index, rect);
        }
    }
}

fn render_card(frame: &mut Frame, app: &mut App, index: usize, rect: Rect) {
    let service = Arc::clone(&app.services[index]);
    let status = service.status();
    let selected = app.selected == index;

    let border_style = if selected {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered()
        .border_type(if selected {
            BorderType::Thick
        } else {
            BorderType::Rounded
        })
        .border_style(border_style);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        app.hit_areas.push(CardHitArea {
            index,
            rect,
            toggle: None,
            logs: Rect::new(0, 0, 0, 0),
        });
        return;
    }

    let status_line = status_line(&service, status, inner.width);
    let name_style = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
    let path = if service.spec.path_label == "." {
        "current directory".to_string()
    } else {
        service.spec.relative_path()
    };
    let metrics_line = metrics_line(&service, app.metrics_enabled);
    let hint_line = hint_line(&service, status, inner.width);

    let mut lines = vec![
        status_line,
        Line::from(Span::styled(service.spec.name.clone(), name_style)),
        Line::from(Span::styled(path, Style::new().fg(Color::DarkGray))),
        metrics_line,
        Line::from(""),
        hint_line,
    ];
    if inner.height < 6 {
        lines.truncate(inner.height as usize);
    }
    frame.render_widget(Paragraph::new(lines), inner);

    let mut toggle_hit = None;
    let mut logs_hit = Rect::new(0, 0, 0, 0);
    if inner.height >= 5 {
        let button_y = inner.y + 4;
        let (toggle_text, toggle_style) = if status == ServiceStatus::Stopping {
            (
                " Stopping ",
                Style::new().fg(Color::Black).bg(Color::DarkGray),
            )
        } else if service.is_running() {
            (
                " Stop ",
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                " Start ",
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        };
        let logs_text = " Logs ";
        let toggle_width = toggle_text.chars().count() as u16;
        let logs_width = logs_text.chars().count() as u16;
        let button_line = Line::from(vec![
            Span::styled(toggle_text, toggle_style),
            Span::raw("  "),
            Span::styled(
                logs_text,
                Style::new()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(button_line),
            Rect::new(inner.x, button_y, inner.width, 1),
        );

        if status != ServiceStatus::Stopping {
            toggle_hit = Some(clip_rect(
                Rect::new(inner.x, button_y, toggle_width, 1),
                inner,
            ));
        }
        logs_hit = clip_rect(
            Rect::new(
                inner.x.saturating_add(toggle_width + 2),
                button_y,
                logs_width,
                1,
            ),
            inner,
        );
        if logs_hit.width == 0 {
            logs_hit = Rect::new(0, 0, 0, 0);
        }
    }

    app.hit_areas.push(CardHitArea {
        index,
        rect,
        toggle: toggle_hit.filter(|rect| rect.width > 0),
        logs: logs_hit,
    });
}

fn status_line(service: &Service, status: ServiceStatus, width: u16) -> Line<'static> {
    let color = status_color(status);
    let label = status.label();
    let badge = service.spec.kind.badge();
    let used = 2 + label.chars().count() + badge.chars().count();
    let padding = (width as usize).saturating_sub(used);
    let mut spans = vec![
        Span::styled("●", Style::new().fg(color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(label, Style::new().fg(color)),
    ];
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }
    spans.push(Span::styled(
        badge,
        Style::new().fg(kind_color(service.spec.kind)),
    ));
    Line::from(spans)
}

fn metrics_line(service: &Service, metrics_enabled: bool) -> Line<'static> {
    let mut parts = Vec::new();
    let metrics = service.metrics();
    if metrics_enabled {
        if metrics.sampled {
            parts.push(format!("cpu {:.0}%", metrics.cpu_percent));
            parts.push(format!("mem {}", format_memory(metrics.memory_bytes)));
        } else {
            parts.push("cpu —".to_string());
            parts.push("mem —".to_string());
        }
    }
    if let Some(port) = service.port() {
        parts.push(format!(":{port}"));
    }
    Line::from(Span::styled(
        parts.join("  ·  "),
        Style::new().fg(Color::Gray),
    ))
}

fn hint_line(service: &Service, status: ServiceStatus, width: u16) -> Line<'static> {
    if status == ServiceStatus::Error
        && let Some(error) = service.last_error()
    {
        let text: String = error
            .chars()
            .take(width.saturating_sub(2) as usize)
            .collect();
        return Line::from(Span::styled(
            format!("✖ {text}"),
            Style::new().fg(Color::Red),
        ));
    }
    if status == ServiceStatus::Stopping {
        return Line::from(Span::styled(
            "waiting for process to exit…",
            Style::new().fg(Color::Yellow),
        ));
    }
    Line::from(Span::styled(
        format!("$ {}", service.spec.command.display()),
        Style::new().fg(Color::DarkGray),
    ))
}

fn clip_rect(rect: Rect, bounds: Rect) -> Rect {
    let x1 = rect.x.max(bounds.x);
    let y1 = rect.y.max(bounds.y);
    let x2 = rect
        .x
        .saturating_add(rect.width)
        .min(bounds.x.saturating_add(bounds.width));
    let y2 = rect
        .y
        .saturating_add(rect.height)
        .min(bounds.y.saturating_add(bounds.height));
    Rect::new(x1, y1, x2.saturating_sub(x1), y2.saturating_sub(y1))
}
