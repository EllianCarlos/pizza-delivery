//! `pizza-delivery tui`: a new-job form with a collapsed "> Additional
//! Parameters" group. The state machine (`update`, `to_request`, `view`) has no
//! terminal dependency; only `run` touches crossterm.

use pizza_delivery_core::triage::{render_triage, Template, TriageOptions};
use pizza_delivery_core::{Priority, Request, Response};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::client::{send, Start};
use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Prompt,
    Repo,
    Expander,
    Template,
    ReportOnly,
    MaxStages,
    Context,
    Risk,
}

const ALWAYS: [Focus; 3] = [Focus::Prompt, Focus::Repo, Focus::Expander];
const OPTIONS: [Focus; 5] = [
    Focus::Template,
    Focus::ReportOnly,
    Focus::MaxStages,
    Focus::Context,
    Focus::Risk,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub prompt: String,
    pub repo: String,
    pub expanded: bool,
    pub template: Option<Template>,
    pub report_only: bool,
    pub max_stages: String,
    pub context_hint: String,
    pub risk_flags: String,
    pub focus: Focus,
    pub status: String,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            repo: String::new(),
            expanded: false,
            template: None,
            report_only: false,
            max_stages: String::new(),
            context_hint: String::new(),
            risk_flags: String::new(),
            focus: Focus::Prompt,
            status: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Tab,
    BackTab,
    Enter,
    Left,
    Right,
    Esc,
    CtrlP,
    CtrlS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Preview,
    Submit,
}

impl Form {
    fn order(&self) -> Vec<Focus> {
        let mut order = ALWAYS.to_vec();
        if self.expanded {
            order.extend(OPTIONS);
        }
        order
    }

    fn step(&mut self, delta: isize) {
        let order = self.order();
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        let next = (at + delta).rem_euclid(order.len() as isize) as usize;
        self.focus = order.get(next).copied().unwrap_or(Focus::Prompt);
    }

    fn toggle_expander(&mut self) {
        self.expanded = !self.expanded;
        if !self.expanded {
            self.focus = Focus::Expander;
        }
    }

    fn cycle_template(&mut self, forward: bool) {
        const ALL: [Option<Template>; 4] = [
            None,
            Some(Template::FullFeature),
            Some(Template::BugFix),
            Some(Template::Investigate),
        ];
        let at = ALL.iter().position(|t| *t == self.template).unwrap_or(0);
        let next = if forward { at + 1 } else { at + ALL.len() - 1 } % ALL.len();
        self.template = ALL.get(next).copied().flatten();
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        match self.focus {
            Focus::Prompt => Some(&mut self.prompt),
            Focus::Repo => Some(&mut self.repo),
            Focus::MaxStages => Some(&mut self.max_stages),
            Focus::Context => Some(&mut self.context_hint),
            Focus::Risk => Some(&mut self.risk_flags),
            _ => None,
        }
    }
}

/// Pure reducer: applies one key and says whether an effect is wanted.
pub fn update(mut form: Form, key: Key) -> (Form, Action) {
    match key {
        Key::Esc => return (form, Action::Quit),
        Key::CtrlP => return (form, Action::Preview),
        Key::CtrlS => return (form, Action::Submit),
        Key::Tab => form.step(1),
        Key::BackTab => form.step(-1),
        Key::Enter => match form.focus {
            Focus::Expander => form.toggle_expander(),
            Focus::Template => form.cycle_template(true),
            Focus::ReportOnly => form.report_only = !form.report_only,
            _ => form.step(1),
        },
        Key::Left | Key::Right => match form.focus {
            Focus::Expander => form.toggle_expander(),
            Focus::Template => form.cycle_template(key == Key::Right),
            Focus::ReportOnly => form.report_only = !form.report_only,
            _ => {}
        },
        Key::Backspace => {
            if let Some(text) = form.text_mut() {
                text.pop();
            }
        }
        Key::Char(c) => match form.focus {
            Focus::Expander if c == ' ' => form.toggle_expander(),
            Focus::Template if c == ' ' => form.cycle_template(true),
            Focus::ReportOnly if c == ' ' => form.report_only = !form.report_only,
            Focus::MaxStages => {
                if c.is_ascii_digit() {
                    form.max_stages.push(c);
                }
            }
            _ => {
                if !c.is_control() {
                    if let Some(text) = form.text_mut() {
                        text.push(c);
                    }
                }
            }
        },
    }
    (form, Action::None)
}

