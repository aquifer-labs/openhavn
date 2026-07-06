// SPDX-License-Identifier: Apache-2.0

//! Hand-rolled extraction of the `name:`/`description:` keys from a `SKILL.md`'s YAML
//! frontmatter — intentionally not a YAML parser (no new dependency for it): just the fenced
//! `---` block, one `key: value` per line, with optional matching quotes stripped.

/// The subset of SKILL.md frontmatter the admission gate cares about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Parse the frontmatter block: the file must start with a `---` line; scanning stops at the
/// closing `---` (or EOF). Lines outside `key: value` shape (no `:`) are ignored rather than
/// rejected — this is a minimal extractor, not a validator (the admission gate decides what's
/// required).
pub fn parse(content: &str) -> Frontmatter {
    let mut lines = content.lines();
    match lines.next() {
        Some(line) if line.trim() == "---" => {}
        _ => return Frontmatter::default(),
    }

    let mut frontmatter = Frontmatter::default();
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "name" => frontmatter.name = Some(value),
            "description" => frontmatter.description = Some(value),
            _ => {}
        }
    }
    frontmatter
}

/// Strips one layer of matching `"..."` or `'...'` quoting, the only YAML scalar style this
/// hand-rolled extractor understands.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_and_description() {
        let fm = parse("---\nname: pdf-tools\ndescription: Work with PDFs\n---\nBody text.\n");
        assert_eq!(fm.name.as_deref(), Some("pdf-tools"));
        assert_eq!(fm.description.as_deref(), Some("Work with PDFs"));
    }

    #[test]
    fn strips_matching_quotes() {
        let fm = parse("---\nname: \"pdf-tools\"\ndescription: 'Work with PDFs'\n---\n");
        assert_eq!(fm.name.as_deref(), Some("pdf-tools"));
        assert_eq!(fm.description.as_deref(), Some("Work with PDFs"));
    }

    #[test]
    fn no_opening_fence_yields_empty_frontmatter() {
        let fm = parse("name: pdf-tools\ndescription: Work with PDFs\n");
        assert_eq!(fm, Frontmatter::default());
    }

    #[test]
    fn stops_at_closing_fence() {
        let fm = parse("---\nname: a\n---\ndescription: not-seen\n");
        assert_eq!(fm.name.as_deref(), Some("a"));
        assert_eq!(fm.description, None);
    }

    #[test]
    fn ignores_unrelated_keys_and_bodyless_lines() {
        let fm = parse("---\nlicense: MIT\nname: a\ndescription: b\ntags\n---\n");
        assert_eq!(fm.name.as_deref(), Some("a"));
        assert_eq!(fm.description.as_deref(), Some("b"));
    }
}
