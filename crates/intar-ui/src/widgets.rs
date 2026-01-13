use crate::app::MainTab;
use crate::colors::Theme;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Widget, Wrap},
};
use std::{borrow::Cow, time::Duration};

const SPINNER_FRAMES: [char; 4] = ['◐', '◓', '◑', '◒'];
const CREDITS_SCROLL_MS_PER_LINE: u128 = 700;

#[must_use]
pub fn spinner_char(tick: usize) -> char {
    SPINNER_FRAMES[(tick / 3) % SPINNER_FRAMES.len()]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStatus {
    Pending,
    Passed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmStatus {
    Starting,
    Booting,
    CloudInit,
    Ready,
    Error,
    Unknown,
}

pub struct VmTreeProbe<'a> {
    pub name: Cow<'a, str>,
    pub status: ProbeStatus,
    pub description: Option<Cow<'a, str>>,
}

pub struct VmTreeNode<'a> {
    pub name: Cow<'a, str>,
    pub status: VmStatus,
    pub cpu: u32,
    pub memory: u32,
    pub disk: u32,
    pub ssh_port: Option<u16>,
    pub boot_passing: usize,
    pub boot_total: usize,
    pub scenario_probes: Vec<VmTreeProbe<'a>>,
}

pub struct ScenarioTreeScreen<'a> {
    pub scenario_name: &'a str,
    pub scenario_description: &'a str,
    pub run_name: Option<&'a str>,
    pub phase: &'a str,
    pub boot_elapsed: Option<Duration>,
    pub run_elapsed: Option<Duration>,
    pub vms: &'a [VmTreeNode<'a>],
    pub action_lines: &'a [Line<'static>],
    pub scroll: u16,
    pub theme: &'a Theme,
    pub tick: usize,
    pub active_tab: MainTab,
}

pub struct BriefingScreen<'a> {
    pub scenario_name: &'a str,
    pub scenario_description: &'a str,
    pub run_name: Option<&'a str>,
    pub phase: &'a str,
    pub boot_elapsed: Option<Duration>,
    pub run_elapsed: Option<Duration>,
    pub vms: &'a [VmTreeNode<'a>],
    pub theme: &'a Theme,
    pub tick: usize,
}

pub struct CompletedScreen<'a> {
    pub scenario_name: &'a str,
    pub run_name: Option<&'a str>,
    pub solve_duration: Duration,
    pub credits: Vec<Line<'static>>,
    pub credits_elapsed: Duration,
    pub theme: &'a Theme,
}

impl CompletedScreen<'_> {
    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::new(1, 1, 0, 0))
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(self.theme.success))
            .style(Style::default().bg(self.theme.surface))
            .title(" EXECUTION SUMMARY ")
            .title_style(Style::default().fg(self.theme.primary).bold());

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 {
            return;
        }

        let duration = format_duration(self.solve_duration);
        let run = self.run_name.unwrap_or("—");
        let lines = vec![
            Line::from(vec![
                Span::styled("SCENARIO: ", Style::default().fg(self.theme.secondary)),
                Span::styled(
                    self.scenario_name,
                    Style::default().fg(self.theme.primary).bold(),
                ),
            ]),
            Line::from(vec![
                Span::styled("RUN-ID ", Style::default().fg(self.theme.secondary)),
                Span::styled(run, Style::default().fg(self.theme.info).bold()),
                Span::styled("  |  TIME ", Style::default().fg(self.theme.secondary)),
                Span::styled(duration, Style::default().fg(self.theme.primary).bold()),
                Span::styled("  |  STATUS ", Style::default().fg(self.theme.secondary)),
                Span::styled("COMPLETE", Style::default().fg(self.theme.success).bold()),
            ]),
        ];

        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(Style::default().bg(self.theme.surface))
            .render(inner, buf);
    }

    fn render_credits(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::uniform(1))
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(self.theme.border))
            .style(Style::default().bg(self.theme.surface))
            .title(" SYSTEM LOG ")
            .title_style(Style::default().fg(self.theme.secondary));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 || inner.width == 0 {
            return;
        }

        let view_height = inner.height as usize;
        let total = self.credits.len();
        let max_scroll = total.saturating_sub(view_height);
        let scroll_lines =
            usize::try_from(self.credits_elapsed.as_millis() / CREDITS_SCROLL_MS_PER_LINE)
                .unwrap_or(usize::MAX);
        let top_offset = scroll_lines.min(max_scroll);
        let top_offset_u16 = u16::try_from(top_offset).unwrap_or(u16::MAX);

        Paragraph::new(self.credits.clone())
            .scroll((top_offset_u16, 0))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(self.theme.fg).bg(self.theme.surface))
            .render(inner, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(self.theme.border))
            .style(Style::default().bg(self.theme.surface));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 {
            return;
        }

        let key_style = if self.theme.is_monochrome() {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .fg(self.theme.on_secondary)
                .bg(self.theme.secondary)
                .bold()
        };

        let keys = vec![
            ("?", "Help"),
            ("R", "Restart"),
            ("T", "Theme"),
            ("Q", "Quit"),
        ];

        let mut spans = Vec::new();
        for (key, desc) in keys {
            spans.push(Span::styled(format!(" {key} "), key_style));
            spans.push(Span::styled(
                format!(" {desc} "),
                Style::default().fg(self.theme.dim),
            ));
            spans.push(Span::raw(" "));
        }

        let content_area = Layout::vertical([Constraint::Length(1)])
            .flex(ratatui::layout::Flex::Center)
            .split(inner)[0];

        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Left)
            .render(content_area, buf);
    }
}