fn options_of(form: &Form) -> Option<TriageOptions> {
    if !form.expanded {
        return None;
    }
    let hint = form.context_hint.trim();
    let options = TriageOptions {
        template: form.template,
        report_only: form.report_only,
        max_stages: (!form.max_stages.is_empty())
            .then(|| form.max_stages.parse().unwrap_or(usize::MAX)),
        context_hint: (!hint.is_empty()).then(|| hint.to_string()),
        risk_flags: form
            .risk_flags
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .collect(),
    };
    (!options.is_empty()).then_some(options)
}

/// Same omission rules as the CLI and the web form: a collapsed or empty
/// "Additional Parameters" group sends no `options` at all.
pub fn to_request(form: &Form, dry_run: bool) -> Request {
    let repo = form.repo.trim();
    let repo = (!repo.is_empty()).then(|| repo.to_string());
    let options = options_of(form);
    if dry_run {
        Request::Triage {
            prompt: form.prompt.clone(),
            repo,
            options,
        }
    } else {
        Request::Submit {
            prompt: form.prompt.clone(),
            repo,
            priority: None::<Priority>,
            options,
        }
    }
}

/// Rows of the form as `(focus, text)`; `focus` is `None` for static rows.
pub fn view(form: &Form) -> Vec<(Option<Focus>, String)> {
    let mark = |f: Focus| if form.focus == f { "» " } else { "  " };
    let mut rows = vec![
        (None, "New job".to_string()),
        (
            Some(Focus::Prompt),
            format!("{}Prompt: {}", mark(Focus::Prompt), form.prompt),
        ),
        (
            Some(Focus::Repo),
            format!("{}Repo:   {}", mark(Focus::Repo), form.repo),
        ),
        (
            Some(Focus::Expander),
            format!(
                "{}{} Additional Parameters",
                mark(Focus::Expander),
                if form.expanded { "v" } else { ">" }
            ),
        ),
    ];
    if form.expanded {
        let template = match form.template {
            None => "(let triage decide)",
            Some(Template::FullFeature) => "full-feature",
            Some(Template::BugFix) => "bug-fix",
            Some(Template::Investigate) => "investigate",
        };
        let flag = if form.report_only { "[x]" } else { "[ ]" };
        let opt =
            |f, label: &str, value: &str| (Some(f), format!("    {}{label} {value}", mark(f)));
        rows.push(opt(Focus::Template, "Template:    ", template));
        rows.push(opt(Focus::ReportOnly, "Report only: ", flag));
        rows.push(opt(Focus::MaxStages, "Max stages:  ", &form.max_stages));
        rows.push(opt(Focus::Context, "Context hint:", &form.context_hint));
        rows.push(opt(Focus::Risk, "Risk flags:  ", &form.risk_flags));
    }
    rows.push((None, String::new()));
    rows.push((None, form.status.clone()));
    rows
}

