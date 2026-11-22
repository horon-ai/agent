//! Search: candidate streams, then reciprocal-rank fusion.
//!
//! Four text streams find candidates — the token index, the trigram substring
//! index, the CJK bigram expansion, and entity names — and one link stream
//! expands the strongest text hits to their one-hop neighbours. Recency and
//! kind authority then rank the candidate set only: they reorder what text or
//! links found, so a preference outranks an event of the same relevance, but an
//! unrelated page never appears just because it is recent.
//!
//! Fusion is reciprocal-rank fusion with the usual constant of 60. Each hit
//! carries the rank it received in every stream, so a surprising order can be
//! explained instead of guessed at. The final order has explicit tie-breaks
//! (score, then last write, then id), so the same index always answers the same
//! query the same way.

use super::index::{PageRow, page_row_from};
use super::schema::Index;
use super::text::{bigram_expression, entity_query, token_expression, trigram_expression};
use anyhow::{Context, Result};
use horon_agent_memory_types::page::{PageKind, is_iso_date};
use rusqlite::{ToSql, params_from_iter};
use std::collections::HashMap;

/// Reciprocal-rank constant. 60 is the value from the RRF paper and keeps one
/// stream's first place from dominating the fusion.
const RRF_K: f32 = 60.0;
/// Candidates taken from each text stream.
const STREAM_LIMIT: usize = 50;
/// Text hits whose neighbours join the link stream.
const LINK_SEEDS: usize = 3;
/// Per-stream weights in the fusion. Text streams carry relevance; a link
/// neighbour is weaker evidence; recency and kind authority reorder pages that
/// are otherwise close, which is what lets a preference edge out an event of
/// the same relevance without letting it beat a much better text match.
const TEXT_WEIGHT: f32 = 1.0;
const LINK_WEIGHT: f32 = 0.8;
const RECENCY_WEIGHT: f32 = 1.0;
const AUTHORITY_WEIGHT: f32 = 1.5;
/// Default result limit when a caller asks for none.
pub const DEFAULT_LIMIT: usize = 10;

/// What to search for and how to narrow it.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Free text. Empty text returns no hits.
    pub text: String,
    /// Most hits to return.
    pub limit: Option<usize>,
    /// Restrict to these kinds; empty means every kind.
    pub kinds: Vec<PageKind>,
    /// Restrict to one project key.
    pub project: Option<String>,
    /// Restrict to pages carrying every one of these tags.
    pub tags: Vec<String>,
    /// Only pages valid at this `YYYY-MM-DD` date.
    pub at: Option<String>,
}

/// Per-stream ranks of one hit, each 1-based. `None` means the stream did not
/// see the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamRanks {
    /// Rank in the token (unicode61) stream.
    pub tokens: Option<usize>,
    /// Rank in the trigram substring stream.
    pub trigram: Option<usize>,
    /// Rank in the CJK bigram stream.
    pub bigram: Option<usize>,
    /// Rank in the entity stream.
    pub entity: Option<usize>,
    /// Rank in the link-neighbour stream.
    pub link: Option<usize>,
    /// Rank among candidates by recency.
    pub recency: Option<usize>,
    /// Rank among candidates by kind authority.
    pub authority: Option<usize>,
}

impl StreamRanks {
    /// The best text-stream rank, if any text stream saw the page.
    pub fn best_text_rank(&self) -> Option<usize> {
        [self.tokens, self.trigram, self.bigram, self.entity]
            .into_iter()
            .flatten()
            .min()
    }

    /// The streams and their weights, in the order the score sums them.
    fn weighted_ranks(&self) -> [(Option<usize>, f32); 7] {
        [
            (self.tokens, TEXT_WEIGHT),
            (self.trigram, TEXT_WEIGHT),
            (self.bigram, TEXT_WEIGHT),
            (self.entity, TEXT_WEIGHT),
            (self.link, LINK_WEIGHT),
            (self.recency, RECENCY_WEIGHT),
            (self.authority, AUTHORITY_WEIGHT),
        ]
    }
}