impl BriefingScreen<'_> {
    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(self.theme.primary))
            .style(Style::default().bg(self.theme.surface))
            .title(" MISSION BRIEFING ")
            .title_style(Style::default().fg(self.theme.primary).bold());

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 {
            return;
        }

        let run_id = self.run_name.unwrap_or("—");
        let boot_timer = format_duration_or_placeholder(self.boot_elapsed);
        let run_timer = format_duration_or_placeholder(self.run_elapsed);
        let spinner = spinner_char(self.tick);

        let lines = vec![
            Line::from(vec![
                Span::styled("OPERATION ", Style::default().fg(self.theme.secondary)),
                Span::styled(
                    self.scenario_name.to_uppercase(),
                    Style::default().fg(self.theme.fg).bold(),
                ),
            ]),
            Line::from(vec![
                Span::styled("STATUS ", Style::default().fg(self.theme.dim)),
                Span::styled(
                    self.phase.to_uppercase(),
                    Style::default().fg(self.theme.warning).bold(),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{spinner} BOOT {boot_timer}"),
                    Style::default().fg(self.theme.primary),
                ),
                Span::raw("  "),
                Span::styled("RUN ", Style::default().fg(self.theme.dim)),
                Span::styled(run_timer, Style::default().fg(self.theme.primary)),
                Span::raw("  "),
                Span::styled("ID ", Style::default().fg(self.theme.dim)),
                Span::styled(run_id, Style::default().fg(self.theme.info)),
            ]),
        ];

        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .style(Style::default().bg(self.theme.surface))
            .render(inner, buf);
    }

    fn render_boot_status(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::new(1, 1, 0, 0))
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(self.theme.border))
            .style(Style::default().bg(self.theme.surface))
            .title(" BOOT STATUS ")
            .title_style(Style::default().fg(self.theme.secondary));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height == 0 {
            return;
        }

        if self.vms.is_empty() {
            Paragraph::new("No VMs defined.")
                .alignment(Alignment::Center)
                .style(Style::default().fg(self.theme.dim))
                .render(inner, buf);
            return;
        }

        let name_width = self
            .vms
            .iter()
            .map(|vm| vm.name.len())
            .max()
            .unwrap_or(0)
            .clamp(6, 18);

        let mut lines = Vec::new();
        for vm in self.vms.iter().take(inner.height as usize) {
            let (status_label, status_color) = vm_status_label(self.theme, vm.status);
            let (status_icon, icon_color) = vm_status_icon(self.theme, vm.status);
            let boot_total = vm.boot_total;
            let boot_label = if boot_total == 0 {
                "boot —".to_string()
            } else {
                format!("boot {}/{}", vm.boot_passing, boot_total)
            };

            lines.push(Line::from(vec![
                Span::styled(format!("{status_icon} "), Style::default().fg(icon_color)),
                Span::styled(
                    format!(
                        "{name:<width$}",
                        name = vm.name.as_ref(),
                        width = name_width
                    ),
                    Style::default().fg(self.theme.fg).bold(),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{status_label:<5}"),
                    Style::default().fg(status_color).bold(),
                ),
                Span::raw("  "),
                Span::styled(boot_label, Style::default().fg(self.theme.secondary)),
            ]));
        }

        Paragraph::new(lines)
            .style(Style::default().bg(self.theme.surface))
            .render(inner, buf);
    }

    fn briefing_boot_height(&self, total_height: u16) -> u16 {
        if self.vms.is_empty() || total_height < 5 {
            return 0;
        }

        let needed = u16::try_from(self.vms.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2);
        let min_boot = 3u16.min(total_height);
        needed.min(total_height).max(min_boot)
    }
}

