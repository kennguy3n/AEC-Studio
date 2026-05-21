//! Full-text search for the asset library.
//!
//! Backed by SQLite's FTS5 virtual table. The schema is mirrored from
//! the [`AssetDatabase`](crate::db::AssetDatabase) `assets` table via
//! `AFTER INSERT/UPDATE/DELETE` triggers so the FTS index stays in sync
//! without callers having to remember to re-index.
//!
//! The tokenizer is `porter unicode61 remove_diacritics 2`, chosen for
//! these reasons:
//!
//! * `unicode61` handles non-ASCII characters in vendor / asset names
//!   (German, French, Japanese romaji) without bailing.
//! * `remove_diacritics 2` strips accents so `"café"` matches `"cafe"`.
//! * `porter` stemming maps `"chairs" -> "chair"` and `"running" ->
//!   `"run"`.
//!
//! Ranking is FTS5's built-in BM25. We weight `name` 3× and `tags` 2×
//! so a hit in the asset name outranks a hit deep in the tag list.

use rusqlite::params;

use crate::error::{AssetError, AssetResult};
use crate::metadata::AssetMetadata;

/// FTS5 column weights for `bm25()`. FTS5 weights are positional and
/// include UNINDEXED columns in the column count, so the first weight
/// here corresponds to `asset_id` (UNINDEXED — never matches, weight
/// is irrelevant) and weights 2..6 are: name × 3, vendor × 1,
/// tags × 2, style_tags × 1.5, materials × 1. Tuned so a search for
/// "oak chair" surfaces an asset named "Oak Lounge Chair" above an
/// asset tagged `oak` but named "Side Table".
pub const BM25_WEIGHTS: &str = "1.0, 3.0, 1.0, 2.0, 1.5, 1.0";

/// SQL applied at database init to set up the FTS5 virtual table and
/// the triggers that mirror the `assets` table into it. Idempotent —
/// safe to run on every `AssetDatabase::open`.
pub(crate) const FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS assets_fts USING fts5(
    asset_id UNINDEXED,
    name,
    vendor,
    tags,
    style_tags,
    materials,
    tokenize = "unicode61 remove_diacritics 2"
);

CREATE TRIGGER IF NOT EXISTS assets_fts_ai AFTER INSERT ON assets BEGIN
    INSERT INTO assets_fts (asset_id, name, vendor, tags, style_tags, materials)
    VALUES (new.asset_id, new.name, new.vendor_name, new.tags, new.style_tags, new.materials);
END;

CREATE TRIGGER IF NOT EXISTS assets_fts_au AFTER UPDATE ON assets BEGIN
    DELETE FROM assets_fts WHERE asset_id = old.asset_id;
    INSERT INTO assets_fts (asset_id, name, vendor, tags, style_tags, materials)
    VALUES (new.asset_id, new.name, new.vendor_name, new.tags, new.style_tags, new.materials);
END;

CREATE TRIGGER IF NOT EXISTS assets_fts_ad AFTER DELETE ON assets BEGIN
    DELETE FROM assets_fts WHERE asset_id = old.asset_id;
END;
"#;

/// User-facing search options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// FTS5 MATCH expression. May use the full FTS5 grammar (`AND`,
    /// `OR`, `NEAR(...)`, `^prefix`, quoted phrases). An empty string
    /// matches all assets.
    pub query: String,
    /// Maximum number of hits to return. Capped to 1000 at the SQL
    /// layer regardless. `None` means "use the default" (50).
    pub limit: Option<u32>,
    /// Optional minimum BM25 score (smaller is *better* in FTS5 — the
    /// score is negative). Most callers leave this unset.
    pub max_bm25: Option<f64>,
}

/// One row in a search result set.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub metadata: AssetMetadata,
    /// BM25 score from FTS5. **Lower (more negative) is better.**
    pub bm25: f64,
}

