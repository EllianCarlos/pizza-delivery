use pizza_delivery_core::stage::StageState;
use pizza_delivery_core::tui::{
    fits, segment_widths, Caps, Connection, FormField, Mode, Model, NewJobForm, SegmentState,
    ViewKind, MIN_COLS, MIN_ROWS,
};
use pizza_delivery_core::view::BOARD_COLUMNS;
use pizza_delivery_core::{JobState, JobView};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

const ASCII_BORDER: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

struct Glyphs {
    cursor: &'static str,
    done: char,
    running: char,
    pending: char,
    failed: char,
    ellipsis: &'static str,
}

fn glyphs(caps: Caps) -> Glyphs {
    if caps.unicode {
        Glyphs {
            cursor: "\u{25b6}",
            done: '\u{2588}',
            running: '\u{2593}',
            pending: '\u{2591}',
            failed: '\u{2592}',
            ellipsis: "\u{2026}",
        }
    } else {
        Glyphs {
            cursor: ">",
            done: '#',
            running: '+',
            pending: '.',
            failed: 'x',
            ellipsis: "~",
        }
    }
}

fn fit(text: &str, width: usize, g: &Glyphs) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= width {
        return flat;
    }
    if width == 0 {
        return String::new();
    }
    let keep: String = flat.chars().take(width.saturating_sub(1)).collect();
    format!("{keep}{}", g.ellipsis)
}

fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        text.chars().take(width).collect()
    } else {
        format!("{text}{}", " ".repeat(width - len))
    }
}

fn tint(caps: Caps, color: Color) -> Style {
    if caps.color {
        Style::default().fg(color)
    } else {
        Style::default()
    }
}

fn job_color(state: JobState) -> Color {
    match state {
        JobState::Queued => Color::Gray,
        JobState::Running => Color::Cyan,
        JobState::WaitingForQuota => Color::Yellow,
        JobState::NeedsHuman => Color::Magenta,
        JobState::Completed => Color::Green,
        JobState::Failed => Color::Red,
        JobState::Cancelled => Color::DarkGray,
    }
}

fn segment_color(state: SegmentState) -> Color {
    match state {
        SegmentState::Pending => Color::DarkGray,
        SegmentState::Running => Color::Cyan,
        SegmentState::Done => Color::Green,
        SegmentState::Failed => Color::Red,
    }
}

fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn block<'a>(title: String, caps: Caps) -> Block<'a> {
    let block = Block::default().borders(Borders::ALL).title(title);
    if caps.unicode {
        block
    } else {
        block.border_set(ASCII_BORDER)
    }
}

pub fn draw(frame: &mut Frame, model: &Model, caps: Caps) {
    let area = frame.area();
    if !fits(area.width, area.height) {
        let text = format!(
            "terminal too small: need {MIN_COLS}x{MIN_ROWS}, have {}x{}",
            area.width, area.height
        );
        frame.render_widget(Paragraph::new(text), area);
        return;
    }
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(header(model, caps)), head);
    frame.render_widget(Paragraph::new(footer(model, caps)), foot);
    let list_area = if model.detail_open() {
        let [list, detail] =
            Layout::vertical([Constraint::Min(6), Constraint::Length(10)]).areas(body);
        draw_detail(frame, model, caps, detail);
        list
    } else {
        body
    };
    if let Mode::NewJob { form } = model.mode() {
        draw_new_job(frame, form, caps, list_area);
        return;
    }
    match model.view() {
        ViewKind::Table => draw_table(frame, model, caps, list_area),
        ViewKind::Board => draw_board(frame, model, caps, list_area),
        ViewKind::Pipeline => draw_pipeline(frame, model, caps, list_area),
        ViewKind::Inbox => draw_inbox(frame, model, caps, list_area),
    }
}

fn form_row(label: &str, value: &str, focused: bool, g: &Glyphs) -> Line<'static> {
    let text = format!("{} {label}: {value}", if focused { g.cursor } else { " " });
    if focused {
        Line::styled(text, selected())
    } else {
        Line::raw(text)
    }
}

