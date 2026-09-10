use crate::engine::{self, Config, Event, Extension, Plan};
use anyhow::{Context, Result, bail};
use reqwest::{
    StatusCode,
    blocking::{Client, RequestBuilder},
    header::{HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

pub const DEFAULT_URL: &str = "https://cleandl.chevybeef.com/api/v1";
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub url: String,
    #[serde(skip)]
    pub key: String,
    pub protected_key: Vec<u8>,
    pub source: String,
    pub destination: String,
    pub server_folders: bool,
    pub trial: Option<bool>,
    pub debug: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            url: DEFAULT_URL.into(),
            key: String::new(),
            protected_key: Vec::new(),
            source: String::new(),
            destination: String::new(),
            server_folders: true,
            trial: None,
            debug: false,
        }
    }
}
pub fn data_dir() -> Result<PathBuf> {
    Ok(dirs_next::data_local_dir()
        .context("Local AppData is unavailable")?
        .join("CleanDLWin"))
}
pub fn load_preferences(dir: &Path) -> Result<Preferences> {
    let path = dir.join("settings.json");
    if !path.exists() {
        return Ok(Preferences::default());
    }
    let mut prefs: Preferences = serde_json::from_slice(&fs::read(path)?)?;
    if !prefs.protected_key.is_empty() {
        prefs.key = String::from_utf8(crate::platform::unprotect(&prefs.protected_key)?)?;
    }
    Ok(prefs)
}
pub fn save_preferences(dir: &Path, prefs: &Preferences) -> Result<()> {
    let mut saved = prefs.clone();
    saved.url = normalize_url(&saved.url)?;
    saved.protected_key = if saved.key.is_empty() {
        Vec::new()
    } else {
        crate::platform::protect(saved.key.as_bytes())?
    };
    write_json(&dir.join("settings.json"), &saved)
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing settings directory")?)?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut file = fs::File::create(&temp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    drop(file);
    crate::platform::replace_file(&temp, path)
}
pub fn cli_import() -> Result<Option<Preferences>> {
    let Some(local) = dirs_next::data_local_dir() else {
        return Ok(None);
    };
    let dir = local.join("cleandl/Logs");
    if !dir.join("cleandl.key").exists() {
        return Ok(None);
    }
    let key = fs::read_to_string(dir.join("cleandl.key"))?
        .trim()
        .to_owned();
    let url = fs::read_to_string(dir.join("cleandl.url")).unwrap_or_else(|_| DEFAULT_URL.into());
    Ok(Some(Preferences {
        url: normalize_url(&url)?,
        key,
        ..Preferences::default()
    }))
}
pub fn normalize_url(raw: &str) -> Result<String> {
    let raw = raw.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(raw).context("Enter a valid server URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.path().ends_with("/api/v1")
    {
        bail!(
            "Server URL must be an HTTP(S) API root ending in /api/v1, without credentials, query or fragment"
        );
    }
    Ok(raw.into())
}
pub fn website(url: &str) -> String {
    format!(
        "{}/app",
        url.trim_end_matches('/').trim_end_matches("/api/v1")
    )
}
pub fn login(url: &str) -> String {
    format!(
        "{}/login",
        url.trim_end_matches('/').trim_end_matches("/api/v1")
    )
}

#[derive(Clone, Serialize, Deserialize)]
struct GlobalRule {
    action: String,
    regex: String,
    #[serde(default)]
    is_active: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Cache {
    schema: u32,
    url: String,
    fingerprint: String,
    version: i64,
    settings: HashMap<String, String>,
    globals: Vec<GlobalRule>,
    extensions: Vec<Extension>,
    resolved: HashSet<String>,
}
#[derive(Deserialize)]
struct Version {
    updated_at: i64,
}
#[derive(Deserialize)]
struct Setting {
    key: String,
    value: String,
}
#[derive(Deserialize)]
struct Account {
    username: String,
}
#[derive(Debug)]
struct Authentication;
impl std::fmt::Display for Authentication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Authentication failed. Check your API key in Settings.")
    }
}
impl std::error::Error for Authentication {}

