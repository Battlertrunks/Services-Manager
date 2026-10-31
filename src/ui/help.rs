//! Help and quit-confirmation modals.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::App;
use crate::ui::{centered_fixed, popup_percent};

pub fn help_area(frame_area: Rect) -> Rect {
    popup_percent(frame_area, 88, 86)
}

pub fn quit_area(frame_area: Rect) -> Rect {
    centered_fixed(frame_area, 62, 9)
}

pub fn render_help(frame: &mut Frame, app: &mut App) {
    let area = help_area(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Cyan))
        .title_top(Line::from(ratatui::text::Span::styled(
            " Help ",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let [body_area, footer_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(inner);

    let raw_lines = app.help_lines();
    let max_scroll = raw_lines.len().saturating_sub(body_area.height as usize) as u16;
    app.help_scroll = app.help_scroll.min(max_scroll);

    let lines: Vec<Line<'static>> = raw_lines
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let style = if index == 0 {
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else if index == 1 {
                Style::new().fg(Color::Gray)
            } else if is_section_heading(&text) {
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if text.trim_start().starts_with('!') {
                Style::new().fg(Color::Yellow)
            } else {
                Style::new().fg(Color::Gray)
            };
            Line::from(ratatui::text::Span::styled(text, style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).scroll((app.help_scroll, 0)),
        body_area,
    );
    frame.render_widget(
        Paragraph::new("↑↓ scroll · Esc/q close").style(Style::new().fg(Color::DarkGray)),
        footer_area,
    );
}

fn is_section_heading(text: &str) -> bool {
    matches!(
        text,
        "Navigation" | "Logs" | "Mouse" | "Workspace" | "Warnings"
    )
}

pub fn render_quit(frame: &mut Frame, app: &App) {
    let area = quit_area(frame.area());
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .title_top(Line::from(ratatui::text::Span::styled(
            " Quit services-manager? ",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let running = app.running_count();
    let lines = vec![
        Line::from(""),
        Line::from(ratatui::text::Span::styled(
            format!(
                "{running} service{} still running.",
                if running == 1 { " is" } else { "s are" }
            ),
            Style::new().fg(Color::White),
        )),
        Line::from(ratatui::text::Span::styled(
            "They will be stopped before services-manager exits.",
            Style::new().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(vec![
            ratatui::text::Span::styled(
                "[y] quit",
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            ratatui::text::Span::raw("        "),
            ratatui::text::Span::styled("[n] cancel", Style::new().fg(Color::Gray)),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}