fn draw_new_job(frame: &mut Frame, form: &NewJobForm, caps: Caps, area: Rect) {
    let g = glyphs(caps);
    let at = |f: FormField| form.focus == f;
    let mut lines = vec![
        form_row("Prompt", &form.prompt, at(FormField::Prompt), &g),
        form_row(
            if form.open {
                "v Additional Parameters"
            } else {
                "> Additional Parameters"
            },
            "",
            at(FormField::Expander),
            &g,
        ),
    ];
    if form.open {
        let report = if form.report_only { "[x]" } else { "[ ]" };
        lines.extend([
            form_row("  Template", &form.template, at(FormField::Template), &g),
            form_row(
                "  Report only (space)",
                report,
                at(FormField::ReportOnly),
                &g,
            ),
            form_row(
                "  Max stages",
                &form.max_stages,
                at(FormField::MaxStages),
                &g,
            ),
            form_row(
                "  Context hint",
                &form.context_hint,
                at(FormField::ContextHint),
                &g,
            ),
            form_row(
                "  Risk flags",
                &form.risk_flags,
                at(FormField::RiskFlags),
                &g,
            ),
        ]);
    }
    if let Some(preview) = &form.preview {
        lines.push(Line::raw(""));
        lines.push(Line::raw("Triage preview"));
        lines.extend(preview.lines().map(|l| Line::raw(format!("  {l}"))));
    }
    let inner = usize::from(area.width.saturating_sub(2));
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|l| {
            let flat: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            Line::styled(fit(&flat, inner, &g), l.style)
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(block("New job".to_owned(), caps)),
        area,
    );
}

fn header(model: &Model, caps: Caps) -> Line<'static> {
    let mut spans = vec![Span::raw("pizza-delivery ")];
    for view in ViewKind::ALL {
        if view == model.view() {
            spans.push(Span::styled(format!("[{}]", view.title()), selected()));
        } else {
            spans.push(Span::raw(format!(" {} ", view.title())));
        }
    }
    let (label, color) = match model.connection() {
        Connection::Live => ("live", Color::Green),
        Connection::Connecting => ("connecting", Color::Yellow),
        Connection::Offline => ("offline", Color::Red),
    };
    spans.push(Span::raw("  "));
    spans.push(Span::styled(label.to_owned(), tint(caps, color)));
    spans.push(Span::raw(format!(
        "  jobs {}  inbox {}",
        model.jobs().len(),
        model.open_feedback()
    )));
    Line::from(spans)
}

fn footer(model: &Model, caps: Caps) -> Line<'static> {
    let g = glyphs(caps);
    let width = usize::from(MIN_COLS);
    let text = match model.mode() {
        Mode::Reply { targets, text } => format!("reply to {}: {text}_", targets.join(",")),
        Mode::NewJob { form } => match form.focus {
            FormField::Prompt => format!("new job: {}_ (^p preview)", form.prompt),
            _ => "new job: tab next, up back, ret expand/send, ^p preview, esc cancel".to_owned(),
        },
        Mode::ConfirmCancel { job } => format!("cancel job {job}? y/N"),
        Mode::Normal => match model.notice() {
            Some(notice) => notice.to_owned(),
            None => "j/k move tab view ret info r reply d drop c cancel n new o web p pr q quit"
                .to_owned(),
        },
    };
    Line::raw(fit(&text, width, &g))
}

fn marker(is_selected: bool, g: &Glyphs) -> String {
    if is_selected {
        format!("{} ", g.cursor)
    } else {
        "  ".to_owned()
    }
}

fn draw_table(frame: &mut Frame, model: &Model, caps: Caps, area: Rect) {
    let g = glyphs(caps);
    let jobs = model.jobs();
    let at = model.selected_index();
    let inner = usize::from(area.width.saturating_sub(2));
    let mut lines: Vec<Line> = Vec::new();
    if jobs.is_empty() {
        lines.push(Line::raw("no jobs yet - press n to submit one"));
    }
    for (i, job) in jobs.iter().enumerate() {
        let prefix = format!(
            "{}{} {} ",
            marker(i == at, &g),
            pad(&job.short_id, 8),
            pad(job.state.as_str(), 17)
        );
        let room = inner.saturating_sub(prefix.chars().count());
        let row = format!("{prefix}{}", fit(&job.prompt, room, &g));
        let style = if i == at {
            selected()
        } else {
            tint(caps, job_color(job.state))
        };
        lines.push(Line::styled(row, style));
    }
    let scroll = scroll_for(at, usize::from(area.height.saturating_sub(2)));
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(block(format!("Table ({})", jobs.len()), caps)),
        area,
    );
}

