use super::{model::*, storage};
use anyhow::{Result, bail, ensure};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use url::Url;

#[derive(Debug)]
pub struct AuthRequired(pub &'static str);
impl std::fmt::Display for AuthRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for AuthRequired {}
pub fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
pub fn server_url(s: &str) -> Result<String> {
    let mut u = Url::parse(s.trim()).map_err(|_| anyhow::anyhow!("Enter a complete server URL."))?;
    ensure!(
        matches!(u.scheme(), "http" | "https")
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "Use an HTTP or HTTPS server URL without credentials, query, or fragment."
    );
    if !u.path().ends_with('/') {
        u.set_path(&format!("{}/", u.path()));
    }
    Ok(u.to_string())
}
fn http() -> &'static Client {
    static HTTP: OnceLock<Client> = OnceLock::new();
    HTTP.get_or_init(|| {
        Client::builder()
            .user_agent(concat!("nyx-music/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client")
    })
}
#[derive(Default)]
struct Tokens {
    access: String,
    refresh: String,
    account: String,
}
fn tokens() -> &'static Mutex<Tokens> {
    static T: OnceLock<Mutex<Tokens>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(Tokens::default()))
}
#[cfg(not(test))]
async fn store_refresh(c: &Config, token: String) -> Result<()> {
    let id = c.connection.clone();
    tokio::task::spawn_blocking(move || {
        nyx_sync::store_secret("nyx-music", &id, "audiobookshelf", &nyx_sync::Secret::new(token)?)
    })
    .await??;
    Ok(())
}
#[cfg(test)]
async fn store_refresh(_c: &Config, _token: String) -> Result<()> {
    Ok(())
}
async fn accept(c: &Config, value: &Value, t: &mut Tokens) -> Result<()> {
    let u = &value["user"];
    let access = u["accessToken"].as_str().or_else(|| value["accessToken"].as_str()).unwrap_or_default();
    let refresh = u["refreshToken"].as_str().or_else(|| value["refreshToken"].as_str()).unwrap_or_default();
    ensure!(!access.is_empty() && !refresh.is_empty(), "Server did not return JWT credentials. Sign in again.");
    store_refresh(c, refresh.to_owned()).await?;
    t.access = access.to_owned();
    t.refresh = refresh.to_owned();
    t.account = c.connection.clone();
    Ok(())
}
pub async fn login(server: String, username: String, password: String) -> Result<Config> {
    let server = server_url(&server)?;
    let previous = storage::config();
    let mut c = if previous.server == server && previous.username == username {
        previous
    } else {
        Config { connection: uuid::Uuid::new_v4().to_string(), ..Config::fresh() }
    };
    c.server = server;
    c.username = username.clone();
    let res = http()
        .post(format!("{}login", c.server))
        .header("x-return-tokens", "true")
        .json(&json!({"username":username,"password":password}))
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Couldn't reach Audiobookshelf."))?;
    if matches!(res.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        bail!("The username or password was not accepted. Check them and try again.");
    }
    let v = checked_json(res).await?;
    c.user = v["user"]["id"].as_str().unwrap_or_default().to_owned();
    ensure!(!c.user.is_empty(), "The server returned no account.");
    let mut t = tokens().lock().await;
    accept(&c, &v, &mut t).await?;
    c.connected = true;
    Ok(c)
}
async fn refresh(c: &Config, t: &mut Tokens) -> Result<()> {
    if t.account != c.connection || t.refresh.is_empty() {
        let id = c.connection.clone();
        t.refresh = tokio::task::spawn_blocking(move || {
            nyx_sync::lookup_secret("nyx-music", &id, "audiobookshelf").map(|s| s.expose().to_owned())
        })
        .await?
        .map_err(|_| AuthRequired("Unlock your desktop keyring, then sign in again to reconnect."))?;
        t.account = c.connection.clone();
    }
    let res = http()
        .post(format!("{}auth/refresh", c.server))
        .header("x-refresh-token", &t.refresh)
        .header("x-return-tokens", "true")
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Couldn't refresh the connection."))?;
    accept(c, &checked_json(res).await?, t).await
}
pub async fn request(c: &Config, method: Method, path: &str, body: Option<&Value>) -> Result<reqwest::Response> {
    ensure!(c.connected, "Connect to Audiobookshelf in Settings.");
    // Serialize refresh/rotation and request creation, not response streaming.
    ensure!(c.connected, "Connect to Audiobookshelf in Settings.");
    let mut t = tokens().lock().await;
    if t.account != c.connection || t.access.is_empty() {
        refresh(c, &mut t).await?;
    }
    let url = format!("{}{}", c.server, path.trim_start_matches('/'));
    let send = |access: &str| {
        let mut r = http().request(method.clone(), &url).bearer_auth(access).timeout(std::time::Duration::from_secs(30));
        if let Some(b) = body {
            r = r.json(b);
        }
        r
    };
    let mut res = send(&t.access)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("Audiobookshelf is unavailable. Your offline media is still available."))?;
    if res.status() == StatusCode::UNAUTHORIZED {
        refresh(c, &mut t).await?;
        res = send(&t.access).send().await.map_err(|_| anyhow::anyhow!("Couldn't reconnect to Audiobookshelf."))?;
    }
    Ok(res)
}
pub async fn range(c: &Config, path: &str, offset: u64, validator: &str) -> Result<reqwest::Response> {
    ensure!(c.connected, "Connect to Audiobookshelf in Settings.");
    let mut t = tokens().lock().await;
    if t.account != c.connection || t.access.is_empty() {
        refresh(c, &mut t).await?;
    }
    let send = |access: &str| {
        let mut r = http().get(format!("{}{}", c.server, path)).bearer_auth(access);
        if offset > 0 {
            r = r.header("Range", format!("bytes={offset}-")).header("If-Range", validator);
        }
        r
    };
    let mut r = send(&t.access).send().await.map_err(|_| anyhow::anyhow!("Couldn't start the download."))?;
    if r.status() == StatusCode::UNAUTHORIZED {
        refresh(c, &mut t).await?;
        r = send(&t.access).send().await.map_err(|_| anyhow::anyhow!("Couldn't restart the download."))?;
    }
    Ok(r)
}
pub async fn checked_json(res: reqwest::Response) -> Result<Value> {
    let mut res = checked(res)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|_| anyhow::anyhow!("Audiobookshelf response was interrupted."))? {
        ensure!(bytes.len() + chunk.len() <= 32 * 1024 * 1024, "Server response is too large.");
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Unexpected Audiobookshelf response."))
}
pub fn checked(res: reqwest::Response) -> Result<reqwest::Response> {
    match res.status() {
        s if s.is_success() => Ok(res),
        StatusCode::UNAUTHORIZED => Err(AuthRequired("Your session has expired. Sign in again to reconnect.").into()),
        StatusCode::FORBIDDEN => bail!("Your account does not have permission for this action."),
        s => bail!("Audiobookshelf returned HTTP {}.", s.as_u16()),
    }
}
pub async fn json(c: &Config, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
    checked_json(request(c, method, path, body).await?).await
}
pub async fn progress(c: &Config, id: &MediaId) -> Result<ServerProgress> {
    let r = request(c, Method::GET, &id.progress_path(), None).await?;
    if r.status() == StatusCode::NOT_FOUND {
        return Ok(ServerProgress::default());
    }
    Ok(serde_json::from_value(checked_json(r).await?)?)
}
pub async fn detail(c: &Config, id: &MediaId) -> Result<Book> {
    let v = json(c, Method::GET, &format!("api/items/{}?expanded=1", encode(&id.item)), None).await?;
    let b = Book::parse(&v, c);
    storage::put("book", &b.id.key(), &b)?;
    if let Some(ep) = &id.episode {
        b.episodes
            .into_iter()
            .find(|b| b.id.episode.as_ref() == Some(ep))
            .ok_or_else(|| anyhow::anyhow!("Episode is no longer available."))
    } else {
        Ok(b)
    }
}
pub async fn logout(c: &Config) -> Result<()> {
    let mut t = tokens().lock().await;
    if t.account == c.connection && !t.refresh.is_empty() {
        let _ = http()
            .post(format!("{}logout", c.server))
            .header("x-refresh-token", &t.refresh)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;
    }
    *t = Tokens::default();
    let id = c.connection.clone();
    tokio::task::spawn_blocking(move || nyx_sync::secret_command("clear", "nyx-music", &id, "audiobookshelf").output()).await??;
    Ok(())
}
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static R: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    R.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("Audiobookshelf runtime")
    })
}
pub fn run<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T>> + Send + 'static,
    done: impl FnOnce(Result<T>) + 'static,
) {
    let (tx, rx) = async_channel::bounded(1);
    runtime().spawn(async move {
        let _ = tx.send(future.await).await;
    });
    gtk::glib::spawn_future_local(async move {
        if let Ok(r) = rx.recv().await {
            if let Err(error) = &r
                && error.is::<AuthRequired>()
            {
                super::authentication_required(error.to_string());
            }
            done(r);
        }
    });
}
pub fn shared_lock() -> Arc<Mutex<()>> {
    static LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(Mutex::new(()))).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base_paths_and_credentials() {
        assert_eq!(server_url("https://example.test/abs").unwrap(), "https://example.test/abs/");
        assert!(server_url("https://user:secret@example.test").is_err());
        assert!(server_url("file:///tmp").is_err());
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn expired_access_token_rotates_once_and_retries_without_url_credentials() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&bytes).to_lowercase();
                let first = headers.lines().next().unwrap();
                assert!(!first.contains("token"));
                let (status, body) = match index {
                    0 => {
                        assert!(first.contains("/abs/api/libraries"));
                        assert!(headers.contains("authorization: bearer expired"));
                        ("401 Unauthorized", "{}")
                    }
                    1 => {
                        assert!(first.contains("/abs/auth/refresh"));
                        assert!(headers.contains("x-refresh-token: refresh-old"));
                        ("200 OK", r#"{"user":{"accessToken":"fresh","refreshToken":"refresh-new"}}"#)
                    }
                    _ => {
                        assert!(headers.contains("authorization: bearer fresh"));
                        ("200 OK", r#"{"libraries":[]}"#)
                    }
                };
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        runtime().block_on(async {
            let c = Config {
                connection: "transport-test".into(),
                server: format!("http://{addr}/abs/"),
                connected: true,
                ..Config::fresh()
            };
            {
                let mut t = tokens().lock().await;
                t.account = c.connection.clone();
                t.access = "expired".into();
                t.refresh = "refresh-old".into();
            }
            let v = json(&c, Method::GET, "api/libraries", None).await.unwrap();
            assert_eq!(v["libraries"], json!([]));
            let mut t = tokens().lock().await;
            assert_eq!(t.refresh, "refresh-new");
            *t = Tokens::default();
        });
        server.join().unwrap();
    }
}
