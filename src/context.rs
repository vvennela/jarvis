//! Bounded, durable current context and metadata-only history. No model calls.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{fmt, path::Path, time::Duration};

pub const MAX_CONTENT_BYTES: usize = 64 * 1024;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const MAX_RESULTS: usize = 32;

#[derive(Debug)]
pub enum ContextError {
    InvalidInput,
    Capacity,
    Database(rusqlite::Error),
}
impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(_) => write!(f, "context database operation failed"),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for ContextError {}
impl From<rusqlite::Error> for ContextError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_resources: usize,
    pub max_events: usize,
    pub max_actions: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_resources: 2048,
            max_events: 4096,
            max_actions: 128,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    File,
    Browser,
    Accessibility,
}
impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Browser => "browser",
            Self::Accessibility => "accessibility",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Actor {
    Unknown,
    AgentCandidate { job_id: String, action_id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    pub id: String,
    pub version: i64,
    pub content: String,
    pub source: Source,
    pub observed_at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub resource_id: String,
    pub version: i64,
    pub kind: String,
    pub actor: Actor,
    pub observed_at_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationResult {
    Changed { version: i64, actor: Actor },
    Unchanged { version: i64 },
    IgnoredOlder,
}

pub struct ContextStore {
    connection: Connection,
    limits: Limits,
}
impl ContextStore {
    pub fn open(path: &Path, limits: Limits) -> Result<Self, ContextError> {
        if limits.max_resources == 0
            || limits.max_resources > 10000
            || limits.max_events == 0
            || limits.max_events > 100000
            || limits.max_actions == 0
            || limits.max_actions > 1024
        {
            return Err(ContextError::InvalidInput);
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA cache_size=-2048; PRAGMA max_page_count=32768; PRAGMA wal_autocheckpoint=128; PRAGMA journal_size_limit=4194304;
            CREATE TABLE IF NOT EXISTS resources(id TEXT PRIMARY KEY,version INTEGER NOT NULL,digest BLOB NOT NULL,content TEXT NOT NULL,source TEXT NOT NULL,observed_at_ms INTEGER NOT NULL);
            CREATE VIRTUAL TABLE IF NOT EXISTS search_index USING fts5(resource_id UNINDEXED,content);
            CREATE TABLE IF NOT EXISTS events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,resource_id TEXT NOT NULL,version INTEGER NOT NULL,kind TEXT NOT NULL,actor TEXT NOT NULL,observed_at_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS tombstones(id TEXT PRIMARY KEY,version INTEGER NOT NULL,source TEXT NOT NULL,observed_at_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS expected_actions(action_id TEXT PRIMARY KEY,job_id TEXT NOT NULL,resource_id TEXT NOT NULL,digest BLOB NOT NULL,start_ms INTEGER NOT NULL,deadline_ms INTEGER NOT NULL);")?;
        Ok(Self { connection, limits })
    }
    fn check_text(id: &str, content: &str, observed_at_ms: i64) -> Result<(), ContextError> {
        if id.is_empty()
            || id.len() > 2048
            || content.len() > MAX_CONTENT_BYTES
            || observed_at_ms < 0
        {
            return Err(ContextError::InvalidInput);
        }
        Ok(())
    }
    /// Only trusted runner code registers exact expected effects. A web page must
    /// never call this API. Matching is deliberately conservative and one-use.
    pub fn expect_action(
        &mut self,
        action_id: &str,
        job_id: &str,
        resource_id: &str,
        content: &str,
        start_ms: i64,
        deadline_ms: i64,
    ) -> Result<(), ContextError> {
        Self::check_text(resource_id, content, start_ms)?;
        if action_id.is_empty()
            || action_id.len() > 128
            || job_id.is_empty()
            || job_id.len() > 128
            || deadline_ms <= start_ms
        {
            return Err(ContextError::InvalidInput);
        }
        let tx = self.connection.savepoint()?;
        tx.execute(
            "DELETE FROM expected_actions WHERE deadline_ms < ?1",
            [start_ms],
        )?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM expected_actions", [], |r| r.get(0))?;
        if count as usize >= self.limits.max_actions {
            return Err(ContextError::Capacity);
        }
        tx.execute(
            "INSERT INTO expected_actions VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                action_id,
                job_id,
                resource_id,
                blake3::hash(content.as_bytes()).as_bytes().as_slice(),
                start_ms,
                deadline_ms
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    /// observed_at_ms is assigned by the trusted collector, not by a page/model.
    /// Content from both sources stays untrusted evidence, never authorization.
    pub fn observe(
        &mut self,
        id: &str,
        content: &str,
        source: Source,
        observed_at_ms: i64,
    ) -> Result<ObservationResult, ContextError> {
        Self::check_text(id, content, observed_at_ms)?;
        let digest = blake3::hash(content.as_bytes());
        let tx = self.connection.savepoint()?;
        let prior:Option<(i64,Vec<u8>,i64,String)>=tx.query_row("SELECT version,digest,observed_at_ms,source FROM resources WHERE id=?1 UNION ALL SELECT version,X'',observed_at_ms,source FROM tombstones WHERE id=?1 LIMIT 1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((_, _, old_time, old_source)) = &prior {
            if old_source != source.label() {
                return Err(ContextError::InvalidInput);
            }
            if observed_at_ms < *old_time {
                return Ok(ObservationResult::IgnoredOlder);
            }
        } else {
            let count: i64 = tx.query_row(
                "SELECT (SELECT COUNT(*) FROM resources)+(SELECT COUNT(*) FROM tombstones)",
                [],
                |r| r.get(0),
            )?;
            if count as usize >= self.limits.max_resources {
                return Err(ContextError::Capacity);
            }
        }
        let actor = consume_actions(&tx, id, digest.as_bytes(), observed_at_ms)?;
        if let Some((version, old_hash, _, _)) = &prior
            && old_hash == digest.as_bytes()
        {
            tx.execute(
                "UPDATE resources SET observed_at_ms=?2 WHERE id=?1",
                params![id, observed_at_ms],
            )?;
            tx.commit()?;
            return Ok(ObservationResult::Unchanged { version: *version });
        }
        let version = match prior {
            Some((v, _, _, _)) => v.checked_add(1).ok_or(ContextError::Capacity)?,
            None => 1,
        };
        tx.execute("DELETE FROM tombstones WHERE id=?1", [id])?;
        tx.execute("INSERT INTO resources VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET version=excluded.version,digest=excluded.digest,content=excluded.content,source=excluded.source,observed_at_ms=excluded.observed_at_ms",params![id,version,digest.as_bytes().as_slice(),content,source.label(),observed_at_ms])?;
        tx.execute("DELETE FROM search_index WHERE resource_id=?1", [id])?;
        tx.execute(
            "INSERT INTO search_index(resource_id,content) VALUES(?1,?2)",
            params![id, content],
        )?;
        record_event(
            &tx,
            id,
            version,
            "changed",
            &actor,
            observed_at_ms,
            self.limits.max_events,
        )?;
        tx.commit()?;
        Ok(ObservationResult::Changed { version, actor })
    }
    /// Apply one bounded collector observation atomically across related resources.
    pub fn observe_many(
        &mut self,
        observations: &[(&str, &str, Source, i64)],
    ) -> Result<Vec<ObservationResult>, ContextError> {
        if observations.is_empty() || observations.len() > 8 {
            return Err(ContextError::InvalidInput);
        }
        self.connection
            .execute_batch("SAVEPOINT observation_batch")?;
        let result: Result<Vec<_>, _> = observations
            .iter()
            .map(|(id, content, source, time)| self.observe(id, content, *source, *time))
            .collect();
        if result.is_err() {
            self.connection
                .execute_batch("ROLLBACK TO observation_batch; RELEASE observation_batch")?;
        } else {
            self.connection.execute_batch("RELEASE observation_batch")?;
        }
        result
    }

    /// Tombstones retain the version sequence without retaining deleted content.
    pub fn forget(
        &mut self,
        id: &str,
        observed_at_ms: i64,
    ) -> Result<Option<ObservationResult>, ContextError> {
        Self::check_text(id, "", observed_at_ms)?;
        let tx = self.connection.savepoint()?;
        let prior:Option<(i64,i64,String,bool)>=tx.query_row("SELECT version,observed_at_ms,source,0 FROM resources WHERE id=?1 UNION ALL SELECT version,observed_at_ms,source,1 FROM tombstones WHERE id=?1 LIMIT 1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((version, time, source, tombstone)) = prior else {
            return Ok(None);
        };
        if observed_at_ms < time {
            return Ok(Some(ObservationResult::IgnoredOlder));
        }
        tx.execute(
            "DELETE FROM expected_actions WHERE resource_id=?1 AND start_ms<=?2",
            params![id, observed_at_ms],
        )?;
        if tombstone {
            tx.execute(
                "UPDATE tombstones SET observed_at_ms=?2 WHERE id=?1",
                params![id, observed_at_ms],
            )?;
            tx.commit()?;
            return Ok(Some(ObservationResult::Unchanged { version }));
        }
        let version = version.checked_add(1).ok_or(ContextError::Capacity)?;
        tx.execute("DELETE FROM resources WHERE id=?1", [id])?;
        tx.execute("DELETE FROM search_index WHERE resource_id=?1", [id])?;
        tx.execute(
            "INSERT INTO tombstones VALUES(?1,?2,?3,?4)",
            params![id, version, source, observed_at_ms],
        )?;
        record_event(
            &tx,
            id,
            version,
            "deleted",
            &Actor::Unknown,
            observed_at_ms,
            self.limits.max_events,
        )?;
        tx.commit()?;
        Ok(Some(ObservationResult::Changed {
            version,
            actor: Actor::Unknown,
        }))
    }
    /// Bounded enumeration for deletion reconciliation within one explicit root.
    pub fn file_ids_under(&self, root: &Path) -> Result<Vec<String>, ContextError> {
        if !root.is_absolute() {
            return Err(ContextError::InvalidInput);
        }
        let root = root.to_str().ok_or(ContextError::InvalidInput)?;
        if root.len() > 2040 {
            return Err(ContextError::InvalidInput);
        }
        let prefix = format!("file:{}/", root.trim_end_matches('/'));
        let mut statement = self.connection.prepare("SELECT id FROM resources WHERE source='file' AND substr(id,1,length(?1))=?1 ORDER BY id LIMIT 1025")?;
        let ids: Vec<String> = statement
            .query_map([prefix], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if ids.len() > 1024 {
            return Err(ContextError::Capacity);
        }
        Ok(ids)
    }

    pub fn get(&self, id: &str) -> Result<Option<Resource>, ContextError> {
        if id.len() > 2048 {
            return Err(ContextError::InvalidInput);
        }
        Ok(self
            .connection
            .query_row(
                "SELECT id,version,content,source,observed_at_ms FROM resources WHERE id=?1",
                [id],
                resource_row,
            )
            .optional()?)
    }
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Resource>, ContextError> {
        if query.is_empty() || query.len() > MAX_QUERY_BYTES || limit == 0 || limit > MAX_RESULTS {
            return Err(ContextError::InvalidInput);
        }
        let terms: Vec<_> = query
            .split_whitespace()
            .take(16)
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect();
        if terms.is_empty() {
            return Err(ContextError::InvalidInput);
        }
        let mut statement=self.connection.prepare("SELECT r.id,r.version,r.content,r.source,r.observed_at_ms FROM search_index JOIN resources r ON search_index.resource_id=r.id WHERE search_index MATCH ?1 ORDER BY rank LIMIT ?2")?;
        Ok(statement
            .query_map(params![terms.join(" AND "), limit as i64], resource_row)?
            .collect::<Result<_, _>>()?)
    }
    pub fn recent(&self, limit: usize) -> Result<Vec<Event>, ContextError> {
        if limit == 0 || limit > MAX_RESULTS {
            return Err(ContextError::InvalidInput);
        }
        let mut statement=self.connection.prepare("SELECT resource_id,version,kind,actor,observed_at_ms FROM events ORDER BY sequence DESC LIMIT ?1")?;
        let rows = statement.query_map([limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (resource_id, version, kind, actor, observed_at_ms) = row?;
            result.push(Event {
                resource_id,
                version,
                kind,
                actor: serde_json::from_str(&actor).map_err(|_| ContextError::InvalidInput)?,
                observed_at_ms,
            });
        }
        Ok(result)
    }
}
fn resource_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Resource> {
    let source: String = row.get(3)?;
    let source = match source.as_str() {
        "file" => Source::File,
        "browser" => Source::Browser,
        "accessibility" => Source::Accessibility,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok(Resource {
        id: row.get(0)?,
        version: row.get(1)?,
        content: row.get(2)?,
        source,
        observed_at_ms: row.get(4)?,
    })
}

fn consume_actions(
    tx: &Connection,
    id: &str,
    digest: &[u8],
    time: i64,
) -> Result<Actor, ContextError> {
    tx.execute(
        "DELETE FROM expected_actions WHERE deadline_ms < ?1",
        [time],
    )?;
    let matches: Vec<(String, String)> = {
        let mut statement=tx.prepare("SELECT job_id,action_id FROM expected_actions WHERE resource_id=?1 AND digest=?2 AND start_ms<=?3 AND deadline_ms>=?3 LIMIT 2")?;
        statement
            .query_map(params![id, digest, time], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    // Any observed target change or no-op consumes the expectation. Correlation
    // alone never proves an actor, and a mismatch must not linger for later use.
    tx.execute(
        "DELETE FROM expected_actions WHERE resource_id=?1 AND start_ms<=?2",
        params![id, time],
    )?;
    Ok(if matches.len() == 1 {
        let (job_id, action_id) = matches[0].clone();
        Actor::AgentCandidate { job_id, action_id }
    } else {
        Actor::Unknown
    })
}
fn record_event(
    tx: &Connection,
    id: &str,
    version: i64,
    kind: &str,
    actor: &Actor,
    time: i64,
    max_events: usize,
) -> Result<(), ContextError> {
    let encoded = serde_json::to_string(actor).map_err(|_| ContextError::InvalidInput)?;
    tx.execute(
        "INSERT INTO events(resource_id,version,kind,actor,observed_at_ms) VALUES(?1,?2,?3,?4,?5)",
        params![id, version, kind, encoded, time],
    )?;
    tx.execute("DELETE FROM events WHERE sequence NOT IN (SELECT sequence FROM events ORDER BY sequence DESC LIMIT ?1)",[max_events as i64])?;
    Ok(())
}
