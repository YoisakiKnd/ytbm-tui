use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::{ACCENT, DIM};

const KEYS: &[(&str, &str)] = &[
    ("/", "搜索"),
    ("L", "音乐库 / 登录 (浏览器一键导入)"),
    ("1-4 / [ ]", "搜索结果分类切换"),
    ("j/k ↑/↓", "移动选择"),
    ("g/G PgUp/Dn", "跳顶/跳底/翻页"),
    ("Enter", "播放 / 打开"),
    ("a / A", "加入队列 / 下一首播放"),
    ("P", "整页加入队列并播放"),
    ("Tab", "主面板 ⇄ 队列"),
    ("x", "队列移除 / 音乐库登出"),
    ("J/K", "队列中下移/上移"),
    ("Space", "暂停 / 继续"),
    ("n / p", "下一首 / 上一首"),
    ("← / →", "快退 / 快进 5s"),
    ("- / =", "音量 -/+ 5%"),
    ("m", "静音"),
    ("r", "循环模式 关→全部→单曲"),
    ("t", "电台自动续播开关"),
    ("s", "打乱待播队列"),
    ("l", "正在播放（封面 + 歌词）"),
    ("R", "重启播放器 (mpv 崩溃后)"),
    ("Esc", "返回 / 关闭"),
    ("q", "退出"),
];

pub fn draw_help(f: &mut Frame) {
    let area = centered(f.area(), 46, KEYS.len() as u16 + 4);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(" 快捷键 (按任意键关闭) ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(key, desc)| {
            Line::from(vec![
                Span::styled(
                    format!("  {key:<12}"),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled((*desc).to_string(), Style::default()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).style(Style::default().fg(DIM)), inner);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let [mid_v] = Layout::vertical([Constraint::Length(h.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [mid] = Layout::horizontal([Constraint::Length(w.min(area.width))])
        .flex(Flex::Center)
        .areas(mid_v);
    mid
}
