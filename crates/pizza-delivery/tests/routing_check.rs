mod harness;

use harness::{Harness, TestResult};

const MODELS_SCRIPT: &str =
    "#!/bin/sh\n[ \"$1\" = models ] && printf 'acme/known\\nacme/other\\n'\n";
const BROKEN_MODELS_SCRIPT: &str = "#!/bin/sh\necho boom >&2\nexit 3\n";

const GOOD: &str = r#"
quota_threshold = 0.75
[agents.opencode]
adapter = "opencode"
command = "{root}/opencode"
[[rules]]
stage = "triage"
chain = [
  { agent = "opencode", model = "acme/known" },
  { agent = "opencode", model = "acme/typo" },
  { agent = "claude", model = "haiku" },
]
[[rules]]
stage = "review"
chain = [{ agent = "opencode", model = "acme/known#fast" }]
"#;

#[test]
fn check_prints_the_resolved_chain_per_stage_and_the_quota_threshold() -> TestResult {
    let h = Harness::with_routing(&[("opencode", MODELS_SCRIPT)], GOOD)?;
    let cli = h.run(&["routing", "check"])?;
    assert_eq!(cli.code, 0, "{}", cli.stderr);
    assert!(
        cli.stdout.contains("quota threshold: 75%"),
        "{}",
        cli.stdout
    );
    let triage = cli
        .stdout
        .lines()
        .find(|l| l.starts_with("triage"))
        .ok_or("no triage line")?;
    assert!(
        triage.contains("opencode:acme/known -> opencode:acme/typo -> claude:haiku"),
        "{triage}"
    );
    assert!(
        cli.stdout.lines().any(|l| l.starts_with("grill")),
        "{}",
        cli.stdout
    );
    Ok(())
}

#[test]
fn check_warns_about_models_the_cli_cannot_list_but_never_fails_for_them() -> TestResult {
    let h = Harness::with_routing(&[("opencode", MODELS_SCRIPT)], GOOD)?;
    let cli = h.run(&["routing", "check"])?;
    assert_eq!(cli.code, 0, "{}", cli.stderr);
    assert!(
        cli.stdout
            .contains("warning: agent 'opencode' model 'acme/typo' is not in `opencode models`"),
        "{}",
        cli.stdout
    );
    assert!(!cli.stdout.contains("acme/known' is not"), "{}", cli.stdout);
    Ok(())
}

#[test]
fn check_survives_a_model_lister_that_fails() -> TestResult {
    let h = Harness::with_routing(&[("opencode", BROKEN_MODELS_SCRIPT)], GOOD)?;
    let cli = h.run(&["routing", "check"])?;
    assert_eq!(cli.code, 0, "{}", cli.stderr);
    assert!(
        cli.stdout.contains("could not list models"),
        "{}",
        cli.stdout
    );
    Ok(())
}

#[test]
fn check_fails_when_a_skill_stage_has_no_skills_capable_candidate() -> TestResult {
    let routing = r#"
[agents.hermes]
adapter = "hermes"
command = "hermes"
[[rules]]
stage = "review"
chain = [{ agent = "hermes" }]
"#;
    let h = Harness::with_routing(&[], routing)?;
    let cli = h.run(&["routing", "check"])?;
    assert_ne!(cli.code, 0);
    assert!(
        cli.stdout.contains(
            "review: no agent in the chain for stage 'review' has the required capabilities"
        ),
        "{}",
        cli.stdout
    );
    Ok(())
}

#[test]
fn check_and_daemon_name_the_bad_file_and_field() -> TestResult {
    let routing = "[[rules]]\nchain = [{ agent = \"ghost\" }]\n";
    let h = Harness::with_routing(&[], routing)?;
    for args in [&["routing", "check"][..], &["daemon"][..]] {
        let cli = h.run(args)?;
        assert_ne!(cli.code, 0, "{args:?}");
        assert!(cli.stderr.contains("routing.toml"), "{}", cli.stderr);
        assert!(
            cli.stderr.contains("rules[0].chain[0].agent"),
            "{}",
            cli.stderr
        );
    }
    Ok(())
}