impl ScenarioTreeScreen<'_> {
    fn render_main_layout(&self, area: Rect, buf: &mut Buffer) {
        // Neo-Brutalist Layout:
        // Top: Header (height 3) with solid border
        // Middle: Full-width content
        // Bottom: Footer (height 1) with solid border

        let vertical = Layout::vertical([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main Content
            Constraint::Length(3), // Footer (increased for visibility)
        ])
        .split(area);

        self.render_header(vertical[0], buf);

        self.render_tab_viewport(vertical[1], buf);

        self.render_footer(vertical[2], buf);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(self.theme.primary))
            .style(Style::default().bg(self.theme.surface));

        let inner = block.inner(area);
        block.render(area, buf);

        let run_id = self.run_name.unwrap_or("—");
        let boot_timer = format_duration_or_placeholder(self.boot_elapsed);
        let run_timer = format_duration_or_placeholder(self.run_elapsed);
        let spinner = spinner_char(self.tick);

        let title_style = if self.theme.is_monochrome() {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .bg(self.theme.primary)
                .fg(self.theme.on_primary)
                .bold()
        };

        let left = Line::from(vec![
            Span::styled(" INTAR CLI ", title_style),
            Span::styled(" ", Style::default()),
            Span::styled(
                self.scenario_name.to_uppercase(),
                Style::default().fg(self.theme.fg).bold(),
            ),
        ]);

        let right = Line::from(vec![
            Span::styled(
                format!(" {spinner} "),
                Style::default().fg(self.theme.secondary),
            ),
            Span::styled(
                self.phase.to_uppercase(),
                Style::default().fg(self.theme.warning).bold(),
            ),
            Span::styled(" | ", Style::default().fg(self.theme.dim)),
            Span::styled("BOOT ", Style::default().fg(self.theme.dim)),
            Span::styled(boot_timer, Style::default().fg(self.theme.primary).bold()),
            Span::styled(" | ", Style::default().fg(self.theme.dim)),
            Span::styled("RUN ", Style::default().fg(self.theme.dim)),
            Span::styled(run_timer, Style::default().fg(self.theme.primary).bold()),
            Span::styled(" | ", Style::default().fg(self.theme.dim)),
            Span::styled(format!("[{run_id}] "), Style::default().fg(self.theme.dim)),
        ]);

        // Centered vertically in the header block
        let content_area = Layout::vertical([Constraint::Length(1)])
            .flex(ratatui::layout::Flex::Center)
            .split(inner)[0];

        Paragraph::new(left).render(content_area, buf);
        Paragraph::new(right)
            .alignment(Alignment::Right)
            .render(content_area, buf);
    }

    fn render_tab_viewport(&self, area: Rect, buf: &mut Buffer) {
        Block::default()
            .style(Style::default().bg(self.theme.surface))
            .render(area, buf);

        let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);

        self.render_tab_header(chunks[0], buf);

        let content_block = Block::default()
            .style(Style::default().bg(self.theme.surface))
            .padding(Padding::new(1, 1, 1, 1));
        let content_area = content_block.inner(chunks[1]);
        content_block.render(chunks[1], buf);

        if content_area.height == 0 || content_area.width == 0 {
            return;
        }

        match self.active_tab {
            MainTab::Briefing => self.render_briefing_view(content_area, buf),
            MainTab::Logs => self.render_logs_view(content_area, buf),
            MainTab::System => self.render_system_view(content_area, buf),
        }
    }

    fn render_tab_header(&self, area: Rect, buf: &mut Buffer) {
        let tabs = vec![
            (MainTab::Briefing, " BRIEFING "),
            (MainTab::Logs, " LOGS "),
            (MainTab::System, " SYSTEM "),
        ];

        let mut spans = Vec::new();
        for (tab, label) in tabs {
            let is_active = self.active_tab == tab;
            let style = if is_active {
                if self.theme.is_monochrome() {
                    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
                } else {
                    Style::default()
                        .bg(self.theme.primary)
                        .fg(self.theme.on_primary)
                        .bold()
                }
            } else {
                Style::default().fg(self.theme.dim)
            };

            let prefix = if is_active { "▶ " } else { "  " };
            let suffix = if is_active { " ◀" } else { "  " };

            if !spans.is_empty() {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(format!("{prefix}{label}{suffix}"), style));
        }

        let block = Block::default()
            .style(Style::default().bg(self.theme.surface))
            .padding(Padding::new(1, 1, 0, 0));

        let inner = block.inner(area);
        block.render(area, buf);

        let content_area = Layout::vertical([Constraint::Length(1)])
            .flex(ratatui::layout::Flex::Center)
            .split(inner)[0];

        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Center)
            .render(content_area, buf);
    }

    fn render_system_view(&self, area: Rect, buf: &mut Buffer) {
        if self.vms.is_empty() {
            Paragraph::new("No VMs available")
                .alignment(Alignment::Center)
                .style(Style::default().fg(self.theme.dim))
                .render(area, buf);
            return;
        }

        self.render_system_tree(area, buf);
    }

    fn render_briefing_view(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let columns = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(area);
        render_context_panel(self.theme, self.scenario_description, columns[0], buf);
        render_objectives_panel(self.theme, self.vms, columns[1], buf);
    }

    fn render_system_tree(&self, area: Rect, buf: &mut Buffer) {
        let mut row = 0u16;
        let max_width = area.width;
        for vm in self.vms {
            if row >= area.height {
                break;
            }

            let (status_label, status_color) = vm_status_label(self.theme, vm.status);
            let (status_icon, icon_color) = vm_status_icon(self.theme, vm.status);

            let scenario_total = vm.scenario_probes.len();
            let scenario_passed = vm
                .scenario_probes
                .iter()
                .filter(|probe| probe.status == ProbeStatus::Passed)
                .count();
            let scen_label = if scenario_total == 0 {
                "scen —".to_string()
            } else {
                format!("scen {scenario_passed}/{scenario_total}")
            };

            let header = Line::from(vec![
                Span::styled(format!("{status_icon} "), Style::default().fg(icon_color)),
                Span::styled(vm.name.as_ref(), Style::default().fg(self.theme.fg).bold()),
                Span::raw(" "),
                Span::styled(status_label, Style::default().fg(status_color)),
                Span::raw("  "),
                Span::styled(scen_label, Style::default().fg(self.theme.secondary)),
            ]);

            let leaf_style = Style::default().fg(self.theme.dim);
            let value_style = Style::default().fg(self.theme.info);
            let leaf_line = |connector: &str, label: &str, value: String| {
                Line::from(vec![
                    Span::styled(format!("  {connector} "), leaf_style),
                    Span::styled(format!("{label:<4}"), leaf_style),
                    Span::raw(" "),
                    Span::styled(value, value_style),
                ])
            };
            let cpu_line = leaf_line("├─", "CPU", format!("{cpu} vCPU", cpu = vm.cpu));
            let mem_line = leaf_line("├─", "MEM", format!("{memory} MB", memory = vm.memory));
            let disk_line = leaf_line("├─", "DISK", format!("{disk} GB", disk = vm.disk));
            let ssh_line = leaf_line(
                "└─",
                "SSH",
                vm.ssh_port.map_or("—".to_string(), |p| p.to_string()),
            );

            for line in [header, cpu_line, mem_line, disk_line, ssh_line] {
                if row >= area.height {
                    break;
                }
                let line_area = Rect {
                    x: area.x,
                    y: area.y + row,
                    width: max_width,
                    height: 1,
                };
                Paragraph::new(line).render(line_area, buf);
                row = row.saturating_add(1);
            }
        }
    }

    fn render_logs_view(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let header_height = if area.height >= 2 { 2 } else { 0 };
        let (header_area, logs_area) = if header_height > 0 {
            let chunks = Layout::vertical([Constraint::Length(header_height), Constraint::Min(0)])
                .split(area);
            (Some(chunks[0]), chunks[1])
        } else {
            (None, area)
        };

        if let Some(header_area) = header_area {
            let header_lines = vec![
                Line::from(Span::styled(
                    "SSH session transcript",
                    Style::default().fg(self.theme.secondary).bold(),
                )),
                Line::from(Span::styled(
                    "SSH input and output will appear here.",
                    Style::default().fg(self.theme.dim),
                )),
            ];
            Paragraph::new(header_lines)
                .style(Style::default().bg(self.theme.surface))
                .wrap(Wrap { trim: true })
                .render(header_area, buf);
        }

        if self.action_lines.is_empty() {
            Paragraph::new("No SSH output yet.")
                .style(Style::default().fg(self.theme.dim))
                .alignment(Alignment::Center)
                .render(logs_area, buf);
            return;
        }

        let total = self.action_lines.len();
        let view_height = logs_area.height as usize;
        let max_scroll = total.saturating_sub(view_height);
        let max_scroll_u16 = u16::try_from(max_scroll).unwrap_or(u16::MAX);
        let scroll = self.scroll.min(max_scroll_u16) as usize;
        let start = total.saturating_sub(view_height).saturating_sub(scroll);
        let end = (start + view_height).min(total);
        let styled_lines: Vec<Line> = self.action_lines[start..end].to_vec();

        Paragraph::new(styled_lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(self.theme.surface))
            .render(logs_area, buf);
    }

    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        // Footer now has a block
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(self.theme.border))
            .style(Style::default().bg(self.theme.surface));

        let inner = block.inner(area);
        block.render(area, buf);

        let keys = vec![
            ("TAB", "View"),
            ("PGUP/PGDN", "Scroll"),
            ("?", "Help"),
            ("T", "Theme"),
            ("R", "Restart"),
            ("Q", "Quit"),
        ];

        let mut spans = Vec::new();
        for (key, desc) in keys {
            let key_style = if self.theme.is_monochrome() {
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                Style::default()
                    .fg(self.theme.on_secondary)
                    .bg(self.theme.secondary)
                    .bold()
            };
            spans.push(Span::styled(format!(" {key} "), key_style));
            spans.push(Span::styled(
                format!(" {desc} "),
                Style::default().fg(self.theme.dim),
            ));
            spans.push(Span::raw(" "));
        }

        // Center the content in the footer block
        let content_area = Layout::vertical([Constraint::Length(1)])
            .flex(ratatui::layout::Flex::Center)
            .split(inner)[0];

        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Left)
            .render(content_area, buf);
    }
}

