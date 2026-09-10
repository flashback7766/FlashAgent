//! flashagent-data: SQLite+FTS5 storage — sessions, messages, full-text
//! search, versioned migrations. One file on disk, in-memory mode for tests.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use thiserror::Error;

/// Storage-layer errors.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Underlying SQLite failure.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// A chat session.
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// Database id.
    pub id: i64,
    /// Human-readable title.
    pub title: String,
    /// Working directory the session is bound to.
    pub cwd: String,
    /// Creation time, unix seconds.
    pub created_at: i64,
    /// Last activity time, unix seconds.
    pub updated_at: i64,
}

/// Message author role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// User input.
    User,
    /// Model output.
    Assistant,
    /// Tool result.
    Tool,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            "tool" => Some(Role::Tool),
            _ => None,
        }
    }
}

/// One stored message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Database id.
    pub id: i64,
    /// Owning session id.
    pub session_id: i64,
    /// Author role.
    pub role: Role,
    /// Visible text content.
    pub content: String,
    /// Reasoning/thinking content, if the model produced any.
    pub reasoning: Option<String>,
    /// Prompt tokens reported by the backend (None if unknown).
    pub tokens_in: Option<i64>,
    /// Completion tokens reported by the backend (None if unknown).
    pub tokens_out: Option<i64>,
    /// Creation time, unix seconds.
    pub created_at: i64,
}

/// Handle to the storage. Sync by design: the service serializes access.
pub struct Store {
    conn: Connection,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Schema batches, applied in order; `meta.schema_version` tracks progress.
const MIGRATIONS: &[&str] = &[
    // v1: sessions, messages, FTS5 index kept in sync by triggers.
    "
    CREATE TABLE sessions (
        id         INTEGER PRIMARY KEY,
        title      TEXT NOT NULL,
        cwd        TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE messages (
        id         INTEGER PRIMARY KEY,
        session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        role       TEXT NOT NULL CHECK (role IN ('user','assistant','tool')),
        content    TEXT NOT NULL,
        reasoning  TEXT,
        tokens_in  INTEGER,
        tokens_out INTEGER,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX idx_messages_session ON messages(session_id, id);
    CREATE VIRTUAL TABLE messages_fts USING fts5(
        content,
        content='messages',
        content_rowid='id',
        tokenize='unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
    END;
    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, content)
        VALUES ('delete', old.id, old.content);
    END;
    CREATE TRIGGER messages_au AFTER UPDATE OF content ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, content)
        VALUES ('delete', old.id, old.content);
        INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
    END;
    ",
];

impl Store {
    /// Open (creating if needed) a database file.
    pub fn open(path: &str) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// In-memory database for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        )?;
        let current: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE((SELECT value FROM meta WHERE key='schema_version'),'0')",
                [],
                |r| r.get::<_, String>(0),
            )?
            .parse()
            .unwrap_or(0);
        for (i, batch) in MIGRATIONS.iter().enumerate() {
            let v = (i + 1) as i64;
            if v <= current {
                continue;
            }
            self.conn.execute_batch(batch)?;
            self.conn.execute(
                "INSERT INTO meta(key,value) VALUES('schema_version',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [v.to_string()],
            )?;
        }
        Ok(())
    }

    /// Create a session; returns its id.
    pub fn create_session(&self, title: &str, cwd: &str) -> Result<i64, StoreError> {
        let t = now();
        self.conn.execute(
            "INSERT INTO sessions(title,cwd,created_at,updated_at) VALUES(?1,?2,?3,?3)",
            rusqlite::params![title, cwd, t],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Rename a session and bump its updated_at.
    pub fn rename_session(&self, id: i64, title: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sessions SET title=?1, updated_at=?2 WHERE id=?3",
            rusqlite::params![title, now(), id],
        )?;
        Ok(())
    }

    /// Delete a session with its messages (cascade) and FTS entries.
    pub fn delete_session(&self, id: i64) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM sessions WHERE id=?1", [id])?;
        Ok(())
    }

