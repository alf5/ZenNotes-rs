//! `zennotes://open?path=…` links for note objects the MCP tools return
//! (upstream apps/desktop/src/main/deep-links.ts `buildOpenNoteDeepLink`).

/// Each path segment goes through `encodeURIComponent` so names with spaces,
/// `#`, `?` or `&` survive; slashes stay readable. Parens are encoded beyond
/// what `encodeURIComponent` does because these URLs live inside markdown
/// `[title](url)` links, where a bare `)` ends the link.
pub fn open_note_link(rel_path: &str) -> String {
    let encoded: Vec<String> = rel_path.replace('\\', "/").split('/').map(encode_segment).collect();
    format!("zennotes://open?path={}", encoded.join("/"))
}

fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for byte in seg.bytes() {
        // encodeURIComponent's unreserved set, minus `(` and `)`.
        if byte.is_ascii_alphanumeric() || b"-_.!~*'".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_upstream() {
        assert_eq!(open_note_link("inbox/Hi.md"), "zennotes://open?path=inbox/Hi.md");
        assert_eq!(
            open_note_link("inbox/Work (old)/a b#c&d?.md"),
            "zennotes://open?path=inbox/Work%20%28old%29/a%20b%23c%26d%3F.md"
        );
        assert_eq!(open_note_link("Café.md"), "zennotes://open?path=Caf%C3%A9.md");
    }
}