fn vm_status_label(theme: &Theme, status: VmStatus) -> (&'static str, Color) {
    match status {
        VmStatus::Ready => ("READY", theme.success),
        VmStatus::Booting | VmStatus::CloudInit => ("BOOT", theme.warning),
        VmStatus::Starting => ("START", theme.warning),
        VmStatus::Error => ("ERROR", theme.error),
        VmStatus::Unknown => ("WAIT", theme.dim),
    }
}

fn vm_status_icon(theme: &Theme, status: VmStatus) -> (&'static str, Color) {
    match status {
        VmStatus::Ready => ("●", theme.success),
        VmStatus::Booting | VmStatus::CloudInit | VmStatus::Starting => ("●", theme.warning),
        VmStatus::Error => ("●", theme.error),
        VmStatus::Unknown => ("○", theme.dim),
    }
}

impl Widget for CompletedScreen<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let footer_height = 3u16.min(area.height);
        let available = area.height.saturating_sub(footer_height);
        let header_height = 4u16.min(available);

        let chunks = Layout::vertical([
            Constraint::Length(header_height),
            Constraint::Min(area.height.saturating_sub(header_height + footer_height)),
            Constraint::Length(footer_height),
        ])
        .split(area);

        self.render_header(chunks[0], buf);
        self.render_credits(chunks[1], buf);
        self.render_footer(chunks[2], buf);
    }
}

