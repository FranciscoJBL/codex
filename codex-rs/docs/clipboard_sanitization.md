# Clipboard & Text Sanitization

This document describes how Codex TUI handles text going *to* and *from* the system clipboard, and how you can extend the sanitization pipeline.

## Overview

Inbound paste text flows through a lightweight, ordered rule pipeline before insertion. Each rule can normalize or clean presentation artifacts (e.g. the current default strips a decorative prefix glyph) while leaving semantic content intact. The pipeline is fully extensible: you can append additional rules or replace the entire set.

| Action | Keys | Behavior |
|--------|------|----------|
| Paste (sanitized) | Ctrl+V | Try image → else apply `sanitize_incoming` over text. |
| Paste (raw) | Ctrl+Alt+V | Try image → else insert raw text (no rules). |

Rationale: paste is the safest interception point to normalize purely presentational artifacts without risking loss of intentional user content.

## Inbound Paste Flow
1. Terminal emits a `Paste` event (bracketed) *or* a burst of rapid key events (non‑bracketed). 
2. `ChatWidget::handle_paste(text)` is invoked.
3. Text is passed through `sanitize_incoming`.
4. `BottomPane` / `ChatComposer` decide whether to: 
   - Insert placeholder for very large pastes (\> 1000 chars).
   - Treat it as an image path and attach image.
   - Insert text directly.

Non‑bracketed rapid pastes are detected via a heuristic “paste burst” (`PasteBurst`) and flushed as a single logical paste.

## Extensible Pipeline
The pipeline lives in `tui/src/clipboard_sanitize.rs` and is shared (for now) by outbound and inbound sanitization. Each rule implements:

```rust
pub trait SanitizeRule: Send + Sync {
    fn name(&self) -> &str;
    fn apply<'a>(&self, input: &'a str) -> Cow<'a, str>;
}
```

Rules are ordered. The output of one becomes the input of the next.

### Default Rule
`strip_user_prefix_glyph`: removes a leading `▌` (with or without a single following space) from each line if present. Leaves other content unchanged.

### Registering a Rule
```rust
use codex_tui::clipboard_sanitize::{fn_rule, register_rule};
use std::borrow::Cow;

// Collapse multiple blank lines down to a single empty line
register_rule(fn_rule("collapse_blank_lines", |s| {
    let mut last_blank = false;
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let blank = line.trim().is_empty();
        if blank && last_blank { continue; }
        if !out.is_empty() { out.push('\n'); }
        out.push_str(line);
        last_blank = blank;
    }
    Cow::Owned(out)
}));
```

### Replacing the Entire Pipeline
```rust
use codex_tui::clipboard_sanitize::{set_rules, fn_rule};
use std::borrow::Cow;

set_rules(vec![
    fn_rule("trim", |s| {
        let t = s.trim();
        if core::ptr::eq(t, s) { Cow::Borrowed(s) } else { Cow::Owned(t.to_string()) }
    }),
    fn_rule("suffix", |s| Cow::Owned(format!("{s}\n-- end --"))),
]);
```

### Guidelines for Writing Rules
- Pure & deterministic – no side effects (logging only if needed at debug level).
- Idempotent – applying twice should not change the result further.
- Prefer returning `Cow::Borrowed` if no changes (avoid unnecessary allocations).
- Keep expensive operations (regex, Unicode normalization) to the *end* of the pipeline when earlier rules may reduce input size.

## Diverging Inbound vs Outbound
Currently `sanitize_incoming` and `sanitize_for_copy` share the pipeline. If we later need direction‑specific transforms (e.g. remove zero‑width characters only on paste) we can:

1. Introduce a second rule list (`INBOUND_RULES`).
2. Fork existing default rules into both lists.
3. Apply new inbound‑exclusive rules after glyph stripping.

The public API already allows this change without modifying call sites.

## Example Inbound‑Only Rule (Concept)
```rust
// (Not active by default)
let zero_width_strip = fn_rule("strip_zero_width", |s| {
    if !s.chars().any(|c| matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')) { return Cow::Borrowed(s); }
    Cow::Owned(s.chars().filter(|c| !matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')).collect())
});
// register_rule(zero_width_strip); // would apply to both directions today
```

## Large Paste Handling
If an inbound paste exceeds 1000 characters, Codex inserts a placeholder token (e.g. `[Pasted Content 1234 chars]`) and stores the real content internally. This keeps the UI responsive on very large multi‑line pastes.

## Image Clipboard Handling
`Ctrl+V` first attempts to read an image from the clipboard:
1. Uses `arboard` to fetch file paths or image data.
2. Converts to PNG and writes a temporary file (unique name).
3. Attaches an `[image WxH FORMAT]` element to the composer.

If no image is present, it falls back to text paste (sanitized inbound).

## Raw Paste Use Cases
Use **Ctrl+Alt+V** when you explicitly need the exact text as produced elsewhere (including any `▌` glyphs or formatting you do not want normalized). Helpful for debugging or reporting issues with how the sanitizer behaves.

## FAQ
**Q: Could sanitization break legitimate content?**  
Only if your pasted text intentionally begins lines with `▌`. Use raw paste (Ctrl+Alt+V) or customize / replace the rule set via `set_rules`.

**Q: Does paste sanitization affect image paths?**  
No practical effect: the glyph rule runs first and only strips a leading decorative glyph; valid paths are unaffected.

## Future Ideas
- Direction‑specific pipelines.
- Redaction rule (API keys / tokens via pattern detection).
- Unicode normalization & homoglyph defense.
- Optional configuration binding for enabling/disabling individual rules.

---
*Last updated: migrated to paste‑only sanitization.*
