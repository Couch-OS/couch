//! Paired-user package management. Repository trust is scoped to each feed;
//! browsing authenticates APKINDEX with the same native verifier as installation.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

const OFFICIAL_KEY: &str = include_str!("official.rsa.pub");
const MAX_INDEX: u64 = 4 * 1024 * 1024;
const MAX_EXPANDED_INDEX: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub id: String,
    pub name: String,
    pub url: String,
    pub fingerprint: String,
    pub official: bool,
    pub trusted: bool,
    #[serde(skip_serializing, default)]
    public_key: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRepository {
    pub id: String,
    pub name: String,
    pub url: String,
    pub public_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Available {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub repository: String,
    pub apk_version: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Operation {
    pub id: String,
    pub state: String,
    pub phase: String,
    pub message: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub repository: Option<String>,
    #[serde(default)]
    pub preserve_connection_config: bool,
}
#[derive(Default)]
struct Cache {
    available: Vec<Available>,
    error: Option<String>,
    refreshed: bool,
}
struct Inner {
    store: Store,
    directory: PathBuf,
    busy: AtomicBool,
    operation: Mutex<Option<Operation>>,
    pending: Mutex<BTreeMap<String, Repository>>,
    cache: Mutex<Cache>,
}
#[derive(Clone)]
pub struct Manager(Arc<Inner>);

impl Manager {
    pub fn new(store: Store) -> Self {
        let directory = store.root.join("management");
        Self(Arc::new(Inner {
            store,
            directory,
            busy: AtomicBool::new(false),
            operation: Mutex::new(None),
            pending: Mutex::new(BTreeMap::new()),
            cache: Mutex::new(Cache::default()),
        }))
    }
    fn layout(&self) -> Result<()> {
        self.0.store.layout()?;
        for path in [&self.0.directory, &self.0.directory.join("keys")] {
            fs::create_dir_all(path).map_err(|e| io("create repository settings", e))?;
            let meta =
                fs::symlink_metadata(path).map_err(|e| io("inspect repository settings", e))?;
            if !meta.is_dir() || meta.permissions().mode() & 0o022 != 0 {
                return Err(err(
                    "repository settings must be an owner-writable directory, not a link",
                ));
            }
        }
        Ok(())
    }
    fn official() -> Vec<Repository> {
        ["stable", "preview"]
            .into_iter()
            .map(|channel| Repository {
                id: format!("official-{channel}"),
                name: format!("Couch {channel}"),
                url: format!("https://dangerouslaser.github.io/couch-integrations/{channel}"),
                fingerprint: fingerprint(OFFICIAL_KEY),
                public_key: OFFICIAL_KEY.into(),
                official: true,
                trusted: true,
            })
            .collect()
    }
    pub fn repositories(&self) -> Result<Vec<Repository>> {
        self.layout()?;
        let path = self.0.directory.join("repositories.json");
        // The key is persisted in a separate field because public API responses
        // never need its full PEM; deserialize persisted records explicitly.
        let custom: Vec<NewRepository> = match fs::read(path) {
            Ok(bytes) if bytes.len() <= 128 * 1024 => serde_json::from_slice(&bytes)
                .map_err(|_| err("repository settings are invalid"))?,
            Ok(_) => return Err(err("repository settings exceed their size limit")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(io("read repository settings", e)),
        };
        let mut result = Self::official();
        for item in custom {
            let mut repository = validate_repository(item)?;
            if result.iter().any(|r| r.id == repository.id) {
                return Err(err("duplicate repository identity"));
            }
            repository.trusted = true;
            result.push(repository);
        }
        if result.len() > 18 {
            return Err(err("too many repositories"));
        }
        Ok(result)
    }
    fn save_repositories(&self, repositories: &[Repository]) -> Result<()> {
        let values: Vec<_> = repositories
            .iter()
            .filter(|r| !r.official)
            .map(|r| {
                serde_json::json!({
                    "id":r.id,"name":r.name,"url":r.url,"public_key":r.public_key
                })
            })
            .collect();
        atomic_write(
            &self.0.directory.join("repositories.json"),
            &serde_json::to_vec(&values).unwrap(),
        )
    }
    pub fn stage_repository(&self, input: NewRepository) -> Result<Repository> {
        let repository = validate_repository(input)?;
        let repositories = self.repositories()?;
        if repositories.len() >= 18 {
            return Err(err("remove a repository before adding another"));
        }
        if repositories
            .iter()
            .any(|r| r.id == repository.id || r.url == repository.url)
        {
            return Err(err(
                "repository is already registered; remove it before changing its key",
            ));
        }
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.len() >= 16 {
            pending.clear();
        }
        pending.insert(repository.id.clone(), repository.clone());
        Ok(repository)
    }
    pub fn confirm_repository(&self, id: &str, fingerprint: &str) -> Result<Repository> {
        let _guard = self.begin()?;
        self.layout()?;
        let _lock = Lock::acquire_wait(
            self.0.directory.join(".lock"),
            MUTATION_LOCK_WAIT,
            libc::LOCK_EX,
        )?;
        let mut pending = self.0.pending.lock().unwrap_or_else(|e| e.into_inner());
        let mut repository = pending
            .get(id)
            .cloned()
            .ok_or_else(|| err("review the repository key again before confirming"))?;
        if repository.fingerprint != fingerprint {
            return Err(err("public-key fingerprint did not match the reviewed key"));
        }
        let mut repositories = self.repositories()?;
        if repositories.len() >= 18
            || repositories
                .iter()
                .any(|r| r.id == id || r.url == repository.url)
        {
            return Err(err("repository selection changed; review it again"));
        }
        repository.trusted = true;
        repositories.push(repository.clone());
        self.save_repositories(&repositories)?;
        pending.remove(id);
        Ok(repository)
    }
    pub fn remove_repository(&self, id: &str) -> Result<()> {
        let _guard = self.begin()?;
        self.layout()?;
        let _lock = Lock::acquire_wait(
            self.0.directory.join(".lock"),
            MUTATION_LOCK_WAIT,
            libc::LOCK_EX,
        )?;
        let mut repositories = self.repositories()?;
        let existing = repositories
            .iter()
            .find(|r| r.id == id)
            .ok_or_else(|| err("repository not found"))?;
        if existing.official {
            return Err(err("official repositories cannot be removed"));
        }
        repositories.retain(|r| r.id != id);
        self.save_repositories(&repositories)?;
        self.0
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .available
            .retain(|p| p.repository != id);
        // Existing packages and connection configuration intentionally survive.
        Ok(())
    }
    fn keys(&self, repository: &Repository) -> Result<PathBuf> {
        let path = self
            .0
            .directory
            .join("keys")
            .join(format!("{}-{}", repository.id, repository.fingerprint));
        fs::create_dir_all(&path).map_err(|e| io("create repository trust directory", e))?;
        let meta =
            fs::symlink_metadata(&path).map_err(|e| io("inspect repository trust directory", e))?;
        if !meta.is_dir() || meta.permissions().mode() & 0o022 != 0 {
            return Err(err("invalid repository trust directory"));
        }
        // apk matches signature key filenames. Custom feeds may choose any key
        // filename; use apk's --keys-dir scan through a matching supplied name.
        // The API accepts a PEM and derives its stable file name from index
        // signature metadata only after checking that metadata is a basename.
        atomic_write(
            &path.join("couch-integrations.rsa.pub"),
            repository.public_key.as_bytes(),
        )?;
        Ok(path)
    }
    pub fn catalog(&self, configured: &[String]) -> Result<serde_json::Value> {
        let repositories = self.repositories()?;
        let _lease = self.0.store.read_lease()?;
        let manifests = self.0.store.list()?;
        let origins = self.origins()?;
        let cache = self.0.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut installed = Vec::new();
        for manifest in manifests {
            let state = self.0.store.selection(&manifest.id)?;
            let repo = origins.get(&manifest.id).cloned();
            let available = cache
                .available
                .iter()
                .find(|p| p.id == manifest.id && repo.as_deref() == Some(&p.repository));
            let update = match available {
                Some(p) if self.newer(&p.version, &manifest.version)? => Some(&p.version),
                _ => None,
            };
            let fallback = state
                .active
                .as_ref()
                .is_none_or(|slot| self.0.store.slot(&manifest.id, slot).is_err());
            let status = if fallback {
                serde_json::json!({"kind":"fallback","message":"The selected package failed validation. The verified previous version is running."})
            } else {
                serde_json::json!({"kind":"installed"})
            };
            installed.push(serde_json::json!({
                "id":manifest.id,"name":manifest.label,"version":manifest.version,
                "available_version":update,
                "status":status, "repository":repo,
                "connection_configured":configured.contains(&manifest.id),
                "can_rollback":state.previous.as_ref().is_some_and(|slot| slot.version != manifest.version && self.0.store.slot(&manifest.id,slot).is_ok()),
            }));
        }
        for entry in fs::read_dir(self.0.store.root.join("state"))
            .map_err(|e| io("read package selections", e))?
        {
            let id = entry
                .map_err(|e| io("read package selection", e))?
                .file_name()
                .to_string_lossy()
                .into_owned();
            if !valid_component(&id) || installed.iter().any(|p| p["id"] == id) {
                continue;
            }
            let selection = self.0.store.selection(&id);
            if selection.as_ref().is_ok_and(|s| s.active.is_none()) {
                continue;
            }
            installed.push(serde_json::json!({"id":id,"name":id,"version":"", "available_version":null,
                "status":{"kind":"invalid","message":"The installed package failed validation. Reinstall it from a trusted repository."},
                "connection_configured":configured.contains(&id),"can_rollback":false,"repository":origins.get(&id)}));
        }
        // Missing packages remain visible when a connection still uses them.
        for id in configured {
            if !installed.iter().any(|p| p["id"] == *id) {
                installed.push(serde_json::json!({"id":id,"name":id,"version":"", "available_version":null,
                    "status":{"kind":"missing","message":"Connection settings are retained. Reinstall this integration to use it."},
                    "connection_configured":true,"can_rollback":false,"repository":origins.get(id)}));
            }
        }
        Ok(
            serde_json::json!({"installed":installed,"available":cache.available,"repositories":repositories,
            "catalog_error":cache.error,"refreshed":cache.refreshed,"busy":self.0.busy.load(Ordering::Acquire),
            "operation":self.0.operation.lock().unwrap_or_else(|e| e.into_inner()).clone()}),
        )
    }
    fn origins(&self) -> Result<BTreeMap<String, String>> {
        match fs::read(self.0.directory.join("origins.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| err("package source records are invalid")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(io("read package sources", e)),
        }
    }
    fn begin(&self) -> Result<BusyGuard> {
        self.0
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| busy())?;
        Ok(BusyGuard(self.0.clone()))
    }
    pub fn current_operation(&self) -> Option<Operation> {
        self.0
            .operation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn operation(&self, id: &str) -> Option<Operation> {
        self.0
            .operation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|o| o.id == id)
            .cloned()
    }
    pub fn start(&self, kind: &str, action: Option<Action>) -> Result<String> {
        if !matches!(
            kind,
            "refresh" | "install" | "update" | "remove" | "rollback"
        ) {
            return Err(err("unknown package operation"));
        }
        if kind != "refresh" && action.as_ref().is_none_or(|a| !valid_component(&a.id)) {
            return Err(err("invalid integration identity"));
        }
        if kind == "remove" && !action.as_ref().unwrap().preserve_connection_config {
            return Err(err("removal must preserve connection configuration"));
        }
        let guard = self.begin()?;
        let id = nonce();
        *self.0.operation.lock().unwrap_or_else(|e| e.into_inner()) = Some(Operation {
            id: id.clone(),
            state: "running".into(),
            phase: kind.into(),
            message: match kind {
                "refresh" => "Checking signed repository indexes…",
                "remove" => "Removing the package; connection settings will be retained…",
                "rollback" => "Validating the previous package…",
                _ => "Downloading and validating the selected package…",
            }
            .into(),
        });
        let manager = self.clone();
        let kind = kind.to_owned();
        std::thread::Builder::new().name("integration-packages".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| manager.perform(&kind, action)));
            let result = result.unwrap_or_else(|_| Err(err("package worker stopped unexpectedly; inspect installed state before retrying")));
            let mut operation = manager.0.operation.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(operation) = operation.as_mut() {
                operation.state = if result.is_ok() {"succeeded"} else {"failed"}.into();
                operation.message = result.map(|_| "Operation completed".into()).unwrap_or_else(|e| e.to_string());
            }
            drop(guard);
        }).map_err(|e| io("start package worker", e))?;
        Ok(id)
    }
    fn perform(&self, kind: &str, action: Option<Action>) -> Result<()> {
        self.layout()?;
        let _lock = Lock::acquire_wait(
            self.0.directory.join(".lock"),
            MUTATION_LOCK_WAIT,
            libc::LOCK_EX,
        )?;
        if kind == "refresh" {
            return self.refresh();
        }
        let action = action.ok_or_else(|| err("missing package selection"))?;
        if kind == "remove" {
            return self.0.store.remove(&action.id);
        }
        if kind == "rollback" {
            return self.0.store.rollback(&action.id).map(|_| ());
        }
        let repositories = self.repositories()?;
        let repository = repositories
            .iter()
            .find(|r| Some(&r.id) == action.repository.as_ref())
            .ok_or_else(|| err("choose a trusted repository"))?;
        let selected = self
            .0
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .available
            .iter()
            .find(|p| p.id == action.id && p.repository == repository.id)
            .cloned()
            .ok_or_else(|| err("refresh repositories before selecting this package"))?;
        if kind == "update" {
            let (_, installed) = self.0.store.resolve(&action.id)?;
            if !self.newer(&selected.version, &installed.version)? {
                return Err(err("no newer integration version is available"));
            }
        }
        let keys = self.keys(repository)?;
        // Refresh that source immediately before install. A signed index can
        // change between browsing and clicking; require the reviewed version.
        let current = self.fetch_index(repository, &keys)?;
        if !current
            .iter()
            .any(|p| p.id == selected.id && p.apk_version == selected.apk_version)
        {
            return Err(err(
                "repository package changed; refresh and review the new version",
            ));
        }
        // apk-tools 2.14 fetch accepts a name, not add's name=version syntax.
        // The expected manifest is checked before admission/activation, so a
        // newly published version racing this fetch fails without replacing it.
        let package = format!("couch-integration-{}", selected.id);
        self.0.store.clone().with_keys_dir(keys).fetch_repository(
            &package,
            &repository.url,
            Some((&selected.id, &selected.version)),
        )?;
        let mut origins = self.origins()?;
        origins.insert(action.id, repository.id.clone());
        atomic_write(
            &self.0.directory.join("origins.json"),
            &serde_json::to_vec(&origins).unwrap(),
        )
    }
    fn refresh(&self) -> Result<()> {
        let repositories = self.repositories()?;
        let mut available = Vec::new();
        let mut errors = Vec::new();
        for repository in repositories {
            match self
                .keys(&repository)
                .and_then(|keys| self.fetch_index(&repository, &keys))
            {
                Ok(mut packages) => available.append(&mut packages),
                Err(error) => errors.push(format!("{}: {error}", repository.name)),
            }
        }
        available.sort_by(|a, b| (&a.id, &a.repository).cmp(&(&b.id, &b.repository)));
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        *self.0.cache.lock().unwrap_or_else(|e| e.into_inner()) = Cache {
            available,
            error: error.clone(),
            refreshed: true,
        };
        error.map_or(Ok(()), |e| Err(err(e)))
    }
    fn newer(&self, candidate: &str, installed: &str) -> Result<bool> {
        let output = Command::new(&self.0.store.apk)
            .args(["version", "--test", candidate, installed])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| io("compare package versions", e))?;
        if !output.status.success() || !matches!(output.stdout.as_slice(), b">\n" | b"<\n" | b"=\n")
        {
            return Err(err("apk could not compare package versions"));
        }
        Ok(output.stdout == b">\n")
    }
    fn latest(&self, packages: Vec<Available>) -> Result<Vec<Available>> {
        let mut latest: BTreeMap<String, Available> = BTreeMap::new();
        for package in packages {
            if match latest.get(&package.id) {
                Some(old) => self.newer(&package.apk_version, &old.apk_version)?,
                None => true,
            } {
                latest.insert(package.id.clone(), package);
            }
        }
        Ok(latest.into_values().collect())
    }
    fn fetch_index(&self, repository: &Repository, keys: &Path) -> Result<Vec<Available>> {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .build()
            .new_agent();
        let mut response = agent
            .get(&format!("{}/armv7/APKINDEX.tar.gz", repository.url))
            .call()
            .map_err(|_| err("cannot download repository index over HTTPS"))?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_INDEX + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io("read index", e))?;
        if bytes.len() as u64 > MAX_INDEX {
            return Err(err("repository index exceeds its size limit"));
        }
        let temporary = self.0.directory.join(format!(".index-{}", nonce()));
        fs::create_dir(&temporary).map_err(|e| io("create index verification directory", e))?;
        let result = (|| {
            // The signature selects a filename only, never another key. Every
            // basename below contains precisely the key explicitly trusted by
            // the user for this repository. Inspecting it grants no trust.
            let (key_names, _) = index_members(&bytes)?;
            for name in key_names {
                atomic_write(&keys.join(name), repository.public_key.as_bytes())?;
            }
            let index = temporary.join("APKINDEX.tar.gz");
            fs::write(&index, &bytes).map_err(|e| io("stage repository index", e))?;
            run_bounded(
                Command::new(&self.0.store.apk)
                    .arg("--repositories-file")
                    .arg("/dev/null")
                    .arg("--keys-dir")
                    .arg(keys)
                    .arg("verify")
                    .arg(&index),
                Duration::from_secs(30),
            )?;
            let (_, text) = index_members(&bytes)?;
            self.latest(parse_index(&text, &repository.id)?)
        })();
        let _ = fs::remove_dir_all(&temporary);
        result
    }
}
struct BusyGuard(Arc<Inner>);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