impl Widget for BriefingScreen<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let mut remaining = area.height;
        let header_height = 4u16.min(remaining);
        remaining = remaining.saturating_sub(header_height);
        let body_area = Rect {
            x: area.x,
            y: area.y + header_height,
            width: area.width,
            height: remaining,
        };

        let columns = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(body_area);
        let left_area = columns[0];
        let right_area = columns[1];

        let min_objectives = 6u16.min(right_area.height);
        let mut boot_height = self.briefing_boot_height(right_area.height);
        boot_height = boot_height.min(right_area.height.saturating_sub(min_objectives));
        let objectives_height = right_area.height.saturating_sub(boot_height);

        let right_chunks = Layout::vertical([
            Constraint::Length(objectives_height),
            Constraint::Length(boot_height),
        ])
        .split(right_area);

        if header_height > 0 {
            let header_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: header_height,
            };
            self.render_header(header_area, buf);
        }
        if left_area.height > 0 && left_area.width > 0 {
            render_context_panel(self.theme, self.scenario_description, left_area, buf);
        }
        if objectives_height > 0 {
            render_objectives_panel(self.theme, self.vms, right_chunks[0], buf);
        }
        if boot_height > 0 {
            self.render_boot_status(right_chunks[1], buf);
        }
    }
}

impl Widget for ScenarioTreeScreen<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        self.render_main_layout(area, buf);
    }
}

