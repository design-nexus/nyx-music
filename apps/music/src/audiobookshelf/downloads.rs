use super::{api, config, model::*, notify, set_status, storage};
use anyhow::{Result, ensure};
use reqwest::StatusCode;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};
use tokio::io::AsyncWriteExt;
fn jobs() -> &'static Mutex<HashMap<String, Arc<AtomicU8>>> {
    static J: OnceLock<Mutex<HashMap<String, Arc<AtomicU8>>>> = OnceLock::new();
    J.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn start(book: Book) {
    let key = book.id.key();
    let flag = Arc::new(AtomicU8::new(0));
    {
        let mut j = jobs().lock().unwrap();
        if j.contains_key(&key) {
            return;
        }
        j.insert(key.clone(), flag.clone());
    }
    let c = config();
    api::run(
        async move {
            let r = download(&c, book, flag).await;
            jobs().lock().unwrap().remove(&key);
            r
        },
        |r| {
            if let Err(e) = r {
                set_status(e.to_string());
            }
            notify();
        },
    );
}
async fn download(c: &Config, book: Book, flag: Arc<AtomicU8>) -> Result<()> {
    let book = if book.files.is_empty() { api::detail(c, &book.id).await? } else { book };
    let key = book.id.key();
    let dir = storage::dir(&book.id);
    tokio::fs::create_dir_all(&dir).await?;
    let mut d: Download = storage::get("download", &key);
    d.book = book.clone();
    d.state = "downloading".into();
    d.error.clear();
    d.files = (0..book.files.len()).map(|i| format!("{i}.audio")).collect();
    ensure!(!d.files.is_empty(), "This item has no downloadable audio.");
    storage::put("book", &key, &book)?;
    storage::put("download", &key, &d)?;
    let result = async {
        let mut complete = 0u64;
        for (i, file) in book.files.iter().enumerate() {
            if flag.load(Ordering::Relaxed) != 0 {
                return Ok(false);
            }
            ensure!(!file.ino.is_empty(), "This episode has no audio file available on the server.");
            let dest = dir.join(&d.files[i]);
            if dest.exists() {
                complete += dest.metadata()?.len();
                continue;
            }
            let part = dir.join(format!("{i}.part"));
            let validator_key = format!("{key}/{i}");
            let validator: String = storage::get("validator", &validator_key);
            let mut offset = tokio::fs::metadata(&part).await.map_or(0, |m| m.len());
            if validator.is_empty() {
                offset = 0;
            }
            let path = format!("api/items/{}/file/{}/download", api::encode(&book.id.item), api::encode(&file.ino));
            let mut res = api::range(c, &path, offset, &validator).await?;
            if res.status() == StatusCode::RANGE_NOT_SATISFIABLE {
                let range = res.headers().get("content-range").and_then(|h| h.to_str().ok()).unwrap_or_default();
                if offset > 0 && range == format!("bytes */{offset}") {
                    tokio::fs::rename(&part, &dest).await?;
                    complete += offset;
                    continue;
                }
                res = api::range(c, &path, 0, "").await?;
                offset = 0;
            }
            offset = resume_offset(res.status(), res.headers().get("content-range").and_then(|h| h.to_str().ok()), offset)?;
            res = api::checked(res)?;
            let remaining = res.content_length();
            let validator = res
                .headers()
                .get("etag")
                .or_else(|| res.headers().get("last-modified"))
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .to_string();
            storage::put("validator", &validator_key, &validator)?;
            let mut out = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(offset == 0)
                .append(offset > 0)
                .open(&part)
                .await?;
            let mut done = offset;
            let mut last = std::time::Instant::now();
            while let Some(chunk) =
                res.chunk().await.map_err(|_| anyhow::anyhow!("Download interrupted; resume it from Downloads."))?
            {
                if flag.load(Ordering::Relaxed) != 0 {
                    out.flush().await?;
                    return Ok(false);
                }
                out.write_all(&chunk).await?;
                done += chunk.len() as u64;
                if last.elapsed().as_secs() >= 1 {
                    d.done = complete + done;
                    d.total = complete + offset + remaining.unwrap_or(done - offset);
                    storage::put("download", &key, &d)?;
                    last = std::time::Instant::now();
                }
            }
            ensure!(done > 0 && remaining.is_none_or(|n| done == offset + n), "Download is incomplete.");
            out.flush().await?;
            out.sync_all().await?;
            drop(out);
            tokio::fs::rename(&part, &dest).await?;
            complete += done;
        }
        d.done = complete;
        d.total = complete;
        Ok::<bool, anyhow::Error>(true)
    }
    .await;
    match result {
        Ok(true) => d.state = "complete".into(),
        Ok(false) => d.state = if flag.load(Ordering::Relaxed) == 2 { "cancelled" } else { "paused" }.into(),
        Err(ref e) => {
            d.state = "paused".into();
            d.error = e.to_string();
        }
    }
    if flag.load(Ordering::Relaxed) == 2 {
        for i in 0..d.files.len() {
            let _ = tokio::fs::remove_file(dir.join(format!("{i}.part"))).await;
        }
    }
    storage::put("download", &key, &d)?;
    result.map(|_| ())
}
pub fn pause(id: &MediaId, cancel: bool) {
    if let Some(f) = jobs().lock().unwrap().get(&id.key()) {
        f.store(if cancel { 2 } else { 1 }, Ordering::Relaxed);
    }
    notify();
}
pub fn delete(id: &MediaId) {
    if jobs().lock().unwrap().contains_key(&id.key()) {
        pause(id, true);
        set_status("Cancelling download. Remove it when cancellation finishes.");
        return;
    }
    if super::playback::current_book().is_some_and(|b| b.id == *id) {
        super::playback::deactivate();
    }
    let dir = storage::dir(id);
    let key = id.key();
    crate::cmd::background(
        move || -> Result<()> {
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
            }
            storage::remove("download", &key)
        },
        |r| {
            if let Err(e) = r {
                set_status(e.to_string());
            }
            notify();
        },
    );
}
pub fn files(book: &Book) -> Option<Vec<std::path::PathBuf>> {
    let d: Download = storage::get("download", &book.id.key());
    if d.state != "complete" || d.files.is_empty() {
        return None;
    }
    let dir = storage::dir(&book.id);
    let files: Vec<_> = d.files.iter().map(|f| dir.join(f)).collect();
    files.iter().all(|f| f.is_file()).then_some(files)
}

