//! Terminal rendering.

mod bundles;
mod dashboard;
mod help;
mod logs;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use crate::app::{App, MessageKind, Overlay};
use crate::service::ServiceStatus;
use crate::workspace::ProjectKind;

pub fn render(frame: &mut Frame, app: &mut App) {
    app.hit_areas.clear();
    let area = frame.area();
    if area.width < 30 || area.height < 12 {
        frame.render_widget(Clear, area);
        let mut lines = vec![
            Line::from(Span::styled(
                "Terminal too small",
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Resize to at least 30 × 12."),
        ];
        match app.overlay {
            Overlay::None => lines.push(Line::from("Press q to quit.")),
            Overlay::Logs(index) => {
                let label = app
                    .services
                    .get(index)
                    .map(|service| {
                        format!(
                            "Logs: {} · {} · Esc/q to close",
                            service.spec.name,
                            service.status().label()
                        )
                    })
                    .unwrap_or_else(|| "Logs · Esc/q to close".to_string());
                lines.push(Line::from(Span::styled(
                    label,
                    Style::new().fg(Color::Cyan),
                )));
            }
            Overlay::Help => lines.push(Line::from(Span::styled(
                "Help is open · Esc/q to close",
                Style::new().fg(Color::Cyan),
            ))),
            Overlay::Bundles => lines.push(Line::from(Span::styled(
                "Bundles need more space · Esc/q to close",
                Style::new().fg(Color::Cyan),
            ))),
            Overlay::BundleEditor => lines.push(Line::from(Span::styled(
                "Bundle editor · Ctrl-S save / Esc cancel",
                Style::new().fg(Color::Cyan),
            ))),
            Overlay::BundleDelete(_) => lines.push(Line::from(Span::styled(
                "Delete bundle? · y confirm / n cancel",
                Style::new().fg(Color::Yellow),
            ))),
            Overlay::ConfirmQuit => lines.push(Line::from(Span::styled(
                format!("Quit? {} running · y quit / n cancel", app.running_count()),
                Style::new().fg(Color::Yellow),
            ))),
        }
        let [block] = Layout::vertical([Constraint::Length(8)])
            .flex(Flex::Center)
            .areas(area);
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .alignment(ratatui::layout::Alignment::Center)
                .wrap(Wrap { trim: false })
                .style(Style::new().fg(Color::Gray)),
            block,
        );
        return;
    }

    let content = area.inner(Margin::new(1, 0));
    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(content);

    if app.services.is_empty() {
        render_empty(frame, app, body_area);
    } else {
        dashboard::render(frame, app, body_area);
    }
    render_header(frame, app, header_area);
    render_footer(frame, app, footer_area);

    if app.overlay != Overlay::None {
        if let Some(modal_area) = overlay_area(app.overlay, area)
            && (modal_area.y < body_area.y
                || modal_area.y.saturating_add(modal_area.height)
                    > body_area.y.saturating_add(body_area.height))
        {
            // The modal overlaps the title or key hints on a small terminal. Give it a clean
            // full-screen backdrop instead of leaving fragmented chrome around it.
            frame.render_widget(Clear, area);
        } else {
            // Keep the title and key hints visible, but remove card remnants around the modal.
            let backdrop = Rect::new(area.x, body_area.y, area.width, body_area.height);
            frame.render_widget(Clear, backdrop);
        }
    }

    match app.overlay {
        Overlay::None => {}
        Overlay::Logs(index) => logs::render(frame, app, index),
        Overlay::Help => help::render_help(frame, app),
        Overlay::ConfirmQuit => help::render_quit(frame, app),
        Overlay::Bundles => bundles::render(frame, app),
        Overlay::BundleEditor => bundles::render_editor(frame, app),
        Overlay::BundleDelete(bundle_id) => bundles::render_delete(frame, app, bundle_id),
    }
}

fn overlay_area(overlay: Overlay, area: Rect) -> Option<Rect> {
    match overlay {
        Overlay::None => None,
        Overlay::Logs(_) => Some(logs::area(area)),
        Overlay::Help => Some(help::help_area(area)),
        Overlay::ConfirmQuit => Some(help::quit_area(area)),
        Overlay::Bundles => Some(bundles::area(area)),
        Overlay::BundleEditor => Some(bundles::editor_area(area)),
        Overlay::BundleDelete(_) => Some(bundles::delete_area(area)),
    }
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let title_area = Rect::new(area.x, area.y, area.width, 1);
    let stats_area = Rect::new(area.x, area.y + 1, area.width, 1);
    let rule_area = Rect::new(area.x, area.y + 2, area.width, 1);

    let compact = area.width < 90;
    let right_text = if area.width < 70 {
        format!("{}/{} running", app.running_count(), app.services.len())
    } else {
        format!(
            "{} running · {} project{}",
            app.running_count(),
            app.services.len(),
            if app.services.len() == 1 { "" } else { "s" }
        )
    };
    let right_width = (right_text.chars().count() as u16 + 1).min(area.width / 2 + 1);
    if right_width > 1 {
        let [left, right] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)])
                .areas(title_area);
        let mut title_spans = vec![Span::styled(
            "▰ services-manager",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )];
        if area.width >= 100 {
            title_spans.push(Span::raw("  "));
            title_spans.push(Span::styled(
                app.root.display().to_string(),
                Style::new().fg(Color::DarkGray),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(title_spans)), left);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                right_text,
                Style::new().fg(Color::Gray),
            )))
            .alignment(ratatui::layout::Alignment::Right),
            right,
        );
    } else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "▰ services-manager",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))),
            title_area,
        );
    }

    let (ready, starting, error, stopped) = app.status_counts();
    let mut counts = Vec::new();
    if compact {
        push_count(&mut counts, ready, "ready", Color::Green);
        push_count(&mut counts, starting, "start", Color::Yellow);
        push_count(&mut counts, error, "error", Color::Red);
        push_count(&mut counts, stopped, "stop", Color::DarkGray);
    } else {
        push_count(&mut counts, ready, "ready", Color::Green);
        push_count(&mut counts, starting, "starting", Color::Yellow);
        push_count(&mut counts, error, "error", Color::Red);
        push_count(&mut counts, stopped, "stopped", Color::DarkGray);
    }

    let mut row_indicator = None;
    if !app.services.is_empty() && app.grid_visible_rows > 0 {
        let total_rows = app.services.len().div_ceil(app.grid_columns.max(1));
        if total_rows > app.grid_visible_rows {
            let first = (app.grid_offset_rows + 1).min(total_rows);
            let last = (app.grid_offset_rows + app.grid_visible_rows).min(total_rows);
            row_indicator = Some(format!("rows {first}-{last}/{total_rows}"));
        }
    }

    if area.width >= 110 {
        let right_stats = if app.metrics_enabled {
            "metrics on".to_string()
        } else {
            "metrics off".to_string()
        };
        let right_width = (right_stats.chars().count() as u16 + 1).min(area.width / 2 + 1);
        if right_width > 1 {
            let [left, right] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width)])
                    .areas(stats_area);
            frame.render_widget(Paragraph::new(Line::from(counts)), left);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    right_stats,
                    Style::new().fg(Color::DarkGray),
                )))
                .alignment(ratatui::layout::Alignment::Right),
                right,
            );
        } else {
            frame.render_widget(Paragraph::new(Line::from(counts)), stats_area);
        }
    } else {
        frame.render_widget(Paragraph::new(Line::from(counts)), stats_area);
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::new().fg(Color::DarkGray),
        ))),
        rule_area,
    );
    if let Some(row_indicator) = row_indicator {
        let width = row_indicator.chars().count() as u16 + 2;
        if width < rule_area.width {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {row_indicator} "),
                    Style::new().fg(Color::Gray),
                )))
                .alignment(ratatui::layout::Alignment::Right),
                Rect::new(rule_area.x + rule_area.width - width, rule_area.y, width, 1),
            );
        }
    }
}