pub struct ConfirmDialog<'a> {
    pub title: &'a str,
    pub message: &'a str,
    pub theme: &'a Theme,
}

pub struct HelpOverlay<'a> {
    pub theme: &'a Theme,
}

impl Widget for ConfirmDialog<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let dialog_width = 50u16;
        let dialog_height = 9u16;

        let x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
        let y = area.y + (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect {
            x,
            y,
            width: dialog_width.min(area.width),
            height: dialog_height.min(area.height),
        };

        Clear.render(dialog_area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::uniform(1))
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(self.theme.warning))
            .style(Style::default().bg(self.theme.surface))
            .title(format!(" {title} ", title = self.title))
            .title_style(Style::default().fg(self.theme.primary).bold());

        let inner = block.inner(dialog_area);
        block.render(dialog_area, buf);

        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

        for (i, line) in self.message.lines().enumerate() {
            if i < 2 {
                Paragraph::new(line)
                    .style(Style::default().fg(self.theme.primary))
                    .alignment(Alignment::Center)
                    .render(chunks[i + 1], buf);
            }
        }

        let buttons = Line::from(vec![
            Span::styled("[Y]", Style::default().fg(self.theme.success).bold()),
            Span::styled("es", Style::default().fg(self.theme.primary)),
            Span::raw("          "),
            Span::styled("[N]", Style::default().fg(self.theme.error).bold()),
            Span::styled("o", Style::default().fg(self.theme.primary)),
        ]);
        Paragraph::new(buttons)
            .alignment(Alignment::Center)
            .render(chunks[4], buf);
    }
}

impl Widget for HelpOverlay<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let dialog_width = 64u16;
        let dialog_height = 13u16;

        let x = area.x + (area.width.saturating_sub(dialog_width)) / 2;
        let y = area.y + (area.height.saturating_sub(dialog_height)) / 2;

        let dialog_area = Rect {
            x,
            y,
            width: dialog_width.min(area.width),
            height: dialog_height.min(area.height),
        };

        Clear.render(dialog_area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::uniform(1))
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(self.theme.border))
            .style(Style::default().bg(self.theme.surface))
            .title(" HELP ")
            .title_style(Style::default().fg(self.theme.primary).bold());

        let inner = block.inner(dialog_area);
        block.render(dialog_area, buf);

        let key_style = if self.theme.is_monochrome() {
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default()
                .fg(self.theme.on_secondary)
                .bg(self.theme.secondary)
                .bold()
        };

        let lines = vec![
            Line::from(vec![
                Span::styled(" TAB ", key_style),
                Span::raw(" Switch view"),
            ]),
            Line::from(vec![
                Span::styled(" SHIFT+TAB ", key_style),
                Span::raw(" Previous view"),
            ]),
            Line::from(vec![
                Span::styled(" PGUP/PGDN ", key_style),
                Span::raw(" Scroll logs"),
            ]),
            Line::from(vec![
                Span::styled(" HOME/END ", key_style),
                Span::raw(" Oldest / newest logs"),
            ]),
            Line::from(vec![
                Span::styled(" R ", key_style),
                Span::raw(" Restart scenario"),
            ]),
            Line::from(vec![
                Span::styled(" T ", key_style),
                Span::raw(" Toggle theme"),
            ]),
            Line::from(vec![Span::styled(" Q ", key_style), Span::raw(" Quit")]),
            Line::from(vec![
                Span::styled(" ? ", key_style),
                Span::raw(" Close help"),
            ]),
        ];

        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .style(Style::default().fg(self.theme.fg).bg(self.theme.surface))
            .render(inner, buf);
    }
}