pub fn draw(frame: &mut Frame, form: &Form) {
    let [body, help] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let lines: Vec<Line> = view(form)
        .into_iter()
        .map(|(focus, text)| {
            if focus.is_some() && focus == Some(form.focus) {
                Line::styled(text, Style::new().add_modifier(Modifier::BOLD))
            } else {
                Line::raw(text)
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    frame.render_widget(
        Paragraph::new(
            "Tab: next  Enter/Space: toggle  Ctrl-P: preview  Ctrl-S: submit  Esc: quit",
        ),
        help,
    );
}

fn describe(result: Result<Response, crate::client::ClientError>) -> String {
    match result {
        Ok(Response::Submitted { short_id, .. }) => format!("Submitted job {short_id}"),
        Ok(Response::Triaged { outcome, .. }) => render_triage(&outcome, false),
        Ok(Response::Error { error }) => format!("Error: {}", error.message),
        Ok(other) => format!("Unexpected response: {other:?}"),
        Err(e) => format!("Error: {e}"),
    }
}

fn key_of(event: crossterm::event::KeyEvent) -> Option<Key> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    match event.code {
        KeyCode::Char('p') if ctrl => Some(Key::CtrlP),
        KeyCode::Char('s') if ctrl => Some(Key::CtrlS),
        KeyCode::Char('c') if ctrl => Some(Key::Esc),
        KeyCode::Char(_) if ctrl => None,
        KeyCode::Char(c) => Some(Key::Char(c)),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::BackTab => Some(Key::BackTab),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Left => Some(Key::Left),
        KeyCode::Right => Some(Key::Right),
        KeyCode::Esc => Some(Key::Esc),
        _ => None,
    }
}

pub fn run(config: &Config) -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, config);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, config: &Config) -> std::io::Result<()> {
    let mut form = Form::default();
    loop {
        terminal.draw(|frame| draw(frame, &form))?;
        let crossterm::event::Event::Key(event) = crossterm::event::read()? else {
            continue;
        };
        let Some(key) = key_of(event) else { continue };
        let (next, action) = update(form, key);
        form = next;
        match action {
            Action::Quit => return Ok(()),
            Action::None => {}
            Action::Preview | Action::Submit => {
                let mut request = to_request(&form, action == Action::Preview);
                canonicalize_repo(&mut request);
                form.status = describe(send(config, &request, Start::IfNeeded));
            }
        }
    }
}