/// One search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// The page.
    pub page: PageRow,
    /// Fused score, higher first.
    pub score: f32,
    /// Per-stream ranks that produced the score.
    pub ranks: StreamRanks,
}

/// The answer to one search.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    /// Hits, best first.
    pub hits: Vec<SearchHit>,
    /// How many distinct pages entered the fusion.
    pub candidates: usize,
}

/// Run `query` against `index`.
pub fn search(index: &Index, query: &SearchQuery) -> Result<SearchResults> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if query.text.trim().is_empty() || limit == 0 {
        return Ok(SearchResults::default());
    }
    if let Some(at) = &query.at
        && !is_iso_date(at)
    {
        anyhow::bail!("search date must be YYYY-MM-DD, not `{at}`");
    }
    let mut ranks: HashMap<String, StreamRanks> = HashMap::new();

    if let Some(expression) = token_expression(&query.text) {
        let hits = fts_stream(index, "pages_fts", &expression, query)?;
        record(&mut ranks, hits, |entry, rank| entry.tokens = Some(rank));
    }
    if let Some(expression) = trigram_expression(&query.text) {
        let hits = fts_stream(index, "pages_bigram", &expression, query)?;
        record(&mut ranks, hits, |entry, rank| entry.trigram = Some(rank));
    }
    if let Some(expression) = bigram_expression(&query.text) {
        let hits = fts_stream(index, "pages_grams", &expression, query)?;
        record(&mut ranks, hits, |entry, rank| entry.bigram = Some(rank));
    }
    let entity_hits = entity_stream(index, &entity_query(&query.text), query)?;
    record(&mut ranks, entity_hits, |entry, rank| entry.entity = Some(rank));

    let text_candidates: Vec<(usize, String)> = ranks
        .iter()
        .filter_map(|(id, entry)| entry.best_text_rank().map(|rank| (rank, id.clone())))
        .collect();
    let link_hits = link_stream(index, text_candidates)?;
    record(&mut ranks, link_hits, |entry, rank| entry.link = Some(rank));

    if ranks.is_empty() {
        return Ok(SearchResults::default());
    }

    let mut pages = pages_by_id(index, ranks.keys())?;
    let tag_ok = allowed_by_tag(index, query)?;
    pages.retain(|id, row| row_matches(query, row) && tag_ok.as_ref().is_none_or(|ids| ids.contains(id)));
    // Recency stream: newest first among the candidates.
    let mut by_recency: Vec<&String> = pages.keys().collect();
    by_recency.sort_by(|left, right| {
        let left_row = &pages[*left];
        let right_row = &pages[*right];
        right_row
            .updated_ms
            .cmp(&left_row.updated_ms)
            .then_with(|| right_row.created_ms.cmp(&left_row.created_ms))
            .then_with(|| left_row.id.cmp(&right_row.id))
    });
    for (index, id) in by_recency.iter().enumerate() {
        if let Some(entry) = ranks.get_mut(*id) {
            entry.recency = Some(index + 1);
        }
    }
    // Authority stream: kind authority first, then recency, then id.
    let mut by_authority: Vec<&String> = pages.keys().collect();
    by_authority.sort_by(|left, right| {
        let left_row = &pages[*left];
        let right_row = &pages[*right];
        let left_authority = PageKind::parse(&left_row.kind).map(PageKind::authority).unwrap_or(0);
        let right_authority = PageKind::parse(&right_row.kind).map(PageKind::authority).unwrap_or(0);
        right_authority
            .cmp(&left_authority)
            .then_with(|| right_row.updated_ms.cmp(&left_row.updated_ms))
            .then_with(|| left_row.id.cmp(&right_row.id))
    });
    for (index, id) in by_authority.iter().enumerate() {
        if let Some(entry) = ranks.get_mut(*id) {
            entry.authority = Some(index + 1);
        }
    }

    let candidate_count = pages.len();
    let mut hits: Vec<SearchHit> = pages
        .into_iter()
        .filter_map(|(id, page)| {
            let entry = ranks.get(&id)?;
            let mut score = 0.0f32;
            for (rank, weight) in entry.weighted_ranks() {
                if let Some(rank) = rank {
                    score += weight / (RRF_K + rank as f32);
                }
            }
            Some(SearchHit {
                page,
                score,
                ranks: *entry,
            })
        })
        .collect();
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.page.updated_ms.cmp(&left.page.updated_ms))
            .then_with(|| left.page.id.cmp(&right.page.id))
    });
    hits.truncate(limit);
    Ok(SearchResults {
        hits,
        candidates: candidate_count,
    })
}

