//! SQLite-backed asset database. Stores content-addressed mesh blobs +
//! per-asset metadata + LOD pointers. The DB does NOT use SQLCipher (assets
//! are shareable across projects); per-project assets live in the project
//! package's `assets/` directory.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::error::{AssetError, AssetResult};
use crate::lod::{LodChain, LodLevel};
use crate::metadata::AssetMetadata;
use crate::query::AssetQuery;

pub struct AssetDatabase {
    conn: Connection,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS assets (
    asset_id      TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    vendor_id     TEXT NOT NULL,
    vendor_name   TEXT NOT NULL,
    vendor_url    TEXT,
    version       TEXT NOT NULL,
    license       TEXT NOT NULL,
    attribution   TEXT,
    tags          TEXT NOT NULL,
    style_tags    TEXT NOT NULL,
    materials     TEXT NOT NULL,
    thumbnail_kind TEXT NOT NULL,
    thumbnail_hash TEXT NOT NULL,
    created_at    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS asset_lods (
    asset_id        TEXT NOT NULL,
    level           INTEGER NOT NULL,
    ratio           REAL NOT NULL,
    triangle_count  INTEGER NOT NULL,
    mesh_hash       TEXT NOT NULL,
    vertex_count    INTEGER NOT NULL,
    PRIMARY KEY (asset_id, level),
    FOREIGN KEY (asset_id) REFERENCES assets(asset_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS asset_blobs (
    blob_hash    TEXT PRIMARY KEY,
    size         INTEGER NOT NULL,
    payload      BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_assets_tags ON assets(tags);
CREATE INDEX IF NOT EXISTS idx_assets_style_tags ON assets(style_tags);
CREATE INDEX IF NOT EXISTS idx_assets_vendor ON assets(vendor_id);
"#;

impl AssetDatabase {
    pub fn open(path: impl AsRef<Path>) -> AssetResult<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> AssetResult<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_schema(&conn)?;
        Ok(Self { conn })
    }

    /// Initialise the base schema, the FTS5 mirror, and rebuild the FTS
    /// index only if it's out of sync. Idempotent and O(1) on the common
    /// path where triggers already keep the index current.
    fn init_schema(conn: &Connection) -> AssetResult<()> {
        conn.execute_batch(SCHEMA)?;
        // FTS5 is part of the bundled SQLite shipped via
        // `rusqlite/bundled-sqlcipher-vendored-openssl`. The triggers
        // keep the FTS table in sync for every future write; the
        // `rebuild_index_if_needed` call covers pre-existing rows from
        // a database that was upgraded from a pre-FTS schema without
        // paying O(n) on every open.
        conn.execute_batch(crate::search::FTS_SCHEMA)?;
        crate::search::rebuild_index_if_needed(conn)?;
        Ok(())
    }

    /// Run an FTS5 full-text search against the asset library. See
    /// [`crate::search`] for query syntax.
    pub fn search(
        &self,
        opts: &crate::search::SearchOptions,
    ) -> AssetResult<Vec<crate::search::SearchHit>> {
        crate::search::search(&self.conn, opts)
    }

    /// Insert a content-addressed mesh blob. Idempotent (BLAKE3 keys dedupe).
    pub fn put_blob(&mut self, blob_hash: &str, payload: &[u8]) -> AssetResult<bool> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO asset_blobs (blob_hash, size, payload) VALUES (?1, ?2, ?3)",
            params![blob_hash, payload.len() as i64, payload],
        )?;
        Ok(inserted > 0)
    }

    pub fn blob_exists(&self, blob_hash: &str) -> AssetResult<bool> {
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM asset_blobs WHERE blob_hash = ?1",
                params![blob_hash],
                |row| row.get(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    pub fn get_blob(&self, blob_hash: &str) -> AssetResult<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT payload FROM asset_blobs WHERE blob_hash = ?1",
                params![blob_hash],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn upsert_metadata(&mut self, m: &AssetMetadata, chain: &LodChain) -> AssetResult<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO assets (
                asset_id, name, vendor_id, vendor_name, vendor_url, version, license,
                attribution, tags, style_tags, materials, thumbnail_kind, thumbnail_hash, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                m.asset_id,
                m.name,
                m.vendor.id,
                m.vendor.name,
                m.vendor.url,
                m.version,
                serde_json::to_string(&m.license)?,
                m.attribution,
                serde_json::to_string(&m.tags)?,
                serde_json::to_string(&m.style_tags)?,
                serde_json::to_string(&m.materials)?,
                serde_json::to_string(&m.thumbnail_kind)?,
                m.thumbnail_hash,
                m.created_at.to_rfc3339(),
            ],
        )?;
        tx.execute(
            "DELETE FROM asset_lods WHERE asset_id = ?1",
            params![m.asset_id],
        )?;
        for (i, level) in chain.levels.iter().enumerate() {
            let blob = m.lods.get(i).ok_or_else(|| {
                AssetError::InvalidManifest(format!(
                    "LOD level {i} declared in chain but missing from `lods`"
                ))
            })?;
            tx.execute(
                "INSERT INTO asset_lods (asset_id, level, ratio, triangle_count, mesh_hash, vertex_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    m.asset_id,
                    level.level as i64,
                    level.ratio,
                    level.triangle_count as i64,
                    blob.mesh_hash,
                    blob.vertex_count as i64,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, asset_id: &str) -> AssetResult<Option<AssetMetadata>> {
        let row = self
            .conn
            .query_row(
                "SELECT * FROM assets WHERE asset_id = ?1",
                params![asset_id],
                row_to_metadata,
            )
            .optional()?;
        let Some(mut meta) = row else { return Ok(None) };
        let mut stmt = self.conn.prepare(
            "SELECT level, ratio, triangle_count, mesh_hash, vertex_count
             FROM asset_lods WHERE asset_id = ?1 ORDER BY level ASC",
        )?;
        let lods = stmt
            .query_map(params![asset_id], |row| {
                Ok((
                    LodLevel {
                        level: row.get::<_, i64>(0)? as u8,
                        ratio: row.get::<_, f64>(1)? as f32,
                        triangle_count: row.get::<_, i64>(2)? as u32,
                    },
                    crate::metadata::MeshBlob {
                        mesh_hash: row.get(3)?,
                        vertex_count: row.get::<_, i64>(4)? as u32,
                        triangle_count: row.get::<_, i64>(2)? as u32,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        meta.lods = lods.iter().map(|(_, blob)| blob.clone()).collect();
        Ok(Some(meta))
    }

    pub fn delete(&mut self, asset_id: &str) -> AssetResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM assets WHERE asset_id = ?1", params![asset_id])?;
        Ok(n > 0)
    }

    /// Run a query and return matching metadata. All predicates —
    /// `name_contains`, `vendor_id`, `tags`, and `style_tags` — are
    /// expressed in SQL so the `LIMIT` clause composes correctly: a
    /// tag-filtered query on a 10k-row library returns the top-N
    /// **matches**, not the top-N rows pre-filter.
    ///
    /// Tag / style-tag predicates use SQLite's `json_each` table-valued
    /// function (always available in the bundled-sqlcipher-vendored-
    /// openssl build we link against) to walk the JSON-encoded
    /// `tags` / `style_tags` columns. Each requested tag becomes one
    /// `EXISTS (SELECT 1 FROM json_each(assets.<col>) WHERE value = ?)`
    /// subclause so the AND semantics match the renderer's
    /// `filterAssets` in-process fallback. The doc on
    /// [`AssetQuery::tags`] / [`AssetQuery::style_tags`] pins this
    /// AND-vs-OR contract.
    pub fn query(&self, q: &AssetQuery) -> AssetResult<Vec<AssetMetadata>> {
        // Build a heterogeneously-typed parameter list. The earlier
        // implementation stuffed every binding (including LIMIT) into a
        // `Vec<String>`, which leaned on SQLite's loose type affinity to
        // coerce `"200"` back into an integer for LIMIT. That worked but
        // it is structurally wrong — LIMIT is an integer column in the
        // SQL grammar and should be bound as one. We now store each
        // binding as a boxed `ToSql` value so LIKE/vendor stay strings
        // while LIMIT is sent as a real `i64`.
        let mut sql = String::from("SELECT * FROM assets WHERE 1=1");
        let mut bindings: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(name) = &q.name_contains {
            // Escape SQLite LIKE wildcards (`%`, `_`) and the escape
            // character itself in user input before wrapping in `%...%`.
            // Without this, `_` would silently match any single character
            // and `%` would match any substring — both surprise the user
            // when they type a literal underscore or percent sign.
            //
            // We use `\` as the escape character via an `ESCAPE` clause so
            // a query like `name_contains = "50%"` matches the literal
            // string "50%", not "fifty-anything".
            sql.push_str(" AND name LIKE ? ESCAPE '\\'");
            bindings.push(Box::new(format!("%{}%", escape_like(name))));
        }
        if let Some(vendor) = &q.vendor_id {
            sql.push_str(" AND vendor_id = ?");
            bindings.push(Box::new(vendor.clone()));
        }
        // Push tag / style-tag filtering into SQL so `LIMIT` applies
        // after the filter, not before. The pre-fix path bound `LIMIT`
        // to the user-supplied page size, fetched up to N rows ordered
        // by `created_at DESC`, then `.retain()`-filtered the in-memory
        // result — which silently dropped matches outside the top-N
        // by creation date. Matches the in-process `filterAssets`
        // fallback in `apps/desktop/electron/bridge.ts` exactly:
        // "filter first, slice last".
        for tag in &q.tags {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM json_each(assets.tags) WHERE json_each.value = ?)",
            );
            bindings.push(Box::new(tag.clone()));
        }
        for style_tag in &q.style_tags {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM json_each(assets.style_tags) WHERE json_each.value = ?)",
            );
            bindings.push(Box::new(style_tag.clone()));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");
        // Bind LIMIT as a typed i64 — not as a String — so SQLite gets an
        // INTEGER value, not text it has to coerce. `u32 -> i64` is
        // lossless and avoids the platform-dependent `usize` cast.
        let limit: i64 = i64::from(q.limit.unwrap_or(200));
        bindings.push(Box::new(limit));

        let mut stmt = self.conn.prepare(&sql)?;
        let params_iter: Vec<&dyn rusqlite::ToSql> = bindings.iter().map(AsRef::as_ref).collect();
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_iter.iter().copied()),
                row_to_metadata,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Escape SQLite LIKE wildcards in user input.
///
/// SQLite's LIKE treats `%` as "zero or more chars" and `_` as "any single
/// char". When user input is wrapped in `%...%` for a "contains" search we
/// must escape both, *and* the escape character itself, so the pattern
/// matches the literal input. The caller is responsible for declaring
/// `ESCAPE '\'` in the SQL.
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

pub(crate) fn row_to_metadata(row: &Row<'_>) -> rusqlite::Result<AssetMetadata> {
    use rusqlite::types::Type;
    use rusqlite::Error::FromSqlConversionFailure;
    let parse_json = |idx: usize| -> rusqlite::Result<serde_json::Value> {
        let s: String = row.get(idx)?;
        serde_json::from_str(&s).map_err(|e| FromSqlConversionFailure(idx, Type::Text, Box::new(e)))
    };
    let tags: Vec<String> = serde_json::from_value(parse_json(8)?)
        .map_err(|e| FromSqlConversionFailure(8, Type::Text, Box::new(e)))?;
    let style_tags: Vec<String> = serde_json::from_value(parse_json(9)?)
        .map_err(|e| FromSqlConversionFailure(9, Type::Text, Box::new(e)))?;
    let materials: Vec<String> = serde_json::from_value(parse_json(10)?)
        .map_err(|e| FromSqlConversionFailure(10, Type::Text, Box::new(e)))?;
    let license: crate::metadata::License = serde_json::from_value(parse_json(6)?)
        .map_err(|e| FromSqlConversionFailure(6, Type::Text, Box::new(e)))?;
    let thumbnail_kind: crate::metadata::ThumbnailKind = serde_json::from_value(parse_json(11)?)
        .map_err(|e| FromSqlConversionFailure(11, Type::Text, Box::new(e)))?;
    let created_at_s: String = row.get(13)?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_s)
        .map_err(|e| FromSqlConversionFailure(13, Type::Text, Box::new(e)))?
        .with_timezone(&chrono::Utc);
    Ok(AssetMetadata {
        asset_id: row.get(0)?,
        name: row.get(1)?,
        vendor: crate::metadata::Vendor {
            id: row.get(2)?,
            name: row.get(3)?,
            url: row.get(4)?,
        },
        version: row.get(5)?,
        license,
        attribution: row.get(7)?,
        tags,
        style_tags,
        lods: Vec::new(),
        materials,
        thumbnail_kind,
        thumbnail_hash: row.get(12)?,
        created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{License, MeshBlob, ThumbnailKind, Vendor};

    fn sample(id: &str) -> AssetMetadata {
        let mut m = AssetMetadata::new(
            id,
            format!("Asset {id}"),
            Vendor {
                id: "vendor_a".into(),
                name: "Vendor A".into(),
                url: None,
            },
            "1.0",
            License::CcBy,
        );
        m.tags = vec!["sofa".into(), "living".into()];
        m.style_tags = vec!["scandinavian".into()];
        m.lods = vec![
            MeshBlob {
                mesh_hash: format!("blake3:{id}_0"),
                vertex_count: 800,
                triangle_count: 1000,
            },
            MeshBlob {
                mesh_hash: format!("blake3:{id}_1"),
                vertex_count: 400,
                triangle_count: 500,
            },
            MeshBlob {
                mesh_hash: format!("blake3:{id}_2"),
                vertex_count: 200,
                triangle_count: 250,
            },
        ];
        m.thumbnail_kind = ThumbnailKind::Placeholder;
        m.thumbnail_hash = format!("blake3:thumb_{id}");
        m
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let m = sample("a1");
        let chain = LodChain::from_ratios(1000, &[]);
        db.upsert_metadata(&m, &chain).unwrap();
        let got = db.get(&m.asset_id).unwrap().unwrap();
        assert_eq!(got.asset_id, m.asset_id);
        assert_eq!(got.lods.len(), 3);
    }

    #[test]
    fn query_filters_by_tag() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let mut a = sample("a1");
        a.tags = vec!["sofa".into()];
        let mut b = sample("a2");
        b.tags = vec!["table".into()];
        let chain = LodChain::from_ratios(1000, &[]);
        db.upsert_metadata(&a, &chain).unwrap();
        db.upsert_metadata(&b, &chain).unwrap();
        let q = AssetQuery {
            tags: vec!["sofa".into()],
            ..AssetQuery::default()
        };
        let res = db.query(&q).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].asset_id, "a1");
    }

    #[test]
    fn query_tag_filter_composes_with_limit() {
        // Regression test for the pre-fix bug where `LIMIT` was
        // applied as a SQL clause **before** the in-Rust tag
        // `.retain()` filter ran. With `LIMIT 3` and a tag-filtered
        // query against a 5-row library where the 3 newest rows are
        // *untagged* and the 2 oldest are tagged, the old path would
        // return zero results — the SQL returned the 3 newest rows,
        // then `.retain()` filtered them all out. The fix pushes the
        // tag predicate into SQL so `LIMIT` only applies to the
        // already-filtered set; we now correctly return both tagged
        // rows. Mirrors the renderer's in-process `filterAssets`
        // semantics (filter first, slice last).
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        // Insert 5 assets. Newer rows have no `sofa` tag, older rows
        // do. The default `ORDER BY created_at DESC` would surface
        // the untagged rows first.
        for i in 0..5 {
            let mut m = sample(&format!("a{i}"));
            // Tag only the two oldest (`a0`, `a1`).
            m.tags = if i < 2 {
                vec!["sofa".into()]
            } else {
                vec!["chair".into()]
            };
            db.upsert_metadata(&m, &chain).unwrap();
            // Force monotonically increasing `created_at` so the
            // ORDER BY is deterministic on hosts where the test runs
            // faster than the timestamp clock resolution.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let q = AssetQuery {
            tags: vec!["sofa".into()],
            limit: Some(3),
            ..AssetQuery::default()
        };
        let res = db.query(&q).unwrap();
        assert_eq!(
            res.len(),
            2,
            "tag filter must compose with LIMIT — pre-fix path returned 0 here",
        );
        let mut ids: Vec<&str> = res.iter().map(|m| m.asset_id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["a0", "a1"]);
    }

    #[test]
    fn query_filters_by_style_tag() {
        // Style-tag filter mirrors the `tags` filter end-to-end —
        // pin the AND semantics for `style_tags` separately so the
        // SQL clause for `assets.style_tags` doesn't regress
        // independently.
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        let mut a = sample("a1");
        a.style_tags = vec!["industrial".into()];
        let mut b = sample("a2");
        b.style_tags = vec!["scandinavian".into()];
        db.upsert_metadata(&a, &chain).unwrap();
        db.upsert_metadata(&b, &chain).unwrap();
        let q = AssetQuery {
            style_tags: vec!["industrial".into()],
            ..AssetQuery::default()
        };
        let res = db.query(&q).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].asset_id, "a1");
    }

    #[test]
    fn query_combined_tag_filters_intersect() {
        // Two `tags` and one `style_tags` predicate must AND: only
        // rows that satisfy **all** subclauses return. Pins the
        // bot-flagged contract on `AssetListQuery::tags` /
        // `style_tags`.
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        let mut a = sample("a1");
        a.tags = vec!["sofa".into(), "furniture".into()];
        a.style_tags = vec!["scandinavian".into()];
        let mut b = sample("a2");
        b.tags = vec!["sofa".into()];
        b.style_tags = vec!["industrial".into()];
        let mut c = sample("a3");
        c.tags = vec!["chair".into(), "furniture".into()];
        c.style_tags = vec!["scandinavian".into()];
        db.upsert_metadata(&a, &chain).unwrap();
        db.upsert_metadata(&b, &chain).unwrap();
        db.upsert_metadata(&c, &chain).unwrap();
        let q = AssetQuery {
            tags: vec!["sofa".into(), "furniture".into()],
            style_tags: vec!["scandinavian".into()],
            ..AssetQuery::default()
        };
        let res = db.query(&q).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].asset_id, "a1");
    }