    /// Sessions ordered by recent activity.
    pub fn list_sessions(&self) -> Result<Vec<Session>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id,title,cwd,created_at,updated_at FROM sessions ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                title: r.get(1)?,
                cwd: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Append a message; returns its id. Bumps the session's updated_at.
    pub fn append_message(
        &self,
        session_id: i64,
        role: Role,
        content: &str,
        reasoning: Option<&str>,
        tokens_in: Option<i64>,
        tokens_out: Option<i64>,
    ) -> Result<i64, StoreError> {
        let t = now();
        self.conn.execute(
            "INSERT INTO messages(session_id,role,content,reasoning,tokens_in,tokens_out,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                session_id,
                role.as_str(),
                content,
                reasoning,
                tokens_in,
                tokens_out,
                t
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.conn.execute(
            "UPDATE sessions SET updated_at=?1 WHERE id=?2",
            rusqlite::params![t, session_id],
        )?;
        Ok(id)
    }

    /// All messages of a session in order.
    pub fn messages(&self, session_id: i64) -> Result<Vec<Message>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id,session_id,role,content,reasoning,tokens_in,tokens_out,created_at
             FROM messages WHERE session_id=?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([session_id], Self::row_to_message)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Full-text search across all messages (FTS5 query syntax).
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<Message>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id,m.session_id,m.role,m.content,m.reasoning,m.tokens_in,m.tokens_out,m.created_at
             FROM messages_fts f JOIN messages m ON m.id=f.rowid
             WHERE messages_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![query, limit as i64], Self::row_to_message)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn row_to_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
        let role: String = r.get(2)?;
        Ok(Message {
            id: r.get(0)?,
            session_id: r.get(1)?,
            role: Role::from_str(&role).unwrap_or(Role::User),
            content: r.get(3)?,
            reasoning: r.get(4)?,
            tokens_in: r.get(5)?,
            tokens_out: r.get(6)?,
            created_at: r.get(7)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_crud_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let id = store.create_session("test", "/tmp").unwrap();
        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "test");
        store.rename_session(id, "renamed").unwrap();
        assert_eq!(store.list_sessions().unwrap()[0].title, "renamed");
        store.delete_session(id).unwrap();
        assert!(store.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn messages_append_and_list_in_order() {
        let store = Store::open_in_memory().unwrap();
        let s = store.create_session("chat", "").unwrap();
        store.append_message(s, Role::User, "привет", None, None, None).unwrap();
        store
            .append_message(s, Role::Assistant, "ответ", Some("думаю"), Some(10), Some(5))
            .unwrap();
        let msgs = store.messages(s).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].reasoning.as_deref(), Some("думаю"));
        assert_eq!(msgs[1].tokens_out, Some(5));
    }

    #[test]
    fn fts_search_finds_cyrillic() {
        let store = Store::open_in_memory().unwrap();
        let s = store.create_session("chat", "").unwrap();
        store.append_message(s, Role::User, "настройка вулкана в арче", None, None, None).unwrap();
        store.append_message(s, Role::User, " unrelated ", None, None, None).unwrap();
        let hits = store.search_messages("вулкана", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, s);
    }

    #[test]
    fn deleting_session_removes_fts_rows() {
        let store = Store::open_in_memory().unwrap();
        let s = store.create_session("chat", "").unwrap();
        store.append_message(s, Role::User, "unique_zebra_word", None, None, None).unwrap();
        store.delete_session(s).unwrap();
        assert!(store.search_messages("unique_zebra_word", 10).unwrap().is_empty());
    }

    #[test]
    fn migrations_are_idempotent_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let id = Store::open(path.to_str().unwrap())
            .unwrap()
            .create_session("persisted", "")
            .unwrap();
        drop(Store::open(path.to_str().unwrap()).unwrap().messages(id));
        let reopened = Store::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.list_sessions().unwrap()[0].title, "persisted");
    }
}
