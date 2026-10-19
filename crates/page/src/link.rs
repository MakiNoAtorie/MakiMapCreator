//! `[[id|label]]` entity links: stored by id so a rename keeps them resolving (ADR-0012).

use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityLink {
    /// The whole `[[id|label]]`, as byte offsets.
    pub bytes: Range<usize>,
    pub id: i64,
    pub label: String,
}

/// Links in `text` in order. The label is one line with no `]`.
// ponytail: links inside code spans and fences also count; skip them if backlinks show false hits.
pub fn entity_links(text: &str) -> Vec<EntityLink> {
    let code = code_ranges(text);
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("[[") {
        let start = from + i;
        if code.iter().any(|r| r.contains(&start)) {
            from = start + 2;
            continue;
        }
        match parse(&text[start + 2..]) {
            Some((id, label, len)) => {
                out.push(EntityLink {
                    bytes: start..start + 2 + len,
                    id,
                    label: label.to_owned(),
                });
                from = start + 2 + len;
            }
            None => from = start + 1,
        }
    }
    out
}

/// Byte ranges of code spans and code blocks: link markup there is plain text.
fn code_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    use pulldown_cmark::{Event, Parser, Tag};
    Parser::new(text)
        .into_offset_iter()
        .filter_map(|(e, r)| match e {
            Event::Code(_) | Event::Start(Tag::CodeBlock(_)) => Some(r),
            _ => None,
        })
        .collect()
}

/// Parses `id|label]]`; returns the id, the label and the bytes used.
fn parse(rest: &str) -> Option<(i64, &str, usize)> {
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let id = rest[..digits].parse().ok()?;
    let body = rest[digits..].strip_prefix('|')?;
    let end = body.find([']', '\n', '\r'])?;
    (end > 0 && body[end..].starts_with("]]")).then_some((id, &body[..end], digits + 1 + end + 2))
}
