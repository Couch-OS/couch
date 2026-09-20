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
/// Where the official feed is published, and where it was published before
/// the feed repository moved owners. A GitHub Pages address follows its
/// owner and is not forwarded, so a repository that has not yet been held to
/// `OFFICIAL_METADATA_REQUIRED` tries the current address and then the
/// previous one: both served the same feed, verified with the same key, in
/// the window around the move. What a remote remembers about a feed
/// (`feed::Seen`) belongs to the repository, not to either address: the
/// previous address cannot serve an older feed than the current one already
/// did. Once metadata is required, `Manager::sources` stops trying the
/// previous address at all: it is known to never carry signed metadata (the
/// repository that served it moved away, and Pages does not forward), so
/// trying it could only turn a real outage of the current address into a
/// confusing refusal instead of a plain "unreachable", never a way to install
/// something the current feed has not signed.
const OFFICIAL_FEED: &str = "https://packages.couch-os.dev";
const PREVIOUS_OFFICIAL_FEED: &str = "https://dangerouslaser.github.io/couch-integrations";
/// Whether an official repository must come with valid signed metadata even
/// on a remote that has never seen any from it. Verified true: the official
/// feed has published signed `feed.json`/`feed.json.sig` weekly since
/// 2026-09-19, and a remote running this code recorded both official
/// channels' metadata as seen, with every package installable, against the
/// live feed. A missing or invalid `feed.json` now refuses an official
/// repository from its very first refresh, the same as any repository this
/// remote has already seen metadata from; a custom repository is still
/// trusted on first use.
const OFFICIAL_METADATA_REQUIRED: bool = true;
const UNREACHABLE: &str = "cannot download repository index over HTTPS";
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
    /// False when the feed's signed metadata says this Couch cannot run the
    /// package; `reason` then says why, and install and update are refused
    /// before anything is downloaded.
    #[serde(default = "yes")]
    pub installable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The file the feed's signed metadata promises for this package.
    #[serde(skip)]
    expected: Option<ExpectedApk>,
}
fn yes() -> bool {
    true
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
    /// Seconds since the Unix epoch. Tests set the time; nothing else does.
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
}
/// Why one address gave no catalog.
enum Failure {
    /// Nothing usable could be downloaded from it.
    Unreachable(Error),
    /// It answered, and what it served must not be used.
    Refused(Error),
}
enum Download {
    Body(Vec<u8>),
    /// The server answered that there is no such file.
    Absent,
}
#[derive(Clone)]
pub struct Manager(Arc<Inner>);

