use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::App;

use super::{badge, fit_w, track_spans, BadgeKind, TrackCols, ACCENT, BADGE_W, DIM};

/// Charts + new releases as one selectable list with section headers.
/// Returns the list hit area + scroll offset for mouse support.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Option<(Rect, usize)> {
    let home = &app.home;
    if home.loading && home.row_count() == 0 {
        centered_hint(f, area, "加载推荐内容中..");
        return None;
    }
    if home.row_count() == 0 {
        centered_hint(
            f,
            area,
            "推荐内容加载失败\n\ng 重试   / 搜索   L 音乐库   ? 帮助",
        );
        return None;
    }

    // The rank prefix eats 3 columns before the shared track layout starts.
    let cols = TrackCols::new(area.width.saturating_sub(3));
    let mut items: Vec<ListItem> = Vec::new();
    items.push(section_header("热门歌曲", area.width));
    for (i, t) in home.tracks.iter().enumerate() {
        let mut line = vec![Span::styled(
            format!("{:>2} ", i + 1),
            Style::default().fg(DIM),
        )];
        line.extend(track_spans(t, cols));
        items.push(ListItem::new(Line::from(line)));
    }
    items.push(section_header("新专辑", area.width));
    for a in &home.albums {
        let year = a.year.map(|y| format!("{y}")).unwrap_or_default();
        items.push(ListItem::new(Line::from(vec![
            badge(BadgeKind::Album),
            Span::raw(fit_w(&a.title, cols.title.saturating_sub(BADGE_W))),
            Span::styled(
                format!(" {}", fit_w(&a.artists, cols.artist)),
                Style::default().fg(DIM),
            ),
            Span::styled(format!(" {year:>5}"), Style::default().fg(DIM)),
        ])));
    }

    // Map the logical selection onto the physical list (skip headers).
    let sel_item = if home.selected < home.tracks.len() {
        1 + home.selected
    } else {
        2 + home.tracks.len() + (home.selected - home.tracks.len())
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(sel_item));
    f.render_stateful_widget(list, area, &mut state);
    Some((area, state.offset()))
}

/// A bold label followed by a rule, so sections read as sections without
/// relying on emoji that terminals size differently.
fn section_header(title: &str, width: u16) -> ListItem<'static> {
    use unicode_width::UnicodeWidthStr;
    let rule_w = (width as usize).saturating_sub(title.width() + 3);
    ListItem::new(Line::from(vec![
        Span::styled(
            title.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {}", "-".repeat(rule_w)), Style::default().fg(DIM)),
    ]))
}

fn centered_hint(f: &mut Frame, area: Rect, text: &str) {
    f.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        area,
    );
}