fn resume_offset(status: StatusCode, range: Option<&str>, offset: u64) -> Result<u64> {
    if status != StatusCode::PARTIAL_CONTENT {
        return Ok(0);
    }
    let range = range
        .and_then(|r| r.strip_prefix("bytes "))
        .and_then(|r| r.split_once('/'))
        .and_then(|(r, total)| r.split_once('-').map(|(start, end)| (start, end, total)));
    let Some((start, end, total)) = range else {
        anyhow::bail!("Server returned an invalid download range.");
    };
    let start = start.parse::<u64>()?;
    let end = end.parse::<u64>()?;
    ensure!(
        start == offset && end >= start && total.parse::<u64>().is_ok_and(|total| end < total),
        "Server returned an invalid download range."
    );
    Ok(offset)
}
pub fn pause_all() {
    for flag in jobs().lock().unwrap().values() {
        flag.store(1, Ordering::Relaxed);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn range_must_match_partial_file() {
        assert_eq!(resume_offset(StatusCode::OK, None, 100).unwrap(), 0);
        assert_eq!(resume_offset(StatusCode::PARTIAL_CONTENT, Some("bytes 100-199/200"), 100).unwrap(), 100);
        assert!(resume_offset(StatusCode::PARTIAL_CONTENT, Some("bytes 0-99/200"), 100).is_err());
        assert!(resume_offset(StatusCode::PARTIAL_CONTENT, Some("bytes 100-200/200"), 100).is_err());
        assert!(resume_offset(StatusCode::PARTIAL_CONTENT, None, 100).is_err());
    }
}
