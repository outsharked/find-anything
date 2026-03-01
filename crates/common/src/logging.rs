use std::sync::OnceLock;

use tracing::{field::Visit, Metadata, Subscriber};
use tracing_subscriber::layer::Context;

static IGNORE_PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();

/// Returns `true` if `msg` matches any pattern in `patterns`.
///
/// This is the core matching logic, separated from the global `OnceLock` so
/// it can be called directly in tests with an explicit pattern set.
pub fn is_ignored_with(patterns: &[regex::Regex], msg: &str) -> bool {
    patterns.iter().any(|p| p.is_match(msg))
}

/// Returns `true` if the given message matches any installed ignore pattern.
///
/// Used by `relay_subprocess_logs` to suppress matching lines before they
/// are re-emitted as tracing events, ensuring the same patterns work for
/// both in-process and subprocess log sources.
pub fn is_ignored(msg: &str) -> bool {
    let Some(patterns) = IGNORE_PATTERNS.get() else {
        return false;
    };
    is_ignored_with(patterns, msg)
}

/// Compile and activate the log-ignore patterns from config.
///
/// Should be called once, after the tracing subscriber is initialised but
/// before any work that would produce the noisy log messages.  Subsequent
/// calls (e.g. if somehow invoked twice) are silently ignored — the first
/// call wins.
///
/// Returns an error if any pattern is not a valid regular expression.
pub fn set_ignore_patterns(patterns: &[String]) -> Result<(), regex::Error> {
    let compiled = patterns
        .iter()
        .map(|p| regex::Regex::new(p))
        .collect::<Result<Vec<_>, _>>()?;
    let _ = IGNORE_PATTERNS.set(compiled);
    Ok(())
}

// ── Per-layer filter ──────────────────────────────────────────────────────────

/// A `tracing_subscriber` per-layer filter that suppresses events whose
/// message matches any pattern installed via [`set_ignore_patterns`].
///
/// Install it on the fmt layer:
/// ```ignore
/// tracing_subscriber::fmt::layer().with_filter(LogIgnoreFilter)
/// ```
pub struct LogIgnoreFilter;

impl<S: Subscriber> tracing_subscriber::layer::Filter<S> for LogIgnoreFilter {
    fn enabled(&self, _meta: &Metadata<'_>, _cx: &Context<'_, S>) -> bool {
        true
    }

    fn event_enabled(
        &self,
        event: &tracing::Event<'_>,
        _cx: &Context<'_, S>,
    ) -> bool {
        let Some(patterns) = IGNORE_PATTERNS.get() else {
            return true;
        };
        if patterns.is_empty() {
            return true;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        // For log-bridged events (from the `log` crate), tracing-log sets
        // metadata().target() to the fixed string "log" and stores the
        // original crate target in the "log.target" field.  Use the field
        // value when present so patterns like "pdf_extract: unknown glyph
        // name" work correctly against log records from external crates.
        let target = visitor.log_target.as_deref().unwrap_or_else(|| event.metadata().target());
        let candidate = format!("{target}: {}", visitor.message);
        // Also match against the message alone so that patterns written
        // against the in-process log target (e.g. "pdf_extract: unknown glyph")
        // continue to work when the same message arrives via subprocess relay
        // (where the tracing target becomes "subprocess").
        !is_ignored_with(patterns, &candidate) && !is_ignored_with(patterns, &visitor.message)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

#[derive(Default)]
struct MessageVisitor {
    message: String,
    /// Set for log-bridged events: the original crate target from the `log.target` field.
    log_target: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "log.target" => self.log_target = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "log.target" => self.log_target = Some(format!("{value:?}")),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_ignored_with;

    fn pats(patterns: &[&str]) -> Vec<regex::Regex> {
        patterns.iter().map(|p| regex::Regex::new(p).unwrap()).collect()
    }

    #[test]
    fn no_patterns_never_ignored() {
        assert!(!is_ignored_with(&[], "anything goes here"));
    }

    #[test]
    fn substring_pattern_matches() {
        let p = pats(&["pdf_extract: unknown glyph"]);
        assert!(is_ignored_with(&p, "pdf_extract: unknown glyph name 'box3' for font ArialMT"));
    }

    #[test]
    fn non_matching_pattern_passes_through() {
        let p = pats(&["lopdf::reader"]);
        assert!(!is_ignored_with(&p, "pdf_extract: unknown glyph name 'box3' for font ArialMT"));
    }

    #[test]
    fn regex_wildcard_pattern_matches() {
        let p = pats(&["unknown glyph name '.*'"]);
        assert!(is_ignored_with(&p, "pdf_extract: unknown glyph name 'box3' for font ArialMT"));
    }

    #[test]
    fn multiple_patterns_any_match_is_ignored() {
        let p = pats(&["no match here", "pdf_extract: unknown"]);
        assert!(is_ignored_with(&p, "pdf_extract: unknown glyph foo"));
    }

    #[test]
    fn multiple_patterns_no_match_passes_through() {
        let p = pats(&["lopdf::reader", "sevenz_rust"]);
        assert!(!is_ignored_with(&p, "pdf_extract: unknown glyph foo"));
    }

    #[test]
    fn invalid_regex_does_not_compile() {
        assert!(regex::Regex::new("[invalid").is_err());
    }
}
