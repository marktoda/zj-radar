//! Test-only support shared across the two rendering oracles: the insta
//! snapshot suite (`render::tests`) and the executable spec
//! (`reference_tests`). Both judge the renderer by the same visible-grid
//! semantics, so the vt100 helper lives here once — if it drifted into two
//! copies the oracles could quietly stop measuring the same thing.

/// Render raw ANSI output into the visible character grid through a real VT
/// parser, one line per terminal row, each row trimmed of trailing spaces and
/// trailing blank rows removed — the human-readable snapshot.
pub(crate) fn grid(raw: &str, width: u16) -> String {
    // +1 row of headroom: when `raw` ends with a trailing newline (e.g. Cards
    // and Comfortable emit a trailing gap row), processing that final newline
    // advances the cursor past the last row and scrolls the top line (the
    // " RADAR" title) off the screen. The extra blank row is removed by the
    // trailing-blank trim below, so scenarios that don't scroll are unaffected.
    let height = (raw.lines().count().max(1) + 1) as u16;
    let mut parser = vt100::Parser::new(height, width, 0);
    let joined = raw.replace('\n', "\r\n");
    parser.process(joined.as_bytes());
    let screen = parser.screen();
    let lines: Vec<String> = (0..height)
        .map(|r| {
            (0..width)
                .map(|c| {
                    screen
                        .cell(r, c)
                        .map(|cell| cell.contents())
                        .unwrap_or_default()
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();

    // Trim trailing blank rows (the headroom row, plus any trailing gap rows).
    let end = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    lines[..end].join("\n")
}