fn scroll_for(at: usize, visible: usize) -> u16 {
    if visible == 0 || at < visible {
        0
    } else {
        u16::try_from(at + 1 - visible).unwrap_or(u16::MAX)
    }
}

fn column_title(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::WaitingForQuota => "parked",
        JobState::NeedsHuman => "needs you",
        JobState::Completed => "done",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancel",
    }
}

fn draw_board(frame: &mut Frame, model: &Model, caps: Caps, area: Rect) {
    let g = glyphs(caps);
    let board = model.board();
    let cells = Layout::horizontal(vec![
        Constraint::Ratio(1, BOARD_COLUMNS.len() as u32);
        BOARD_COLUMNS.len()
    ])
    .split(area);
    for (c, column) in board.columns.iter().enumerate() {
        let Some(cell) = cells.get(c) else { continue };
        let inner = usize::from(cell.width.saturating_sub(2));
        let active = c == model.selected_column();
        let lines: Vec<Line> = column
            .jobs
            .iter()
            .enumerate()
            .map(|(r, job)| {
                let text = fit(&job.short_id, inner, &g);
                if active && r == model.selected_row() {
                    Line::styled(text, selected())
                } else {
                    Line::styled(text, tint(caps, job_color(job.state)))
                }
            })
            .collect();
        let title = fit(
            &format!("{} {}", column_title(column.state), column.jobs.len()),
            inner,
            &g,
        );
        frame.render_widget(Paragraph::new(lines).block(block(title, caps)), *cell);
    }
}

fn draw_pipeline(frame: &mut Frame, model: &Model, caps: Caps, area: Rect) {
    let g = glyphs(caps);
    let jobs = model.jobs();
    let at = model.selected_index();
    let inner = usize::from(area.width.saturating_sub(2));
    let bar = inner.saturating_sub(2 + 9);
    let mut lines: Vec<Line> = Vec::new();
    if jobs.is_empty() {
        lines.push(Line::raw("no jobs yet - press n to submit one"));
    }
    for (i, job) in jobs.iter().enumerate() {
        let lead = format!("{}{} ", marker(i == at, &g), pad(&job.short_id, 8));
        let lead_style = if i == at {
            selected()
        } else {
            Style::default()
        };
        let mut spans = vec![Span::styled(lead, lead_style)];
        let lane = model.lane(job);
        if lane.is_empty() {
            spans.push(Span::raw("(no plan yet)"));
        } else {
            let weights: Vec<u64> = lane.iter().map(|s| s.weight).collect();
            for (segment, width) in lane.iter().zip(segment_widths(&weights, bar)) {
                let fill = match segment.state {
                    SegmentState::Pending => g.pending,
                    SegmentState::Running => g.running,
                    SegmentState::Done => g.done,
                    SegmentState::Failed => g.failed,
                };
                let initial = segment
                    .name
                    .chars()
                    .next()
                    .map_or(fill, |c| c.to_ascii_uppercase());
                let mut text = String::new();
                text.push(initial);
                text.push_str(&fill.to_string().repeat(width.saturating_sub(1)));
                spans.push(Span::styled(text, tint(caps, segment_color(segment.state))));
            }
        }
        lines.push(Line::from(spans));
    }
    let scroll = scroll_for(at, usize::from(area.height.saturating_sub(2)));
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(block(format!("Pipeline ({})", jobs.len()), caps)),
        area,
    );
}

