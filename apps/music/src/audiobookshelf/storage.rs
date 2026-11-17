use super::model::*;
use anyhow::Result;
use rusqlite::params;
use serde::{Serialize, de::DeserializeOwned};

fn db() -> Result<rusqlite::Connection> {
    crate::library::db::open()
}
pub fn put<T: Serialize>(kind: &str, key: &str, value: &T) -> Result<()> {
    db()?.execute(
        "INSERT INTO abs_records VALUES (?1,?2,?3) ON CONFLICT(kind,key) DO UPDATE SET value=excluded.value",
        params![kind, key, serde_json::to_string(value)?],
    )?;
    Ok(())
}
pub fn get<T: DeserializeOwned + Default>(kind: &str, key: &str) -> T {
    db().and_then(|c| {
        Ok(c.query_row("SELECT value FROM abs_records WHERE kind=?1 AND key=?2", params![kind, key], |r| r.get::<_, String>(0))?)
    })
    .ok()
    .and_then(|s| serde_json::from_str(&s).ok())
    .unwrap_or_default()
}
pub fn list<T: DeserializeOwned>(kind: &str) -> Vec<T> {
    (|| -> Result<Vec<T>> {
        let c = db()?;
        let mut stmt = c.prepare("SELECT value FROM abs_records WHERE kind=?1")?;
        Ok(stmt
            .query_map([kind], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok().and_then(|s| serde_json::from_str(&s).ok()))
            .collect())
    })()
    .unwrap_or_default()
}
pub fn remove(kind: &str, key: &str) -> Result<()> {
    db()?.execute("DELETE FROM abs_records WHERE kind=?1 AND key=?2", params![kind, key])?;
    Ok(())
}
pub fn config() -> Config {
    let c: Config = get("config", "active");
    if c.speed <= 0.0 { Config::fresh() } else { c }
}
pub fn progress(id: &MediaId) -> Progress {
    get("progress", &id.key())
}
pub fn books(c: &Config) -> Vec<Book> {
    list::<Book>("book").into_iter().filter(|b| b.id.connection == c.connection && b.id.user == c.user).collect()
}
pub fn downloads(c: &Config) -> Vec<Download> {
    list::<Download>("download")
        .into_iter()
        .filter(|d| d.book.id.connection == c.connection && d.book.id.user == c.user)
        .collect()
}
pub fn dir(id: &MediaId) -> std::path::PathBuf {
    crate::paths::data_dir().join("audiobookshelf").join(crate::library::hash(&id.key()))
}
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |t| t.as_millis() as i64)
}

pub fn cache_book(b: &Book) -> Result<()> {
    let mut b = b.clone();
    let old: Book = get("book", &b.id.key());
    if b.files.is_empty() {
        b.files = old.files;
    }
    if b.chapters.is_empty() {
        b.chapters = old.chapters;
    }
    if b.episodes.is_empty() {
        b.episodes = old.episodes;
    }
    put("book", &b.id.key(), &b)
}

/// Serialize checkpoint and acknowledgement changes so an HTTP response cannot replace a newer checkpoint.
pub fn change_progress(id: &MediaId, change: impl FnOnce(&mut Progress)) -> Result<Progress> {
    let mut c = db()?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let key = id.key();
    let mut p: Progress = tx
        .query_row("SELECT value FROM abs_records WHERE kind='progress' AND key=?1", [&key], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    change(&mut p);
    tx.execute(
        "INSERT INTO abs_records VALUES ('progress',?1,?2) ON CONFLICT(kind,key) DO UPDATE SET value=excluded.value",
        params![key, serde_json::to_string(&p)?],
    )?;
    tx.commit()?;
    Ok(p)
}

/// Count account-scoped progress in one query instead of opening SQLite for each book.
pub fn progress_summary(c: &Config) -> (u64, u64) {
    let prefix = format!("abs://{}/{}/", c.connection, c.user);
    db().and_then(|db|Ok(db.query_row(
        "SELECT coalesce(sum(CASE WHEN json_extract(value,'$.dirty') THEN 1 ELSE 0 END),0), coalesce(sum(CASE WHEN json_type(value,'$.conflict')='object' THEN 1 ELSE 0 END),0) FROM abs_records WHERE kind='progress' AND substr(key,1,length(?1))=?1",
        [prefix],|r|Ok((r.get::<_,i64>(0)?.max(0) as u64,r.get::<_,i64>(1)?.max(0) as u64)))?)).unwrap_or_default()
}

/// One read for Home's progress indicators, scoped to the selected account.
pub fn progress_map(c: &Config) -> std::collections::HashMap<String, Progress> {
    (|| -> Result<_> {
        let db = db()?;
        let mut statement =
            db.prepare("SELECT key,value FROM abs_records WHERE kind='progress' AND substr(key,1,length(?1))=?1")?;
        let prefix = format!("abs://{}/{}/", c.connection, c.user);
        Ok(statement
            .query_map([prefix], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok().and_then(|(k, v)| serde_json::from_str(&v).ok().map(|p| (k, p))))
            .collect())
    })()
    .unwrap_or_default()
}