/// `(rank, page id)` pairs from a stream.
type StreamHits = Vec<(usize, String)>;

fn record(ranks: &mut HashMap<String, StreamRanks>, hits: StreamHits, set: impl Fn(&mut StreamRanks, usize)) {
    for (rank, id) in hits {
        let entry = ranks.entry(id).or_default();
        set(entry, rank);
    }
}

fn fts_stream(index: &Index, table: &str, expression: &str, query: &SearchQuery) -> Result<StreamHits> {
    let mut filters = Filters::new();
    let match_placeholder = filters.bind(expression.to_string());
    filters.apply(query)?;
    let sql = format!(
        "SELECT p.id, rank FROM {table} f JOIN pages p ON p.rowid = f.rowid
         WHERE {table} MATCH {match_placeholder}{}
         ORDER BY rank, p.id ASC LIMIT {STREAM_LIMIT}",
        filters.suffix()
    );
    let mut statement = index
        .conn()
        .prepare(&sql)
        .with_context(|| format!("cannot prepare the {table} search"))?;
    let rows = statement.query_map(params_from_iter(filters.params()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;
    let mut scored = Vec::new();
    for row in rows {
        scored.push(row?);
    }
    Ok(dense_ranks(scored))
}

fn entity_stream(index: &Index, compact_query: &str, query: &SearchQuery) -> Result<StreamHits> {
    if compact_query.is_empty() {
        return Ok(Vec::new());
    }
    let mut filters = Filters::new();
    let needle = filters.bind(compact_query.to_string());
    filters.apply(query)?;
    let sql = format!(
        "SELECT p.id, MAX(length(e.name_norm)) AS score FROM entities e JOIN pages p ON p.id = e.page_id
         WHERE e.name_norm <> '' AND instr({needle}, e.name_norm) > 0{}
         GROUP BY p.id
         ORDER BY score DESC, p.id ASC LIMIT {STREAM_LIMIT}",
        filters.suffix()
    );
    let mut statement = index.conn().prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(filters.params()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;
    let mut scored = Vec::new();
    for row in rows {
        scored.push(row?);
    }
    Ok(dense_ranks(scored))
}

fn link_stream(index: &Index, mut text_candidates: Vec<(usize, String)>) -> Result<StreamHits> {
    if text_candidates.is_empty() {
        return Ok(Vec::new());
    }
    text_candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    text_candidates.truncate(LINK_SEEDS);
    // (seed rank, weight, neighbour) then keep the strongest rank per neighbour.
    let mut neighbours: Vec<(usize, f32, String)> = Vec::new();
    for (seed_rank, seed) in &text_candidates {
        for link in index.links_from(seed)? {
            neighbours.push((*seed_rank, link.weight, link.dst_id));
        }
        for link in index.links_to(seed)? {
            neighbours.push((*seed_rank, link.weight, link.src_id));
        }
    }
    neighbours.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    let mut seen: Vec<String> = Vec::new();
    let mut hits = Vec::new();
    for (_, _, id) in neighbours {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        hits.push((hits.len() + 1, id));
    }
    Ok(hits)
}

/// Rank rows by descending score, giving equal scores the same rank. Equal
/// scores mean equal evidence, so the fusion's later streams (recency,
/// authority) decide between them instead of the alphabetical order of ids.
fn dense_ranks(scored: Vec<(String, f64)>) -> StreamHits {
    let mut hits = Vec::with_capacity(scored.len());
    let mut rank = 0usize;
    let mut previous: Option<f64> = None;
    for (id, score) in scored {
        if previous.map(|value| value.to_bits() != score.to_bits()).unwrap_or(true) {
            rank += 1;
            previous = Some(score);
        }
        hits.push((rank, id));
    }
    hits
}

fn pages_by_id<'a>(index: &Index, ids: impl Iterator<Item = &'a String>) -> Result<HashMap<String, PageRow>> {
    let mut sorted: Vec<&String> = ids.collect();
    sorted.sort();
    if sorted.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = (1..=sorted.len())
        .map(|position| format!("?{position}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, path, kind, category, project, title, updated_ms, created_ms, active,
                valid_from, valid_to, source FROM pages WHERE id IN ({placeholders})"
    );
    let mut statement = index.conn().prepare(&sql)?;
    let rows = statement.query_map(params_from_iter(sorted.iter()), page_row_from)?;
    let mut out = HashMap::new();
    for row in rows {
        let row = row?;
        out.insert(row.id.clone(), row);
    }
    Ok(out)
}

/// Page ids carrying every requested tag, or `None` when no tag filter is set.
fn allowed_by_tag(index: &Index, query: &SearchQuery) -> Result<Option<std::collections::HashSet<String>>> {
    if query.tags.is_empty() {
        return Ok(None);
    }
    let mut by_page: HashMap<String, Vec<String>> = HashMap::new();
    for (page_id, tag) in index.all_tags()? {
        by_page.entry(page_id).or_default().push(tag);
    }
    let mut ids = std::collections::HashSet::new();
    for (page_id, tags) in by_page {
        if query.tags.iter().all(|wanted| tags.contains(wanted)) {
            ids.insert(page_id);
        }
    }
    Ok(Some(ids))
}

/// The in-memory half of the filters, so a link neighbour cannot slip past a
/// kind, project or validity restriction that the SQL streams applied.
fn row_matches(query: &SearchQuery, row: &PageRow) -> bool {
    if !query.kinds.is_empty() {
        match PageKind::parse(&row.kind) {
            Some(kind) if query.kinds.contains(&kind) => {}
            _ => return false,
        }
    }
    if let Some(project) = &query.project
        && row.project.as_deref() != Some(project.as_str())
    {
        return false;
    }
    if let Some(at) = &query.at {
        if let Some(from) = &row.valid_from
            && at < from
        {
            return false;
        }
        if let Some(to) = &row.valid_to
            && at > to
        {
            return false;
        }
    }
    true
}

/// Bound `WHERE` clauses with their parameters, numbered as they are added.
struct Filters<'a> {
    clauses: Vec<String>,
    params: Vec<Box<dyn ToSql + 'a>>,
}

impl<'a> Filters<'a> {
    fn new() -> Self {
        Self {
            clauses: Vec::new(),
            params: Vec::new(),
        }
    }

    fn bind(&mut self, value: String) -> String {
        self.params.push(Box::new(value));
        format!("?{}", self.params.len())
    }

    fn params(&self) -> impl Iterator<Item = &dyn ToSql> {
        self.params.iter().map(|param| param.as_ref())
    }

    fn suffix(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!(" AND {}", self.clauses.join(" AND "))
        }
    }

    fn apply(&mut self, query: &SearchQuery) -> Result<()> {
        if !query.kinds.is_empty() {
            let kinds = query
                .kinds
                .iter()
                .map(|kind| format!("'{}'", kind.as_str()))
                .collect::<Vec<_>>()
                .join(", ");
            self.clauses.push(format!("p.kind IN ({kinds})"));
        }
        if let Some(project) = &query.project {
            let placeholder = self.bind(project.clone());
            self.clauses.push(format!("p.project = {placeholder}"));
        }
        for tag in &query.tags {
            let placeholder = self.bind(tag.clone());
            self.clauses
                .push(format!("p.id IN (SELECT page_id FROM tags WHERE tag = {placeholder})"));
        }
        if let Some(at) = &query.at {
            let at_after = self.bind(at.clone());
            let at_before = self.bind(at.clone());
            self.clauses
                .push(format!("(p.valid_from IS NULL OR p.valid_from <= {at_after})"));
            self.clauses
                .push(format!("(p.valid_to IS NULL OR p.valid_to >= {at_before})"));
        }
        Ok(())
    }
}