impl Manager {
    pub fn new(store: Store) -> Self {
        Self::with_clock(store, || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs())
        })
    }
    fn with_clock(store: Store, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        let directory = store.root.join("management");
        Self(Arc::new(Inner {
            store,
            directory,
            busy: AtomicBool::new(false),
            operation: Mutex::new(None),
            pending: Mutex::new(BTreeMap::new()),
            cache: Mutex::new(Cache::default()),
            clock: Box::new(clock),
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
                url: format!("{OFFICIAL_FEED}/{channel}"),
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
        // A repository added now starts with nothing remembered, whatever an
        // earlier one with the same name published.
        self.forget_feed(id)?;
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
        self.forget_feed(id)?;
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
                Some(p) if self.newer(&p.version, &manifest.version)? => Some(p),
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
                "available_version":update.map(|p| &p.version),
                "update_installable":update.is_none_or(|p| p.installable),
                "update_reason":update.and_then(|p| p.reason.as_ref()),
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
            return self.refresh(self.repositories()?);
        }
        let action = action.ok_or_else(|| err("missing package selection"))?;
        if kind == "remove" {
            return self.0.store.remove(&action.id);
        }
        if kind == "rollback" {
            return self.0.store.rollback(&action.id).map(|_| ());
        }
        self.install(kind, action, &self.repositories()?)
    }
    fn install(&self, kind: &str, action: Action, repositories: &[Repository]) -> Result<()> {
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
        // Known from the feed's signed metadata, so nothing is downloaded to
        // find out: not the index again, and not the package.
        refuse_not_installable(&selected)?;
        if kind == "update" {
            let (_, installed) = self.0.store.resolve(&action.id)?;
            if !self.newer(&selected.version, &installed.version)? {
                return Err(err("no newer integration version is available"));
            }
        }
        let keys = self.keys(repository)?;
        // Refresh that source immediately before install. A signed index can
        // change between browsing and clicking; require the reviewed version.
        let (current, source) = self.fetch_index(repository, &keys)?;
        let current = current
            .iter()
            .find(|p| p.id == selected.id && p.apk_version == selected.apk_version)
            .ok_or_else(|| err("repository package changed; refresh and review the new version"))?;
        refuse_not_installable(current)?;
        // apk-tools 2.14 fetch accepts a name, not add's name=version syntax.
        // The expected manifest is checked before admission/activation, so a
        // newly published version racing this fetch fails without replacing it.
        let package = format!("couch-integration-{}", selected.id);
        self.0.store.clone().with_keys_dir(keys).fetch_repository(
            &package,
            &source,
            Some((&selected.id, &selected.version)),
            current.expected.as_ref(),
        )?;
        let mut origins = self.origins()?;
        origins.insert(action.id, repository.id.clone());
        atomic_write(
            &self.0.directory.join("origins.json"),
            &serde_json::to_vec(&origins).unwrap(),
        )
    }
    /// One repository that cannot be used does not empty the others: each
    /// contributes its packages or its reason.
    fn refresh(&self, repositories: Vec<Repository>) -> Result<()> {
        let mut available = Vec::new();
        let mut errors = Vec::new();
        for repository in repositories {
            match self
                .keys(&repository)
                .and_then(|keys| self.fetch_index(&repository, &keys))
                .map(|(available, _)| available)
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
    /// The addresses to try for a repository, in order. Only an official
    /// repository at its built-in address has a second one, and only while
    /// `required` is false for it: once signed metadata is required, the
    /// previous address is never tried, because it can never carry any (see
    /// the comment on `PREVIOUS_OFFICIAL_FEED`) and trying it anyway would
    /// let a feed that only answers there turn a real outage of the current
    /// address into a refusal instead of a plain "unreachable".
    fn sources(repository: &Repository, required: bool) -> Vec<String> {
        let mut sources = vec![repository.url.clone()];
        if repository.official && !required {
            if let Some(channel) = repository.url.strip_prefix(OFFICIAL_FEED) {
                sources.push(format!("{PREVIOUS_OFFICIAL_FEED}{channel}"));
            }
        }
        sources
    }
    /// The verified index, and the address that served it: a package is then
    /// fetched from the same place as the index that named it.
    fn fetch_index(
        &self,
        repository: &Repository,
        keys: &Path,
    ) -> Result<(Vec<Available>, String)> {
        let mut state = self.feed_state();
        let seen = state.get(&repository.id).copied().unwrap_or_default();
        let required = feed::required(repository.official, seen, OFFICIAL_METADATA_REQUIRED);
        let (mut unreachable, mut refused) = (err(UNREACHABLE), None);
        for source in Self::sources(repository, required) {
            match self.fetch_index_from(repository, &source, keys, seen, required) {
                Ok((available, verified)) => {
                    if let Some(verified) = verified {
                        if verified.clock_unreliable {
                            eprintln!(
                                "couch-integrations: the clock is earlier than {}'s feed metadata was issued, so its expiry date was not checked",
                                repository.id
                            );
                        }
                        let now = feed::Seen {
                            sequence: verified.sequence,
                            seen_metadata: true,
                        };
                        if now != seen {
                            state.insert(repository.id.clone(), now);
                            self.save_feed_state(&state)?;
                        }
                    }
                    return Ok((available, source));
                }
                // A feed that answered with something unusable says more than
                // an address that did not answer at all.
                Err(Failure::Refused(error)) => refused = refused.or(Some(error)),
                Err(Failure::Unreachable(error)) => unreachable = error,
            }
        }
        Err(refused.unwrap_or(unreachable))
    }
    fn fetch_index_from(
        &self,
        repository: &Repository,
        source: &str,
        keys: &Path,
        seen: feed::Seen,
        required: bool,
    ) -> std::result::Result<(Vec<Available>, Option<feed::Verified>), Failure> {
        // Redirects are followed here, one at a time, so that each can be
        // held to `feed::redirect`; and a status is read, not raised, so that
        // "there is no feed.json" can be told from "the feed is out of reach".
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .new_agent();
        let directory = format!("{source}/armv7");
        let index = format!("{directory}/APKINDEX.tar.gz");
        let bytes = match download(&agent, &index, MAX_INDEX, "repository index")? {
            Download::Body(bytes) => bytes,
            Download::Absent => return Err(Failure::Unreachable(err(UNREACHABLE))),
        };
        if bytes.len() as u64 > MAX_INDEX {
            return Err(Failure::Refused(err(
                "repository index exceeds its size limit",
            )));
        }
        let metadata = |name: &str, limit| {
            download(
                &agent,
                &format!("{directory}/{name}"),
                limit,
                "the package feed's signed metadata",
            )
        };
        let document = metadata("feed.json", feed::MAX_METADATA)?;
        let signature = match document {
            Download::Body(_) => match metadata("feed.json.sig", feed::MAX_SIGNATURE)? {
                Download::Body(signature) => Some(signature),
                Download::Absent => None,
            },
            Download::Absent => None,
        };
        let verified = feed::check(
            match &document {
                Download::Body(document) => feed::Served::Metadata {
                    document,
                    signature: signature.as_deref(),
                },
                Download::Absent => feed::Served::Missing,
            },
            &feed::Check {
                public_key: &repository.public_key,
                seen,
                required,
                // Only an official repository's address names its channel.
                channel: repository
                    .official
                    .then(|| repository.url.rsplit('/').next().unwrap_or_default()),
                index: &bytes,
                now: (self.0.clock)(),
            },
        )
        .map_err(Failure::Refused)?;
        let temporary = self.0.directory.join(format!(".index-{}", nonce()));
        fs::create_dir(&temporary)
            .map_err(|e| Failure::Refused(io("create index verification directory", e)))?;
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
        let mut available = result.map_err(Failure::Refused)?;
        if let Some(verified) = &verified {
            for package in &mut available {
                describe(package, &verified.packages);
            }
        }
        Ok((available, verified))
    }
    /// What this remote remembers about each repository's feed, by repository
    /// id. Beside the other package manager records, and like them written
    /// whole. A record that cannot be read is started again rather than
    /// leaving the remote without packages: that forgets only what an
    /// untouched remote never knew.
    fn feed_state(&self) -> BTreeMap<String, feed::Seen> {
        let path = self.0.directory.join("feed-state.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                eprintln!(
                    "couch-integrations: {} is not valid; starting it again",
                    path.display()
                );
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        }
    }
    fn save_feed_state(&self, state: &BTreeMap<String, feed::Seen>) -> Result<()> {
        atomic_write(
            &self.0.directory.join("feed-state.json"),
            &serde_json::to_vec(state).unwrap(),
        )
    }
    fn forget_feed(&self, id: &str) -> Result<()> {
        let mut state = self.feed_state();
        match state.remove(id) {
            Some(_) => self.save_feed_state(&state),
            None => Ok(()),
        }
    }
}
/// One file, following redirects by the feed's rule. The body is cut off one
/// byte past `limit`, which the caller reports as it sees fit.
fn download(
    agent: &ureq::Agent,
    address: &str,
    limit: u64,
    what: &str,
) -> std::result::Result<Download, Failure> {
    let unreachable =
        |why: &str| Failure::Unreachable(err(format!("cannot download {what} over HTTPS{why}")));
    let mut address = url::Url::parse(address).map_err(|_| unreachable(""))?;
    for followed in 0.. {
        let mut response = agent
            .get(address.as_str())
            .call()
            .map_err(|_| unreachable(""))?;
        match response.status().as_u16() {
            200 => {
                let mut bytes = Vec::new();
                response
                    .body_mut()
                    .as_reader()
                    .take(limit + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| unreachable(""))?;
                return Ok(Download::Body(bytes));
            }
            301 | 302 | 303 | 307 | 308 => {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| unreachable(""))?;
                address = feed::redirect(&address, location, followed)
                    .map_err(|error| unreachable(&format!(": {error}")))?;
            }
            // Not there, in whatever way this host says so (a bucket answers
            // 403 for a file it does not have).
            400..=499 => return Ok(Download::Absent),
            _ => return Err(unreachable("")),
        }
    }
    unreachable!("a redirect is followed or refused")
}
/// What the feed's signed metadata says about a package the index offers.
fn describe(package: &mut Available, metadata: &[feed::Package]) {
    let file = format!(
        "couch-integration-{}-{}.apk",
        package.id, package.apk_version
    );
    let Some(entry) = metadata
        .iter()
        .find(|entry| entry.apk == file && entry.id == package.id)
    else {
        // The index is the one the metadata signed for, so the package is
        // still the feed's; it is only not described.
        eprintln!("couch-integrations: the feed's metadata does not list {file}");
        return;
    };
    package.expected = Some(ExpectedApk {
        file,
        size: entry.size,
        sha256: entry.sha256.clone(),
    });
    if entry.min_core_protocol_version > PROTOCOL_VERSION {
        package.installable = false;
        package.reason = Some(feed::NEEDS_NEWER_COUCH.into());
    }
}
fn refuse_not_installable(package: &Available) -> Result<()> {
    if package.installable {
        return Ok(());
    }
    Err(err(format!(
        "{}. Update this remote, then install the package again",
        package
            .reason
            .as_deref()
            .unwrap_or("This package cannot be installed")
    )))
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
            installable: true,
            reason: None,
            expected: None,
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
    fn only_official_repositories_fall_back_and_only_while_metadata_is_not_required() {
        let official = Manager::official();
        assert_eq!(official.len(), 2);
        for (repository, channel) in official.iter().zip(["stable", "preview"]) {
            assert_eq!(
                Manager::sources(repository, false),
                [
                    format!("https://packages.couch-os.dev/{channel}"),
                    format!("https://dangerouslaser.github.io/couch-integrations/{channel}"),
                ]
            );
            // Required, which every official repository now always is: the
            // previous address is not tried, since it can never carry signed
            // metadata and trying it would only turn a real outage of the
            // current address into a confusing refusal.
            assert_eq!(
                Manager::sources(repository, true),
                [format!("https://packages.couch-os.dev/{channel}")]
            );
        }
        // A user's repository is fetched from its own address only, even one
        // that claims the official host, and so is anything not marked official.
        let mut theirs = official[0].clone();
        theirs.official = false;
        assert_eq!(Manager::sources(&theirs, false), [theirs.url.clone()]);
        let mut elsewhere = official[0].clone();
        elsewhere.url = "https://example.com/feed".into();
        assert_eq!(Manager::sources(&elsewhere, false), [elsewhere.url.clone()]);
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
    // ---- Feed metadata, against a feed served from this machine ----

    use crate::feed::tests::{document, TestKey, ISSUED};
    use std::{
        io::{BufRead, BufReader},
        net::{TcpListener, TcpStream},
        sync::atomic::AtomicU64,
    };

    #[derive(Clone)]
    enum Route {
        Body(Vec<u8>),
        Redirect(String),
        Status(u16),
    }
    /// The `feed.json` to publish beside an index, given that index's bytes.
    type Metadata<'a> = &'a dyn Fn(&[u8]) -> serde_json::Value;
    /// A feed on a local port. Whatever has no route answers 404.
    struct Feed {
        address: String,
        routes: Arc<Mutex<BTreeMap<String, Route>>>,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
    }
    impl Feed {
        fn new() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = format!("http://{}", listener.local_addr().unwrap());
            let routes = Arc::new(Mutex::new(BTreeMap::<String, Route>::new()));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let (served, log, stopping) = (routes.clone(), requests.clone(), stop.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(mut stream) = stream else { continue };
                    let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
                    let Some(Ok(request)) = lines.next() else {
                        continue;
                    };
                    while lines.next().is_some_and(|l| l.is_ok_and(|l| !l.is_empty())) {}
                    let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
                    let route = served.lock().unwrap().get(&path).cloned();
                    log.lock().unwrap().push(path);
                    let (status, extra, body) = match route {
                        Some(Route::Body(body)) => (200, String::new(), body),
                        Some(Route::Redirect(to)) => (302, format!("Location: {to}\r\n"), vec![]),
                        Some(Route::Status(status)) => (status, String::new(), vec![]),
                        None => (404, String::new(), b"not here".to_vec()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status} Test\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                }
            });
            Self {
                address,
                routes,
                requests,
                stop,
            }
        }
        fn route(&self, path: &str, route: Route) {
            self.routes.lock().unwrap().insert(path.into(), route);
        }
        fn remove(&self, path: &str) {
            self.routes.lock().unwrap().remove(path);
        }
        /// An index offering these `(id, apk version)` packages under
        /// `/<name>/armv7`, with signed metadata if there is a document.
        fn publish(
            &self,
            name: &str,
            packages: &[(&str, &str)],
            key: &TestKey,
            metadata: Option<Metadata>,
        ) -> Vec<u8> {
            let text: String = packages
                .iter()
                .map(|(id, version)| {
                    format!("P:couch-integration-{id}\nV:{version}\nA:armv7\nT:Test\n\n")
                })
                .collect();
            let index = archive(&[
                (".SIGN.RSA.test.rsa.pub", b"checked by the fixture apk"),
                ("APKINDEX", text.as_bytes()),
            ]);
            let directory = format!("/{name}/armv7");
            self.route(
                &format!("{directory}/APKINDEX.tar.gz"),
                Route::Body(index.clone()),
            );
            match metadata {
                Some(metadata) => {
                    let bytes = serde_json::to_vec(&metadata(&index)).unwrap();
                    self.route(
                        &format!("{directory}/feed.json.sig"),
                        Route::Body(key.sign(&bytes)),
                    );
                    self.route(&format!("{directory}/feed.json"), Route::Body(bytes));
                }
                None => {
                    self.remove(&format!("{directory}/feed.json"));
                    self.remove(&format!("{directory}/feed.json.sig"));
                }
            }
            index
        }
        fn requested(&self, suffix: &str) -> usize {
            let requests = self.requests.lock().unwrap();
            requests.iter().filter(|r| r.ends_with(suffix)).count()
        }
    }
    impl Drop for Feed {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            let _ = TcpStream::connect(self.address.trim_start_matches("http://"));
        }
    }

    /// Stands in for apk: `verify` accepts (real signature checking is
    /// covered by tools/integrations/smoke.sh), `version --test` compares,
    /// `fetch` hands over whatever the test put in `served/` and notes that
    /// it ran, `add` unpacks.
    const FIXTURE_APK: &str = r#"#!/bin/sh