fn draw_inbox(frame: &mut Frame, model: &Model, caps: Caps, area: Rect) {
    let g = glyphs(caps);
    let entries = model.inbox();
    let at = model.selected_index();
    let inner = usize::from(area.width.saturating_sub(2));
    let mut lines: Vec<Line> = Vec::new();
    if entries.is_empty() {
        lines.push(Line::raw("inbox is empty"));
    }
    for (i, entry) in entries.iter().enumerate() {
        let prefix = format!(
            "{}{} {} {} ",
            marker(i == at, &g),
            pad(&entry.short_id, 8),
            pad(&entry.severity, 6),
            pad(&entry.kind, 10)
        );
        let room = inner.saturating_sub(prefix.chars().count());
        let row = format!("{prefix}{}", fit(&entry.text, room, &g));
        let style = if i == at {
            selected()
        } else {
            Style::default()
        };
        lines.push(Line::styled(row, style));
    }
    let scroll = scroll_for(at, usize::from(area.height.saturating_sub(2)));
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(block(format!("Inbox ({})", entries.len()), caps)),
        area,
    );
}

fn stage_mark(state: Option<StageState>) -> &'static str {
    match state {
        Some(StageState::Succeeded) => "[x]",
        Some(StageState::Running) => "[>]",
        Some(StageState::Failed) => "[!]",
        _ => "[ ]",
    }
}

fn draw_detail(frame: &mut Frame, model: &Model, caps: Caps, area: Rect) {
    let g = glyphs(caps);
    let inner = usize::from(area.width.saturating_sub(2));
    let Some(job) = model.selected_job() else {
        frame.render_widget(
            Paragraph::new("nothing selected").block(block("Detail".to_owned(), caps)),
            area,
        );
        return;
    };
    let detail = model.detail(&job);
    let mut lines: Vec<Line> = vec![Line::styled(
        fit(
            &format!("{} {} - {}", job.short_id, job.state.as_str(), job.prompt),
            inner,
            &g,
        ),
        tint(caps, job_color(job.state)),
    )];
    for planned in &detail.plan.stages {
        let state = detail
            .stages
            .iter()
            .rev()
            .find(|s| s.name == planned.name)
            .map(|s| s.state);
        lines.push(Line::raw(fit(
            &format!(
                "{} {} - {}",
                stage_mark(state),
                planned.name,
                planned.reason
            ),
            inner,
            &g,
        )));
    }
    for entry in detail
        .feedback
        .iter()
        .filter(|e| e.status == pizza_delivery_core::inbox::EntryStatus::Open)
    {
        lines.push(Line::raw(fit(
            &format!("! {} {}: {}", entry.severity, entry.kind, entry.text),
            inner,
            &g,
        )));
        if let Some(evidence) = &entry.evidence {
            lines.push(Line::raw(fit(
                &format!("    evidence: {evidence}"),
                inner,
                &g,
            )));
        }
    }
    let room = usize::from(area.height.saturating_sub(2));
    let fixed = lines.len();
    let recent = room.saturating_sub(fixed).min(3);
    for event in detail.events.iter().rev().take(recent).rev() {
        lines.push(Line::raw(fit(
            &format!("  {:>4} {} ({})", event.seq, event.kind, event_actor(event)),
            inner,
            &g,
        )));
    }
    lines.truncate(room);
    frame.render_widget(
        Paragraph::new(lines).block(block(format!("Detail {}", job_label(&job)), caps)),
        area,
    );
}

fn job_label(job: &JobView) -> String {
    job.short_id.clone()
}

