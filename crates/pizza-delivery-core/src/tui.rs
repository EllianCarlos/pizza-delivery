use std::collections::HashMap;

use crate::api::JobView;
use crate::event::{Event, EventPayload};
use crate::id::{short_id, JobId};
use crate::inbox::{fold_inbox, EntryStatus, InboxEntry};
use crate::job::{apply, Job, JobState};
use crate::stage::{fold_stages, StageState};
use crate::view::{
    board_view, event_from_view, job_detail, plan_view, BoardView, EventView, JobDetailView,
};

pub const MIN_COLS: u16 = 80;
pub const MIN_ROWS: u16 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub color: bool,
    pub unicode: bool,
}

impl Caps {
    pub fn detect(no_color: Option<&str>, locale: &[Option<String>]) -> Caps {
        let color = no_color.map_or(true, str::is_empty);
        let chosen = locale
            .iter()
            .flatten()
            .find(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase());
        let unicode = chosen.is_some_and(|v| v.contains("utf-8") || v.contains("utf8"));
        Caps { color, unicode }
    }

    pub fn from_env() -> Caps {
        let var = |k: &str| std::env::var(k).ok();
        Caps::detect(
            var("NO_COLOR").as_deref(),
            &[var("LC_ALL"), var("LC_CTYPE"), var("LANG")],
        )
    }
}

pub fn fits(cols: u16, rows: u16) -> bool {
    cols >= MIN_COLS && rows >= MIN_ROWS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Table,
    Board,
    Pipeline,
    Inbox,
}

impl ViewKind {
    pub const ALL: [ViewKind; 4] = [
        ViewKind::Table,
        ViewKind::Board,
        ViewKind::Pipeline,
        ViewKind::Inbox,
    ];

