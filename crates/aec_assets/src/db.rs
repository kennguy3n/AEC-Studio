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
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> AssetResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
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

    /// Run a query and return matching metadata. The query is SQL-side for
    /// vendor + name + license + indexed columns; tag/style-tag filters are
    /// applied in Rust (small libraries).
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
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");
        // Bind LIMIT as a typed i64 — not as a String — so SQLite gets an
        // INTEGER value, not text it has to coerce. `u32 -> i64` is
        // lossless and avoids the platform-dependent `usize` cast.
        let limit: i64 = i64::from(q.limit.unwrap_or(200));
        bindings.push(Box::new(limit));

        let mut stmt = self.conn.prepare(&sql)?;
        let params_iter: Vec<&dyn rusqlite::ToSql> = bindings.iter().map(AsRef::as_ref).collect();
        let mut rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_iter.iter().copied()),
                row_to_metadata,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        // Apply tag/style-tag filters in Rust.
        rows.retain(|m| {
            q.tags.iter().all(|tag| m.tags.iter().any(|t| t == tag))
                && q.style_tags
                    .iter()
                    .all(|tag| m.style_tags.iter().any(|t| t == tag))
        });
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

fn row_to_metadata(row: &Row<'_>) -> rusqlite::Result<AssetMetadata> {
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
}
