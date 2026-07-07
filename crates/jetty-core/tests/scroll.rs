/// Tests for scrollback history and vertical scrolling.
use jetty_core::Terminal;

#[test]
fn snapshot_scroll_offset_and_max_after_scroll_up() {
    // 20 cols, 5 rows, feed 10 lines so scrollback history is non-empty.
    let mut term = make_term_with_lines(20, 5, 10);

    // At the bottom, scroll_offset should be 0 and scroll_max > 0.
    let snap_bottom = term.snapshot();
    assert_eq!(snap_bottom.scroll_offset, 0, "at bottom, scroll_offset must be 0");
    assert!(snap_bottom.scroll_max > 0, "expected non-zero scroll_max after feeding 10 lines into 5-row terminal");

    // After scrolling up, scroll_offset must be > 0.
    term.scroll_lines(3);
    let snap_up = term.snapshot();
    assert!(snap_up.scroll_offset > 0, "scroll_offset should be > 0 after scrolling up");
    assert!(snap_up.scroll_max > 0, "scroll_max should remain > 0 while scrolled up");
}

#[test]
fn scroll_to_offset_absolute() {
    // 20 cols, 5 rows, feed 20 lines so we have ample scrollback history.
    let mut term = make_term_with_lines(20, 5, 20);

    // At the bottom, scroll_offset should be 0.
    assert_eq!(term.scroll_offset(), 0, "expected scroll_offset 0 at bottom");
    assert!(term.scroll_max() > 0, "expected non-zero scroll_max after 20 lines");

    // Scroll to absolute offset 3.
    term.scroll_to_offset(3);
    assert_eq!(term.scroll_offset(), 3, "scroll_to_offset(3) should result in scroll_offset==3");

    // Scroll back to 0 (bottom).
    term.scroll_to_offset(0);
    assert_eq!(term.scroll_offset(), 0, "scroll_to_offset(0) should result in scroll_offset==0");
}

/// Feed N numbered lines to the terminal and return a terminal with the
/// screen sized to `cols x rows`.
fn make_term_with_lines(cols: usize, rows: usize, count: usize) -> Terminal {
    let mut term = Terminal::new(cols, rows);
    for i in 0..count {
        // Each line: "L<i>\r\n" — the \r\n advances to the next row.
        let line = format!("L{}\r\n", i);
        term.feed(line.as_bytes());
    }
    term
}

#[test]
fn set_scrollback_shrink_caps_and_frees_history() {
    // 20 cols, 5 rows, ~300 lines → well over 100 lines of history.
    let mut term = make_term_with_lines(20, 5, 300);
    assert!(term.scroll_max() > 100, "premise: >100 history lines before shrink");

    term.set_scrollback_lines(100);
    assert_eq!(term.scroll_max(), 100, "shrink must cap history at the new limit");

    // Scrolling to the very top must clamp within the new cap.
    term.scroll_lines(1000);
    assert!(
        term.scroll_offset() <= 100,
        "scroll_offset {} exceeds the shrunk cap",
        term.scroll_offset()
    );
}

#[test]
fn set_scrollback_grow_raises_cap_for_new_lines() {
    let mut term = make_term_with_lines(20, 5, 300);
    term.set_scrollback_lines(50);
    assert_eq!(term.scroll_max(), 50, "shrink to 50 first");

    // Growing raises the cap but cannot restore already-trimmed lines.
    term.set_scrollback_lines(200);
    assert_eq!(
        term.scroll_max(),
        50,
        "growing must NOT restore trimmed history"
    );

    // New output accumulates up to the raised cap.
    for i in 0..300 {
        term.feed(format!("G{}\r\n", i).as_bytes());
    }
    assert_eq!(term.scroll_max(), 200, "new lines fill up to the raised cap");
}

#[test]
fn scrollback_bottom_shows_last_lines() {
    // 20 cols, 5 rows, feed 10 lines → rows 0..4 of the snapshot at bottom
    // should contain the last 5 lines (L5..L9).
    let term = make_term_with_lines(20, 5, 10);
    let snap = term.snapshot();

    // At the bottom, the last lines fed should be visible.
    // Lines L5 through L9 should appear somewhere in the 5 visible rows.
    let visible: Vec<String> = (0..5).map(|r| snap.row_text(r)).collect();
    let visible_text = visible.join("|");

    // At least L8 and L9 must be present (the most recently written).
    assert!(
        visible_text.contains("L8") || visible_text.contains("L9"),
        "expected recent lines in bottom view, got: {:?}",
        visible
    );
}

#[test]
fn scroll_up_reveals_earlier_lines() {
    // 20 cols, 5 rows, feed 10 lines so L0..L4 are in scrollback.
    // At bottom we see L5..L9 (approximately).
    // After scroll_lines(3) up, we should see earlier lines (L2..L6 area).
    let mut term = make_term_with_lines(20, 5, 10);

    let snap_bottom = term.snapshot();
    let bottom_text: Vec<String> = (0..5).map(|r| snap_bottom.row_text(r)).collect();

    // Scroll up 3 lines into history.
    term.scroll_lines(3);
    let snap_scrolled = term.snapshot();
    let scrolled_text: Vec<String> = (0..5).map(|r| snap_scrolled.row_text(r)).collect();

    // After scrolling up, at least one row should differ from the bottom view.
    assert_ne!(
        bottom_text, scrolled_text,
        "scrolled view should differ from bottom view"
    );

    // The scrolled view should contain at least one earlier line that was NOT
    // visible at the bottom. L0..L4 are candidates (they were pushed into history).
    let scrolled_all = scrolled_text.join("|");
    let bottom_all = bottom_text.join("|");

    // Find at least one line that is in the scrolled view but not the bottom view.
    let earlier_lines = ["L0", "L1", "L2", "L3", "L4"];
    let found_earlier = earlier_lines.iter().any(|&l| scrolled_all.contains(l));
    assert!(
        found_earlier,
        "expected an earlier line (L0..L4) in scrolled view.\nBottom: {:?}\nScrolled: {:?}",
        bottom_all,
        scrolled_all
    );
}

#[test]
fn scroll_to_bottom_restores_live_view() {
    let mut term = make_term_with_lines(20, 5, 10);
    let snap_before = term.snapshot();

    // Scroll up into history.
    term.scroll_lines(5);
    // Then scroll back to bottom.
    term.scroll_to_bottom();
    let snap_after = term.snapshot();

    // After scroll_to_bottom, the view should match the original bottom view.
    for row in 0..5 {
        assert_eq!(
            snap_before.row_text(row),
            snap_after.row_text(row),
            "row {} differs after scroll_to_bottom",
            row
        );
    }
}