fn canonicalize_repo(request: &mut Request) {
    if let Request::Submit { repo, .. } | Request::Triage { repo, .. } = request {
        *repo = repo.take().map(|r| {
            std::fs::canonicalize(&r)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(r)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn keys(mut form: Form, keys: &[Key]) -> Form {
        for key in keys {
            form = update(form, *key).0;
        }
        form
    }

    fn typed(s: &str) -> Vec<Key> {
        s.chars().map(Key::Char).collect()
    }

    fn screen(form: &Form) -> String {
        let mut terminal = match Terminal::new(TestBackend::new(80, 16)) {
            Ok(t) => t,
            Err(e) => return e.to_string(),
        };
        let _ = terminal.draw(|f| draw(f, form));
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_expander_starts_closed_and_tab_skips_option_fields() {
        let form = Form::default();
        assert!(!form.expanded);
        let form = keys(form, &[Key::Tab, Key::Tab, Key::Tab]);
        assert_eq!(form.focus, Focus::Prompt);
        assert_eq!(
            keys(Form::default(), &[Key::BackTab]).focus,
            Focus::Expander
        );
    }

    #[test]
    fn open_expander_visits_the_five_options_in_order() {
        let mut form = keys(Form::default(), &[Key::Tab, Key::Tab, Key::Enter]);
        assert!(form.expanded);
        assert_eq!(form.focus, Focus::Expander);
        let mut seen = Vec::new();
        for _ in 0..6 {
            form = update(form, Key::Tab).0;
            seen.push(form.focus);
        }
        assert_eq!(
            seen,
            vec![
                Focus::Template,
                Focus::ReportOnly,
                Focus::MaxStages,
                Focus::Context,
                Focus::Risk,
                Focus::Prompt
            ]
        );
    }

    #[test]
    fn right_and_left_toggle_the_expander_and_closing_pulls_focus_back() {
        let mut form = keys(Form::default(), &[Key::Tab, Key::Tab, Key::Right]);
        assert!(form.expanded);
        form = keys(form, &[Key::Tab]);
        assert_eq!(form.focus, Focus::Template);
        form.expanded = true;
        form.focus = Focus::Expander;
        assert!(!update(form, Key::Left).0.expanded);
    }

    #[test]
    fn typing_edits_the_focused_field_and_max_stages_takes_digits_only() {
        let mut form = keys(Form::default(), &typed("fix it"));
        form = keys(form, &[Key::Backspace]);
        assert_eq!(form.prompt, "fix i");
        form.expanded = true;
        form.focus = Focus::MaxStages;
        form = keys(form, &typed("a3x1"));
        assert_eq!(form.max_stages, "31");
        form.focus = Focus::Template;
        form = keys(form, &[Key::Right, Key::Right]);
        assert_eq!(form.template, Some(Template::BugFix));
        form = keys(form, &[Key::Left, Key::Left, Key::Left]);
        assert_eq!(form.template, Some(Template::Investigate));
        form.focus = Focus::ReportOnly;
        assert!(keys(form, &[Key::Char(' ')]).report_only);
    }

    #[test]
    fn effects_are_requested_by_ctrl_keys_and_esc() {
        let f = Form::default();
        assert_eq!(update(f.clone(), Key::CtrlP).1, Action::Preview);
        assert_eq!(update(f.clone(), Key::CtrlS).1, Action::Submit);
        assert_eq!(update(f, Key::Esc).1, Action::Quit);
    }

    #[test]
    fn collapsed_or_empty_options_send_no_options() {
        let mut form = keys(Form::default(), &typed("do it"));
        form.template = Some(Template::BugFix);
        form.max_stages = "3".into();
        assert!(matches!(
            to_request(&form, false),
            Request::Submit { options: None, .. }
        ));
        form.expanded = true;
        form.template = None;
        form.max_stages.clear();
        form.context_hint = "   ".into();
        form.risk_flags = " , ".into();
        assert!(matches!(
            to_request(&form, true),
            Request::Triage { options: None, .. }
        ));
    }

    #[test]
    fn filled_options_match_the_cli_mapping() {
        let form = Form {
            prompt: "do it".into(),
            repo: " /r ".into(),
            expanded: true,
            template: Some(Template::Investigate),
            report_only: true,
            max_stages: "4".into(),
            context_hint: "  see ADR ".into(),
            risk_flags: "touches-auth, schema-change data-loss".into(),
            ..Form::default()
        };
        let want = TriageOptions {
            template: Some(Template::Investigate),
            report_only: true,
            max_stages: Some(4),
            context_hint: Some("see ADR".into()),
            risk_flags: vec![
                "touches-auth".into(),
                "schema-change".into(),
                "data-loss".into(),
            ],
        };
        assert_eq!(
            to_request(&form, false),
            Request::Submit {
                prompt: "do it".into(),
                repo: Some("/r".into()),
                priority: None,
                options: Some(want.clone())
            }
        );
        assert_eq!(
            to_request(&form, true),
            Request::Triage {
                prompt: "do it".into(),
                repo: Some("/r".into()),
                options: Some(want)
            }
        );
    }

    #[test]
    fn rendering_shows_the_collapsed_and_the_open_state() {
        let closed = screen(&Form::default());
        assert!(closed.contains("> Additional Parameters"), "{closed}");
        assert!(!closed.contains("Max stages"), "{closed}");
        let open = screen(&keys(Form::default(), &[Key::Tab, Key::Tab, Key::Enter]));
        assert!(open.contains("v Additional Parameters"), "{open}");
        for label in [
            "Template",
            "Report only",
            "Max stages",
            "Context hint",
            "Risk flags",
        ] {
            assert!(open.contains(label), "{label}: {open}");
        }
        let at = |s: &str| open.find(s);
        assert!(at("Template") < at("Report only") && at("Context hint") < at("Risk flags"));
    }
}