set -eu
here=$(dirname "$0")
command=
for argument in "$@"; do
  case "$argument" in verify|version|fetch|add) command=$argument; break;; esac
done
case "$command" in
  verify) exit 0;;
  version)
    if [ "$3" = "$4" ]; then echo "="
    elif [ "$(printf '%s\n%s\n' "$3" "$4" | sort -V | tail -n 1)" = "$3" ]; then echo ">"
    else echo "<"; fi;;
  fetch)
    echo fetch >> "$here/apk.log"
    while [ $# -gt 0 ]; do
      if [ "$1" = --output ]; then shift; output=$1; fi
      shift
    done
    cp "$here"/served/*.apk "$output"/;;
  add)
    while [ $# -gt 0 ]; do
      if [ "$1" = --root ]; then shift; root=$1; fi
      last=$1; shift
    done
    tar -xzf "$last" -C "$root";;
  *) exit 64;;
esac
"#;

    struct Remote {
        fixture: Fixture,
        manager: Manager,
        now: Arc<AtomicU64>,
    }
    impl Remote {
        fn new() -> Self {
            let fixture = Fixture::new();
            fs::create_dir_all(fixture.0.join("served")).unwrap();
            let apk = fixture.0.join("apk");
            fs::write(&apk, FIXTURE_APK).unwrap();
            fs::set_permissions(&apk, fs::Permissions::from_mode(0o755)).unwrap();
            let now = Arc::new(AtomicU64::new(ISSUED + 3600));
            let clock = now.clone();
            let manager = Manager::with_clock(
                Store::new(fixture.0.join("integrations")).with_apk(apk),
                move || clock.load(Ordering::Acquire),
            );
            manager.layout().unwrap();
            Self {
                fixture,
                manager,
                now,
            }
        }
        /// What a refresh says and what the catalog then offers, as `id version`.
        fn refresh(&self, repositories: &[&Repository]) -> (Result<()>, Vec<String>) {
            let result = self
                .manager
                .refresh(repositories.iter().map(|r| (*r).clone()).collect());
            let catalog = self.manager.catalog(&[]).unwrap();
            let offered = catalog["available"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| {
                    format!(
                        "{} {} from {}",
                        p["id"].as_str().unwrap(),
                        p["version"].as_str().unwrap(),
                        p["repository"].as_str().unwrap()
                    )
                })
                .collect();
            (result, offered)
        }
        fn seen(&self, id: &str) -> Option<feed::Seen> {
            self.manager.feed_state().get(id).copied()
        }
        fn fetches(&self) -> usize {
            fs::read_to_string(self.fixture.0.join("apk.log")).map_or(0, |log| log.lines().count())
        }
    }
    fn repository(id: &str, feed: &Feed, key: &TestKey) -> Repository {
        Repository {
            id: id.into(),
            name: format!("Feed {id}"),
            url: format!("{}/{id}", feed.address),
            fingerprint: fingerprint(&key.public_pem),
            official: false,
            trusted: true,
            public_key: key.public_pem.clone(),
        }
    }
    fn sequence(sequence: u64) -> impl Fn(&[u8]) -> serde_json::Value {
        move |index| document(sequence, index)
    }
    const DENON: [(&str, &str); 1] = [("denon", "0.2.1-r0")];

    #[test]
    fn valid_metadata_is_remembered_and_an_older_feed_is_then_refused() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        let (result, offered) = remote.refresh(&[&mine]);
        result.unwrap();
        assert_eq!(offered, ["denon 0.2.1 from mine"]);
        let seen = feed::Seen {
            sequence: 10,
            seen_metadata: true,
        };
        assert_eq!(remote.seen("mine"), Some(seen));
        assert_eq!(
            fs::read(remote.manager.0.directory.join("feed-state.json")).unwrap(),
            br#"{"mine":{"sequence":10,"seen_metadata":true}}"#
        );
        // The same publication again (a re-read, a weekly re-sign that has not
        // happened yet) is fine, and a newer one moves the mark.
        remote.refresh(&[&mine]).0.unwrap();
        feed.publish("mine", &DENON, &key, Some(&sequence(12)));
        remote.refresh(&[&mine]).0.unwrap();
        assert_eq!(remote.seen("mine").unwrap().sequence, 12);
        // Rolled back: validly signed, consistent with its index, and older.
        feed.publish("mine", &DENON, &key, Some(&sequence(11)));
        let (result, offered) = remote.refresh(&[&mine]);
        assert_eq!(
            result.unwrap_err().0,
            format!("Feed mine: {}", feed::OLDER_THAN_SEEN)
        );
        assert!(offered.is_empty());
        assert_eq!(remote.seen("mine").unwrap().sequence, 12);
        // What is remembered belongs to the repository, not to the address it
        // was read from: another address for the same repository (as the
        // official feed has) cannot serve something older either.
        let elsewhere = Feed::new();
        elsewhere.publish("mine", &DENON, &key, Some(&sequence(11)));
        let moved = repository("mine", &elsewhere, &key);
        assert!(remote.refresh(&[&moved]).0.is_err());
        elsewhere.publish("mine", &DENON, &key, Some(&sequence(12)));
        remote.refresh(&[&moved]).0.unwrap();
        // A restart remembers.
        let restarted = Manager::with_clock(remote.manager.0.store.clone(), || ISSUED);
        assert_eq!(restarted.feed_state().get("mine").unwrap().sequence, 12);
    }

    #[test]
    fn metadata_is_optional_until_a_repository_has_published_it_once() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        // A feed that has never published metadata works as it always has.
        feed.publish("mine", &DENON, &key, None);
        let (result, offered) = remote.refresh(&[&mine]);
        result.unwrap();
        assert_eq!(offered, ["denon 0.2.1 from mine"]);
        assert_eq!(remote.seen("mine"), None);
        assert_eq!(feed.requested("/feed.json"), 1);
        assert_eq!(feed.requested("/feed.json.sig"), 0);
        // So does one whose host says "forbidden" for a file it does not have.
        feed.route("/mine/armv7/feed.json", Route::Status(403));
        remote.refresh(&[&mine]).0.unwrap();
        // Once it has published, taking the metadata away hides nothing.
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        remote.refresh(&[&mine]).0.unwrap();
        feed.publish("mine", &DENON, &key, None);
        let (result, offered) = remote.refresh(&[&mine]);
        assert!(result.unwrap_err().0.contains("signed metadata is missing"));
        assert!(offered.is_empty());
        // Nor does a server error pass for "not published".
        feed.route("/mine/armv7/feed.json", Route::Status(500));
        let error = remote.refresh(&[&mine]).0.unwrap_err();
        assert!(
            error
                .0
                .contains("cannot download the package feed's signed metadata"),
            "{error}"
        );
        // Removing the repository and adding it again starts over.
        remote.manager.forget_feed("mine").unwrap();
        feed.publish("mine", &DENON, &key, None);
        remote.refresh(&[&mine]).0.unwrap();
    }

    #[test]
    fn removing_or_adding_a_repository_forgets_what_its_feed_published() {
        let remote = Remote::new();
        let remembered = |id: &str| {
            let mut state = remote.manager.feed_state();
            state.insert(
                id.into(),
                feed::Seen {
                    sequence: 99,
                    seen_metadata: true,
                },
            );
            remote.manager.save_feed_state(&state).unwrap();
        };
        remembered("mine");
        remembered("official-preview");
        let pending = remote.manager.stage_repository(custom()).unwrap();
        remote
            .manager
            .confirm_repository("mine", &pending.fingerprint)
            .unwrap();
        assert_eq!(remote.seen("mine"), None);
        remembered("mine");
        remote.manager.remove_repository("mine").unwrap();
        assert_eq!(remote.seen("mine"), None);
        assert_eq!(remote.seen("official-preview").unwrap().sequence, 99);
        // An unreadable record is started again, not an end to all packages.
        fs::write(remote.manager.0.directory.join("feed-state.json"), b"{").unwrap();
        assert!(remote.manager.feed_state().is_empty());
    }

    #[test]
    fn metadata_that_is_present_has_to_be_right_even_the_first_time() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        let refused = |why: &str| {
            let (result, offered) = remote.refresh(&[&mine]);
            let error = result.unwrap_err();
            assert!(error.0.contains(why), "{error}");
            assert!(offered.is_empty(), "{why}");
            assert_eq!(remote.seen("mine"), None, "{why}");
        };
        // Signed by somebody else.
        feed.publish("mine", &DENON, &TestKey::new(2048), Some(&sequence(10)));
        refused("metadata signature is not valid");
        // Edited after signing.
        let index = feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        let mut edited = document(10, &index);
        edited["sequence"] = 11.into();
        feed.route(
            "/mine/armv7/feed.json",
            Route::Body(serde_json::to_vec(&edited).unwrap()),
        );
        refused("metadata signature is not valid");
        // Published without its signature.
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        feed.remove("/mine/armv7/feed.json.sig");
        refused("metadata signature is not valid");
        // Valid metadata beside another index: an old index under new
        // metadata, or the other way round.
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        feed.publish("other", &[("denon", "0.2.0-r0")], &key, None);
        let old = feed.routes.lock().unwrap()["/other/armv7/APKINDEX.tar.gz"].clone();
        feed.route("/mine/armv7/APKINDEX.tar.gz", old);
        refused("repository index is not the one");
        // Larger than metadata may be.
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        feed.route(
            "/mine/armv7/feed.json",
            Route::Body(vec![b' '; feed::MAX_METADATA as usize + 1]),
        );
        refused("metadata exceeds its size limit");
        // And nothing above was remembered, so the honest feed still works.
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        remote.refresh(&[&mine]).0.unwrap();
    }

    #[test]
    fn an_expired_feed_is_refused_unless_the_clock_is_not_set() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        // Frozen: a month-old publication served for ever.
        let expires = feed::timestamp("2026-10-19T21:00:00Z").unwrap();
        remote.now.store(expires + 1, Ordering::Release);
        let (result, offered) = remote.refresh(&[&mine]);
        assert!(result
            .unwrap_err()
            .0
            .contains("expired on 2026-10-19T21:00:00Z"));
        assert!(offered.is_empty());
        // A remote that started without the time (1970) reads the same feed,
        // and still holds it to its sequence.
        remote.now.store(86_400, Ordering::Release);
        let (result, offered) = remote.refresh(&[&mine]);
        result.unwrap();
        assert_eq!(offered, ["denon 0.2.1 from mine"]);
        feed.publish("mine", &DENON, &key, Some(&sequence(9)));
        assert_eq!(
            remote.refresh(&[&mine]).0.unwrap_err().0,
            format!("Feed mine: {}", feed::OLDER_THAN_SEEN)
        );
    }

    #[test]
    fn metadata_in_a_later_format_counts_as_none() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        let later = |index: &[u8]| {
            let mut later = document(10, index);
            later["schema"] = 2.into();
            later
        };
        feed.publish("mine", &DENON, &key, Some(&later));
        let (result, offered) = remote.refresh(&[&mine]);
        result.unwrap();
        assert_eq!(offered, ["denon 0.2.1 from mine"]);
        assert_eq!(remote.seen("mine"), None);
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        remote.refresh(&[&mine]).0.unwrap();
        feed.publish("mine", &DENON, &key, Some(&later));
        let error = remote.refresh(&[&mine]).0.unwrap_err();
        assert!(error.0.contains("newer format"), "{error}");
    }

    #[test]
    fn an_official_repository_has_to_name_its_own_channel() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mut preview = repository("preview", &feed, &key);
        preview.official = true;
        let mut stable = repository("stable", &feed, &key);
        stable.official = true;
        // `document` says "preview": the preview feed copied over stable.
        feed.publish("preview", &DENON, &key, Some(&sequence(10)));
        feed.publish("stable", &DENON, &key, Some(&sequence(10)));
        let (result, offered) = remote.refresh(&[&preview, &stable]);
        assert_eq!(
            result.unwrap_err().0,
            "Feed stable: The package feed's metadata belongs to another channel"
        );
        assert_eq!(offered, ["denon 0.2.1 from preview"]);
        // Anybody's own repository calls its channel what it likes.
        stable.official = false;
        remote.refresh(&[&preview, &stable]).0.unwrap();
    }

    #[test]
    fn an_official_repository_is_refused_without_metadata_from_the_first_refresh() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        // `document` names the channel "preview", so this id, matching the
        // channel an official repository is held to, is the one that keeps
        // its otherwise-valid metadata from being refused for the wrong
        // reason.
        let (mut official, custom) = (
            repository("preview", &feed, &key),
            repository("mine", &feed, &key),
        );
        official.official = true;
        // Official, and this remote has never seen metadata from it: no
        // trust on first use any more, unlike a custom repository.
        feed.publish("preview", &DENON, &key, None);
        // A custom repository is unaffected: still trusted the first time.
        feed.publish("mine", &DENON, &key, None);
        let (result, offered) = remote.refresh(&[&official, &custom]);
        assert_eq!(
            result.unwrap_err().0,
            "Feed preview: The package feed's signed metadata is missing"
        );
        // The custom repository still loads, with no metadata at all.
        assert_eq!(offered, ["denon 0.2.1 from mine"]);
        assert_eq!(remote.seen("preview"), None);
        assert_eq!(remote.seen("mine"), None);
        // Invalid metadata (signed by somebody else) is refused the same
        // way, still on the very first refresh.
        feed.publish("preview", &DENON, &TestKey::new(2048), Some(&sequence(10)));
        let error = remote.refresh(&[&official]).0.unwrap_err();
        assert!(
            error.0.contains("metadata signature is not valid"),
            "{error}"
        );
        assert_eq!(remote.seen("preview"), None);
        // Once it publishes something valid, it is accepted like any other.
        feed.publish("preview", &DENON, &key, Some(&sequence(10)));
        remote.refresh(&[&official]).0.unwrap();
        assert_eq!(remote.seen("preview").unwrap().sequence, 10);
    }

    #[test]
    fn one_repository_that_cannot_be_used_does_not_empty_the_others() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let (good, bad, gone) = (
            repository("good", &feed, &key),
            repository("bad", &feed, &key),
            repository("gone", &feed, &key),
        );
        feed.publish("good", &DENON, &key, Some(&sequence(10)));
        feed.publish(
            "bad",
            &[("kodi", "0.1.0-r0")],
            &TestKey::new(2048),
            Some(&sequence(10)),
        );
        let (result, offered) = remote.refresh(&[&bad, &good, &gone]);
        assert_eq!(
            result.unwrap_err().0,
            "Feed bad: The package feed's metadata signature is not valid; \
             Feed gone: cannot download repository index over HTTPS"
        );
        assert_eq!(offered, ["denon 0.2.1 from good"]);
        let catalog = remote.manager.catalog(&[]).unwrap();
        assert!(catalog["catalog_error"]
            .as_str()
            .unwrap()
            .starts_with("Feed bad: "));
    }

    #[test]
    fn a_redirect_to_plain_http_is_a_failed_download() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let (mine, moved) = (
            repository("mine", &feed, &key),
            repository("moved", &feed, &key),
        );
        feed.publish("mine", &DENON, &key, Some(&sequence(10)));
        // The whole feed is there, one plain-HTTP redirect away.
        feed.route(
            "/moved/armv7/APKINDEX.tar.gz",
            Route::Redirect(format!("{}/mine/armv7/APKINDEX.tar.gz", feed.address)),
        );
        let (result, offered) = remote.refresh(&[&moved]);
        assert_eq!(
            result.unwrap_err().0,
            "Feed moved: cannot download repository index over HTTPS: \
             the download was redirected to an address that is not HTTPS"
        );
        assert!(offered.is_empty());
        assert_eq!(feed.requested("/mine/armv7/APKINDEX.tar.gz"), 0);
        // The same for the metadata, by a relative address.
        feed.route(
            "/mine/armv7/feed.json",
            Route::Redirect("/mine/armv7/feed-elsewhere.json".into()),
        );
        let error = remote.refresh(&[&mine]).0.unwrap_err();
        assert!(
            error
                .0
                .contains("signed metadata over HTTPS: the download was redirected"),
            "{error}"
        );
        assert_eq!(feed.requested("/feed-elsewhere.json"), 0);
        // A redirect with nowhere to go is no download either.
        feed.route("/mine/armv7/feed.json", Route::Status(302));
        assert!(remote.refresh(&[&mine]).0.is_err());
    }

    fn needs(protocol: u32) -> impl Fn(&[u8]) -> serde_json::Value {
        move |index| {
            let mut value = document(10, index);
            value["packages"][0]["protocol_version"] = protocol.into();
            value["packages"][0]["min_core_protocol_version"] = protocol.into();
            value
        }
    }
    fn install(id: &str) -> Action {
        Action {
            id: id.into(),
            repository: Some("mine".into()),
            preserve_connection_config: true,
        }
    }

    #[test]
    fn a_package_that_needs_a_newer_couch_is_refused_before_any_download() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        feed.publish("mine", &DENON, &key, Some(&needs(PROTOCOL_VERSION + 1)));
        remote.refresh(&[&mine]).0.unwrap();
        let catalog = remote.manager.catalog(&[]).unwrap();
        assert_eq!(catalog["available"][0]["installable"], false);
        assert_eq!(catalog["available"][0]["reason"], "Needs a newer Couch");
        let asked = feed.requests.lock().unwrap().len();
        let error = remote
            .manager
            .install("install", install("denon"), std::slice::from_ref(&mine))
            .unwrap_err();
        assert!(error.0.starts_with("Needs a newer Couch. "), "{error}");
        assert_eq!(feed.requests.lock().unwrap().len(), asked);
        assert_eq!(remote.fetches(), 0);
        // The feed moved on between browsing and installing: the catalog
        // still says installable, the index read just before the download
        // says otherwise, and that is the one believed.
        feed.publish("mine", &DENON, &key, Some(&needs(PROTOCOL_VERSION)));
        remote.refresh(&[&mine]).0.unwrap();
        let catalog = remote.manager.catalog(&[]).unwrap();
        assert_eq!(catalog["available"][0]["installable"], true);
        assert!(catalog["available"][0].get("reason").is_none());
        feed.publish("mine", &DENON, &key, Some(&needs(PROTOCOL_VERSION + 1)));
        let error = remote
            .manager
            .install("install", install("denon"), std::slice::from_ref(&mine))
            .unwrap_err();
        assert!(error.0.starts_with("Needs a newer Couch. "), "{error}");
        assert_eq!(remote.fetches(), 0);
    }

    /// A package the way `tests/lifecycle.rs` builds one.
    fn package(path: &Path, version: &str) {
        let manifest = serde_json::json!({"protocol_version":1,"id":"fixture","label":"Fixture",
            "version":version,"executable":"bin/plugin","capabilities":[],"settings":[]});
        let mut frame = Vec::new();
        couch_plugin::write_frame(
            &mut frame,
            &serde_json::json!({"id":1,"body":{"type":"hello","manifest":manifest}}),
        )
        .unwrap();
        let bytes: String = frame.iter().map(|byte| format!("\\{byte:03o}")).collect();
        let script = format!("#!/bin/sh\nprintf '{bytes}'\nsleep 5\n");
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            File::create(path).unwrap(),
            flate2::Compression::default(),
        ));
        for (name, content, mode) in [
            (
                "manifest.json",
                serde_json::to_vec(&manifest).unwrap(),
                0o644,
            ),
            ("bin/plugin", script.into_bytes(), 0o755),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_mode(mode);
            header.set_size(content.len() as u64);
            header.set_cksum();
            let name = format!("usr/lib/couch/integrations/fixture/{name}");
            archive
                .append_data(&mut header, name, content.as_slice())
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn a_downloaded_package_has_to_be_the_one_the_metadata_describes() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        let file = remote
            .fixture
            .0
            .join("served/couch-integration-fixture-1.0.0-r0.apk");
        package(&file, "1.0.0");
        let bytes = fs::read(&file).unwrap();
        let described = |size: u64, sha256: String| {
            move |index: &[u8]| {
                let mut value = document(10, index);
                value["packages"] = serde_json::json!([{"id":"fixture","version":"1.0.0",
                    "apk":"couch-integration-fixture-1.0.0-r0.apk","size":size,"sha256":sha256,
                    "protocol_version":1,"min_core_protocol_version":1}]);
                value
            }
        };
        let fixture = [("fixture", "1.0.0-r0")];
        let attempt = |metadata: Metadata| {
            feed.publish("mine", &fixture, &key, Some(metadata));
            remote.refresh(&[&mine]).0.unwrap();
            remote
                .manager
                .install("install", install("fixture"), std::slice::from_ref(&mine))
        };
        let mismatch =
            "The downloaded package is not the one the package feed's signed metadata describes";
        let wrong_hash = attempt(&described(bytes.len() as u64, feed::sha256_hex(b"another")));
        assert_eq!(wrong_hash.unwrap_err().0, mismatch);
        let wrong_size = attempt(&described(bytes.len() as u64 + 1, feed::sha256_hex(&bytes)));
        assert_eq!(wrong_size.unwrap_err().0, mismatch);
        assert_eq!(remote.fetches(), 2);
        assert!(remote.manager.0.store.list().unwrap().is_empty());
        // Nothing of a refused download is left behind.
        let left: Vec<_> = fs::read_dir(remote.manager.0.store.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".fetch-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
        attempt(&described(
            bytes.len() as u64,
            feed::sha256_hex(&bytes).to_uppercase(),
        ))
        .unwrap();
        assert_eq!(remote.manager.0.store.list().unwrap()[0].version, "1.0.0");
        assert_eq!(remote.manager.origins().unwrap()["fixture"], "mine");
        // A feed without metadata installs as it always has.
        let (other, plain) = (
            Remote::new(),
            repository("mine", &feed, &TestKey::new(2048)),
        );
        fs::copy(
            &file,
            other
                .fixture
                .0
                .join("served/couch-integration-fixture-1.0.0-r0.apk"),
        )
        .unwrap();
        feed.publish("mine", &fixture, &key, None);
        other.refresh(&[&plain]).0.unwrap();
        other
            .manager
            .install("install", install("fixture"), std::slice::from_ref(&plain))
            .unwrap();
    }

    #[test]
    fn an_update_the_core_cannot_run_is_offered_with_its_reason() {
        let (remote, feed, key) = (Remote::new(), Feed::new(), TestKey::new(2048));
        let mine = repository("mine", &feed, &key);
        let file = remote
            .fixture
            .0
            .join("served/couch-integration-fixture-1.0.0-r0.apk");
        package(&file, "1.0.0");
        feed.publish("mine", &[("fixture", "1.0.0-r0")], &key, None);
        remote.refresh(&[&mine]).0.unwrap();
        remote
            .manager
            .install("install", install("fixture"), std::slice::from_ref(&mine))
            .unwrap();
        let newer = |index: &[u8]| {
            let mut value = document(10, index);
            value["packages"] = serde_json::json!([{"id":"fixture","version":"2.0.0",
                "apk":"couch-integration-fixture-2.0.0-r0.apk","size":1,"sha256":"00",
                "protocol_version":PROTOCOL_VERSION + 1,
                "min_core_protocol_version":PROTOCOL_VERSION + 1}]);
            value
        };
        feed.publish("mine", &[("fixture", "2.0.0-r0")], &key, Some(&newer));
        remote.refresh(&[&mine]).0.unwrap();
        let catalog = remote.manager.catalog(&[]).unwrap();
        let installed = &catalog["installed"][0];
        assert_eq!(installed["available_version"], "2.0.0");
        assert_eq!(installed["update_installable"], false);
        assert_eq!(installed["update_reason"], "Needs a newer Couch");
        let fetched = remote.fetches();
        assert!(remote
            .manager
            .install("update", install("fixture"), std::slice::from_ref(&mine))
            .is_err());
        assert_eq!(remote.fetches(), fetched);
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
