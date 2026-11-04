//! Bundle manager, editor, and delete confirmation modals.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::{App, BundleEditorFocus, BundlePane};
use crate::bundles::Bundle;
use crate::ui::{centered_fixed, kind_color, popup_percent, status_color};

pub fn area(frame_area: Rect) -> Rect {
    popup_percent(frame_area, 92, 88)
}

pub fn editor_area(frame_area: Rect) -> Rect {
    popup_percent(frame_area, 86, 86)
}

pub fn delete_area(frame_area: Rect) -> Rect {
    centered_fixed(frame_area, 64, 10)
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = area(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title_top(Line::from(Span::styled(
            " Bundles ",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < 20 {
        return;
    }

    let [main_area, footer_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(inner);
    let [list_area, gap, detail_area] = Layout::horizontal([
        Constraint::Percentage(36),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(main_area);
    let _ = gap;

    render_bundle_list(frame, app, list_area);
    render_bundle_detail(frame, app, detail_area);

    let notice = app
        .bundle_ui
        .notice
        .clone()
        .or_else(|| app.bundle_error().map(str::to_string))
        .unwrap_or_else(|| {
            if app.bundles.is_empty() {
                "No bundles yet. Press n to create one.".to_string()
            } else {
                String::new()
            }
        });
    let notice_color = if app.bundle_error().is_some() {
        Color::Red
    } else {
        Color::Gray
    };
    let footer = Rect::new(footer_area.x, footer_area.y, footer_area.width, 1);
    frame.render_widget(
        Paragraph::new(notice)
            .style(Style::new().fg(notice_color))
            .wrap(Wrap { trim: true }),
        footer,
    );
    let hints = if app.bundle_ui.pane == BundlePane::Members {
        "↑↓ members · enter/o logs · tab back · d delete · e edit · n new · esc close"
    } else {
        "↑↓ bundles · tab members · enter enable/disable · n new · e edit · d delete · o logs · esc close"
    };
    frame.render_widget(
        Paragraph::new(hints).style(Style::new().fg(Color::DarkGray)),
        Rect::new(footer_area.x, footer_area.y + 1, footer_area.width, 1),
    );
}

fn render_bundle_list(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.bundle_ui.pane == BundlePane::List;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title_top(Line::from(Span::styled(
            " Saved bundles ",
            Style::new().fg(Color::Gray),
        )));
    let items: Vec<ListItem> = app
        .bundles
        .iter()
        .map(|bundle| ListItem::new(bundle_row(app, bundle)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::new()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(app.bundle_ui.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn bundle_row<'a>(app: &App, bundle: &Bundle) -> Line<'a> {
    let (ready, starting, error, _stopped, missing) = app.bundle_status_counts(bundle);
    let color = if error > 0 || missing > 0 {
        Color::Red
    } else if starting > 0 {
        Color::Yellow
    } else if ready > 0 {
        Color::Green
    } else {
        Color::DarkGray
    };
    let active = app.active_bundle_id == Some(bundle.id);
    let mut spans = vec![
        Span::styled(
            if active { "▶ " } else { "  " },
            Style::new().fg(Color::Green),
        ),
        Span::styled("● ", Style::new().fg(color)),
        Span::styled(bundle.name.clone(), Style::new().fg(Color::White)),
        Span::styled(
            format!("  {} svc", bundle.service_ids.len()),
            Style::new().fg(Color::DarkGray),
        ),
    ];
    if missing > 0 {
        spans.push(Span::styled(
            format!(" · {missing} missing"),
            Style::new().fg(Color::Red),
        ));
    }
    Line::from(spans)
}

fn render_bundle_detail(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.bundle_ui.pane == BundlePane::Members;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title_top(Line::from(Span::styled(
            " Details ",
            Style::new().fg(Color::Gray),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let Some(bundle) = app.selected_bundle() else {
        frame.render_widget(
            Paragraph::new("Select a bundle, or press n to create one.")
                .style(Style::new().fg(Color::DarkGray))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true }),
            inner,
        );
        return;
    };

    let (ready, starting, error, stopped, missing) = app.bundle_status_counts(bundle);
    let active = app.active_bundle_id == Some(bundle.id);
    let summary = format!(
        "{ready} ready · {starting} starting · {error} error · {stopped} stopped · {missing} missing"
    );
    let [header_area, member_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Fill(1)]).areas(inner);

    let mut title_spans = vec![Span::styled(
        bundle.name.clone(),
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
    )];
    title_spans.push(Span::raw("  "));
    if active {
        title_spans.push(Span::styled(
            "ENABLED",
            Style::new()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        title_spans.push(Span::styled("disabled", Style::new().fg(Color::DarkGray)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(title_spans)),
        Rect::new(header_area.x, header_area.y, header_area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(summary)
            .style(Style::new().fg(Color::Gray))
            .wrap(Wrap { trim: true }),
        Rect::new(
            header_area.x,
            header_area.y + 1,
            header_area.width,
            header_area.height.saturating_sub(1),
        ),
    );

    if bundle.service_ids.is_empty() {
        frame.render_widget(
            Paragraph::new("This bundle has no services.").style(Style::new().fg(Color::DarkGray)),
            member_area,
        );
        return;
    }

    let items: Vec<ListItem> = bundle
        .service_ids
        .iter()
        .map(|service_id| {
            let mut spans = Vec::new();
            if let Some(index) = app.service_index_by_id(service_id) {
                let service = &app.services[index];
                let status = service.status();
                let color = status_color(status);
                spans.push(Span::styled("● ", Style::new().fg(color)));
                spans.push(Span::styled(
                    format!("{:<10}", status.label()),
                    Style::new().fg(color),
                ));
                spans.push(Span::styled(
                    format!("{:<20}", service.spec.name),
                    Style::new().fg(Color::White),
                ));
                spans.push(Span::styled(
                    format!("{:<24}", service.spec.relative_path()),
                    Style::new().fg(Color::DarkGray),
                ));
                spans.push(Span::styled(
                    service.spec.kind.badge(),
                    Style::new().fg(kind_color(service.spec.kind)),
                ));
                if let Some(port) = service.port() {
                    spans.push(Span::styled(
                        format!("  :{port}"),
                        Style::new().fg(Color::Cyan),
                    ));
                }
            } else {
                spans.push(Span::styled("● ", Style::new().fg(Color::Red)));
                spans.push(Span::styled(
                    format!("{:<10}", "missing"),
                    Style::new().fg(Color::Red),
                ));
                spans.push(Span::styled(
                    service_id.clone(),
                    Style::new().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::new()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = ListState::default().with_selected(Some(app.bundle_ui.member_selected));
    frame.render_stateful_widget(list, member_area, &mut state);
}

pub fn render_editor(frame: &mut Frame, app: &mut App) {
    let Some(editor) = app.bundle_ui.editor.as_ref() else {
        return;
    };
    let area = editor_area(frame.area());
    frame.render_widget(Clear, area);
    let title = if editor.bundle_id.is_some() {
        " Edit bundle "
    } else {
        " New bundle "
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title_top(Line::from(Span::styled(
            title,
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    let [name_area, error_area, services_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(2),
    ])
    .areas(inner);

    let name_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if editor.focus == BundleEditorFocus::Name {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title_top(Line::from(Span::styled(
            " Bundle name ",
            Style::new().fg(Color::Gray),
        )));
    let name_inner = name_block.inner(name_area);
    frame.render_widget(name_block, name_area);
    let name_text = if editor.name.is_empty() {
        Span::styled("type a name…", Style::new().fg(Color::DarkGray))
    } else {
        Span::styled(editor.name.clone(), Style::new().fg(Color::White))
    };
    frame.render_widget(Paragraph::new(Line::from(name_text)), name_inner);
    if editor.focus == BundleEditorFocus::Name {
        let x = name_inner
            .x
            .saturating_add(editor.name.chars().count() as u16)
            .min(
                name_inner
                    .x
                    .saturating_add(name_inner.width.saturating_sub(1)),
            );
        frame.set_cursor_position((x, name_inner.y));
    }

    if let Some(error) = &editor.error {
        frame.render_widget(
            Paragraph::new(error.clone()).style(Style::new().fg(Color::Red)),
            error_area,
        );
    } else if editor.focus == BundleEditorFocus::Name {
        frame.render_widget(
            Paragraph::new("Tab to select services · Ctrl-S to save")
                .style(Style::new().fg(Color::DarkGray)),
            error_area,
        );
    } else {
        frame.render_widget(
            Paragraph::new("Space/Enter to toggle · Tab back to the name")
                .style(Style::new().fg(Color::DarkGray)),
            error_area,
        );
    }

    let focused = editor.focus == BundleEditorFocus::Services;
    let services_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        }))
        .title_top(Line::from(Span::styled(
            " Select services ",
            Style::new().fg(Color::Gray),
        )));
    let services_inner = services_block.inner(services_area);
    frame.render_widget(services_block, services_area);
    if app.services.is_empty() {
        frame.render_widget(
            Paragraph::new("No services are available in this workspace.")
                .style(Style::new().fg(Color::DarkGray)),
            services_inner,
        );
    } else {
        let items: Vec<ListItem> = app
            .services
            .iter()
            .map(|service| {
                let checked = editor.selected.contains(&service.spec.id);
                let marker = if checked { "[x]" } else { "[ ]" };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{marker} "),
                        Style::new().fg(if checked {
                            Color::Green
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(
                        format!("{} ", service.spec.kind.badge()),
                        Style::new().fg(kind_color(service.spec.kind)),
                    ),
                    Span::styled(
                        format!("{:<22}", service.spec.name),
                        Style::new().fg(Color::White),
                    ),
                    Span::styled(
                        service.spec.relative_path(),
                        Style::new().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(
                Style::new()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        let cursor = editor.cursor.min(app.services.len().saturating_sub(1));
        let mut state = ListState::default().with_selected(Some(cursor));
        frame.render_stateful_widget(list, services_inner, &mut state);
    }

    let selected_count = editor.selected.len();
    let footer_line = if editor.missing_services > 0 {
        format!(
            "{selected_count} selected · {} missing services will be dropped on save",
            editor.missing_services
        )
    } else {
        format!("{selected_count} selected")
    };
    frame.render_widget(
        Paragraph::new(footer_line).style(Style::new().fg(Color::Gray)),
        Rect::new(footer_area.x, footer_area.y, footer_area.width, 1),
    );
    frame.render_widget(
        Paragraph::new("Tab focus · ↑↓ select · Space/Enter toggle · Ctrl-S save · Esc cancel")
            .style(Style::new().fg(Color::DarkGray)),
        Rect::new(footer_area.x, footer_area.y + 1, footer_area.width, 1),
    );
}

pub fn render_delete(frame: &mut Frame, app: &App, bundle_id: i64) {
    let area = delete_area(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Red).add_modifier(Modifier::BOLD))
        .title_top(Line::from(Span::styled(
            " Delete bundle ",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(bundle) = app.bundles.iter().find(|bundle| bundle.id == bundle_id) else {
        frame.render_widget(
            Paragraph::new("Bundle no longer exists.")
                .alignment(Alignment::Center)
                .style(Style::new().fg(Color::Red)),
            inner,
        );
        return;
    };

    let active = app.active_bundle_id == Some(bundle_id);
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("Delete \"{}\"?", bundle.name),
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    if active {
        lines.push(Line::from(Span::styled(
            "This bundle is enabled; its running services will be stopped.",
            Style::new().fg(Color::Yellow),
        )));
    }
    lines.push(Line::from(Span::styled(
        "This cannot be undone.",
        Style::new().fg(Color::Gray),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            "[y] delete",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("        "),
        Span::styled("[n] cancel", Style::new().fg(Color::Gray)),
    ]));
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}
