# horon-agent-token-estimate

Content-aware token estimation for the agent harness. Replaces the flat
`bytes / 4` heuristic that assumed every four bytes cost one token.

## Why the old heuristic was wrong in both directions

Measured against this repository's own files with `tiktoken 0.14.0`:

| Content | Files | Old error (o200k) | What the error did |
| --- | --- | --- | --- |
| Rust source | 15 | +13.4% | compacted too early, discarded usable context |
| Korean documents | 17 | -7.0% | compacted late |
| JSON | 5 | -32.0% | compacted too late, the provider answered 400 |
| TOML | 36 | -9.9% | compacted late |

A single constant cannot serve scripts that differ by a factor of two in
characters per token, so the estimator prices each script class separately.

## Measured residuals after this estimator

The table below is the residual against the real tokenizer, not against the old
heuristic. Negative means the estimate is lower than the tokenizer's count.

| Content | Residual | Reading |
| --- | --- | --- |
| Rust source | +2.8% | close enough to ignore |
| Korean documents | -11.8% | undercounts, so compaction fires slightly late |
| TOML | +5.2% | close enough to ignore |
| JSON | -23.8% | the worst remaining case; see the follow-up below |

For comparison, the old `bytes/4` heuristic was -32.0% on JSON, so this crate
cuts that error by roughly a quarter but does not remove it.

## The JSON residual is a named follow-up, not a solved problem

JSON-shaped content is still undercounted by about 24% in aggregate. The cause
is structural: JSON packs many short alphanumeric runs behind dense punctuation,
and a character-class rate cannot see that a run like `"schema"` merges into one
token while `{"` costs its own.

This crate does not attempt a fix. Recording the residual and leaving the gap
open is deliberate - a signal invented without measurement would trade a known
23.8% error for an unknown one. The follow-up task is to measure a
structure-aware signal against the same five JSON files before changing anything.

`json_aggregate_residual_is_documented` in `tests/measured_baseline.rs` pins a
floor of 76% of the measured count for the JSON sample, so a change that makes
this worse fails the suite rather than passing silently.

## Two constants, not one

`CHARS_PER_TOKEN_UPPER_BOUND` (value 4) exists only for the token-to-character
inverse, where the caller needs a character budget and must not exceed it. The
forward direction uses `estimate_tokens`, which prices each script. Using the
estimator for the inverse would be wrong in the other direction: dividing a
token budget by a content-dependent rate can produce a character allowance
larger than the budget intends.

`TOKEN_ESTIMATE_VERSION` names the model so a stored estimate can be attributed
to the rates that produced it.

## The CJK merge discount

A flat per-character rate cannot serve both prose and a long unbroken run. In
Chinese prose a character costs about 0.58 tokens because the vocabulary merges
common bigrams; the same character repeated a thousand times costs about a full
token each, because there is no bigram to merge.

`cjk_merge(len, floor)` resolves that: a short run is charged the prose rate, and
the discount fades as the run grows, so a long run approaches its full price.
`CJK_MERGE_WINDOW` is 18.0 and the per-class floors are 0.53 for Han and 0.70 for
kana, each derived from the measured rate for that script.

## Known limits

- A repeated single CJK character is underestimated by about 7-8%: the model
  charges the run rate while the tokenizer charges one token per character.
- JSON aggregate is -23.8%, as above.
- The rates were fitted on prose samples; a document of unusual composition
  (mostly symbols, or mostly identifiers) will drift.
