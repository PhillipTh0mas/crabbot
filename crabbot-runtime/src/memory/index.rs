use rusqlite::{OptionalExtension, params};
use sqlite_vec::sqlite3_vec_init;
use tokio_rusqlite::{Connection, Result as TrResult};
use zerocopy::AsBytes;

use std::path::Path;
use std::sync::Once;

use crate::{
    error::{Error, Result},
    memory::types::{Chunk, MemorySearchHit, SearchFilters},
};

use rusqlite::ffi::sqlite3_auto_extension;

static SQLITE_VEC_INIT: Once = Once::new();

fn init_sqlite_vec_once() {
    SQLITE_VEC_INIT.call_once(|| unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    });
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryIndex {
    conn: Connection,
    embed_dim: usize,
}

impl MemoryIndex {
    pub async fn open(sqlite_path: &Path, embed_dim: usize) -> Result<Self> {
        init_sqlite_vec_once();

        let conn = Connection::open(sqlite_path)
            .await
            .map_err(|e| Error::other(format!("open sqlite failed: {e}")))?;

        let idx = Self { conn, embed_dim };
        idx.init_schema().await?;
        Ok(idx)
    }

    async fn init_schema(&self) -> Result<()> {
        let dim = self.embed_dim;

        self.conn
            .call(move |c: &mut rusqlite::Connection| -> TrResult<()> {
                // regular table is fine
                c.execute_batch(
                    r#"
                    PRAGMA journal_mode=WAL;
                    PRAGMA synchronous=NORMAL;

                    CREATE TABLE IF NOT EXISTS mem_chunks (
                        id            TEXT PRIMARY KEY,
                        kind          TEXT NOT NULL,
                        date          TEXT NULL,
                        path          TEXT NOT NULL,
                        start         INTEGER NOT NULL,
                        end           INTEGER NOT NULL,
                        text          TEXT NOT NULL,
                        updated_ts_ms INTEGER NOT NULL
                    );
                    "#,
                )?;

                // check existing mem_vec schema
                let sql: Option<String> = c
                    .query_row(
                        r#"SELECT sql FROM sqlite_master WHERE type='table' AND name='mem_vec'"#,
                        [],
                        |r| r.get(0),
                    )
                    .optional()?;

                let want = format!("vec0(embedding float[{}])", dim);

                let recreate = match sql {
                    None => true,
                    Some(s) => !s.contains(&want),
                };

                if recreate {
                    c.execute_batch("DROP TABLE IF EXISTS mem_vec;")?;
                    let ddl = format!(
                        "CREATE VIRTUAL TABLE mem_vec USING vec0(embedding float[{}]);",
                        dim
                    );
                    c.execute_batch(&ddl)?;
                }

                Ok(())
            })
            .await
            .map_err(|e| Error::other(format!("sqlite call failed: {e}")))?;

        Ok(())
    }

