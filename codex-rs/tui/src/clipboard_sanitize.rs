//! Clipboard text sanitization pipeline (currently focused on copy, reusable for inbound paste).
//!
//! Goals:
//! 1. Centralize transformations applied before placing conversation text on the system clipboard.
//! 2. Provide an ordered, easily extensible rule list (pipeline) rather than a single closure.
//! 3. Allow reuse for inbound sanitization (e.g. paste) so both directions can converge if desired.
//! 4. Keep raw copying (bypassing all rules) trivial for power‑users / debugging.
//!
//! Design:
//! - Each rule implements `SanitizeRule` returning a (possibly) transformed String.
//! - Rules execute sequentially; output of one is fed into the next.
//! - Global rule list guarded by RwLock; mutation is rare (startup/tests), reads are frequent.
//! - Default rules are intentionally minimal (currently: strip decorative user prompt glyph `▌`).
//! - Future examples (outbound): trim trailing spaces, collapse multiple blank lines, redact secrets patterns.
//! - Future examples (inbound): strip zero-width chars, normalize Unicode (NFC), limit repeated whitespace.
//!
//! Extension HOWTO:
//! 1. Define a rule: `let rule = fn_rule("my_rule", |s| /* transform */ s.to_string());`
//! 2. Register it: `register_rule(rule);` (appends at end; ordering matters.)
//! 3. For full control: build a Vec and call `set_rules(...)` to replace the pipeline.
//! 4. Tests: assert both single-run output and idempotency if appropriate.
//!
//! Inbound vs Outbound:
//! - We expose `sanitize_for_copy` (outbound) and `sanitize_incoming` (inbound). For now they share
//!   the same pipeline; later they could diverge if we add rules that should only run in one direction.
//!
//! Thread safety: rules are stored behind Arc; registration swaps the whole Vec atomically.
//! Tests cover idempotency, ordering, and custom rule injection.

use std::borrow::Cow;
use std::sync::{Arc, RwLock};

/// Decorative glyph used as a visual prefix for user lines in the TUI.
pub const LIVE_PREFIX_GLYPH: char = '▌';

/// Trait for an individual sanitization rule.
///
/// Guidelines for new rules:
/// - Prefer pure, deterministic, side‑effect free transforms.
/// - Aim for idempotency: applying the rule twice should not change output further.
/// - Return `Cow::Borrowed` when no modification is needed to avoid allocations for large texts.
pub trait SanitizeRule: Send + Sync {
    fn name(&self) -> &str;
    fn apply<'a>(&self, input: &'a str) -> Cow<'a, str>;
}

/// Simple rule defined by a function.
struct FnRule {
    name: &'static str,
    f: Box<dyn Fn(&str) -> Cow<'_, str> + Send + Sync>,
}

impl SanitizeRule for FnRule {
    fn name(&self) -> &str { self.name }
    fn apply<'a>(&self, input: &'a str) -> Cow<'a, str> { (self.f)(input) }
}

/// Internal container for the active ordered rules.
struct RuleSet { rules: Vec<Arc<dyn SanitizeRule>> }

impl RuleSet {
    fn apply(&self, input: &str) -> String {
        let mut cow: Cow<'_, str> = Cow::Borrowed(input);
        for rule in &self.rules {
            // Each rule can borrow or allocate. If it allocates, cow becomes owned.
            let next = rule.apply(&cow);
            // Avoid double allocation: only replace if different reference or new owned data.
            cow = match (cow, next) {
                (Cow::Borrowed(prev), Cow::Borrowed(cur)) if core::ptr::eq(prev, cur) => Cow::Borrowed(cur),
                (_, new_cow) => new_cow,
            };
        }
        cow.into_owned()
    }
}

lazy_static::lazy_static! {
    static ref RULES: RwLock<Arc<RuleSet>> = RwLock::new(Arc::new(RuleSet { rules: default_rules() }));
}

