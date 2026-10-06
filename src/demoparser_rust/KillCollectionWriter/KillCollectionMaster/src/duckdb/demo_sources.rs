//! Original demo provenance rows.

use duckdb::{params, Connection, Result as DuckResult};
use interface::models::demo_source::DemoSource;

/// Upsert inside the caller's transaction. Keeping this transaction-neutral lets collection
/// rows and provenance become durable in one commit.
pub fn upsert_demo_sources(conn: &Connection, sources: &[DemoSource]) -> DuckResult<()> {
    if sources.is_empty() {
        return Ok(());
    }

    let mut stmt = conn.prepare(
        "INSERT INTO demo_sources (
            demo_name, source_path, size_bytes, modified_ns, written_at
         ) VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT (demo_name) DO UPDATE SET
            source_path = excluded.source_path,
            size_bytes = excluded.size_bytes,
            modified_ns = excluded.modified_ns,
            written_at = CASE
                WHEN demo_sources.source_path <> excluded.source_path
                  OR demo_sources.size_bytes <> excluded.size_bytes
                  OR demo_sources.modified_ns <> excluded.modified_ns
                THEN now()
                ELSE demo_sources.written_at
            END",
    )?;
    for source in sources {
        let demo_name = interface::utils::parser_utils::canonical_demo_name(&source.demo_name);
        stmt.execute(params![
            &demo_name,
            &source.source_path,
            source.size_bytes,
            source.modified_ns,
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duckdb::schema::initialize_tables;

    #[test]
    fn upsert_replaces_source_identity() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_tables(&conn).unwrap();
        let mut source = DemoSource {
            demo_name: "match.dem.gz".into(),
            source_path: "D:/one.dem".into(),
            size_bytes: 10,
            modified_ns: 20,
        };
        upsert_demo_sources(&conn, &[source.clone()]).unwrap();
        source.demo_name = "match.dem.zst".into();
        source.source_path = "D:/two.dem.zst".into();
        source.size_bytes = 7;
        upsert_demo_sources(&conn, &[source]).unwrap();
        let stored: (String, i64) = conn
            .query_row(
                "SELECT source_path, size_bytes FROM demo_sources WHERE demo_name = 'match'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, ("D:/two.dem.zst".into(), 7));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM demo_sources", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "transport changes must update one provenance row");
    }
}
