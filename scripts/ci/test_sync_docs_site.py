"""Tests for the docs-to-Mintlify mirror.

The cases pin the conversions a page can silently get wrong: text MDX would read
as code, a table cell split by a `|` inside a code span, link targets, and the
heading ids that links must use. They run against the real `docs/` tree, so a
new page that breaks one of these rules fails here before it fails the site build.
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path, PurePosixPath

sys.path.insert(0, str(Path(__file__).resolve().parent))

import sync_docs_site as sync

PAGES = set(sync.source_pages())
ORIGIN = PurePosixPath("docs/runtime/TOOLS.md")


def convert(line: str, origin: PurePosixPath = ORIGIN) -> str:
    return sync.convert_line(line, origin, PAGES, "en")


class EscapeTests(unittest.TestCase):
    def test_braces_and_tags_outside_code_are_escaped(self) -> None:
        self.assertEqual(convert("use {name} and <tag> here"), r"use \{name\} and \<tag> here")

    def test_tag_like_placeholder_in_korean_is_escaped(self) -> None:
        self.assertEqual(convert("경로는 <디렉터리> 아래다"), r"경로는 \<디렉터리> 아래다")

    def test_code_spans_are_left_alone(self) -> None:
        self.assertEqual(convert("run `a {b} <c>` now"), "run `a {b} <c>` now")

    def test_wiki_link_syntax_is_escaped(self) -> None:
        self.assertEqual(convert("### Rules ([[stream_rules]])"), r"### Rules (\[\[stream_rules]])")

    def test_table_cell_pipe_inside_code_is_escaped(self) -> None:
        self.assertEqual(convert('| `a "x"|"y"` | b |'), '| `a "x"\\|"y"` | b |')

    def test_details_and_summary_tags_are_left_for_mdx(self) -> None:
        for line in ("<details>", "</details>", "<summary>Exact numbers</summary>",
                     "<summary>정확한 수치</summary>"):
            with self.subTest(line=line):
                self.assertEqual(convert(line), line)
        # A tag that is not one of these still has to be escaped.
        self.assertEqual(convert("<detail>"), r"\<detail>")


class LinkTests(unittest.TestCase):
    def test_link_to_another_doc_becomes_a_site_route(self) -> None:
        self.assertEqual(convert("[m](../provider/MODEL.md)"), "[m](/reference/provider/model)")

    def test_korean_link_target_maps_to_the_english_route(self) -> None:
        self.assertEqual(convert("[m](../provider/MODEL.ko.md)"), "[m](/reference/provider/model)")

    def test_readme_in_a_folder_is_that_folders_index(self) -> None:
        self.assertEqual(convert("[d](../diagram/README.md)"), "[d](/reference/diagram/index)")

    def test_link_outside_docs_goes_to_github(self) -> None:
        self.assertEqual(
            convert("[r](../../README.md)"), f"[r]({sync.GITHUB_BLOB}README.md)"
        )

    def test_image_points_at_the_copied_svg(self) -> None:
        out = convert("![x](01-system-overview.svg)", PurePosixPath("docs/diagram/README.md"))
        self.assertEqual(out, "![x](/images/reference/diagram/01-system-overview.svg)")

    def test_external_link_and_autolink(self) -> None:
        self.assertEqual(convert("[e](https://example.com/a)"), "[e](https://example.com/a)")
        self.assertEqual(convert("<https://example.com/a>"), "[https://example.com/a](https://example.com/a)")


class AnchorTests(unittest.TestCase):
    def test_slash_in_a_heading_is_kept_in_the_site_id(self) -> None:
        mapping = sync.anchor_map(PurePosixPath("docs/performance/BENCHMARK.md"))
        self.assertEqual(mapping.get("ab-measurement"), "a/b-measurement")

    def test_same_page_fragment_uses_the_site_id(self) -> None:
        out = sync.rewrite_target("#ab-measurement", PurePosixPath("docs/performance/BENCHMARK.md"), PAGES, "en")
        self.assertEqual(out, "#a/b-measurement")

    def test_unmapped_fragment_is_unchanged(self) -> None:
        out = sync.rewrite_target("#overview", PurePosixPath("docs/performance/BENCHMARK.md"), PAGES, "en")
        self.assertEqual(out, "#overview")


class ExtraSourceTests(unittest.TestCase):
    def test_extra_pages_are_generated_with_the_philosophy_in_both_languages(self) -> None:
        self.assertIsNotNone(sync.repo_source(PurePosixPath("philosophy.md"), "ko"))
        self.assertIsNone(sync.repo_source(PurePosixPath("sdk/python.md"), "ko"))

    def test_korean_philosophy_links_to_the_philosophy_page(self) -> None:
        out = sync.rewrite_target("PHILOSOPHY.md", PurePosixPath("PHILOSOPHY.ko.md"), PAGES, "ko")
        self.assertEqual(out, "/ko/reference/philosophy")

    def test_link_to_a_page_with_no_korean_source_falls_back_to_english(self) -> None:
        out = sync.rewrite_target("../../packages/sdk/python/README.md", PurePosixPath("docs/runtime/TOOLS.md"), PAGES, "ko")
        self.assertEqual(out, "/reference/sdk/python")

    def test_package_readme_link_to_an_unmirrored_file_goes_to_github(self) -> None:
        out = sync.rewrite_target("RELEASING.md", PurePosixPath("packages/sdk/python/README.md"), PAGES, "en")
        self.assertEqual(out, f"{sync.GITHUB_BLOB}packages/sdk/python/RELEASING.md")


class PageTests(unittest.TestCase):
    def test_describe_cuts_a_long_paragraph_at_a_sentence(self) -> None:
        first = "The daemon keeps every session in one place and clients attach to it over a socket."
        body = ["", first + " " + "word " * 60 + "end."]
        self.assertEqual(sync.describe(body), first)

    def test_describe_skips_headings_and_tables(self) -> None:
        self.assertEqual(sync.describe(["## Head", "| a | b |", "", "Real text."]), "Real text.")

    def test_every_page_has_a_route_and_a_listed_navigation_entry(self) -> None:
        self.assertEqual(sync.navigation_problems(sync.expected_files()), [])

    def test_routes_are_unique(self) -> None:
        routes = [sync.route(page) for page in PAGES]
        self.assertEqual(len(routes), len(set(routes)))


if __name__ == "__main__":
    unittest.main()
