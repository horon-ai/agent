"""Tests for record_charts.py, the drawer of the hand-written record charts.

These charts are committed files that CI never redraws, so the contracts pinned
here are the ones whose failure would let a picture drift away from the record:

* every committed SVG is exactly what the script produces, so a table edit that
  is not redrawn fails before it ships;
* every number a chart shows is still present in both RESULTS documents, so a
  chart cannot quote a figure the record no longer states;
* each chart is linked from both documents, and the English chart carries no
  Korean text (the pair gate would otherwise pass while the page mixed scripts).
"""

from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

import record_charts  # noqa: E402

ROOT = record_charts.ROOT
DOCS = (
    ROOT / "docs" / "performance" / "RESULTS.md",
    ROOT / "docs" / "performance" / "RESULTS.ko.md",
)
HANGUL = re.compile(r"[\u3131-\u318e\uac00-\ud7a3]")


class Charts(unittest.TestCase):
    def test_committed_charts_are_what_the_script_draws(self) -> None:
        stale = [path.relative_to(ROOT).as_posix() for path in record_charts.stale()]
        self.assertEqual(stale, [], "run python3 scripts/perf/record_charts.py")

    def test_every_number_a_chart_shows_is_in_both_records(self) -> None:
        texts = {path.name: path.read_text(encoding="utf-8") for path in DOCS}
        for chart in record_charts.CHARTS_DATA:
            for token in chart.tokens:
                with self.subTest(chart=chart.stem, token=token):
                    for name, text in texts.items():
                        self.assertIn(token, text, f"{chart.stem}: {token} is not in {name}")

    def test_english_charts_carry_no_korean_and_korean_charts_do(self) -> None:
        for chart in record_charts.CHARTS_DATA:
            with self.subTest(chart=chart.stem):
                self.assertIsNone(HANGUL.search(record_charts.render_chart(chart, "en")))
                self.assertIsNotNone(HANGUL.search(record_charts.render_chart(chart, "ko")))

    def test_both_documents_show_every_chart(self) -> None:
        for path, suffix in ((DOCS[0], ".svg"), (DOCS[1], ".ko.svg")):
            text = path.read_text(encoding="utf-8")
            for chart in record_charts.CHARTS_DATA:
                with self.subTest(document=path.name, chart=chart.stem):
                    self.assertIn(f"record-{chart.stem}{suffix}", text)

    def test_chart_names_do_not_collide_with_the_automatic_renderer(self) -> None:
        # bench_docs.py rewrites everything it draws; a shared name would be
        # overwritten on the next measurement instead of kept.
        for chart in record_charts.CHARTS_DATA:
            for lang in ("en", "ko"):
                name = record_charts.chart_name(chart.stem, lang)
                self.assertTrue(name.startswith("record-"), name)


if __name__ == "__main__":
    unittest.main()