fn push_count(counts: &mut Vec<Span<'static>>, count: usize, label: &'static str, color: Color) {
    if !counts.is_empty() {
        counts.push(Span::styled("   ", Style::new().fg(Color::DarkGray)));
    }
    counts.push(Span::styled(
        format!("● {count} {label}"),
        Style::new().fg(color),
    ));
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    if area.height < 2 {
        return;
    }
    let hints = if area.width < 60 {
        "↑↓←→ · enter · o logs · ? help · q quit"
    } else if area.width < 90 {
        "↑↓←→ move · enter toggle · o logs · b bundles · ? help · q quit"
    } else {
        "↑↓←→/hjkl move · enter start/stop · o logs · b bundles · a start all · x stop all · r rescan · ? help · q quit"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hints,
            Style::new().fg(Color::DarkGray),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let line = if let Some(message) = app.current_message() {
        let (color, icon) = match message.kind {
            MessageKind::Info => (Color::Cyan, "i"),
            MessageKind::Success => (Color::Green, "✓"),
            MessageKind::Warning => (Color::Yellow, "!"),
            MessageKind::Error => (Color::Red, "✖"),
        };
        Line::from(vec![
            Span::styled(
                format!("{icon} "),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(message.text.clone(), Style::new().fg(color)),
        ])
    } else if let Some(service) = app.selected_service() {
        let mut spans = vec![
            Span::styled("▸ ", Style::new().fg(Color::Cyan)),
            Span::styled(
                service.spec.name.clone(),
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {}", service.spec.relative_path()),
                Style::new().fg(Color::DarkGray),
            ),
            Span::styled(
                format!("  ·  {}", service.spec.command.display()),
                Style::new().fg(Color::Gray),
            ),
        ];
        if let Some(port) = service.port() {
            spans.push(Span::styled(
                format!("  ·  :{port}"),
                Style::new().fg(Color::Cyan),
            ));
        }
        Line::from(spans)
    } else {
        Line::from(Span::styled(
            "No services discovered",
            Style::new().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(
        Paragraph::new(line),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

fn render_empty(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width.saturating_sub(4).clamp(20, 78);
    let height = 15_u16.min(area.height);
    let popup = centered_fixed(area, width, height);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray))
        .title_top(Line::from(Span::styled(
            " No runnable projects ",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.height == 0 {
        return;
    }

    let mut lines = vec![
        Line::from(Span::styled(
            "No Node projects with a dev, start, or serve script were found.",
            Style::new().fg(Color::White),
        )),
        Line::from(Span::styled(
            app.root.display().to_string(),
            Style::new().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Add package.json projects under this directory,",
            Style::new().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            "or create services-manager.toml to define them explicitly.",
            Style::new().fg(Color::Gray),
        )),
    ];
    if let Some(warning) = app.warnings.first() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            warning.clone(),
            Style::new().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "r rescan  ·  ? help  ·  q quit",
        Style::new().fg(Color::Cyan),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::new())
            .wrap(Wrap { trim: false }),
        inner,
    );
}

pub(crate) fn status_color(status: ServiceStatus) -> Color {
    match status {
        ServiceStatus::Stopped => Color::DarkGray,
        ServiceStatus::Starting | ServiceStatus::Stopping => Color::Yellow,
        ServiceStatus::Ready => Color::Green,
        ServiceStatus::Error => Color::Red,
    }
}

pub(crate) fn kind_color(kind: ProjectKind) -> Color {
    match kind {
        ProjectKind::Mfe => Color::Magenta,
        ProjectKind::Service => Color::Blue,
        ProjectKind::App => Color::Gray,
    }
}

pub(crate) fn format_memory(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1}G", bytes as f64 / MIB / 1024.0)
    } else if bytes >= 1024 * 1024 {
        format!("{:.0}M", bytes as f64 / MIB)
    } else if bytes >= 1024 {
        format!("{:.0}K", bytes as f64 / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

pub(crate) fn centered_fixed(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

pub(crate) fn popup_percent(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Percentage(percent_x.clamp(10, 100))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Percentage(percent_y.clamp(10, 100))])
        .flex(Flex::Center)
        .areas(area);
    area
}
