use serde::{Deserialize, Serialize};

pub const CONTEXT_CAP_BYTES: usize = 32_000;
const TRUNCATION_MARK: &str = "\n[truncated to fit the size cap]\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceUse {
    pub source: String,
    pub bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub text: String,
    pub included: Vec<SourceUse>,
    pub omitted: Vec<String>,
}

fn cut(text: &str, max: usize) -> &str {
    let end = (0..=max.min(text.len()))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    &text[..end]
}

fn heading(source: &str) -> String {
    format!("## {source}\n\n")
}

pub fn assemble_context(sections: &[Section], cap: usize) -> Context {
    let all: Vec<&str> = sections.iter().map(|s| s.source.as_str()).collect();
    let header_bound = format!(
        "# Context\n\nSources: {}\nOmitted (size cap): {}\n\n",
        all.join(", "),
        all.join(", ")
    )
    .len();
    let mut remaining = cap.saturating_sub(header_bound);
    let mut body = String::new();
    let mut included = Vec::new();
    let mut omitted = Vec::new();
    for section in sections {
        let head = heading(&section.source);
        let text = section.text.trim();
        let fits = head.len() + text.len() < remaining;
        let room = remaining.saturating_sub(head.len() + TRUNCATION_MARK.len() + 1);
        if text.is_empty() {
            continue;
        }
        let piece = if fits {
            Some((format!("{head}{text}\n"), false))
        } else if room > 0 {
            let kept = cut(text, room);
            Some((format!("{head}{kept}{TRUNCATION_MARK}"), true))
        } else {
            None
        };
        match piece {
            Some((piece, truncated)) if !truncated || !cut_is_empty(text, room) => {
                remaining = remaining.saturating_sub(piece.len());
                included.push(SourceUse {
                    source: section.source.clone(),
                    bytes: piece.len(),
                    truncated,
                });
                body.push_str(&piece);
            }
            _ => omitted.push(section.source.clone()),
        }
    }
    let names: Vec<&str> = included.iter().map(|s| s.source.as_str()).collect();
    let mut header = format!("# Context\n\nSources: {}\n", names.join(", "));
    if !omitted.is_empty() {
        header.push_str(&format!("Omitted (size cap): {}\n", omitted.join(", ")));
    }
    header.push('\n');
    Context {
        text: format!("{header}{body}"),
        included,
        omitted,
    }
}

fn cut_is_empty(text: &str, room: usize) -> bool {
    cut(text, room).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(source: &str, text: &str) -> Section {
        Section {
            source: source.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn small_sources_are_all_included_and_listed() {
        let ctx = assemble_context(
            &[
                section("engram", "obs one"),
                section("mnemon", "prefers rust"),
            ],
            CONTEXT_CAP_BYTES,
        );
        assert!(ctx.text.contains("Sources: engram, mnemon"));
        assert!(ctx.text.contains("## engram\n\nobs one"));
        assert!(ctx.text.contains("## mnemon\n\nprefers rust"));
        assert!(ctx.omitted.is_empty());
        assert!(ctx.included.iter().all(|s| !s.truncated));
    }

    #[test]
    fn the_size_cap_is_never_exceeded_and_overflow_is_truncated_then_omitted() {
        let big = "x".repeat(5_000);
        let sections = [
            section("engram", &big),
            section("ledger", &big),
            section("conventions", &big),
        ];
        let ctx = assemble_context(&sections, 6_000);
        assert!(ctx.text.len() <= 6_000, "len {}", ctx.text.len());
        assert_eq!(ctx.included[0].source, "engram");
        assert!(!ctx.included[0].truncated);
        assert!(ctx.included.iter().any(|s| s.truncated));
        assert!(ctx.text.contains("[truncated to fit the size cap]"));
        assert!(!ctx.omitted.is_empty() || ctx.included.len() == 3);
        for source in &ctx.included {
            assert!(ctx.text.contains(&source.source));
        }
    }

    #[test]
    fn truncation_respects_multibyte_characters() {
        let text = "é".repeat(3_000);
        let ctx = assemble_context(&[section("engram", &text)], 2_000);
        assert!(ctx.text.len() <= 2_000);
        assert!(ctx.included[0].truncated);
    }

    #[test]
    fn a_tiny_cap_omits_sources_and_says_so() {
        let ctx = assemble_context(&[section("engram", "something")], 10);
        assert!(ctx.included.is_empty());
        assert_eq!(ctx.omitted, vec!["engram".to_owned()]);
    }

    #[test]
    fn empty_sources_are_skipped() {
        let ctx = assemble_context(&[section("engram", "  \n")], CONTEXT_CAP_BYTES);
        assert!(ctx.included.is_empty());
        assert!(ctx.omitted.is_empty());
    }
}