fn event_actor(event: &pizza_delivery_core::view::EventView) -> &'static str {
    event.actor.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pizza_delivery_core::event::{Actor, Event, EventPayload};
    use pizza_delivery_core::schedule::Priority;
    use pizza_delivery_core::tui::Key;
    use pizza_delivery_core::view::event_view;
    use pizza_delivery_core::JobId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const ASCII: Caps = Caps {
        color: false,
        unicode: false,
    };
    const UTF8: Caps = Caps {
        color: true,
        unicode: true,
    };

    fn ev(cursor: u64, job: &str, seq: u64, ts: i64, payload: EventPayload) -> Event {
        Event {
            id: format!("e{cursor}"),
            job_id: JobId::new(job),
            stage_id: None,
            seq,
            ts_ms: ts,
            actor: Actor::Daemon,
            payload,
        }
    }

    fn model(events: &[(u64, Event)]) -> Model {
        let mut model = Model::new();
        let views: Vec<_> = events.iter().map(|(c, e)| event_view(*c, e)).collect();
        model.apply(&views);
        model.set_connection(Connection::Live);
        model
    }

    fn submitted(cursor: u64, job: &str, prompt: &str, ts: i64) -> (u64, Event) {
        (
            cursor,
            ev(
                cursor,
                job,
                1,
                ts,
                EventPayload::JobSubmitted {
                    prompt: prompt.into(),
                    repo: None,
                    priority: Priority::Normal,
                    follow_up: None,
                    options: None,
                },
            ),
        )
    }

    fn feedback(cursor: u64, job: &str) -> (u64, Event) {
        (
            cursor,
            ev(
                cursor,
                job,
                2,
                300,
                EventPayload::FeedbackRecorded {
                    kind: "question".into(),
                    severity: "high".into(),
                    text: "which database should hold the ledger?".into(),
                    confidence: None,
                    alternatives: vec![],
                    impact: None,
                    evidence: Some("docs/adr/0001.md".into()),
                },
            ),
        )
    }

    fn claimed(cursor: u64, job: &str) -> (u64, Event) {
        (
            cursor,
            ev(
                cursor,
                job,
                2,
                200,
                EventPayload::StageClaimed {
                    holder: "w".into(),
                    attempt: 1,
                    lease_expires_ms: 9,
                },
            ),
        )
    }

    fn frame(model: &Model, caps: Caps, cols: u16, rows: u16) -> Vec<String> {
        let mut terminal = match Terminal::new(TestBackend::new(cols, rows)) {
            Ok(t) => t,
            Err(_) => return vec![],
        };
        let _ = terminal.draw(|f| draw(f, model, caps));
        let buffer = terminal.backend().buffer().clone();
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn sample() -> Model {
        model(&[
            submitted(1, "01JAAAAAAAAAAAAAAAAAAAAAAA", "fix the flaky build", 100),
            submitted(
                2,
                "01JBBBBBBBBBBBBBBBBBBBBBBB",
                "add dark mode to the web ui",
                200,
            ),
            claimed(3, "01JBBBBBBBBBBBBBBBBBBBBBBB"),
            feedback(4, "01JAAAAAAAAAAAAAAAAAAAAAAA"),
        ])
    }

    #[test]
    fn table_snapshot_in_ascii() {
        let lines = frame(&sample(), ASCII, 80, 24);
        let blank = format!("|{}|", " ".repeat(78));
        let mut expected = vec![
            "pizza-delivery [Table] Board  Pipeline  Inbox   live  jobs 2  inbox 1".to_owned(),
            format!("+Table (2){}+", "-".repeat(69)),
            format!(
                "|{:<78}|",
                "> 01JBBBBB running           add dark mode to the web ui"
            ),
            format!(
                "|{:<78}|",
                "  01JAAAAA queued            fix the flaky build"
            ),
        ];
        expected.extend(std::iter::repeat(blank.clone()).take(18));
        expected.push(format!("+{}+", "-".repeat(78)));
        expected.push(
            "j/k move tab view ret info r reply d drop c cancel n new o web p pr q quit".to_owned(),
        );
        assert_eq!(lines, expected);
    }

    #[test]
    fn board_snapshot_has_one_column_per_state() {
        let mut m = sample();
        m.handle_key(Key::Char('2'));
        let lines = frame(&m, ASCII, 80, 24);
        assert!(lines[0].contains("[Board]"));
        for title in [
            "queued 1",
            "running 1",
            "parked 0",
            "needs you",
            "done 0",
            "failed 0",
            "cancel 0",
        ] {
            assert!(
                lines[1].contains(&title[..title.len().min(9)]),
                "{title} missing in {}",
                lines[1]
            );
        }
        assert!(lines[2].contains("01JA"));
        assert!(lines[2].contains("01JB"));
    }

    fn staged(cursor: u64, job: &str, stage: &str, payload: EventPayload) -> (u64, Event) {
        let mut e = ev(cursor, job, cursor, 150, payload);
        e.stage_id = Some(stage.to_owned());
        (cursor, e)
    }

    #[test]
    fn pipeline_lane_shows_one_sized_segment_per_planned_stage() {
        use pizza_delivery_core::budget::{SizeTier, StageEstimate};
        use pizza_delivery_core::triage::{PlannedStage, Resolution, Template, Triage};
        let job = "01JAAAAAAAAAAAAAAAAAAAAAAA";
        let plan: Vec<PlannedStage> = ["grill", "spec", "implement", "review"]
            .iter()
            .map(|n| PlannedStage {
                name: (*n).into(),
                reason: "r".into(),
            })
            .collect();
        let estimate = |stage: &str, tokens: u64| StageEstimate {
            stage: stage.into(),
            tier: SizeTier::Small,
            tokens,
            wall_ms: 1,
        };
        let mut m = model(&[
            submitted(1, job, "do it", 100),
            (
                2,
                ev(
                    2,
                    job,
                    2,
                    110,
                    EventPayload::TriageCompleted {
                        triage: Triage {
                            repo: Resolution {
                                path: "/r".into(),
                                confidence: 100,
                                alternatives: vec![],
                            },
                            template: Template::FullFeature,
                            plan,
                            risk_flags: vec![],
                            task: "t".into(),
                        },
                    },
                ),
            ),
            (
                3,
                ev(
                    3,
                    job,
                    3,
                    120,
                    EventPayload::BudgetEstimated {
                        stages: vec![
                            estimate("grill", 100),
                            estimate("spec", 100),
                            estimate("implement", 600),
                            estimate("review", 200),
                        ],
                    },
                ),
            ),
            staged(
                4,
                job,
                "s1",
                EventPayload::StagePlanned {
                    n: 1,
                    name: "grill".into(),
                },
            ),
            staged(
                5,
                job,
                "s1",
                EventPayload::StageClaimed {
                    holder: "w".into(),
                    attempt: 1,
                    lease_expires_ms: 9,
                },
            ),
            staged(6, job, "s1", EventPayload::StageSucceeded {}),
            staged(
                7,
                job,
                "s2",
                EventPayload::StagePlanned {
                    n: 2,
                    name: "spec".into(),
                },
            ),
            staged(
                8,
                job,
                "s2",
                EventPayload::StageClaimed {
                    holder: "w".into(),
                    attempt: 1,
                    lease_expires_ms: 9,
                },
            ),
        ]);
        m.handle_key(Key::Char('3'));
        let lines = frame(&m, ASCII, 80, 24);
        assert!(lines[0].contains("[Pipeline]"));
        let bar = format!(
            "G{}S{}I{}R{}",
            "#".repeat(5),
            "+".repeat(5),
            ".".repeat(41),
            ".".repeat(12)
        );
        assert!(lines[2].contains(&bar), "{}", lines[2]);
    }

    #[test]
    fn pipeline_without_a_plan_says_so() {
        let mut m = sample();
        m.handle_key(Key::Char('3'));
        assert!(frame(&m, ASCII, 80, 24)[2].contains("(no plan yet)"));
    }

    #[test]
    fn inbox_lists_open_feedback_and_detail_shows_the_job() {
        let mut m = sample();
        m.handle_key(Key::Char('4'));
        m.handle_key(Key::Enter);
        let lines = frame(&m, ASCII, 80, 24);
        assert!(lines[0].contains("[Inbox]"));
        assert!(lines[2].contains("high"));
        assert!(lines[2].contains("question"));
        assert!(lines[2].contains("which database"));
        let detail_title = lines.iter().position(|l| l.starts_with("+Detail 01JA"));
        assert!(detail_title.is_some(), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("! high question")));
    }

    #[test]
    fn inbox_detail_shows_the_evidence_an_entry_cites() {
        let mut m = sample();
        m.handle_key(Key::Char('4'));
        m.handle_key(Key::Enter);
        let lines = frame(&m, ASCII, 80, 24);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("evidence: docs/adr/0001.md")),
            "{lines:#?}"
        );
    }

    #[test]
    fn the_new_job_form_shows_the_triage_preview() {
        let mut m = sample();
        m.handle_key(Key::Char('n'));
        m.show_preview("repo: /r (confidence 90%)\nplan:\n  1. spec: why".into());
        let lines = frame(&m, ASCII, 80, 24);
        assert!(
            lines.iter().any(|l| l.contains("Triage preview")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("1. spec: why")),
            "{lines:#?}"
        );
    }

    #[test]
    fn selection_is_visible_without_color() {
        let lines = frame(&sample(), ASCII, 80, 24);
        assert!(lines[2].starts_with("|> "));
        assert!(lines[3].starts_with("|  "));
    }

    #[test]
    fn ascii_fallback_uses_no_non_ascii_cells() {
        let mut m = sample();
        m.handle_key(Key::Char('3'));
        m.handle_key(Key::Enter);
        for key in ['1', '2', '3', '4'] {
            m.handle_key(Key::Char(key));
            for line in frame(&m, ASCII, 80, 24) {
                assert!(line.is_ascii(), "non-ascii in {line:?}");
            }
        }
    }

    #[test]
    fn unicode_terminals_get_box_drawing_borders() {
        let lines = frame(&sample(), UTF8, 80, 24);
        assert!(lines[1].starts_with('\u{250c}'));
        assert!(lines[2].contains('\u{25b6}'));
    }

    #[test]
    fn no_color_means_no_color_in_any_cell() {
        let mut terminal = match Terminal::new(TestBackend::new(80, 24)) {
            Ok(t) => t,
            Err(_) => return,
        };
        let m = sample();
        let _ = terminal.draw(|f| draw(f, &m, ASCII));
        let buffer = terminal.backend().buffer().clone();
        for y in 0..24 {
            for x in 0..80 {
                let cell = &buffer[(x, y)];
                assert_eq!(cell.fg, Color::Reset, "fg at {x},{y}");
                assert_eq!(cell.bg, Color::Reset, "bg at {x},{y}");
            }
        }
    }

    #[test]
    fn color_terminals_tint_job_states() {
        let mut terminal = match Terminal::new(TestBackend::new(80, 24)) {
            Ok(t) => t,
            Err(_) => return,
        };
        let m = sample();
        let _ = terminal.draw(|f| draw(f, &m, UTF8));
        let buffer = terminal.backend().buffer().clone();
        let tinted = (0..80).any(|x| buffer[(x, 3)].fg != Color::Reset);
        assert!(tinted);
    }

    #[test]
    fn a_small_terminal_gets_a_size_message_instead_of_the_ui() {
        let lines = frame(&sample(), ASCII, 60, 20);
        assert_eq!(lines[0], "terminal too small: need 80x24, have 60x20");
        assert!(lines[1..].iter().all(String::is_empty));
    }

    #[test]
    fn footer_shows_the_reply_prompt_while_typing() {
        let mut m = sample();
        m.handle_key(Key::Char('4'));
        m.handle_key(Key::Char('r'));
        m.handle_key(Key::Char('h'));
        m.handle_key(Key::Char('i'));
        let lines = frame(&m, ASCII, 80, 24);
        assert!(lines[23].starts_with("reply to "));
        assert!(lines[23].ends_with(": hi_"));
    }

    #[test]
    fn new_job_form_shows_the_additional_parameters_collapsed_then_open() {
        let mut m = Model::new();
        m.handle_key(Key::Char('n'));
        let closed = frame(&m, ASCII, 80, 24).join("\n");
        assert!(closed.contains("> Additional Parameters"), "{closed}");
        assert!(!closed.contains("Max stages"), "{closed}");
        m.handle_key(Key::Tab);
        m.handle_key(Key::Enter);
        let open = frame(&m, ASCII, 80, 24).join("\n");
        for label in [
            "Template",
            "Report only",
            "Max stages",
            "Context hint",
            "Risk flags",
        ] {
            assert!(open.contains(label), "{open}");
        }
    }

    #[test]
    fn footer_asks_for_cancel_confirmation() {
        let mut m = sample();
        m.handle_key(Key::Char('c'));
        let lines = frame(&m, ASCII, 80, 24);
        assert!(lines[23].starts_with("cancel job "));
        assert!(lines[23].ends_with("? y/N"));
    }

    #[test]
    fn offline_status_is_shown_in_the_header() {
        let mut m = sample();
        m.set_connection(Connection::Offline);
        assert!(frame(&m, ASCII, 80, 24)[0].contains("offline"));
    }

    #[test]
    fn empty_table_invites_the_first_job() {
        let lines = frame(&Model::new(), ASCII, 80, 24);
        assert!(lines[2].contains("no jobs yet"));
    }
}