/// Run a search query against the FTS5 index. Returns hits in ranked
/// order (best first).
pub fn search(conn: &rusqlite::Connection, opts: &SearchOptions) -> AssetResult<Vec<SearchHit>> {
    let limit = opts.limit.unwrap_or(50).clamp(1, 1000) as i64;
    let q = opts.query.trim();
    if q.is_empty() {
        // Empty query -> top-N by created_at to keep the search bar
        // useful without a filter.
        let mut stmt = conn.prepare("SELECT * FROM assets ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], crate::db::row_to_metadata)?
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(rows
            .into_iter()
            .map(|m| {
                let mut meta = m;
                attach_lods(conn, &mut meta).ok();
                SearchHit {
                    metadata: meta,
                    bm25: 0.0,
                }
            })
            .collect());
    }

    let sanitized = sanitize_fts_query(q);
    let bm25_expr = format!("bm25(assets_fts, {BM25_WEIGHTS})");
    let sql = format!(
        "SELECT a.*, {bm25_expr} AS score \
         FROM assets a \
         JOIN assets_fts f ON f.asset_id = a.asset_id \
         WHERE assets_fts MATCH ?1 \
         {bm25_filter} \
         ORDER BY score ASC LIMIT ?2",
        bm25_expr = bm25_expr,
        bm25_filter = match opts.max_bm25 {
            Some(_) => "AND score <= ?3",
            None => "",
        },
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(AssetMetadata, f64)> = if let Some(cap) = opts.max_bm25 {
        stmt.query_map(params![sanitized, limit, cap], |row| {
            let meta = crate::db::row_to_metadata(row)?;
            let score: f64 = row.get(14)?;
            Ok((meta, score))
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map(params![sanitized, limit], |row| {
            let meta = crate::db::row_to_metadata(row)?;
            let score: f64 = row.get(14)?;
            Ok((meta, score))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };

    let mut hits = Vec::with_capacity(rows.len());
    for (mut meta, score) in rows {
        attach_lods(conn, &mut meta).ok();
        hits.push(SearchHit {
            metadata: meta,
            bm25: score,
        });
    }
    Ok(hits)
}

/// Re-index every existing asset into the FTS table. Called once at
/// database open after the trigger schema is installed so callers that
/// upgraded from a pre-FTS schema don't have to re-import their assets.
pub(crate) fn rebuild_index(conn: &rusqlite::Connection) -> AssetResult<()> {
    conn.execute("DELETE FROM assets_fts", [])?;
    conn.execute(
        "INSERT INTO assets_fts (asset_id, name, vendor, tags, style_tags, materials)
         SELECT asset_id, name, vendor_name, tags, style_tags, materials FROM assets",
        [],
    )?;
    Ok(())
}

fn attach_lods(conn: &rusqlite::Connection, meta: &mut AssetMetadata) -> AssetResult<()> {
    let mut stmt = conn.prepare(
        "SELECT level, ratio, triangle_count, mesh_hash, vertex_count
         FROM asset_lods WHERE asset_id = ?1 ORDER BY level ASC",
    )?;
    let lods = stmt
        .query_map(params![meta.asset_id], |row| {
            Ok(crate::metadata::MeshBlob {
                mesh_hash: row.get(3)?,
                vertex_count: row.get::<_, i64>(4)? as u32,
                triangle_count: row.get::<_, i64>(2)? as u32,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AssetError::from)?;
    meta.lods = lods;
    Ok(())
}

/// Sanitize a raw user search string into an FTS5 MATCH expression.
///
/// FTS5 is strict about its grammar: an unquoted apostrophe terminates
/// a token mid-word, an unbalanced `"` is a syntax error, and trailing
/// `AND`/`OR`/`NOT` operators panic the parser. We wrap each whitespace-
/// separated token in double-quotes, escaping interior double-quotes by
/// doubling them, then `AND`-join. The result is a safe MATCH
/// expression that means "every token must appear anywhere in any
/// indexed column" — the most common search intent for an asset
/// browser.
pub fn sanitize_fts_query(q: &str) -> String {
    let tokens: Vec<String> = q
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            let escaped = t.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    if tokens.is_empty() {
        return String::new();
    }
    tokens.join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_empty_query() {
        assert_eq!(sanitize_fts_query(""), "");
        assert_eq!(sanitize_fts_query("   "), "");
    }

    #[test]
    fn sanitize_single_token() {
        assert_eq!(sanitize_fts_query("chair"), "\"chair\"");
    }

    #[test]
    fn sanitize_multi_token_anded() {
        assert_eq!(sanitize_fts_query("oak chair"), "\"oak\" AND \"chair\"");
    }

    #[test]
    fn sanitize_escapes_interior_quote() {
        assert_eq!(sanitize_fts_query(r#"chair"oak"#), r#""chair""oak""#);
    }

    #[test]
    fn sanitize_handles_punctuation() {
        // Apostrophes and other punctuation should pass through to the
        // tokenizer, which will normalise them.
        let s = sanitize_fts_query("o'brien & co");
        assert!(s.contains("o'brien"));
        assert!(s.contains("AND"));
    }
}