    pub async fn delete_by_path(&self, path: &str) -> Result<()> {
        let path = path.to_string();

        self.conn
            .call(move |c| -> TrResult<()> {
                let rowids: Vec<i64> = {
                    let mut stmt = c.prepare("SELECT rowid FROM mem_chunks WHERE path = ?1")?;
                    stmt.query_map([path.as_str()], |r| r.get::<_, i64>(0))?
                        .collect::<std::result::Result<Vec<_>, _>>()?
                };

                let tx = c.transaction()?;

                for rowid in rowids {
                    tx.execute("DELETE FROM mem_vec WHERE rowid = ?1", [rowid])?;
                }
                tx.execute("DELETE FROM mem_chunks WHERE path = ?1", [path.as_str()])?;

                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(|e| Error::other(format!("sqlite call failed: {e}")))?;

        Ok(())
    }

    pub async fn upsert_chunks(&self, chunks: &[Chunk], embeddings: &[Vec<f32>]) -> Result<()> {
        if chunks.len() != embeddings.len() {
            return Err(Error::other(
                "upsert_chunks: chunks/embeddings length mismatch",
            ));
        }

        let dim = self.embed_dim;

        // Validate BEFORE crossing into sqlite thread (avoid needing crate::Error inside closure)
        for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
            if emb.len() != dim {
                return Err(Error::other(format!(
                    "embedding dim mismatch for chunk {}: got {}, want {}",
                    chunk.chunk_id,
                    emb.len(),
                    dim
                )));
            }
        }

        let now_ts_ms = crate::time::now_ts_ms();
        let chunks = chunks.to_vec();
        let embeddings = embeddings.to_vec();

        self.conn
            .call(move |c| -> TrResult<()> {
                let tx = c.transaction()?;
                {
                    let mut ins_chunk = tx.prepare(
                        r#"
                        INSERT INTO mem_chunks (id, kind, date, path, start, end, text, updated_ts_ms)
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                        ON CONFLICT(id) DO UPDATE SET
                            kind=excluded.kind,
                            date=excluded.date,
                            path=excluded.path,
                            start=excluded.start,
                            end=excluded.end,
                            text=excluded.text,
                            updated_ts_ms=excluded.updated_ts_ms
                        "#,
                    )?;

                    let mut sel_rowid = tx.prepare("SELECT rowid FROM mem_chunks WHERE id = ?1")?;
                    let mut up_vec =
                        tx.prepare("INSERT OR REPLACE INTO mem_vec(rowid, embedding) VALUES (?1, ?2)")?;

                    for (chunk, emb) in chunks.iter().zip(embeddings.iter()) {
                        ins_chunk.execute(params![
                            chunk.chunk_id,
                            chunk.kind.as_str(),
                            chunk.date,
                            chunk.path,
                            chunk.start as i64,
                            chunk.end as i64,
                            chunk.text,
                            now_ts_ms
                        ])?;

                        let rowid: i64 =
                            sel_rowid.query_row([chunk.chunk_id.as_str()], |r| r.get::<_, i64>(0))?;

                        up_vec.execute(params![rowid, emb.as_bytes()])?;
                    }
                }


                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(|e| Error::other(format!("sqlite call failed: {e}")))?;

        Ok(())
    }

    pub async fn search(
        &self,
        query_emb: &[f32],
        top_k: usize,
        filters: SearchFilters,
    ) -> Result<Vec<MemorySearchHit>> {
        let dim = self.embed_dim;
        if query_emb.len() != dim {
            return Err(Error::other(format!(
                "query embedding dim mismatch: got {}, want {}",
                query_emb.len(),
                dim
            )));
        }

        // over-fetch so filtering doesn’t starve results
        let fetch_k = (top_k.saturating_mul(4)).max(32);

        let qemb = query_emb.to_vec();
        let filt = filters.clone();

        let out: Vec<MemorySearchHit> = self
            .conn
            .call(move |c| -> TrResult<Vec<MemorySearchHit>> {
                let mut stmt = c.prepare(
                    r#"
                    SELECT rowid, distance
                    FROM mem_vec
                    WHERE embedding MATCH ?1
                    ORDER BY distance
                    LIMIT ?2
                    "#,
                )?;

                let pairs: Vec<(i64, f64)> = stmt
                    .query_map(params![qemb.as_bytes(), fetch_k as i64], |r| {
                        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;

                if pairs.is_empty() {
                    return Ok(vec![]);
                }

                let mut stmt2 = c.prepare(
                    r#"
                    SELECT rowid, id, kind, date, path, text
                    FROM mem_chunks
                    WHERE rowid = ?1
                    "#,
                )?;

                let mut out: Vec<MemorySearchHit> = Vec::new();

                for (rowid, dist) in pairs {
                    let row: Option<(String, String, Option<String>, String, String)> = stmt2
                        .query_row([rowid], |r| {
                            Ok((
                                r.get::<_, String>(1)?,
                                r.get::<_, String>(2)?,
                                r.get::<_, Option<String>>(3)?,
                                r.get::<_, String>(4)?,
                                r.get::<_, String>(5)?,
                            ))
                        })
                        .optional()?;

                    let Some((id, kind, date, path, text)) = row else {
                        continue;
                    };

                    if !filt.matches(&kind, &date) {
                        continue;
                    }

                    out.push(MemorySearchHit {
                        chunk_id: id,
                        kind,
                        date,
                        path,
                        distance: dist,
                        text,
                    });

                    if out.len() >= top_k {
                        break;
                    }
                }

                Ok(out)
            })
            .await
            .map_err(|e| Error::other(format!("sqlite call failed: {e}")))?; // <- single ?

        Ok(out)
    }
}
