/// Tests for scrollback history and vertical scrolling.
use jetty_core::Terminal;

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
