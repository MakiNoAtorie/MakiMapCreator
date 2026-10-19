use mmc_page::markdown::{RowKind, present, present_table_row};
use mmc_page::{EntityLink, entity_links};

#[test]
fn finds_entity_links_with_byte_ranges() {
    let text = "Near [[12|Old Town]] and [[3|Café]].";
    assert_eq!(
        entity_links(text),
        [
            EntityLink {
                bytes: 5..20,
                id: 12,
                label: "Old Town".into()
            },
            EntityLink {
                bytes: 25..36,
                id: 3,
                label: "Café".into()
            },
        ]
    );
}

#[test]
fn ignores_malformed_links() {
    for text in [
        "[[x|a]]",
        "[[1a]]",
        "[[1|]]",
        "[[1|a\n]]",
        "[[1|a]",
        "[[|a]]",
        "[1|a]]",
    ] {
        assert!(entity_links(text).is_empty(), "{text:?}");
    }
    assert_eq!(entity_links("[[[7|a]]")[0].id, 7);
}

#[test]
fn preview_shows_the_label_as_a_link_and_hides_the_markup() {
    let p = present("See [[4|Rivertown]] now\n", 0, None, false, false);
    assert_eq!(p.text, "See Rivertown now\n");
    let span = p.spans.iter().find(|s| s.style.entity == Some(4)).unwrap();
    assert_eq!(&p.text[span.bytes.clone()], "Rivertown");
    assert!(span.style.link && !span.style.code);
    // A click on the label maps back to the label in the source.
    assert_eq!(p.source_at(p.text.find('R').unwrap()), "See [[4|".len());
}

#[test]
fn cursor_line_shows_link_markup_dimmed() {
    let p = present("[[4|R]]\n", 0, Some(0), false, false);
    assert_eq!(p.text, "[[4|R]]\n");
}

#[test]
fn link_pipes_do_not_split_table_cells() {
    let p = present_table_row("| [[1|A]] | b |\n", 0, RowKind::Body, &[3, 3]);
    assert!(
        p.text.contains('A') && !p.text.contains("[["),
        "{:?}",
        p.text
    );
    assert_eq!(p.text.matches('b').count(), 1);
}

#[test]
fn links_in_code_are_text() {
    let text = "Use `[[1|a]]` or\n\n```\n[[2|b]]\n```\n\n    [[3|c]]\n\nsee [[4|d]].";
    let ids: Vec<i64> = entity_links(text).iter().map(|l| l.id).collect();
    assert_eq!(ids, [4]);
}