fn client(prefs: &Preferences) -> Result<Client> {
    if prefs.key.trim().is_empty() {
        bail!("Add your API key in Settings, or use Help > Demo.");
    }
    let mut headers = HeaderMap::new();
    let mut key =
        HeaderValue::from_str(prefs.key.trim()).context("API key contains invalid characters")?;
    key.set_sensitive(true);
    headers.insert("X-API-Key", key);
    Ok(Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}
fn request<T: DeserializeOwned>(request: RequestBuilder, cancel: &AtomicBool) -> Result<T> {
    engine::cancelled(cancel)?;
    let response = request.send()?;
    engine::cancelled(cancel)?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Err(Authentication.into());
    }
    if !response.status().is_success() {
        bail!("Server returned HTTP {}", response.status());
    }
    Ok(response.json()?)
}
pub fn test_connection(prefs: &Preferences, cancel: &AtomicBool) -> Result<String> {
    let url = normalize_url(&prefs.url)?;
    let client = client(prefs)?;
    let account: Account = request(client.get(format!("{url}/account/me")), cancel)?;
    Ok(format!("Connected as {}", account.username))
}
fn folders(
    prefs: &Preferences,
    settings: &HashMap<String, String>,
    demo: bool,
) -> Result<(PathBuf, PathBuf)> {
    let home = dirs_next::home_dir().context("Home directory is unavailable")?;
    let source = if !prefs.server_folders && !prefs.source.trim().is_empty() {
        prefs.source.trim()
    } else {
        settings
            .get("source_folder")
            .map_or("Downloads", String::as_str)
    };
    let source = if Path::new(source).is_absolute() {
        PathBuf::from(source)
    } else {
        home.join(source)
    };
    let destination = if !prefs.server_folders && !prefs.destination.trim().is_empty() {
        prefs.destination.trim()
    } else {
        settings.get("windows_prefix").map_or(
            if demo { r"\\truenas\downloads" } else { "" },
            String::as_str,
        )
    };
    let destination = if destination.is_empty() {
        home
    } else {
        PathBuf::from(destination)
    };
    if !destination.is_absolute() {
        bail!("Destination must be an absolute local or UNC path");
    }
    Ok((source, destination))
}
fn cache_config(cache: &Cache) -> Config {
    Config {
        settings: cache.settings.clone(),
        ignore_rules: cache
            .globals
            .iter()
            .filter(|r| r.is_active && r.action == "ignore")
            .map(|r| r.regex.clone())
            .collect(),
        delete_rules: cache
            .globals
            .iter()
            .filter(|r| r.is_active && r.action == "delete")
            .map(|r| r.regex.clone())
            .collect(),
        extensions: cache.extensions.clone(),
    }
}
fn load_cache(dir: &Path, url: &str, fingerprint: &str) -> Option<Cache> {
    let cache: Cache = serde_json::from_slice(&fs::read(dir.join("cache.json")).ok()?).ok()?;
    (cache.schema == 1 && cache.url == url && cache.fingerprint == fingerprint).then_some(cache)
}