pub struct ShutdownScreen<'a> {
    pub vm_names: Vec<String>,
    pub theme: &'a Theme,
}

impl Widget for ShutdownScreen<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);

        let block = Block::default()
            .borders(Borders::ALL)
            .padding(Padding::uniform(1))
            .border_style(Style::default().fg(self.theme.border))
            .style(Style::default().bg(self.theme.surface));

        let inner = block.inner(area);
        block.render(area, buf);

        let total_lines = 3 + self.vm_names.len();
        let start_y = inner.y
            + (inner
                .height
                .saturating_sub(u16::try_from(total_lines).unwrap_or(inner.height)))
                / 2;

        Paragraph::new("Shutting down…")
            .style(Style::default().fg(self.theme.warning).bold())
            .alignment(Alignment::Center)
            .render(
                Rect {
                    x: inner.x,
                    y: start_y,
                    width: inner.width,
                    height: 1,
                },
                buf,
            );

        for (i, vm_name) in self.vm_names.iter().enumerate() {
            let line = Line::from(vec![
                Span::styled("· ", Style::default().fg(self.theme.warning)),
                Span::styled(
                    format!("Stopping {vm_name}"),
                    Style::default().fg(self.theme.secondary),
                ),
            ]);
            Paragraph::new(line).alignment(Alignment::Center).render(
                Rect {
                    x: inner.x,
                    y: start_y + 2 + u16::try_from(i).unwrap_or(0),
                    width: inner.width,
                    height: 1,
                },
                buf,
            );
        }
    }
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;

    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

fn format_duration_or_placeholder(duration: Option<Duration>) -> String {
    duration.map_or_else(|| "--:--".to_string(), format_duration)
}

fn render_context_panel(theme: &Theme, description: &str, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::uniform(1))
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.surface))
        .title(" CONTEXT ")
        .title_style(Style::default().fg(theme.secondary));

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let text = if description.trim().is_empty() {
        "No description provided."
    } else {
        description
    };

    let lines = markdown_lines(theme, text);

    Paragraph::new(lines)
        .style(Style::default().bg(theme.surface))
        .wrap(Wrap { trim: true })
        .render(inner, buf);
}

fn render_objectives_panel(theme: &Theme, vms: &[VmTreeNode<'_>], area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::uniform(1))
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.surface))
        .title(" OBJECTIVES ")
        .title_style(Style::default().fg(theme.secondary));

    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let lines = objectives_lines(theme, vms, inner.width);

    Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(theme.surface))
        .render(inner, buf);
}

fn markdown_lines<'a>(theme: &Theme, text: &str) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let body_style = Style::default().fg(theme.fg);
    let h1_style = Style::default().fg(theme.primary).bold();
    let h2_style = Style::default().fg(theme.secondary).bold();
    let bullet_style = Style::default().fg(theme.secondary);

    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            lines.push(Line::raw(""));
            continue;
        }

        if let Some(rest) = line.strip_prefix("### ") {
            lines.push(Line::from(markdown_spans(theme, rest, h2_style)));
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            lines.push(Line::from(markdown_spans(theme, rest, h2_style)));
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            lines.push(Line::from(markdown_spans(theme, rest, h1_style)));
            continue;
        }

        let trimmed = line.trim_start();
        let indent = line.len().saturating_sub(trimmed.len());
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let mut spans = Vec::new();
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(indent.min(6))));
            }
            spans.push(Span::styled("• ", bullet_style));
            spans.extend(markdown_spans(theme, item, body_style));
            lines.push(Line::from(spans));
            continue;
        }

        lines.push(Line::from(markdown_spans(theme, line, body_style)));
    }

    lines
}