    pub fn title(self) -> &'static str {
        match self {
            ViewKind::Table => "Table",
            ViewKind::Board => "Board",
            ViewKind::Pipeline => "Pipeline",
            ViewKind::Inbox => "Inbox",
        }
    }

    fn next(self) -> ViewKind {
        match self {
            ViewKind::Table => ViewKind::Board,
            ViewKind::Board => ViewKind::Pipeline,
            ViewKind::Pipeline => ViewKind::Inbox,
            ViewKind::Inbox => ViewKind::Table,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Reply { targets: Vec<String>, text: String },
    NewJob { text: String },
    ConfirmCancel { job: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Live,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    Tab,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Quit,
    Submit { prompt: String },
    Reply { targets: Vec<String>, text: String },
    Dismiss { entries: Vec<String> },
    Cancel { job: String },
    OpenWeb,
    OpenPr { url: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentState {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub name: String,
    pub weight: u64,
    pub state: SegmentState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    From(u64),
    Snapshot,
}

pub fn resume_plan(last: Option<(u64, &str)>, probe: &[EventView]) -> Resume {
    match last {
        None => Resume::Snapshot,
        Some((cursor, id)) => match probe.first() {
            Some(first) if first.cursor == cursor && first.id == id => Resume::From(cursor),
            _ => Resume::Snapshot,
        },
    }
}

pub fn segment_widths(weights: &[u64], total: usize) -> Vec<usize> {
    if weights.is_empty() {
        return Vec::new();
    }
    let count = weights.len();
    if total <= count {
        return vec![1; count];
    }
    let sum: u128 = weights.iter().map(|w| u128::from(*w)).sum::<u128>().max(1);
    let mut widths: Vec<usize> = weights
        .iter()
        .map(|w| ((u128::from(*w) * total as u128 / sum) as usize).max(1))
        .collect();
    loop {
        let used: usize = widths.iter().sum();
        let widest = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .map_or(0, |(i, _)| i);
        if used < total {
            widths[widest] += 1;
        } else if used > total {
            widths[widest] -= 1;
        } else {
            return widths;
        }
    }
}

#[derive(Debug, Clone)]
pub struct Model {
    entries: Vec<(u64, Event)>,
    view: ViewKind,
    detail_open: bool,
    mode: Mode,
    notice: Option<String>,
    connection: Connection,
    index: usize,
    column: usize,
    row: usize,
}

impl Default for Model {
    fn default() -> Self {
        Model {
            entries: Vec::new(),
            view: ViewKind::Table,
            detail_open: false,
            mode: Mode::Normal,
            notice: None,
            connection: Connection::Connecting,
            index: 0,
            column: 0,
            row: 0,
        }
    }
}

impl Model {
    pub fn new() -> Model {
        Model::default()
    }

    pub fn apply(&mut self, views: &[EventView]) {
        for view in views {
            if self.entries.last().is_some_and(|(c, _)| view.cursor <= *c) {
                continue;
            }
            if let Some(event) = event_from_view(view) {
                self.entries.push((view.cursor, event));
            }
        }
    }

    pub fn reset(&mut self) {
        self.entries.clear();
    }

    pub fn cursor(&self) -> u64 {
        self.entries.last().map_or(0, |(c, _)| *c)
    }

    pub fn last_marker(&self) -> Option<(u64, &str)> {
        self.entries.last().map(|(c, e)| (*c, e.id.as_str()))
    }

    pub fn view(&self) -> ViewKind {
        self.view
    }

    pub fn detail_open(&self) -> bool {
        self.detail_open
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.notice = Some(text.into());
    }

    pub fn connection(&self) -> Connection {
        self.connection
    }

    pub fn set_connection(&mut self, connection: Connection) {
        self.connection = connection;
    }

    pub fn jobs(&self) -> Vec<JobView> {
        let mut order: Vec<String> = Vec::new();
        let mut state: HashMap<String, Job> = HashMap::new();
        for (_, event) in &self.entries {
            let key = event.job_id.as_str().to_owned();
            let previous = state.remove(&key);
            let known = previous.is_some();
            if let Some(next) = apply(previous, event) {
                if !known {
                    order.push(key.clone());
                }
                state.insert(key, next);
            }
        }
        let ids: Vec<JobId> = order.iter().map(|k| JobId::new(k.clone())).collect();
        let mut jobs: Vec<JobView> = order
            .iter()
            .filter_map(|key| {
                let job = state.get(key)?;
                Some(JobView {
                    id: job.id.clone(),
                    short_id: short_id(&job.id, &ids),
                    state: job.state,
                    prompt: job.prompt.clone(),
                    submitted_at_ms: job.submitted_at_ms,
                })
            })
            .collect();
        jobs.sort_by(|a, b| {
            b.submitted_at_ms
                .cmp(&a.submitted_at_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        jobs
    }

    pub fn board(&self) -> BoardView {
        board_view(&self.jobs())
    }

    pub fn inbox(&self) -> Vec<InboxEntry> {
        fold_inbox(self.entries.iter().map(|(_, e)| e))
            .into_iter()
            .filter(|e| e.status == EntryStatus::Open)
            .collect()
    }

    pub fn open_feedback(&self) -> usize {
        self.inbox().len()
    }

    pub fn selected_entry(&self) -> Option<InboxEntry> {
        if self.view != ViewKind::Inbox {
            return None;
        }
        let entries = self.inbox();
        let at = self.index.min(entries.len().saturating_sub(1));
        entries.into_iter().nth(at)
    }

    pub fn selected_index(&self) -> usize {
        let len = self.list_len();
        self.index.min(len.saturating_sub(1))
    }

    pub fn selected_column(&self) -> usize {
        self.column
    }

    pub fn selected_row(&self) -> usize {
        let len = self.column_len(self.column);
        self.row.min(len.saturating_sub(1))
    }

    pub fn selected_job(&self) -> Option<JobView> {
        match self.view {
            ViewKind::Table | ViewKind::Pipeline => {
                self.jobs().into_iter().nth(self.selected_index())
            }
            ViewKind::Board => self
                .board()
                .columns
                .into_iter()
                .nth(self.column)
                .and_then(|c| c.jobs.into_iter().nth(self.selected_row())),
            ViewKind::Inbox => {
                let entry = self.selected_entry()?;
                self.jobs().into_iter().find(|j| j.id == entry.job)
            }
        }
    }

    pub fn detail(&self, job: &JobView) -> JobDetailView {
        let own: Vec<(u64, Event)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.job_id == job.id)
            .cloned()
            .collect();
        let feedback = fold_inbox(self.entries.iter().map(|(_, e)| e));
        job_detail(job.clone(), &own, &feedback)
    }

    pub fn lane(&self, job: &JobView) -> Vec<Segment> {
        let own: Vec<Event> = self
            .entries
            .iter()
            .filter(|(_, e)| e.job_id == job.id)
            .map(|(_, e)| e.clone())
            .collect();
        let plan = plan_view(&own);
        let stages = fold_stages(&own);
        plan.stages
            .into_iter()
            .map(|planned| {
                let state = stages.iter().rev().find(|s| s.name == planned.name).map_or(
                    SegmentState::Pending,
                    |s| match s.state {
                        StageState::Queued => SegmentState::Pending,
                        StageState::Running => SegmentState::Running,
                        StageState::Succeeded => SegmentState::Done,
                        StageState::Failed => SegmentState::Failed,
                    },
                );
                Segment {
                    name: planned.name,
                    weight: planned.estimated_tokens.unwrap_or(1).max(1),
                    state,
                }
            })
            .collect()
    }

    pub fn pr_url(&self, job: &JobView) -> Option<String> {
        self.entries
            .iter()
            .rev()
            .filter(|(_, e)| e.job_id == job.id)
            .find_map(|(_, e)| match &e.payload {
                EventPayload::SandboxAudit {
                    pr_url: Some(url), ..
                } => Some(url.clone()),
                _ => None,
            })
    }

    fn column_len(&self, column: usize) -> usize {
        self.board().columns.get(column).map_or(0, |c| c.jobs.len())
    }

    fn list_len(&self) -> usize {
        match self.view {
            ViewKind::Table | ViewKind::Pipeline => self.jobs().len(),
            ViewKind::Inbox => self.inbox().len(),
            ViewKind::Board => self.column_len(self.column),
        }
    }

    fn step(&mut self, delta: isize) {
        let len = self.list_len();
        let current = match self.view {
            ViewKind::Board => self.selected_row(),
            _ => self.selected_index(),
        };
        let target = current
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
        match self.view {
            ViewKind::Board => self.row = target,
            _ => self.index = target,
        }
    }

    fn jump(&mut self, last: bool) {
        let len = self.list_len();
        let target = if last { len.saturating_sub(1) } else { 0 };
        match self.view {
            ViewKind::Board => self.row = target,
            _ => self.index = target,
        }
    }

    fn shift_column(&mut self, delta: isize) {
        if self.view != ViewKind::Board {
            return;
        }
        let max = self.board().columns.len().saturating_sub(1);
        self.column = self.column.saturating_add_signed(delta).min(max);
    }

    fn switch(&mut self, view: ViewKind) {
        self.view = view;
        self.index = 0;
        self.row = 0;
        if view == ViewKind::Board {
            self.column = self
                .board()
                .columns
                .iter()
                .position(|c| !c.jobs.is_empty())
                .unwrap_or(0);
        }
    }

    fn open_feedback_for(&self, job: &JobView) -> Vec<InboxEntry> {
        self.inbox()
            .into_iter()
            .filter(|e| e.job == job.id)
            .collect()
    }

    pub fn handle_key(&mut self, key: Key) -> Option<Command> {
        if key == Key::Interrupt {
            return Some(Command::Quit);
        }
        self.notice = None;
        match self.mode.clone() {
            Mode::Normal => self.normal_key(key),
            Mode::Reply { targets, mut text } => match edit(&mut text, &key) {
                Edit::Submit if !text.trim().is_empty() => {
                    self.mode = Mode::Normal;
                    Some(Command::Reply { targets, text })
                }
                Edit::Submit => None,
                Edit::Cancel => {
                    self.mode = Mode::Normal;
                    None
                }
                Edit::Continue => {
                    self.mode = Mode::Reply { targets, text };
                    None
                }
            },
            Mode::NewJob { mut text } => match edit(&mut text, &key) {
                Edit::Submit if !text.trim().is_empty() => {
                    self.mode = Mode::Normal;
                    Some(Command::Submit { prompt: text })
                }
                Edit::Submit => None,
                Edit::Cancel => {
                    self.mode = Mode::Normal;
                    None
                }
                Edit::Continue => {
                    self.mode = Mode::NewJob { text };
                    None
                }
            },
            Mode::ConfirmCancel { job } => {
                self.mode = Mode::Normal;
                match key {
                    Key::Char('y' | 'Y') => Some(Command::Cancel { job }),
                    _ => None,
                }
            }
        }
    }

    fn normal_key(&mut self, key: Key) -> Option<Command> {
        match key {
            Key::Char('q') => return Some(Command::Quit),
            Key::Char('j') | Key::Down => self.step(1),
            Key::Char('k') | Key::Up => self.step(-1),
            Key::Char('h') | Key::Left => self.shift_column(-1),
            Key::Char('l') | Key::Right => self.shift_column(1),
            Key::Char('g') => self.jump(false),
            Key::Char('G') => self.jump(true),
            Key::Tab => self.switch(self.view.next()),
            Key::Char('1') => self.switch(ViewKind::Table),
            Key::Char('2') => self.switch(ViewKind::Board),
            Key::Char('3') => self.switch(ViewKind::Pipeline),
            Key::Char('4') => self.switch(ViewKind::Inbox),
            Key::Enter => self.detail_open = !self.detail_open,
            Key::Char('n') => {
                self.mode = Mode::NewJob {
                    text: String::new(),
                }
            }
            Key::Char('o') => return Some(Command::OpenWeb),
            Key::Char('r') => self.begin_reply(),
            Key::Char('d') => return self.dismiss(),
            Key::Char('c') => self.begin_cancel(),
            Key::Char('p') => return self.open_pr(),
            _ => {}
        }
        None
    }

    fn begin_reply(&mut self) {
        if let Some(entry) = self.selected_entry() {
            self.mode = Mode::Reply {
                targets: vec![entry.short_id],
                text: String::new(),
            };
            return;
        }
        match self.selected_job() {
            Some(job) if !self.open_feedback_for(&job).is_empty() => {
                self.mode = Mode::Reply {
                    targets: vec![job.short_id],
                    text: String::new(),
                };
            }
            Some(_) => self.set_notice("no open feedback to reply to"),
            None => self.set_notice("nothing selected"),
        }
    }

    fn dismiss(&mut self) -> Option<Command> {
        if let Some(entry) = self.selected_entry() {
            return Some(Command::Dismiss {
                entries: vec![entry.id],
            });
        }
        let Some(job) = self.selected_job() else {
            self.set_notice("nothing selected");
            return None;
        };
        let entries: Vec<String> = self
            .open_feedback_for(&job)
            .into_iter()
            .map(|e| e.id)
            .collect();
        if entries.is_empty() {
            self.set_notice("no open feedback to dismiss");
            return None;
        }
        Some(Command::Dismiss { entries })
    }

    fn begin_cancel(&mut self) {
        match self.selected_job() {
            Some(job)
                if matches!(
                    job.state,
                    JobState::Completed | JobState::Failed | JobState::Cancelled
                ) =>
            {
                self.set_notice("job already finished");
            }
            Some(job) => self.mode = Mode::ConfirmCancel { job: job.short_id },
            None => self.set_notice("nothing selected"),
        }
    }

    fn open_pr(&mut self) -> Option<Command> {
        let Some(job) = self.selected_job() else {
            self.set_notice("nothing selected");
            return None;
        };
        match self.pr_url(&job) {
            Some(url) => Some(Command::OpenPr { url }),
            None => {
                self.set_notice("no pull request for this job yet");
                None
            }
        }
    }
}

enum Edit {
    Continue,
    Submit,
    Cancel,
}

fn edit(text: &mut String, key: &Key) -> Edit {
    match key {
        Key::Enter => Edit::Submit,
        Key::Esc => Edit::Cancel,
        Key::Backspace => {
            text.pop();
            Edit::Continue
        }
        Key::Char(c) => {
            text.push(*c);
            Edit::Continue
        }
        _ => Edit::Continue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;
    use crate::view::event_view;

    fn ev(cursor: u64, job: &str, seq: u64, ts: i64, payload: EventPayload) -> EventView {
        let e = Event {
            id: format!("e{cursor}"),
            job_id: JobId::new(job),
            stage_id: None,
            seq,
            ts_ms: ts,
            actor: Actor::Daemon,
            payload,
        };
        event_view(cursor, &e)
    }

    fn submitted(cursor: u64, job: &str, ts: i64) -> EventView {
        ev(
            cursor,
            job,
            1,
            ts,
            EventPayload::JobSubmitted {
                prompt: format!("prompt of {job}"),
                repo: None,
                priority: crate::schedule::Priority::Normal,
                follow_up: None,
                options: None,
            },
        )
    }

    fn feedback(cursor: u64, job: &str) -> EventView {
        ev(
            cursor,
            job,
            2,
            ts_of(cursor),
            EventPayload::FeedbackRecorded {
                kind: "question".into(),
                severity: "high".into(),
                text: "which db?".into(),
                confidence: None,
                alternatives: vec![],
                impact: None,
                evidence: None,
            },
        )
    }

    fn ts_of(cursor: u64) -> i64 {
        cursor as i64 * 10
    }

    fn press(model: &mut Model, keys: &[Key]) -> Vec<Command> {
        keys.iter()
            .filter_map(|k| model.handle_key(k.clone()))
            .collect()
    }

    fn typed(text: &str) -> Vec<Key> {
        text.chars().map(Key::Char).collect()
    }

    #[test]
    fn live_events_move_a_job_between_board_columns() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100)]);
        let in_column = |m: &Model, state: JobState| {
            m.board()
                .columns
                .into_iter()
                .find(|c| c.state == state)
                .map_or(0, |c| c.jobs.len())
        };
        assert_eq!(in_column(&model, JobState::Queued), 1);
        model.apply(&[ev(
            2,
            "JOB1AAAA",
            2,
            200,
            EventPayload::StageClaimed {
                holder: "w".into(),
                attempt: 1,
                lease_expires_ms: 9,
            },
        )]);
        assert_eq!(in_column(&model, JobState::Queued), 0);
        assert_eq!(in_column(&model, JobState::Running), 1);
    }

    #[test]
    fn replayed_events_are_ignored() {
        let mut model = Model::new();
        let first = submitted(1, "JOB1AAAA", 100);
        model.apply(&[first.clone(), first]);
        assert_eq!(model.jobs().len(), 1);
        assert_eq!(model.cursor(), 1);
    }

    #[test]
    fn jobs_list_newest_first() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "OLDER000", 100), submitted(2, "NEWER000", 200)]);
        let ids: Vec<String> = model
            .jobs()
            .into_iter()
            .map(|j| j.id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["NEWER000", "OLDER000"]);
    }

    #[test]
    fn vim_and_arrow_keys_move_the_selection() {
        let mut model = Model::new();
        model.apply(&[
            submitted(1, "AAAAAAAA", 100),
            submitted(2, "BBBBBBBB", 200),
            submitted(3, "CCCCCCCC", 300),
        ]);
        let at = |m: &Model| m.selected_job().map(|j| j.id.as_str().to_owned());
        assert_eq!(at(&model).as_deref(), Some("CCCCCCCC"));
        press(&mut model, &[Key::Char('j')]);
        assert_eq!(at(&model).as_deref(), Some("BBBBBBBB"));
        press(&mut model, &[Key::Down, Key::Down]);
        assert_eq!(at(&model).as_deref(), Some("AAAAAAAA"));
        press(&mut model, &[Key::Char('k'), Key::Up]);
        assert_eq!(at(&model).as_deref(), Some("CCCCCCCC"));
        press(&mut model, &[Key::Char('G')]);
        assert_eq!(at(&model).as_deref(), Some("AAAAAAAA"));
        press(&mut model, &[Key::Char('g')]);
        assert_eq!(at(&model).as_deref(), Some("CCCCCCCC"));
    }

    #[test]
    fn board_navigation_moves_across_columns() {
        let mut model = Model::new();
        model.apply(&[
            submitted(1, "AAAAAAAA", 100),
            submitted(2, "BBBBBBBB", 200),
            ev(
                3,
                "BBBBBBBB",
                2,
                300,
                EventPayload::StageClaimed {
                    holder: "w".into(),
                    attempt: 1,
                    lease_expires_ms: 9,
                },
            ),
        ]);
        press(&mut model, &[Key::Char('2')]);
        assert_eq!(
            model
                .selected_job()
                .map(|j| j.id.as_str().to_owned())
                .as_deref(),
            Some("AAAAAAAA")
        );
        press(&mut model, &[Key::Char('l')]);
        assert_eq!(
            model
                .selected_job()
                .map(|j| j.id.as_str().to_owned())
                .as_deref(),
            Some("BBBBBBBB")
        );
        press(&mut model, &[Key::Left]);
        assert_eq!(
            model
                .selected_job()
                .map(|j| j.id.as_str().to_owned())
                .as_deref(),
            Some("AAAAAAAA")
        );
    }

    #[test]
    fn the_board_opens_on_the_first_column_with_jobs() {
        let mut model = Model::new();
        model.apply(&[
            submitted(1, "AAAAAAAA", 100),
            ev(2, "AAAAAAAA", 2, 200, EventPayload::JobCompleted {}),
        ]);
        press(&mut model, &[Key::Char('2')]);
        assert_eq!(
            model
                .selected_job()
                .map(|j| j.id.as_str().to_owned())
                .as_deref(),
            Some("AAAAAAAA")
        );
    }

    #[test]
    fn tab_cycles_through_the_views() {
        let mut model = Model::new();
        let mut seen = vec![model.view()];
        for _ in 0..4 {
            press(&mut model, &[Key::Tab]);
            seen.push(model.view());
        }
        assert_eq!(
            seen,
            [
                ViewKind::Table,
                ViewKind::Board,
                ViewKind::Pipeline,
                ViewKind::Inbox,
                ViewKind::Table
            ]
        );
    }

    #[test]
    fn quit_keys_ask_to_quit() {
        let mut model = Model::new();
        assert_eq!(press(&mut model, &[Key::Char('q')]), [Command::Quit]);
        assert_eq!(press(&mut model, &[Key::Interrupt]), [Command::Quit]);
    }

    #[test]
    fn replying_to_a_job_with_feedback_sends_the_text_to_that_job() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100), feedback(2, "JOB1AAAA")]);
        let mut keys = vec![Key::Char('r')];
        keys.extend(typed("use sqlite"));
        keys.push(Key::Enter);
        let short = model.selected_job().map(|j| j.short_id).unwrap_or_default();
        assert_eq!(
            press(&mut model, &keys),
            [Command::Reply {
                targets: vec![short],
                text: "use sqlite".into()
            }]
        );
        assert_eq!(model.mode(), &Mode::Normal);
    }

    #[test]
    fn replying_without_feedback_explains_why() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100)]);
        press(&mut model, &[Key::Char('r')]);
        assert_eq!(model.mode(), &Mode::Normal);
        assert_eq!(model.notice(), Some("no open feedback to reply to"));
    }

    #[test]
    fn reply_in_the_inbox_targets_the_selected_entry() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100), feedback(2, "JOB1AAAA")]);
        press(&mut model, &[Key::Char('4')]);
        let entry = model
            .selected_entry()
            .map(|e| e.short_id)
            .unwrap_or_default();
        let keys = vec![Key::Char('r'), Key::Char('x'), Key::Enter];
        let cmds = press(&mut model, &keys);
        assert_eq!(
            cmds,
            [Command::Reply {
                targets: vec![entry],
                text: "x".into()
            }]
        );
    }

    #[test]
    fn escape_abandons_a_reply_and_backspace_edits_it() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100), feedback(2, "JOB1AAAA")]);
        let cmds = press(
            &mut model,
            &[
                Key::Char('r'),
                Key::Char('a'),
                Key::Char('b'),
                Key::Backspace,
            ],
        );
        assert!(cmds.is_empty());
        assert!(matches!(model.mode(), Mode::Reply { text, .. } if text == "a"));
        press(&mut model, &[Key::Esc]);
        assert_eq!(model.mode(), &Mode::Normal);
    }

    #[test]
    fn dismissing_in_the_inbox_dismisses_the_selected_entry() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100), feedback(2, "JOB1AAAA")]);
        press(&mut model, &[Key::Char('4')]);
        assert_eq!(
            press(&mut model, &[Key::Char('d')]),
            [Command::Dismiss {
                entries: vec!["e2".into()]
            }]
        );
    }

    #[test]
    fn dismissed_entries_leave_the_inbox() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100), feedback(2, "JOB1AAAA")]);
        assert_eq!(model.open_feedback(), 1);
        model.apply(&[ev(
            3,
            "JOB1AAAA",
            3,
            300,
            EventPayload::FeedbackDismissed { entry: "e2".into() },
        )]);
        assert_eq!(model.open_feedback(), 0);
    }

    #[test]
    fn cancel_needs_confirmation() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100)]);
        let short = model.selected_job().map(|j| j.short_id).unwrap_or_default();
        assert!(press(&mut model, &[Key::Char('c')]).is_empty());
        assert_eq!(model.mode(), &Mode::ConfirmCancel { job: short.clone() });
        assert!(press(&mut model, &[Key::Char('n')]).is_empty());
        assert_eq!(model.mode(), &Mode::Normal);
        let cmds = press(&mut model, &[Key::Char('c'), Key::Char('y')]);
        assert_eq!(cmds, [Command::Cancel { job: short }]);
    }

    #[test]
    fn a_finished_job_cannot_be_cancelled() {
        let mut model = Model::new();
        model.apply(&[
            submitted(1, "JOB1AAAA", 100),
            ev(2, "JOB1AAAA", 2, 200, EventPayload::JobCompleted {}),
        ]);
        press(&mut model, &[Key::Char('c')]);
        assert_eq!(model.mode(), &Mode::Normal);
        assert_eq!(model.notice(), Some("job already finished"));
    }

    #[test]
    fn new_job_collects_a_prompt_and_submits_it() {
        let mut model = Model::new();
        let mut keys = vec![Key::Char('n')];
        keys.extend(typed("fix the build"));
        keys.push(Key::Enter);
        assert_eq!(
            press(&mut model, &keys),
            [Command::Submit {
                prompt: "fix the build".into()
            }]
        );
    }

    #[test]
    fn an_empty_new_job_prompt_is_not_submitted() {
        let mut model = Model::new();
        assert!(press(&mut model, &[Key::Char('n'), Key::Enter]).is_empty());
        assert!(matches!(model.mode(), Mode::NewJob { .. }));
    }

    #[test]
    fn o_opens_the_web_ui() {
        let mut model = Model::new();
        assert_eq!(press(&mut model, &[Key::Char('o')]), [Command::OpenWeb]);
    }

    #[test]
    fn p_opens_the_pull_request_once_the_job_has_one() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "JOB1AAAA", 100)]);
        assert!(press(&mut model, &[Key::Char('p')]).is_empty());
        assert_eq!(model.notice(), Some("no pull request for this job yet"));
        model.apply(&[ev(
            2,
            "JOB1AAAA",
            2,
            200,
            EventPayload::SandboxAudit {
                profile_hash: "h".into(),
                mounts: vec![],
                egress_allowlist: vec![],
                blocked_requests: vec![],
                mcp_activations: vec![],
                diff_policy_result: None,
                pushed_ref: None,
                pr_url: Some("https://example.test/pr/1".into()),
            },
        )]);
        assert_eq!(
            press(&mut model, &[Key::Char('p')]),
            [Command::OpenPr {
                url: "https://example.test/pr/1".into()
            }]
        );
    }

    #[test]
    fn enter_toggles_the_detail_panel() {
        let mut model = Model::new();
        assert!(!model.detail_open());
        press(&mut model, &[Key::Enter]);
        assert!(model.detail_open());
        press(&mut model, &[Key::Enter]);
        assert!(!model.detail_open());
    }

    #[test]
    fn selection_is_clamped_when_the_list_shrinks_after_reset() {
        let mut model = Model::new();
        model.apply(&[submitted(1, "AAAAAAAA", 100), submitted(2, "BBBBBBBB", 200)]);
        press(&mut model, &[Key::Char('G')]);
        model.reset();
        model.apply(&[submitted(1, "AAAAAAAA", 100)]);
        assert_eq!(model.selected_index(), 0);
        assert!(model.selected_job().is_some());
    }

    #[test]
    fn resume_continues_when_the_cursor_event_is_still_there() {
        let probe = [submitted(5, "J", 1)];
        assert_eq!(resume_plan(Some((5, "e5")), &probe), Resume::From(5));
    }

    #[test]
    fn resume_takes_a_snapshot_when_the_gap_is_gone() {
        assert_eq!(resume_plan(Some((5, "e5")), &[]), Resume::Snapshot);
        let other = [ev(5, "J", 1, 1, EventPayload::JobCompleted {})];
        let mismatched = EventView {
            id: "different".into(),
            ..other[0].clone()
        };
        assert_eq!(
            resume_plan(Some((5, "e5")), &[mismatched]),
            Resume::Snapshot
        );
        assert_eq!(resume_plan(None, &[]), Resume::Snapshot);
    }

    #[test]
    fn segment_widths_are_proportional_and_fill_the_bar() {
        assert_eq!(segment_widths(&[1, 3], 20), [5, 15]);
        assert_eq!(segment_widths(&[1, 1, 1], 10).iter().sum::<usize>(), 10);
        assert_eq!(segment_widths(&[100, 1], 10).iter().sum::<usize>(), 10);
        assert!(segment_widths(&[100, 1], 10).iter().all(|w| *w >= 1));
        assert_eq!(segment_widths(&[5, 5, 5], 2), [1, 1, 1]);
        assert!(segment_widths(&[], 10).is_empty());
    }

    #[test]
    fn caps_honor_no_color_and_locale() {
        let utf = [Some("en_US.UTF-8".to_owned())];
        assert_eq!(
            Caps::detect(None, &utf),
            Caps {
                color: true,
                unicode: true
            }
        );
        assert!(!Caps::detect(Some("1"), &utf).color);
        assert!(Caps::detect(Some(""), &utf).color);
        let c = [None, None, Some("C".to_owned())];
        assert!(!Caps::detect(None, &c).unicode);
        let override_first = [Some("C".to_owned()), None, Some("en_US.UTF-8".to_owned())];
        assert!(!Caps::detect(None, &override_first).unicode);
        assert!(!Caps::detect(None, &[]).unicode);
        assert!(Caps::detect(None, &[Some("".into()), Some("de_DE.utf8".into())]).unicode);
    }

    #[test]
    fn terminal_size_requirement_is_80_by_24() {
        assert!(fits(80, 24));
        assert!(!fits(79, 24));
        assert!(!fits(80, 23));
    }
}