/// Display the active folder mode without turning server values into overrides.
pub fn folder_display(prefs: &Preferences, dir: &Path) -> (String, String) {
    if !prefs.server_folders {
        return (prefs.source.clone(), prefs.destination.clone());
    }
    let cached_folders = normalize_url(&prefs.url)
        .ok()
        .and_then(|url| {
            load_cache(
                dir,
                &url,
                &format!("{:x}", Sha256::digest(prefs.key.trim().as_bytes())),
            )
        })
        .and_then(|cache| folders(prefs, &cache.settings, false).ok());
    match cached_folders {
        Some((source, destination)) => (
            source.display().to_string(),
            destination.display().to_string(),
        ),
        None => (
            "From server — click Preview to load".into(),
            "From server — click Preview to load".into(),
        ),
    }
}
pub fn prepare(
    prefs: &Preferences,
    dir: &Path,
    demo: bool,
    cancel: &AtomicBool,
    emit: &mut impl FnMut(Event),
) -> Result<Plan> {
    if demo {
        emit(Event::Phase("Scanning with offline demo rules…".into()));
        let config = engine::demo_config();
        let (source, destination) = folders(prefs, &config.settings, true)?;
        let paths = engine::scan(&source, cancel)?;
        let mut plan = engine::preview(&config, paths, source, destination, cancel)?;
        plan.demo = true;
        plan.trial = true;
        return Ok(plan);
    }
    let url = normalize_url(&prefs.url)?;
    let client = client(prefs)?;
    let fingerprint = format!("{:x}", Sha256::digest(prefs.key.trim().as_bytes()));
    let cached = load_cache(dir, &url, &fingerprint);
    emit(Event::Phase("Checking server configuration…".into()));
    let online = (|| -> Result<(Cache, Vec<PathBuf>)> {
        let version: Version = request(client.get(format!("{url}/configuration-version")), cancel)?;
        let mut cache = if let Some(cache) =
            cached.as_ref().filter(|c| c.version == version.updated_at)
        {
            cache.clone()
        } else {
            let settings: Vec<Setting> = request(client.get(format!("{url}/settings")), cancel)?;
            let globals = request(client.get(format!("{url}/global-rules")), cancel)?;
            Cache {
                schema: 1,
                url: url.clone(),
                fingerprint,
                version: version.updated_at,
                settings: settings.into_iter().map(|s| (s.key, s.value)).collect(),
                globals,
                extensions: Vec::new(),
                resolved: HashSet::new(),
            }
        };
        let (source, _) = folders(prefs, &cache.settings, false)?;
        emit(Event::Phase(format!("Scanning {}…", source.display())));
        let paths = engine::scan(&source, cancel)?;
        let names = engine::extension_names(&paths);
        let mut missing: Vec<_> = names.difference(&cache.resolved).cloned().collect();
        missing.sort();
        if !missing.is_empty() {
            emit(Event::Phase("Resolving extension rules…".into()));
            let extensions: Vec<Extension> = request(
                client
                    .post(format!("{url}/extensions/resolve"))
                    .json(&serde_json::json!({"names": missing})),
                cancel,
            )?;
            cache.extensions.extend(extensions);
            cache.resolved.extend(missing);
        }
        Ok((cache, paths))
    })();
    engine::cancelled(cancel)?;
    let (cache, paths, offline, warning) = match online {
        Ok((cache, paths)) => {
            let warning = write_json(&dir.join("cache.json"), &cache)
                .err()
                .map(|e| format!("Could not save offline cache: {e:#}"));
            (cache, paths, false, warning)
        }
        Err(error) if error.downcast_ref::<Authentication>().is_some() => return Err(error),
        Err(error) => {
            let cache = cached.context(format!(
                "Server unavailable and no matching cached settings exist: {error:#}"
            ))?;
            let (source, _) = folders(prefs, &cache.settings, false)?;
            let paths = engine::scan(&source, cancel)?;
            (
                cache,
                paths,
                true,
                Some(format!("Using cached configuration: {error:#}")),
            )
        }
    };
    let (source, destination) = folders(prefs, &cache.settings, false)?;
    let mut plan = engine::preview(&cache_config(&cache), paths, source, destination, cancel)?;
    plan.cached = offline;
    plan.trial = prefs.trial.unwrap_or_else(|| {
        cache
            .settings
            .get("trial_run")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    });
    if let Some(warning) = warning {
        plan.warnings.push(warning);
    }
    Ok(plan)
}
pub fn log(dir: &Path, message: &str) {
    let _ = fs::create_dir_all(dir);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("cleandlwin.log"))
    {
        let _ = writeln!(
            f,
            "{} {message}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
    }
}

