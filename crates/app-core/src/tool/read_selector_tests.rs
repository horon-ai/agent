use super::*;

fn lines(count: usize) -> String {
    (1..=count).map(|n| format!("L{n}\n")).collect()
}

#[test]
fn parses_every_selector_form() {
    assert_eq!(
        parse_selector("10-40").unwrap(),
        Selector::Lines {
            ranges: vec![(10, Some(40))],
            raw: false,
        }
    );
    assert_eq!(
        parse_selector("7").unwrap(),
        Selector::Lines {
            ranges: vec![(7, Some(7))],
            raw: false,
        }
    );
    assert_eq!(
        parse_selector("10-").unwrap(),
        Selector::Lines {
            ranges: vec![(10, None)],
            raw: false,
        }
    );
    assert_eq!(
        parse_selector("10+20").unwrap(),
        Selector::Lines {
            ranges: vec![(10, Some(29))],
            raw: false,
        }
    );
    assert_eq!(
        parse_selector("-50").unwrap(),
        Selector::Tail {
            count: 50,
            ranges: Vec::new(),
            raw: false,
        }
    );
    assert_eq!(
        parse_selector("1,3-5").unwrap(),
        Selector::Lines {
            ranges: vec![(1, Some(1)), (3, Some(5))],
            raw: false,
        }
    );
    assert_eq!(parse_selector("raw").unwrap(), Selector::Raw);
    assert_eq!(
        parse_selector("raw:50-100").unwrap(),
        Selector::Lines {
            ranges: vec![(50, Some(100))],
            raw: true,
        }
    );
    assert_eq!(
        parse_selector("1-50:raw").unwrap(),
        Selector::Lines {
            ranges: vec![(1, Some(50))],
            raw: true,
        }
    );
    assert_eq!(
        parse_selector("raw:-60").unwrap(),
        Selector::Tail {
            count: 60,
            ranges: Vec::new(),
            raw: true,
        }
    );
}

#[test]
fn rejects_malformed_selectors_with_the_documented_message() {
    for selector in ["0", "10-5", "abc", "10++2", "raw:raw", "1:2:3", ""] {
        let error = parse_selector(selector).expect_err("selector should be rejected");
        let text = error.to_string();
        assert!(
            text.starts_with(&format!("Invalid selector ':{selector}'.")),
            "selector={selector:?} error={text}"
        );
        assert!(text.contains("Use :N, :N-M, :N+K"), "error={text}");
    }
}

#[test]
fn splits_a_selector_off_a_path_and_leaves_literals_alone() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cwd = temp.path();
    let (path, selector) = split_path_and_selector("src/x.rs:2-3", cwd).unwrap();
    assert_eq!(path, "src/x.rs");
    assert_eq!(
        selector,
        Selector::Lines {
            ranges: vec![(2, Some(3))],
            raw: false,
        }
    );

    // A path with no colon is a literal path.
    let (path, selector) = split_path_and_selector("src/x.rs", cwd).unwrap();
    assert_eq!(path, "src/x.rs");
    assert_eq!(selector, Selector::None);

    // A suffix that cannot be a selector leaves the colon in the name.
    let (path, selector) = split_path_and_selector("notes:draft.md", cwd).unwrap();
    assert_eq!(path, "notes:draft.md");
    assert_eq!(selector, Selector::None);

    // A scheme's `//` is never a separator.
    let (path, selector) = split_path_and_selector("agent://child/0", cwd).unwrap();
    assert_eq!(path, "agent://child/0");
    assert_eq!(selector, Selector::None);
}

#[test]
fn an_existing_file_wins_over_a_selector_lookalike() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("a:1");
    std::fs::write(&path, "one\ntwo\n").expect("write file");

    let (authored, selector) = split_path_and_selector("a:1", temp.path()).unwrap();
    assert_eq!(authored, "a:1");
    assert_eq!(selector, Selector::None);
}

#[test]
fn a_malformed_selector_on_a_missing_path_is_an_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    let error = split_path_and_selector("src/x.rs:10-5", temp.path()).expect_err("should be rejected");
    assert!(error.to_string().contains("Invalid selector ':10-5'."), "{error}");
}

#[test]
fn splits_a_selector_off_a_uri_without_touching_the_scheme() {
    let (path, selector) = split_uri_and_selector("artifact://12:-3").unwrap();
    assert_eq!(path, "artifact://12");
    assert_eq!(
        selector,
        Selector::Tail {
            count: 3,
            ranges: Vec::new(),
            raw: false,
        }
    );
    let (path, selector) = split_uri_and_selector("artifact://12").unwrap();
    assert_eq!(path, "artifact://12");
    assert_eq!(selector, Selector::None);
}

