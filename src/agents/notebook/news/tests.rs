//! notebook-7b V1: the launch index, from the certain prefix only; never a launch failure.
use serde_json::{Value, json};

use super::*;
use crate::agents::notebook::tests::{notes, person, tx, world};
use crate::agents::notebook::{STATE, append};

fn entry(title: &str, author: &str) -> Value {
    json!({"id": "0123456789abcdef", "author": {"name": author}, "title": title})
}

fn older(n: usize) -> String {
    format!("+{n} older notes: run mux-agent notes")
}

#[test]
fn an_empty_notebook_is_the_one_line() {
    assert_eq!(render(&[]), HEAD);
    assert!(HEAD.contains("mux-agent notes"), "{HEAD}");
}

#[test]
fn entries_are_listed_newest_first_with_their_authors() {
    let got = render(&[entry("first", "a"), entry("second", "person")]);
    assert_eq!(got, format!("{HEAD}\n- second (person)\n- first (a)"));
}

/// Three entries; the newest two fill INDEX_MAX exactly with the `+1` line, so one byte more keeps only the newest.
/// `y` is the second-newest title's length in bytes.
fn boundary() -> usize {
    let room = INDEX_MAX - HEAD.len() - older(1).len() - 3;
    // Two lines: "- <title> (a)" is the title plus 6 bytes; the newest title is 10 bytes.
    room - (10 + 6) - 6
}

#[test]
fn at_one_kilobyte_the_last_entry_that_fits_is_kept_and_the_next_adds_the_count() {
    let y = boundary();
    let three = |second: String| {
        vec![
            entry(&"o".repeat(200), "a"),
            entry(&second, "a"),
            entry("tttttttttt", "a"),
        ]
    };
    let fits = render(&three("t".repeat(y)));
    assert_eq!(fits.len(), INDEX_MAX, "{fits}");
    assert_eq!(
        fits,
        format!(
            "{HEAD}\n- tttttttttt (a)\n- {} (a)\n{}",
            "t".repeat(y),
            older(1)
        )
    );
    let over = render(&three("t".repeat(y + 1)));
    assert_eq!(over, format!("{HEAD}\n- tttttttttt (a)\n{}", older(2)));
}

#[test]
fn a_multibyte_title_at_the_boundary_counts_its_bytes() {
    let y = boundary();
    // y + 1 bytes in y characters: by characters it would fit, by bytes it does not.
    let title = format!("é{}", "t".repeat(y - 1));
    let got = render(&[
        entry(&"o".repeat(200), "a"),
        entry(&title, "a"),
        entry("tttttttttt", "a"),
    ]);
    assert_eq!(got, format!("{HEAD}\n- tttttttttt (a)\n{}", older(2)));
    assert!(got.len() <= INDEX_MAX);
    // y bytes with the same character: kept whole.
    let title = format!("é{}", "t".repeat(y - 2));
    let got = render(&[
        entry(&"o".repeat(200), "a"),
        entry(&title, "a"),
        entry("tttttttttt", "a"),
    ]);
    assert!(got.contains(&format!("- {title} (a)\n")), "{got}");
    assert_eq!(got.len(), INDEX_MAX);
}

#[test]
fn the_index_reads_the_notebook_of_the_scope() {
    let (w, s) = world();
    assert_eq!(index(&w.ctx, &s), HEAD, "no notebook yet");
    for title in ["first", "second"] {
        append(&tx(&w, &s), &person(), title, "body", None).unwrap();
    }
    assert_eq!(
        index(&w.ctx, &s),
        format!("{HEAD}\n- second (person)\n- first (person)")
    );
}

#[test]
fn an_uncertain_prefix_that_cannot_complete_is_the_line_alone() {
    let (w, s) = world();
    for title in ["first", "second"] {
        append(&tx(&w, &s), &person(), title, "body", None).unwrap();
    }
    let at = notes(&w, &s);
    let end = std::fs::metadata(at.join(crate::agents::notebook::LOG))
        .unwrap()
        .len();
    std::fs::write(
        at.join(STATE),
        json!({"durable": end, "uncertain": true}).to_string(),
    )
    .unwrap();
    crate::state::fs::fail_sync_dir(Some(&at));
    let got = index(&w.ctx, &s);
    crate::state::fs::fail_sync_dir(None);
    assert_eq!(got, HEAD);
}

#[test]
fn an_unreadable_notebook_is_the_line_and_its_check() {
    let (w, s) = world();
    let at = notes(&w, &s);
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::fs::write(&at, "not a folder").unwrap();
    let check = crate::agents::notebook::entries(&tx(&w, &s))
        .unwrap_err()
        .check;
    assert_ne!(check, "notes");
    assert_eq!(
        index(&w.ctx, &s),
        format!("{HEAD}\n(notebook unreadable: {check})")
    );
}