pub fn log_preview_items(dir: &Path, plan: &Plan, debug: bool) {
    for item in &plan.items {
        if debug || item.status.starts_with("Cannot plan:") {
            log(
                dir,
                &format!(
                    "Preview {}: {} — {}",
                    item.source.display(),
                    item.action.label(),
                    item.status
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    #[test]
    fn preview_failures_include_operation_and_are_logged_without_debug() {
        use std::os::windows::fs::OpenOptionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        fs::create_dir(&source).unwrap();
        let path = source.join("locked.txt");
        fs::write(&path, b"test").unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let cancel = AtomicBool::new(false);
        let plan = engine::preview(
            &engine::demo_config(),
            engine::scan(&source, &cancel).unwrap(),
            source.clone(),
            scratch.path().join("destination"),
            &cancel,
        )
        .unwrap();
        assert!(plan.items[0].status.contains("Cannot inspect source"));
        assert!(plan.items[0].status.contains("os error 32"));
        log_preview_items(scratch.path(), &plan, false);
        let logged = fs::read_to_string(scratch.path().join("cleandlwin.log")).unwrap();
        assert!(logged.contains("locked.txt"));
        assert!(logged.contains("Cannot inspect source"));
        assert!(logged.contains("os error 32"));
        drop(locked);
        let plan = engine::preview(
            &engine::demo_config(),
            engine::scan(&source, &cancel).unwrap(),
            source,
            scratch.path().join("destination"),
            &cancel,
        )
        .unwrap();
        assert_eq!(plan.counts(), (1, 0));
    }
    fn server(responses: Vec<(&'static str, String)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/v1", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = socket.read(&mut buffer).unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|l| {
                                l.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                requests.push(String::from_utf8_lossy(&bytes).into_owned());
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        (url, handle)
    }
    #[test]
    fn server_folder_display_uses_current_cache_not_old_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let mut prefs = Preferences {
            key: "key".into(),
            source: r"C:\OldDownloads".into(),
            destination: r"\\old-server\unreachable".into(),
            server_folders: true,
            ..Preferences::default()
        };
        let cache = Cache {
            schema: 1,
            url: prefs.url.clone(),
            fingerprint: format!("{:x}", Sha256::digest(b"key")),
            version: 2,
            settings: HashMap::from([
                ("source_folder".into(), r"C:\NewDownloads".into()),
                ("windows_prefix".into(), r"C:\NewDestination".into()),
            ]),
            globals: Vec::new(),
            extensions: Vec::new(),
            resolved: HashSet::new(),
        };
        write_json(&temp.path().join("cache.json"), &cache).unwrap();
        assert_eq!(
            folder_display(&prefs, temp.path()),
            (r"C:\NewDownloads".into(), r"C:\NewDestination".into())
        );
        prefs.server_folders = false;
        assert_eq!(
            folder_display(&prefs, temp.path()),
            (prefs.source.clone(), prefs.destination.clone())
        );
        prefs.server_folders = true;
        prefs.key = "different-account".into();
        let (_, destination) = folder_display(&prefs, temp.path());
        assert!(destination.starts_with("From server"));
        assert!(!destination.contains("NewDestination"));
        assert!(!destination.contains("old-server"));
    }
    #[test]
    fn urls_are_validated() {
        assert_eq!(
            normalize_url(" https://example.test/api/v1/ ").unwrap(),
            "https://example.test/api/v1"
        );
        for url in [
            "file:///api/v1",
            "https://example.test",
            "https://u:p@example.test/api/v1",
            "https://example.test/api/v1?q=secret",
        ] {
            assert!(normalize_url(url).is_err());
        }
    }
    #[test]
    fn preferences_encrypt_key() {
        let t = tempfile::tempdir().unwrap();
        let p = Preferences {
            key: "secret-test-key".into(),
            ..Preferences::default()
        };
        save_preferences(t.path(), &p).unwrap();
        assert!(
            !fs::read_to_string(t.path().join("settings.json"))
                .unwrap()
                .contains("secret-test-key")
        );
        assert_eq!(load_preferences(t.path()).unwrap().key, p.key);
    }
    #[test]
    fn fetch_cache_offline_and_key_isolation() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), b"a").unwrap();
        let (url, handle) = server(vec![
            ("200 OK", r#"{"updated_at":1}"#.into()),
            ("200 OK", "[]".into()),
            ("200 OK", "[]".into()),
            (
                "200 OK",
                r#"[{"name":"txt","default_destination":["Documents"]}]"#.into(),
            ),
        ]);
        let mut prefs = Preferences {
            url,
            key: "key".into(),
            source: src.display().to_string(),
            destination: t.path().join("dst").display().to_string(),
            server_folders: false,
            ..Preferences::default()
        };
        let cancel = AtomicBool::new(false);
        let p = prepare(&prefs, t.path(), false, &cancel, &mut |_| {}).unwrap();
        assert!(!p.cached);
        assert_eq!(p.counts(), (1, 0));
        let requests = handle.join().unwrap();
        assert!(requests[3].contains(r#""names":["txt"]"#));
        assert!(
            prepare(&prefs, t.path(), false, &cancel, &mut |_| {})
                .unwrap()
                .cached
        );
        prefs.key = "other".into();
        assert!(prepare(&prefs, t.path(), false, &cancel, &mut |_| {}).is_err());
    }
    #[test]
    fn version_reuse_and_refresh_drop_stale_rules() {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), b"a").unwrap();
        let (url, handle) = server(vec![
            ("200 OK", r#"{"updated_at":1}"#.into()),
            ("200 OK", "[]".into()),
            ("200 OK", "[]".into()),
            (
                "200 OK",
                r#"[{"name":"txt","default_destination":["Old"]}]"#.into(),
            ),
            ("200 OK", r#"{"updated_at":1}"#.into()),
            ("200 OK", r#"{"updated_at":2}"#.into()),
            ("200 OK", r#"[{"key":"trial_run","value":"true"}]"#.into()),
            ("200 OK", "[]".into()),
            (
                "200 OK",
                r#"[{"name":"txt","default_destination":["New"]}]"#.into(),
            ),
        ]);
        let prefs = Preferences {
            url,
            key: "key".into(),
            source: src.display().to_string(),
            destination: t.path().join("dst").display().to_string(),
            server_folders: false,
            ..Preferences::default()
        };
        let cancel = AtomicBool::new(false);
        for _ in 0..2 {
            let p = prepare(&prefs, t.path(), false, &cancel, &mut |_| {}).unwrap();
            assert!(
                matches!(&p.items[0].action, engine::Action::Move(p) if p.ends_with("Old/a.txt"))
            );
        }
        let p = prepare(&prefs, t.path(), false, &cancel, &mut |_| {}).unwrap();
        assert!(p.trial);
        assert!(matches!(&p.items[0].action, engine::Action::Move(p) if p.ends_with("New/a.txt")));
        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 9);
        assert!(requests[4].starts_with("GET /api/v1/configuration-version"));
    }
    #[test]
    fn authentication_never_uses_cache() {
        let t = tempfile::tempdir().unwrap();
        let (url, handle) = server(vec![("401 Unauthorized", "{}".into())]);
        let cache = Cache {
            schema: 1,
            url: url.clone(),
            fingerprint: format!("{:x}", Sha256::digest(b"key")),
            version: 1,
            settings: HashMap::new(),
            globals: Vec::new(),
            extensions: Vec::new(),
            resolved: HashSet::new(),
        };
        write_json(&t.path().join("cache.json"), &cache).unwrap();
        let prefs = Preferences {
            url,
            key: "key".into(),
            ..Preferences::default()
        };
        let e = prepare(
            &prefs,
            t.path(),
            false,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(e.downcast_ref::<Authentication>().is_some());
        handle.join().unwrap();
    }
}