    #[test]
    fn escape_like_escapes_wildcards_and_backslash() {
        assert_eq!(escape_like("plain"), "plain");
        assert_eq!(escape_like("50%"), "50\\%");
        assert_eq!(escape_like("foo_bar"), "foo\\_bar");
        assert_eq!(escape_like("a\\b"), "a\\\\b");
        assert_eq!(escape_like("%_\\"), "\\%\\_\\\\");
    }

    #[test]
    fn name_contains_treats_underscore_and_percent_as_literals() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);

        // Three assets whose names look adversarial w.r.t. LIKE wildcards.
        let mut a = sample("a1");
        a.name = "Sofa 50%".into();
        let mut b = sample("a2");
        b.name = "Sofa 50X".into(); // would match `50_` under bare LIKE
        let mut c = sample("a3");
        c.name = "Sofa Five".into();
        db.upsert_metadata(&a, &chain).unwrap();
        db.upsert_metadata(&b, &chain).unwrap();
        db.upsert_metadata(&c, &chain).unwrap();

        // `50%` must match only the literal "50%", not "50X".
        let q_pct = AssetQuery {
            name_contains: Some("50%".into()),
            ..AssetQuery::default()
        };
        let r_pct = db.query(&q_pct).unwrap();
        assert_eq!(r_pct.len(), 1);
        assert_eq!(r_pct[0].asset_id, "a1");

        // `50_` should match nothing — there's no asset whose name contains
        // a literal underscore. Without escaping this would have matched
        // "Sofa 50%" and "Sofa 50X" via the `_` wildcard.
        let q_us = AssetQuery {
            name_contains: Some("50_".into()),
            ..AssetQuery::default()
        };
        let r_us = db.query(&q_us).unwrap();
        assert!(
            r_us.is_empty(),
            "underscore was treated as wildcard: {r_us:?}"
        );
    }

    #[test]
    fn put_blob_is_idempotent() {
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let payload = b"hello mesh";
        assert!(db.put_blob("blake3:1", payload).unwrap());
        assert!(!db.put_blob("blake3:1", payload).unwrap());
        assert!(db.blob_exists("blake3:1").unwrap());
        let got = db.get_blob("blake3:1").unwrap().unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn fts_search_finds_asset_by_name_token() {
        use crate::search::SearchOptions;
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        let mut chair = sample("chair-1");
        chair.name = "Oak Lounge Chair".into();
        chair.tags = vec!["seating".into()];
        let mut table = sample("table-1");
        table.name = "Walnut Coffee Table".into();
        table.tags = vec!["surface".into()];
        db.upsert_metadata(&chair, &chain).unwrap();
        db.upsert_metadata(&table, &chain).unwrap();

        let hits = db
            .search(&SearchOptions {
                query: "chair".into(),
                ..SearchOptions::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].metadata.asset_id, "chair-1");
    }

    #[test]
    fn fts_search_ranks_name_above_tag() {
        use crate::search::SearchOptions;
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        // BM25 IDF requires more than 2 documents to differentiate
        // between term-frequency-weighted columns — with N=2 docs, IDF
        // collapses to log(1) = 0 and the BM25 contribution drops to 0
        // regardless of weight. Seed several decoy documents so the
        // weight ordering is observable.
        for i in 0..6 {
            let mut decoy = sample(&format!("decoy-{i}"));
            decoy.name = format!("Decoy Item {i}");
            decoy.tags = vec!["misc".into()];
            db.upsert_metadata(&decoy, &chain).unwrap();
        }
        let mut tagged = sample("with-tag");
        tagged.name = "Side Table".into();
        tagged.tags = vec!["oak".into()];
        let mut named = sample("with-name");
        named.name = "Oak Bookshelf".into();
        named.tags = vec!["furniture".into()];
        db.upsert_metadata(&tagged, &chain).unwrap();
        db.upsert_metadata(&named, &chain).unwrap();

        let hits = db
            .search(&SearchOptions {
                query: "oak".into(),
                ..SearchOptions::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 2);
        // bm25 weights `name` 3x vs `tags` 2x; lower (more negative) is
        // better, so the name-hit asset should rank first.
        assert_eq!(hits[0].metadata.asset_id, "with-name");
        assert_eq!(hits[1].metadata.asset_id, "with-tag");
        assert!(hits[0].bm25 < hits[1].bm25, "name should outscore tag");
    }

    #[test]
    fn fts_search_multi_token_is_anded() {
        use crate::search::SearchOptions;
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        let mut chair = sample("c");
        chair.name = "Oak Lounge Chair".into();
        let mut shelf = sample("s");
        shelf.name = "Oak Bookshelf".into();
        db.upsert_metadata(&chair, &chain).unwrap();
        db.upsert_metadata(&shelf, &chain).unwrap();

        let hits = db
            .search(&SearchOptions {
                query: "oak chair".into(),
                ..SearchOptions::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].metadata.asset_id, "c");
    }

    #[test]
    fn fts_search_diacritics_normalised() {
        use crate::search::SearchOptions;
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        let mut a = sample("cafe");
        a.name = "Café Table".into();
        db.upsert_metadata(&a, &chain).unwrap();

        // `remove_diacritics 2` -> ASCII "cafe" matches "café".
        let hits = db
            .search(&SearchOptions {
                query: "cafe".into(),
                ..SearchOptions::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn fts_search_empty_query_returns_recent_assets() {
        use crate::search::SearchOptions;
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        for i in 0..3 {
            let m = sample(&format!("a{i}"));
            db.upsert_metadata(&m, &chain).unwrap();
        }
        let hits = db.search(&SearchOptions::default()).unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn fts_search_delete_removes_from_index() {
        use crate::search::SearchOptions;
        let mut db = AssetDatabase::open_in_memory().unwrap();
        let chain = LodChain::from_ratios(1000, &[]);
        let mut m = sample("c");
        m.name = "Oak Chair".into();
        db.upsert_metadata(&m, &chain).unwrap();
        assert_eq!(
            db.search(&SearchOptions {
                query: "oak".into(),
                ..SearchOptions::default()
            })
            .unwrap()
            .len(),
            1
        );
        db.delete(&m.asset_id).unwrap();
        assert!(db
            .search(&SearchOptions {
                query: "oak".into(),
                ..SearchOptions::default()
            })
            .unwrap()
            .is_empty());
    }
}