fn markdown_spans<'a>(theme: &Theme, text: &str, base: Style) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut idx = 0;
    let len = text.len();
    let code_style = Style::default().fg(theme.info);
    let link_style = Style::default()
        .fg(theme.info)
        .add_modifier(Modifier::UNDERLINED);
    let dim_style = Style::default().fg(theme.dim);

    while idx < len {
        let rest = &text[idx..];

        if let Some(after) = rest.strip_prefix("**")
            && let Some(end) = after.find("**")
        {
            let content = &after[..end];
            spans.push(Span::styled(
                content.to_string(),
                base.add_modifier(Modifier::BOLD),
            ));
            idx += 2 + end + 2;
            continue;
        }

        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            let content = &after[..end];
            spans.push(Span::styled(content.to_string(), code_style));
            idx += 1 + end + 1;
            continue;
        }

        if let Some(after) = rest.strip_prefix('[')
            && let Some(label_end) = after.find("](")
        {
            let label = &after[..label_end];
            let after_label = &after[label_end + 2..];
            if let Some(url_end) = after_label.find(')') {
                let url = &after_label[..url_end];
                spans.push(Span::styled(label.to_string(), link_style));
                spans.push(Span::styled(format!(" ({url})"), dim_style));
                idx += 1 + label_end + 2 + url_end + 1;
                continue;
            }
        }

        let next = [rest.find("**"), rest.find('`'), rest.find('[')]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(rest.len());

        let segment = &rest[..next];
        if !segment.is_empty() {
            spans.push(Span::styled(segment.to_string(), base));
        }
        idx += next.max(1);
    }

    spans
}

fn objectives_lines<'a>(
    theme: &Theme,
    vms: &'a [VmTreeNode<'a>],
    area_width: u16,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let mut has_objectives = false;

    for vm in vms {
        if vm.scenario_probes.is_empty() {
            continue;
        }
        has_objectives = true;
        let (passed, total) = vm
            .scenario_probes
            .iter()
            .fold((0usize, 0usize), |mut acc, p| {
                if p.status == ProbeStatus::Passed {
                    acc.0 += 1;
                }
                acc.1 += 1;
                acc
            });

        let header = Line::from(vec![
            Span::styled("VM ", Style::default().fg(theme.dim)),
            Span::styled(vm.name.as_ref(), Style::default().fg(theme.primary).bold()),
            Span::styled("  ", Style::default()),
            Span::styled("[", Style::default().fg(theme.dim)),
            Span::styled(
                format!("{passed}/{total}"),
                Style::default().fg(theme.secondary).bold(),
            ),
            Span::styled("]", Style::default().fg(theme.dim)),
        ]);
        lines.push(header);

        let sep_width = area_width.saturating_sub(2) as usize;
        if sep_width > 0 {
            lines.push(Line::from(Span::styled(
                "-".repeat(sep_width),
                Style::default().fg(theme.dim),
            )));
        }

        for probe in &vm.scenario_probes {
            let (icon, color) = match probe.status {
                ProbeStatus::Passed => ("✓", theme.success),
                ProbeStatus::Failed => ("✗", theme.error),
                ProbeStatus::Pending => ("·", theme.dim),
            };

            let text_style = if probe.status == ProbeStatus::Passed {
                Style::default().fg(theme.dim)
            } else {
                Style::default().fg(theme.fg)
            };

            let status_label = match probe.status {
                ProbeStatus::Passed => "PASS",
                ProbeStatus::Failed => "FAIL",
                ProbeStatus::Pending => "WAIT",
            };

            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(icon, Style::default().fg(color)),
                Span::raw(" "),
                Span::styled(
                    format!("{status_label:<4}"),
                    Style::default().fg(color).bold(),
                ),
                Span::raw(" "),
                Span::styled(probe.name.as_ref(), text_style),
            ]));

            if let Some(desc) = probe.description.as_ref() {
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(desc.as_ref(), Style::default().fg(theme.dim)),
                ]));
            }
        }
        lines.push(Line::raw(""));
    }

    if !has_objectives {
        lines.push(Line::from(Span::styled(
            "No objectives configured.",
            Style::default().fg(theme.dim),
        )));
    }

    lines
}