#[test]
fn explicit_ranges_carry_a_leading_and_three_trailing_context_lines() {
    let text = lines(10);
    let rendered = render_with_selector(
        &text,
        &Selector::Lines {
            ranges: vec![(2, Some(3))],
            raw: false,
        },
        false,
    )
    .unwrap();

    let body = rendered.body;
    assert!(body.contains("    1\tL1\n"), "{body}");
    assert!(body.contains("    6\tL6\n"), "{body}");
    assert!(!body.contains("L7"), "{body}");
    assert_eq!(rendered.last_line, 6);
    assert_eq!(rendered.total_lines, 10);
    assert!(!rendered.more);
}

#[test]
fn a_tail_selector_is_exactly_the_last_lines() {
    let text = lines(10);
    let rendered = render_with_selector(
        &text,
        &Selector::Tail {
            count: 2,
            ranges: Vec::new(),
            raw: false,
        },
        false,
    )
    .unwrap();
    assert_eq!(rendered.body, "    9\tL9\n   10\tL10\n");
}

#[test]
fn raw_renders_without_numbers_or_a_length_cut() {
    let long = "x".repeat(MAX_LINE_LEN + 10);
    let text = format!("first\n{long}\nthird\n");
    let rendered = render_with_selector(
        &text,
        &Selector::Lines {
            ranges: vec![(2, Some(2))],
            raw: true,
        },
        false,
    )
    .unwrap();
    assert_eq!(rendered.body, format!("{long}\n"));
}

#[test]
fn a_range_past_the_end_reports_the_file_length() {
    let text = lines(10);
    let error = render_with_selector(
        &text,
        &Selector::Lines {
            ranges: vec![(40, None)],
            raw: false,
        },
        false,
    )
    .expect_err("out-of-range read should fail");
    assert_eq!(
        error.to_string(),
        "Line 40 is beyond end of file (10 lines total). Use :1 to read from the start, or :10 to read the last line."
    );
}

#[test]
fn a_multi_range_list_skips_the_out_of_range_part() {
    let text = lines(10);
    let rendered = render_with_selector(
        &text,
        &Selector::Lines {
            ranges: vec![(1, Some(1)), (40, Some(50))],
            raw: false,
        },
        false,
    )
    .unwrap();
    assert!(rendered.body.contains("    1\tL1"), "{}", rendered.body);
    assert!(
        rendered
            .body
            .contains("[Range 40-50 is beyond end of file (10 lines total); skipped]"),
        "{}",
        rendered.body
    );
}

#[test]
fn disjoint_spans_are_separated_by_an_ellipsis_line() {
    let text = lines(30);
    let rendered = render_with_selector(
        &text,
        &Selector::Lines {
            ranges: vec![(2, Some(2)), (20, Some(20))],
            raw: false,
        },
        false,
    )
    .unwrap();
    let body = rendered.body;
    assert!(body.contains("\n…\n"), "{body}");
    assert!(body.contains("    2\tL2"), "{body}");
    assert!(body.contains("   20\tL20"), "{body}");
}

#[test]
fn an_open_range_stops_at_the_default_limit_and_asks_for_the_next_page() {
    let text = lines(DEFAULT_LIMIT + 40);
    let rendered = render_with_selector(
        &text,
        &Selector::Lines {
            ranges: vec![(1, None)],
            raw: false,
        },
        false,
    )
    .unwrap();
    assert!(rendered.more, "an open-ended read of a long file must offer more");
    assert!(rendered.last_line >= DEFAULT_LIMIT);
}

#[test]
fn a_page_stops_at_the_byte_budget() {
    let line = "y".repeat(1000);
    let text = format!("{}\n", vec![line; 400].join("\n"));
    let rendered = render_range(&text, 0, 5000, false);
    assert!(rendered.last_line > 0, "some lines should render");
    assert!(rendered.last_line < 400, "the page must stop before the end");
    assert!(rendered.more, "a budget-limited page must offer a continuation");
}

#[test]
fn hashline_numbers_and_tags_come_with_a_selector() {
    let text = "alpha\nbeta\ngamma\n";
    let rendered = render_with_selector(
        text,
        &Selector::Lines {
            ranges: vec![(2, Some(2))],
            raw: false,
        },
        true,
    )
    .unwrap();
    let lines: Vec<&str> = rendered.body.lines().collect();
    assert_eq!(lines.len(), 3, "{}", rendered.body);
    assert!(lines[0].contains("#") && lines[0].ends_with(":alpha"), "{}", lines[0]);
    assert!(lines[1].contains(":beta"), "{}", lines[1]);
}