fn default_rules() -> Vec<Arc<dyn SanitizeRule>> {
    vec![Arc::new(FnRule {
        name: "strip_user_prefix_glyph",
        f: Box::new(|input: &str| {
            // Fast path: if the glyph never appears, borrow input.
            if !input.contains(LIVE_PREFIX_GLYPH) { return Cow::Borrowed(input); }
            let mut changed = false;
            let mut out = String::with_capacity(input.len());
            for (i, line) in input.lines().enumerate() {
                let transformed = if let Some(rest) = line.strip_prefix("▌ ") {
                    changed = true; rest
                } else if let Some(rest) = line.strip_prefix(LIVE_PREFIX_GLYPH) {
                    changed = true; rest
                } else {
                    line
                };
                if i > 0 { out.push('\n'); }
                out.push_str(transformed);
            }
            if changed { Cow::Owned(out) } else { Cow::Borrowed(input) }
        }),
    })]
}

/// Replace the entire rule pipeline (primarily for tests or advanced configuration).
pub fn set_rules(new_rules: Vec<Arc<dyn SanitizeRule>>) {
    *RULES.write().expect("rules lock") = Arc::new(RuleSet { rules: new_rules });
}

/// Append a new rule at the end of the current pipeline.
pub fn register_rule(rule: Arc<dyn SanitizeRule>) {
    let mut current = (*RULES.read().expect("rules lock")).rules.clone();
    current.push(rule);
    set_rules(current);
}

/// Reset rules to the built‑in defaults.
pub fn reset_to_defaults() { set_rules(default_rules()); }

/// Apply all active rules to the provided raw text.
pub fn sanitize_for_copy(raw: &str) -> String { RULES.read().expect("rules lock").apply(raw) }

/// Sanitize incoming pasted text. Currently identical to `sanitize_for_copy`, but kept
/// separate for semantic clarity and to allow future divergence (e.g. inbound-specific
/// normalization like zero-width char stripping that we might not want on outbound copy).
pub fn sanitize_incoming(raw: &str) -> String { RULES.read().expect("rules lock").apply(raw) }

/// Create a simple function rule (public helper for tests / extensions).
pub fn fn_rule(name: &'static str, f: impl Fn(&str) -> Cow<'_, str> + Send + Sync + 'static) -> Arc<dyn SanitizeRule> {
    Arc::new(FnRule { name, f: Box::new(f) })
}

// Example (commented) inbound‑only rule idea:
// let zero_width_strip = fn_rule("strip_zero_width", |s| {
//     if !s.chars().any(|c| matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')) { return Cow::Borrowed(s); }
//     let filtered: String = s.chars().filter(|c| !matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')).collect();
//     Cow::Owned(filtered)
// });
// register_rule(zero_width_strip);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rule_strips_glyph() {
        reset_to_defaults();
        let input = "▌ line one\n▌ line two\nplain";
        let out = sanitize_for_copy(input);
        assert_eq!(out, "line one\nline two\nplain");
    }

    #[test]
    fn idempotent_on_clean_input() {
        reset_to_defaults();
        let input = "line one\nline two";
        assert_eq!(sanitize_for_copy(input), input);
    }

    #[test]
    fn can_register_additional_rule_ordering_respected() {
        reset_to_defaults();
        // Add rule that uppercases everything AFTER glyph removal.
        register_rule(fn_rule("uppercase", |s| s.to_ascii_uppercase()));
        let input = "▌ hello";
        assert_eq!(sanitize_for_copy(input), "HELLO");
    }

    #[test]
    fn replace_rules_custom_pipeline() {
        use std::borrow::Cow;
        set_rules(vec![
            fn_rule("trim", |s| {
                let trimmed = s.trim();
                if core::ptr::eq(trimmed, s) { Cow::Borrowed(s) } else { Cow::Owned(trimmed.to_string()) }
            }),
            fn_rule("suffix", |s| Cow::Owned(format!("{s}-X"))),
        ]);
        let input = "  hi  ";
        assert_eq!(sanitize_for_copy(input), "hi-X");
    }

    #[test]
    fn sanitize_incoming_matches_copy_for_now() {
        reset_to_defaults();
        let input = "▌ abc";
        assert_eq!(sanitize_incoming(input), sanitize_for_copy(input));
    }

    #[test]
    fn raw_paste_would_preserve_glyph() {
        reset_to_defaults();
        let input = "▌ hello world";
        // Simulate raw paste (bypass) by not calling sanitize_incoming.
        // The glyph should remain. Sanitized path removes it.
        assert_eq!(input.starts_with("▌"), true);
        assert_eq!(sanitize_incoming(input).starts_with("▌"), false);
    }
}