fn fingerprint(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}
fn validate_repository(input: NewRepository) -> Result<Repository> {
    if !valid_component(&input.id)
        || input.id.starts_with("official-")
        || input.name.trim().is_empty()
        || input.name.len() > 100
    {
        return Err(err(
            "use a short repository ID and name; official names are reserved",
        ));
    }
    let url = url::Url::parse(&input.url).map_err(|_| err("invalid repository URL"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || input.url.chars().any(char::is_whitespace)
    {
        return Err(err(
            "repository must be an HTTPS base URL without credentials, query or fragment",
        ));
    }
    let key = input.public_key.replace("\r\n", "\n");
    let key = format!("{}\n", key.trim());
    let inner = key
        .strip_prefix("-----BEGIN PUBLIC KEY-----\n")
        .and_then(|v| v.strip_suffix("\n-----END PUBLIC KEY-----\n"));
    if key.len() > 8192
        || inner.is_none_or(|v| {
            v.len() < 64
                || !v
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\n'))
        })
    {
        return Err(err("paste a PEM public key, never a private key"));
    }
    Ok(Repository {
        id: input.id,
        name: input.name.trim().into(),
        url: url.as_str().trim_end_matches('/').into(),
        fingerprint: fingerprint(&key),
        public_key: key,
        official: false,
        trusted: false,
    })
}
fn index_members(bytes: &[u8]) -> Result<(Vec<String>, String)> {
    let decoder = flate2::read::MultiGzDecoder::new(bytes);
    let mut decoded = Vec::new();
    decoder
        .take(MAX_EXPANDED_INDEX + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| err("invalid compressed repository index"))?;
    if decoded.len() as u64 > MAX_EXPANDED_INDEX {
        return Err(err("expanded repository index is too large"));
    }
    let mut archive = tar::Archive::new(decoded.as_slice());
    archive.set_ignore_zeros(true);
    let mut keys = Vec::new();
    let mut index = None;
    for (number, entry) in archive
        .entries()
        .map_err(|_| err("invalid index archive"))?
        .enumerate()
    {
        if number >= 20 {
            return Err(err("too many repository index members"));
        }
        let mut entry = entry.map_err(|_| err("invalid index member"))?;
        let path = entry
            .path()
            .map_err(|_| err("invalid index member path"))?
            .to_string_lossy()
            .into_owned();
        if !entry.header().entry_type().is_file() {
            return Err(err("index contains a non-file member"));
        }
        if let Some(name) = path.strip_prefix(".SIGN.RSA.") {
            if name.len() > 160
                || !name.ends_with(".pub")
                || name.starts_with('.')
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            {
                return Err(err("invalid repository signing key identity"));
            }
            keys.push(name.to_owned());
        } else if path == "APKINDEX" && index.is_none() {
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|_| err("invalid index text"))?;
            index = Some(text);
        } else if path != "DESCRIPTION" {
            return Err(err("unexpected or duplicate index member"));
        }
    }
    if keys.is_empty() {
        return Err(err("repository index is unsigned"));
    }
    Ok((
        keys,
        index.ok_or_else(|| err("repository has no package index"))?,
    ))
}
fn parse_index(text: &str, repository: &str) -> Result<Vec<Available>> {
    let mut packages = BTreeMap::new();
    for (number, record) in text
        .split("\n\n")
        .filter(|r| !r.trim().is_empty())
        .enumerate()
    {
        if number >= 1024 {
            return Err(err("too many repository packages"));
        }
        let mut fields = BTreeMap::new();
        for line in record.lines() {
            if let Some((key, value)) = line.split_once(':') {
                if fields.insert(key, value).is_some() {
                    return Err(err("duplicate repository package field"));
                }
            }
        }
        let Some(id) = fields
            .get("P")
            .and_then(|p| p.strip_prefix("couch-integration-"))
        else {
            continue;
        };
        let apk_version = fields
            .get("V")
            .ok_or_else(|| err("package has no version"))?;
        if !valid_component(id) || !valid_version(apk_version) || fields.get("A") != Some(&"armv7")
        {
            return Err(err("invalid integration index entry"));
        }
        let (version, revision) = apk_version
            .rsplit_once("-r")
            .ok_or_else(|| err("invalid APK release version"))?;
        if !valid_version(version)
            || revision.is_empty()
            || !revision.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(err("invalid APK release version"));
        }
        let value = Available {
            id: id.into(),
            name: id.into(),
            version: version.into(),
            description: fields.get("T").unwrap_or(&"").chars().take(1024).collect(),
            apk_version: (*apk_version).into(),
            repository: repository.into(),
        };
        // Feeds retain old APKs. APK semantic version comparison picks the
        // newest during install; expose one candidate using the same comparator
        // below in select_latest, not lexical sorting here.
        packages
            .entry(id.to_string())
            .or_insert_with(Vec::new)
            .push(value);
    }
    // Preserve all versions for now; selection is finalized by native apk.
    Ok(packages.into_values().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("couch-management-{}", nonce())))
        }
        fn manager(&self) -> Manager {
            Manager::new(Store::new(self.0.join("integrations")))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn custom() -> NewRepository {
        NewRepository {
            id: "mine".into(),
            name: "My feed".into(),
            url: "https://example.com/feed/".into(),
            public_key: OFFICIAL_KEY.into(),
        }
    }
    #[test]
    fn trust_requires_exact_review_and_survives_restart() {
        let fixture = Fixture::new();
        let manager = fixture.manager();
        let pending = manager.stage_repository(custom()).unwrap();
        assert!(!pending.trusted);
        assert_eq!(manager.repositories().unwrap().len(), 2);
        assert!(manager.confirm_repository("mine", "wrong").is_err());
        assert!(manager
            .confirm_repository("other", &pending.fingerprint)
            .is_err());
        assert_eq!(manager.repositories().unwrap().len(), 2);
        let trusted = manager
            .confirm_repository("mine", &pending.fingerprint)
            .unwrap();
        assert!(trusted.trusted);
        assert_eq!(fixture.manager().repositories().unwrap().len(), 3);
        assert!(manager.stage_repository(custom()).is_err());
        assert!(manager
            .confirm_repository("mine", &pending.fingerprint)
            .is_err());
        assert!(manager.remove_repository("official-preview").is_err());
        manager.remove_repository("mine").unwrap();
        assert_eq!(fixture.manager().repositories().unwrap().len(), 2);
    }
    #[test]
    fn repository_rejects_credential_urls_private_keys_and_reserved_names() {
        for url in [
            "http://example.com",
            "file:///tmp/feed",
            "https://user:pass@example.com",
            "https://example.com/?key=secret",
            "https://example.com/#x",
            "https://example.com/\nother",
        ] {
            let mut input = custom();
            input.url = url.into();
            assert!(validate_repository(input).is_err(), "{url}");
        }
        for id in ["../keys", "official-preview", "-option", ""] {
            let mut input = custom();
            input.id = id.into();
            assert!(validate_repository(input).is_err());
        }
        let mut input = custom();
        input.public_key = "-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----".into();
        assert!(validate_repository(input).is_err());
        assert_eq!(
            fingerprint(OFFICIAL_KEY),
            "80f3a73d86759cda103cb4f9a876cd4caee9d25c235c6d782b4be8a900b2696c"
        );
    }
    #[test]
    fn signed_index_parser_rejects_ambiguous_entries_and_keeps_versions() {
        let index="P:couch-integration-denon\nV:0.1.0-r0\nA:armv7\nT:Denon AVR\n\nP:couch-integration-denon\nV:0.2.0-r0\nA:armv7\n\n";
        let packages = parse_index(index, "test").unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].version, "0.1.0");
        assert!(parse_index(&index.replace("A:armv7", "A:x86_64"), "test").is_err());
        assert!(parse_index(&index.replace("T:Denon AVR", "V:9.9.9-r0"), "test").is_err());
        assert!(parse_index(&index.replace("0.1.0-r0", "../escape"), "test").is_err());
        assert!(parse_index("", "stable").unwrap().is_empty());
    }
    fn archive(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut output = Vec::new();
        for (name, bytes) in members {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, *name, *bytes).unwrap();
            let bytes = builder.into_inner().unwrap();
            let mut gzip =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gzip.write_all(&bytes).unwrap();
            output.extend(gzip.finish().unwrap());
        }
        output
    }
    #[test]
    fn untrusted_index_metadata_cannot_escape_key_directory() {
        let bytes = archive(&[
            (".SIGN.RSA.my-key.rsa.pub", b"signature"),
            ("APKINDEX", b""),
        ]);
        let (keys, index) = index_members(&bytes).unwrap();
        assert_eq!(keys, vec!["my-key.rsa.pub"]);
        assert!(index.is_empty());
        assert!(index_members(&archive(&[("APKINDEX", b"")])).is_err());
        assert!(index_members(&archive(&[(".SIGN.RSA..pub", b"bad"), ("APKINDEX", b"")])).is_err());
        assert!(index_members(&archive(&[
            (".SIGN.RSA.key.pub", b"sig"),
            ("APKINDEX", b""),
            ("APKINDEX", b"")
        ]))
        .is_err());
    }
    #[test]
    fn only_one_mutation_runs_and_removal_preserves_config() {
        let fixture = Fixture::new();
        let manager = fixture.manager();
        manager.layout().unwrap();
        fs::write(fixture.0.join("config.json"), b"keep my house").unwrap();
        let guard = manager.begin().unwrap();
        assert!(manager.start("refresh", None).unwrap_err().is_busy());
        drop(guard);
        let action = Action {
            id: "sample".into(),
            repository: None,
            preserve_connection_config: false,
        };
        assert!(manager.start("remove", Some(action)).is_err());
        let id = manager
            .start(
                "remove",
                Some(Action {
                    id: "sample".into(),
                    repository: None,
                    preserve_connection_config: true,
                }),
            )
            .unwrap();
        for _ in 0..100 {
            if manager.operation(&id).unwrap().state != "running" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(manager.operation(&id).unwrap().state, "succeeded");
        assert_eq!(
            fs::read(fixture.0.join("config.json")).unwrap(),
            b"keep my house"
        );
    }
    #[test]
    fn changed_custom_key_uses_distinct_trust_directory() {
        let fixture = Fixture::new();
        let manager = fixture.manager();
        manager.layout().unwrap();
        let mut repo = validate_repository(custom()).unwrap();
        let old = manager.keys(&repo).unwrap();
        repo.fingerprint = "different".into();
        let new = manager.keys(&repo).unwrap();
        assert_ne!(old, new);
        assert_eq!(
            fs::read(new.join("couch-integrations.rsa.pub")).unwrap(),
            OFFICIAL_KEY.as_bytes()
        );
    }
    #[test]
    fn catalog_reports_fallback_and_invalid_packages_without_false_rollback() {
        let fixture = Fixture::new();
        let manager = fixture.manager();
        manager.layout().unwrap();
        let store = &manager.0.store;
        let mut slots = Vec::new();
        for version in ["1.0.0", "2.0.0"] {
            let path = store.slot_path("example", version);
            fs::create_dir_all(path.join("bin")).unwrap();
            fs::write(path.join("bin/plugin"), version).unwrap();
            fs::set_permissions(path.join("bin/plugin"), fs::Permissions::from_mode(0o755))
                .unwrap();
            fs::write(
                path.join("manifest.json"),
                serde_json::to_vec(&serde_json::json!({
                    "protocol_version":1,"id":"example","label":"Example","version":version,
                    "executable":"bin/plugin","capabilities":[],"settings":[]
                }))
                .unwrap(),
            )
            .unwrap();
            slots.push(Slot {
                version: version.into(),
                sha256: tree_digest(&path).unwrap(),
            });
        }
        store
            .select(
                "example",
                &Selection {
                    active: Some(slots[1].clone()),
                    previous: Some(slots[0].clone()),
                },
            )
            .unwrap();
        fs::write(
            store.slot_path("example", "2.0.0").join("bin/plugin"),
            "corrupted",
        )
        .unwrap();
        let catalog = manager.catalog(&[]).unwrap();
        assert_eq!(catalog["installed"][0]["version"], "1.0.0");
        assert_eq!(catalog["installed"][0]["status"]["kind"], "fallback");
        assert_eq!(catalog["installed"][0]["can_rollback"], false);
        fs::write(
            store.slot_path("example", "1.0.0").join("bin/plugin"),
            "corrupted",
        )
        .unwrap();
        let catalog = manager.catalog(&[]).unwrap();
        assert_eq!(catalog["installed"][0]["status"]["kind"], "invalid");
        assert_eq!(catalog["installed"][0]["can_rollback"], false);
    }
}
