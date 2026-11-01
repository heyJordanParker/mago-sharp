//! Database watcher for real-time file change monitoring.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;
use std::time::Instant;

use globset::GlobBuilder;
use globset::GlobSet;
use globset::GlobSetBuilder;
use notify::Config;
use notify::Event;
use notify::EventHandler;
use notify::EventKind;
use notify::PollWatcher;
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher as NotifyWatcher;
use notify::event::MetadataKind;
use notify::event::ModifyKind;

use crate::Database;
use crate::DatabaseReader;
use crate::ReadDatabase;
use crate::error::DatabaseError;
use crate::exclusion::Exclusion;
use crate::file::File;
use crate::file::FileId;
use crate::file::FileType;
use crate::loader::DatabaseLoader;
use crate::loader::Manifests;
use crate::loader::calculate_pattern_specificity;
use crate::loader::mark_package;
use crate::loader::resolve_file_type;
use crate::utils::bytes_to_path;

const DEFAULT_POLL_INTERVAL_MS: u64 = 1000;
const WAIT_INTERNAL_MS: u64 = 100;
const WAIT_DEBOUNCE_MS: u64 = 300;
const STABILITY_CHECK_MS: u64 = 10;

/// The start of the name of each cookie file [`start_watcher`] writes, so every watcher leaves out every cookie.
const COOKIE_PREFIX: &str = ".mago-watch-cookie-";

/// How long [`start_watcher`] waits for its cookie's event before it polls instead. FSEvents reported a cookie after
/// 7.7 seconds at the most, in eight watcher starts of `mago analyze --watch` beside the compile tests, on a Mac at a
/// load of about 60.
const COOKIE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ChangedFile {
    id: FileId,
    path: PathBuf,
}

/// What an event tells [`DatabaseWatcher::wait`].
enum Report {
    /// These files changed.
    Changed(Vec<ChangedFile>),
    /// The operating system dropped events, as FSEvents and inotify do under a flood of changes, so any watched file
    /// can have changed. FSEvents names the folder whose events it dropped, and inotify names none.
    Dropped(Option<PathBuf>),
}

/// Options for configuring the file system watcher.
#[derive(Debug, Clone)]
pub struct WatchOptions {
    pub poll_interval: Option<Duration>,
    pub additional_excludes: Vec<Exclusion<'static>>,
}

impl Default for WatchOptions {
    #[inline]
    fn default() -> Self {
        Self { poll_interval: Some(Duration::from_millis(DEFAULT_POLL_INTERVAL_MS)), additional_excludes: vec![] }
    }
}

/// Database watcher service that monitors file changes and updates the database.
pub struct DatabaseWatcher<'config> {
    database: Database<'config>,
    watcher: Option<Box<dyn NotifyWatcher + Send>>,
    watched_paths: Vec<PathBuf>,
    receiver: Option<Receiver<Report>>,
    /// Configured base paths paired with their loader-style specificity score.
    ///
    /// Carrying the score (computed via [`calculate_pattern_specificity`] over the original
    /// pattern, before stripping any glob suffix) is what lets the watcher and the loader
    /// agree on which [`FileType`] a newly-added file should get.
    host_base_paths: Vec<(PathBuf, usize)>,
    patch_base_paths: Vec<(PathBuf, usize)>,
    include_base_paths: Vec<(PathBuf, usize)>,
}

impl<'config> DatabaseWatcher<'config> {
    #[inline]
    #[must_use]
    pub fn new(database: Database<'config>) -> Self {
        Self {
            database,
            watcher: None,
            watched_paths: Vec::new(),
            receiver: None,
            host_base_paths: Vec::new(),
            patch_base_paths: Vec::new(),
            include_base_paths: Vec::new(),
        }
    }

    /// Starts watching for file changes in the configured directories.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if:
    /// - A glob pattern is invalid
    /// - The file system watcher cannot be created
    /// - Directories cannot be watched
    #[inline]
    pub fn watch(&mut self, options: WatchOptions) -> Result<(), DatabaseError> {
        self.watch_of::<RecommendedWatcher>(options, &unique_cookie(), COOKIE_TIMEOUT)
    }

    /// [`Self::watch`] with a watcher of type `W`, which [`start_watcher_of`] starts with `cookie` and
    /// `cookie_timeout`.
    fn watch_of<W>(
        &mut self,
        options: WatchOptions,
        cookie: &str,
        cookie_timeout: Duration,
    ) -> Result<(), DatabaseError>
    where
        W: NotifyWatcher + Send + 'static,
    {
        self.stop();

        let config = &self.database.configuration;

        let (tx, rx) = mpsc::channel();

        let mut all_exclusions = vec![
            Exclusion::Pattern(Cow::Borrowed("**/node_modules/**")),
            Exclusion::Pattern(Cow::Borrowed("**/.git/**")),
            Exclusion::Pattern(Cow::Borrowed("**/.idea/**")),
            Exclusion::Pattern(Cow::Borrowed("**/vendor/**")),
        ];
        all_exclusions.extend(config.excludes.iter().cloned());
        all_exclusions.extend(options.additional_excludes);

        let glob_settings = &config.glob;
        let mut glob_builder = GlobSetBuilder::new();
        for ex in &all_exclusions {
            if let Exclusion::Pattern(pat) = ex {
                let glob = GlobBuilder::new(pat)
                    .case_insensitive(glob_settings.case_insensitive)
                    .literal_separator(glob_settings.literal_separator)
                    .backslash_escape(glob_settings.backslash_escape)
                    .empty_alternates(glob_settings.empty_alternates)
                    .build()?;
                glob_builder.add(glob);
            }
        }

        let glob_excludes = glob_builder.build()?;

        let path_excludes: HashSet<PathBuf> = all_exclusions
            .iter()
            .filter_map(|ex| match ex {
                Exclusion::Path(p) => Some(p.as_ref().to_path_buf()),
                Exclusion::Pattern(_) => None,
            })
            .collect();

        let extensions: HashSet<Vec<u8>> = config.extensions.iter().map(|c| c.to_vec()).collect();
        let workspace = config.workspace.as_ref().to_path_buf();

        // Build the set of explicitly configured watch paths so that events from
        // these directories are never filtered out by the default glob exclusions
        // (e.g., a project that explicitly includes `vendor/revolt` should still
        // receive change events for files under that directory). They keep the configuration's order, so the watcher
        // starts on them in the same order each run.
        let mut watched_paths: Vec<PathBuf> = Vec::new();

        let mut host_base_paths = Vec::new();
        for path in &config.paths {
            // Compute specificity on the original pattern (with glob intact); the watch path
            // strips trailing globs so the watcher can recurse into a real directory.
            let specificity = calculate_pattern_specificity(path.as_ref());
            let watch_path = Self::extract_watch_path(path.as_ref());
            let absolute_path = if watch_path.is_absolute() { watch_path } else { config.workspace.join(watch_path) };

            let canonical = absolute_path.canonicalize().unwrap_or_else(|_| absolute_path.clone());
            host_base_paths.push((canonical, specificity));
            if !watched_paths.contains(&absolute_path) {
                watched_paths.push(absolute_path);
            }
        }

        let mut include_base_paths = Vec::new();
        for path in &config.includes {
            let specificity = calculate_pattern_specificity(path.as_ref());
            let watch_path = Self::extract_watch_path(path.as_ref());
            let absolute_path = if watch_path.is_absolute() { watch_path } else { config.workspace.join(watch_path) };
            let canonical = absolute_path.canonicalize().unwrap_or_else(|_| absolute_path.clone());
            include_base_paths.push((canonical, specificity));
            if !watched_paths.contains(&absolute_path) {
                watched_paths.push(absolute_path);
            }
        }

        let mut patch_base_paths = Vec::new();
        for path in &config.patches {
            let specificity = calculate_pattern_specificity(path.as_ref());
            let watch_path = Self::extract_watch_path(path.as_ref());
            let absolute_path = if watch_path.is_absolute() { watch_path } else { config.workspace.join(watch_path) };
            let canonical = absolute_path.canonicalize().unwrap_or_else(|_| absolute_path.clone());
            patch_base_paths.push((canonical, specificity));
            if !watched_paths.contains(&absolute_path) {
                watched_paths.push(absolute_path);
            }
        }

        let explicit_watch_paths: Vec<PathBuf> = watched_paths
            .iter()
            .filter(|wp| glob_excludes.is_match(wp.as_path()) || path_excludes.contains(wp.as_path()))
            .cloned()
            .collect();

        let watch_list: Vec<(PathBuf, RecursiveMode)> =
            watched_paths.iter().map(|path| (path.clone(), RecursiveMode::Recursive)).collect();
        let watcher = start_watcher_of::<W, _>(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res
                    && let Some(report) = Self::handle_event(
                        event,
                        &workspace,
                        &glob_excludes,
                        &path_excludes,
                        &extensions,
                        &explicit_watch_paths,
                    )
                {
                    let _ = tx.send(report);
                }
            },
            &watch_list,
            cookie,
            options.poll_interval.unwrap_or(Duration::from_millis(DEFAULT_POLL_INTERVAL_MS)),
            cookie_timeout,
        )?;
        for path in &watched_paths {
            tracing::debug!("Watching path: {}", path.display());
        }

        tracing::info!("Database watcher started for workspace: {}", config.workspace.display());

        self.watcher = Some(watcher);
        self.watched_paths = watched_paths;
        self.receiver = Some(rx);
        self.host_base_paths = host_base_paths;
        self.patch_base_paths = patch_base_paths;
        self.include_base_paths = include_base_paths;

        Ok(())
    }

    /// Stops watching if currently active.
    #[inline]
    pub fn stop(&mut self) {
        if let Some(mut watcher) = self.watcher.take() {
            for path in &self.watched_paths {
                let _ = watcher.unwatch(path);
                tracing::debug!("Stopped watching: {}", path.display());
            }
        }
        self.watched_paths.clear();
        self.receiver = None;
        self.host_base_paths.clear();
        self.patch_base_paths.clear();
        self.include_base_paths.clear();
    }

    /// Checks if the watcher is currently active.
    #[inline]
    #[must_use]
    pub fn is_watching(&self) -> bool {
        self.watcher.is_some()
    }

    /// Extracts the base directory path from a potentially glob-pattern path.
    ///
    /// For glob patterns (containing *, ?, [, {), this returns the directory portion
    /// before the first glob metacharacter. For regular paths, returns the path as-is.
    ///
    /// # Examples
    ///
    /// - `"src/**/*.php"` → `"src"`
    /// - `"lib/*/foo.php"` → `"lib"`
    /// - `"tests/fixtures"` → `"tests/fixtures"` (unchanged)
    fn extract_watch_path(pattern: &[u8]) -> PathBuf {
        let is_glob =
            pattern.contains(&b'*') || pattern.contains(&b'?') || pattern.contains(&b'[') || pattern.contains(&b'{');

        if !is_glob {
            return bytes_to_path(pattern).into_owned();
        }

        let first_glob_pos =
            pattern.iter().position(|&b| matches!(b, b'*' | b'?' | b'[' | b'{')).unwrap_or(pattern.len());

        let base = &pattern[..first_glob_pos];

        let mut end = base.len();
        while end > 0 && matches!(base[end - 1], b'/' | b'\\') {
            end -= 1;
        }
        let base = &base[..end];

        if base.is_empty() { PathBuf::from(".") } else { bytes_to_path(base).into_owned() }
    }

    fn handle_event(
        event: Event,
        workspace: &Path,
        glob_excludes: &GlobSet,
        path_excludes: &HashSet<PathBuf>,
        extensions: &HashSet<Vec<u8>>,
        explicit_watch_paths: &[PathBuf],
    ) -> Option<Report> {
        tracing::debug!("Watcher received event: kind={:?}, paths={:?}", event.kind, event.paths);

        if event.need_rescan() {
            return Some(Report::Dropped(event.paths.into_iter().next()));
        }

        // The poll watcher reports an edit as a new write time, the one change of metadata that counts as an edit.
        if matches!(event.kind, EventKind::Other | EventKind::Any | EventKind::Access(_))
            || matches!(event.kind, EventKind::Modify(ModifyKind::Metadata(kind)) if kind != MetadataKind::WriteTime)
        {
            tracing::debug!("Ignoring non-modification event: {:?}", event.kind);

            return None;
        }

        let mut changed_files = Vec::new();

        for path in event.paths {
            // Check if file has a valid extension
            if let Some(ext) = path.extension() {
                if !extensions.contains(ext.as_encoded_bytes()) {
                    continue;
                }
            } else {
                continue;
            }

            let is_explicitly_watched = explicit_watch_paths.iter().any(|wp| path.starts_with(wp));
            if !is_explicitly_watched {
                // Check glob pattern exclusions
                if glob_excludes.is_match(&path) {
                    tracing::debug!("Skipping path excluded by pattern: {}", path.display());
                    continue;
                }

                // Check exact path exclusions
                if path_excludes.contains(&path) {
                    tracing::debug!("Skipping excluded path: {}", path.display());
                    continue;
                }

                // Check if any parent directory is in path_excludes
                let mut should_skip = false;
                for ancestor in path.ancestors().skip(1) {
                    if path_excludes.contains(ancestor) {
                        tracing::debug!("Skipping path under excluded directory: {}", path.display());
                        should_skip = true;
                        break;
                    }
                }

                if should_skip {
                    continue;
                }
            }

            // Normalize to forward slashes for cross-platform determinism
            #[cfg(windows)]
            let logical_name = path
                .strip_prefix(workspace)
                .unwrap_or(&path)
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .map(|i| if *i == b'\\' { b'/' } else { *i })
                .collect::<Vec<_>>();
            #[cfg(not(windows))]
            let logical_name = path.strip_prefix(workspace).unwrap_or(&path).as_os_str().as_encoded_bytes().to_owned();

            let file_id = FileId::new(&logical_name);

            changed_files.push(ChangedFile { id: file_id, path });
        }

        if changed_files.is_empty() { None } else { Some(Report::Changed(changed_files)) }
    }

    /// Waits for file changes and updates the database.
    ///
    /// This method blocks until file changes are detected, then updates the database
    /// in place and returns the IDs of changed files. When the operating system dropped
    /// events, each watched file that differs from what the database read counts as changed.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if:
    /// - The watcher is not currently active ([`DatabaseError::WatcherNotActive`])
    /// - Updating the database with changed files fails
    #[inline]
    pub fn wait(&mut self) -> Result<Vec<FileId>, DatabaseError> {
        let Some(receiver) = &self.receiver else {
            return Err(DatabaseError::WatcherNotActive);
        };

        let config = &self.database.configuration;
        let workspace = config.workspace.as_ref().to_path_buf();

        match receiver.recv_timeout(Duration::from_millis(WAIT_INTERNAL_MS)) {
            Ok(report) => {
                let mut reports = vec![report];
                loop {
                    match receiver.recv_timeout(Duration::from_millis(WAIT_DEBOUNCE_MS)) {
                        Ok(more) => reports.push(more),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => {
                            self.stop();
                            return Err(DatabaseError::WatcherNotActive);
                        }
                    }
                }

                let mut all_changed = Vec::new();
                // The folders whose events the operating system dropped, each watched folder for an event that names none.
                let mut dropped_in: Option<Vec<PathBuf>> = None;
                for report in reports {
                    match report {
                        Report::Changed(changed_files) => all_changed.extend(changed_files),
                        Report::Dropped(folder) => dropped_in
                            .get_or_insert_default()
                            .extend(folder.map_or_else(|| self.watched_paths.clone(), |folder| vec![folder])),
                    }
                }
                if let Some(mut folders) = dropped_in {
                    folders.sort();
                    folders.dedup();
                    tracing::info!(
                        "The file watcher dropped events in {}, so this run reads the watched files again.",
                        folders.iter().map(|folder| folder.display().to_string()).collect::<Vec<_>>().join(", ")
                    );
                    all_changed.extend(self.changed_on_disk());
                }

                let mut latest_changes: HashMap<FileId, ChangedFile> = HashMap::new();
                for changed in all_changed {
                    latest_changes.insert(changed.id, changed);
                }
                let all_changed: Vec<ChangedFile> = latest_changes.into_values().collect();
                let mut changed_ids = Vec::new();
                let mut manifests = Manifests::default();

                for changed_file in &all_changed {
                    changed_ids.push(changed_file.id);

                    let Ok(file) = self.database.get(&changed_file.id) else {
                        if changed_file.path.exists() {
                            let new_file_type = classify_added_file(
                                &changed_file.path,
                                &self.host_base_paths,
                                &self.patch_base_paths,
                                &self.include_base_paths,
                            );
                            match File::read(&workspace, &changed_file.path, new_file_type) {
                                Ok(file) if file.is_companion() => {}
                                Ok(mut file) => {
                                    mark_package(&mut file, &workspace, &mut manifests);
                                    self.database.add(file);
                                    tracing::debug!("Added new file to database: {}", changed_file.path.display());
                                }
                                Err(e) => {
                                    tracing::error!("Failed to load a new file: {e}");
                                }
                            }
                        }

                        continue;
                    };

                    if !changed_file.path.exists() {
                        self.database.delete(changed_file.id);
                        tracing::trace!("Deleted file from database: {}", String::from_utf8_lossy(&file.name));
                        continue;
                    }

                    match Self::read_stable_contents(&changed_file.path) {
                        Ok(contents) => {
                            if self.database.update(changed_file.id, Cow::Owned(contents)) {
                                tracing::trace!("Updated file in database: {}", String::from_utf8_lossy(&file.name));
                            } else {
                                tracing::warn!(
                                    "Failed to update file in database (ID not found): {}",
                                    String::from_utf8_lossy(&file.name)
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to read file {}: {}", changed_file.path.display(), e);
                        }
                    }
                }

                Ok(changed_ids)
            }
            Err(RecvTimeoutError::Timeout) => Ok(Vec::new()),
            Err(RecvTimeoutError::Disconnected) => {
                self.stop();
                Err(DatabaseError::WatcherNotActive)
            }
        }
    }

    /// Each file the database's configuration holds that changed, was added or was removed since the database read it,
    /// as the loader that read the files at the start reads them now. A failed read is logged and gives no file.
    fn changed_on_disk(&self) -> Vec<ChangedFile> {
        let on_disk = match DatabaseLoader::new(self.database.configuration.clone()).load() {
            Ok(on_disk) => on_disk,
            Err(error) => {
                tracing::error!("Failed to read the watched files again: {error}");

                return Vec::new();
            }
        };

        let added_or_edited = on_disk
            .files()
            .filter(|file| !self.database.get_ref(&file.id).is_ok_and(|read| read.contents == file.contents));
        let removed = self.database.files().filter(|file| on_disk.get_ref(&file.id).is_err());

        added_or_edited
            .chain(removed)
            .filter_map(|file| Some(ChangedFile { id: file.id, path: file.path.clone()? }))
            .collect()
    }

    /// Reads file contents with a stability check to handle partial writes.
    ///
    /// Some IDEs and formatters write files in multiple steps (save, then format).
    /// This method reads the file, waits briefly, and re-reads to ensure the content
    /// has stabilized before returning.
    fn read_stable_contents(path: &Path) -> std::io::Result<Vec<u8>> {
        let contents = std::fs::read(path)?;

        std::thread::sleep(Duration::from_millis(STABILITY_CHECK_MS));

        if path.exists()
            && let Ok(reread) = std::fs::read(path)
            && reread != contents
        {
            tracing::debug!("File content changed during stability check: {}", path.display());

            return Ok(reread);
        }

        Ok(contents)
    }

    /// Returns a reference to the database.
    #[inline]
    #[must_use]
    pub fn database(&self) -> &Database<'config> {
        &self.database
    }

    /// Returns a reference to the database.
    #[inline]
    #[must_use]
    pub fn read_only_database(&self) -> ReadDatabase {
        self.database.read_only()
    }

    /// Returns a mutable reference to the database.
    #[inline]
    pub fn database_mut(&mut self) -> &mut Database<'config> {
        &mut self.database
    }

    /// Provides temporary mutable access to the database through a closure.
    ///
    /// This method helps Rust's borrow checker understand that the mutable borrow
    /// of the database is scoped to just the closure execution, allowing the watcher
    /// to be used again after the closure returns.
    ///
    /// The closure is bounded with for<'x> to explicitly show that the database
    /// reference lifetime is scoped to the closure execution only.
    #[inline]
    pub fn with_database_mut<F, R>(&mut self, f: F) -> R
    where
        F: for<'borrow> FnOnce(&'borrow mut Database<'config>) -> R,
    {
        f(&mut self.database)
    }

    /// Consumes the watcher and returns the database.
    #[inline]
    #[must_use]
    pub fn into_database(self) -> Database<'config> {
        let mut md = ManuallyDrop::new(self);
        md.stop();
        // SAFETY: `md` is a `ManuallyDrop<Self>`, so its `Drop` impl will not run; reading the
        // `database` field byte-for-byte is safe because we never read or drop it again.
        unsafe { std::ptr::read(&raw const md.database) }
    }
}

impl Drop for DatabaseWatcher<'_> {
    #[inline]
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts a watcher of each of `paths` that gives `handler` each change made after it returns, and no cookie file.
///
/// An operating system watcher can start reporting some time after it is asked to: on a busy Mac, FSEvents starts a
/// stream late and never reports a change made before then. So, as Watchman syncs its clients, the watcher writes and
/// deletes a cookie file, of a name no other watcher uses, in each watched folder, and returns once it reports every
/// cookie, since it reports each later change in that folder after it. A folder the cookie cannot be written to, such
/// as a read-only one, gets a warning that names it and the error. A watcher that reports a written cookie of no
/// folder within [`COOKIE_TIMEOUT`], as on a volume FSEvents does not cover, gives way to one that checks the files
/// every `poll_interval`, with a warning that names each such folder.
///
/// # Errors
///
/// Returns the error of starting a watcher or of watching one of `paths`.
pub fn start_watcher<H>(
    handler: H,
    paths: &[(PathBuf, RecursiveMode)],
    poll_interval: Duration,
) -> Result<Box<dyn NotifyWatcher + Send>, DatabaseError>
where
    H: EventHandler + Clone,
{
    start_watcher_of::<RecommendedWatcher, H>(handler, paths, &unique_cookie(), poll_interval, COOKIE_TIMEOUT)
}

/// A start of cookie names that no other watcher of any `mago` uses.
fn unique_cookie() -> String {
    static WATCHERS: AtomicUsize = AtomicUsize::new(0);

    format!("{COOKIE_PREFIX}{}-{}-", std::process::id(), WATCHERS.fetch_add(1, Ordering::Relaxed))
}

/// [`start_watcher`] with a watcher of type `W`, whose cookie in the folder at index `n` of the watched folders is
/// named `cookie` then `n`, and which has `cookie_timeout` to report them.
fn start_watcher_of<W, H>(
    handler: H,
    paths: &[(PathBuf, RecursiveMode)],
    cookie: &str,
    poll_interval: Duration,
    cookie_timeout: Duration,
) -> Result<Box<dyn NotifyWatcher + Send>, DatabaseError>
where
    W: NotifyWatcher + Send + 'static,
    H: EventHandler + Clone,
{
    let (reported, cookies_reported) = mpsc::channel();
    let without_cookies = |mut handler: H| {
        let cookie = cookie.to_owned();
        let reported = reported.clone();
        move |result: notify::Result<Event>| {
            let Ok(mut event) = result else {
                return handler.handle_event(result);
            };
            let named_paths = !event.paths.is_empty();
            event.paths.retain(|path| {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                if name.starts_with(&cookie) {
                    let _ = reported.send(name.to_owned());
                }

                !name.starts_with(COOKIE_PREFIX)
            });
            if !named_paths || !event.paths.is_empty() {
                handler.handle_event(Ok(event));
            }
        }
    };

    let mut watcher =
        W::new(without_cookies(handler.clone()), Config::default()).map_err(DatabaseError::WatcherInit)?;
    for (path, recursive_mode) in paths {
        watcher.watch(path, *recursive_mode).map_err(DatabaseError::WatcherWatch)?;
    }

    // Each folder's cookie, by its name, as the watcher reports a path under the folder's real path.
    let started = Instant::now();
    let mut unreported: Vec<(&Path, String)> = Vec::new();
    for (index, folder) in paths.iter().map(|(path, _)| path.as_path()).filter(|path| path.is_dir()).enumerate() {
        let name = format!("{cookie}{index}");
        let path = folder.join(&name);
        if let Err(error) = std::fs::write(&path, b"") {
            tracing::warn!(
                "Could not write the file watcher's cookie in {}, so a change made there as the watch starts can be missed: {error}",
                folder.display()
            );
            continue;
        }
        if let Err(error) = std::fs::remove_file(&path) {
            tracing::warn!("Could not delete the file watcher's cookie {}: {error}", path.display());
        }
        unreported.push((folder, name));
    }
    let deadline = started + cookie_timeout;
    while !unreported.is_empty()
        && let Ok(name) = cookies_reported.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        unreported.retain(|(_, cookie)| *cookie != name);
    }
    if unreported.is_empty() {
        tracing::debug!("The file watcher reported its cookies after {:?}.", started.elapsed());

        return Ok(Box::new(watcher));
    }

    drop(watcher);
    tracing::warn!(
        "The file watcher reported no change in {} within {cookie_timeout:?}, so this run checks the watched files every {poll_interval:?} instead.",
        unreported.iter().map(|(folder, _)| folder.display().to_string()).collect::<Vec<_>>().join(", "),
    );
    // Notify's poller compares write times in whole seconds, so it also compares contents to see a second edit within
    // the same second.
    let mut poller = PollWatcher::new(
        without_cookies(handler),
        Config::default().with_poll_interval(poll_interval).with_compare_contents(true),
    )
    .map_err(DatabaseError::WatcherInit)?;
    for (path, recursive_mode) in paths {
        poller.watch(path, *recursive_mode).map_err(DatabaseError::WatcherWatch)?;
    }

    Ok(Box::new(poller))
}

/// Picks the [`FileType`] for a newly-discovered file based on which configured base path
/// it lives under.
///
/// Computes the per-tier maximum specificity over every base path the file matched (paired
/// with the pattern's [`calculate_pattern_specificity`] score) and delegates to
/// [`resolve_file_type`] for the actual conflict resolution. The watcher and the loader
/// therefore reach the same `FileType` for the same file under the same configuration —
/// see the helper's docs for the priority rules.
fn classify_added_file(
    path: &Path,
    host_bases: &[(PathBuf, usize)],
    patch_bases: &[(PathBuf, usize)],
    include_bases: &[(PathBuf, usize)],
) -> FileType {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let max_spec = |bases: &[(PathBuf, usize)]| {
        bases.iter().filter(|(b, _)| canonical.starts_with(b.as_path())).map(|(_, s)| *s).max()
    };

    resolve_file_type(max_spec(host_bases), max_spec(include_bases), max_spec(patch_bases))
}

#[cfg(test)]
mod classify_added_file_tests {
    use super::*;

    fn bases(items: &[&str]) -> Vec<(PathBuf, usize)> {
        items.iter().map(|p| (PathBuf::from(p), calculate_pattern_specificity(p.as_bytes()))).collect()
    }

    #[test]
    fn defaults_to_host_when_no_base_matches() {
        let ft =
            classify_added_file(Path::new("/ws/orphan/foo.php"), &bases(&["/ws/src"]), &[], &bases(&["/ws/stubs"]));
        assert_eq!(ft, FileType::Host);
    }

    #[test]
    fn vendored_when_only_include_matches() {
        // Regression test: prior to this fix the watcher hardcoded `FileType::Host` for
        // every newly-discovered file, so a fresh PHP file dropped into an `includes`
        // directory at runtime was wrongly added as source and linted as such.
        let ft = classify_added_file(Path::new("/ws/stubs/foo.php"), &bases(&["/ws/src"]), &[], &bases(&["/ws/stubs"]));
        assert_eq!(ft, FileType::Vendored);
    }

    #[test]
    fn host_when_only_host_matches() {
        let ft = classify_added_file(Path::new("/ws/src/foo.php"), &bases(&["/ws/src"]), &[], &bases(&["/ws/stubs"]));
        assert_eq!(ft, FileType::Host);
    }

    #[test]
    fn matches_loader_at_equal_specificity_for_same_dir_under_both() {
        // The same directory configured under both `paths` and `includes`: vendored wins,
        // matching the loader's "vendored beats host at equal specificity" rule. Catches
        // the divergence the original watcher hit (it was awarding the file to host).
        let ft =
            classify_added_file(Path::new("/ws/shared/foo.php"), &bases(&["/ws/shared"]), &[], &bases(&["/ws/shared"]));
        assert_eq!(ft, FileType::Vendored);
    }

    #[test]
    fn include_wins_when_strictly_more_specific() {
        // An include path nested inside a host path overrides for files under the nested
        // path. Matches the loader's "vendored beats host when strictly more specific".
        let ft = classify_added_file(
            Path::new("/ws/src/vendor/stub.php"),
            &bases(&["/ws/src"]),
            &[],
            &bases(&["/ws/src/vendor"]),
        );
        assert_eq!(ft, FileType::Vendored);
    }

    #[test]
    fn exact_host_file_beats_directory_include_via_loader_score() {
        // Catches the second divergence: with the component-count heuristic the watcher
        // treated `src/` and `src/foo.php` as equally specific (both 2 components) and
        // gave the file to vendored; the loader-aligned specificity score (file × 1000
        // beats dir × 100) instead keeps it on host.
        let ft =
            classify_added_file(Path::new("/ws/src/foo.php"), &bases(&["/ws/src/foo.php"]), &[], &bases(&["/ws/src"]));
        assert_eq!(ft, FileType::Host);
    }

    #[test]
    fn patch_when_only_patch_matches() {
        let ft = classify_added_file(
            Path::new("/ws/patches/foo.php"),
            &bases(&["/ws/src"]),
            &bases(&["/ws/patches"]),
            &bases(&["/ws/stubs"]),
        );
        assert_eq!(ft, FileType::Patch);
    }

    #[test]
    fn patch_wins_over_host_when_strictly_more_specific() {
        let ft = classify_added_file(
            Path::new("/ws/src/patches/foo.php"),
            &bases(&["/ws/src"]),
            &bases(&["/ws/src/patches"]),
            &[],
        );
        assert_eq!(ft, FileType::Patch);
    }

    #[test]
    fn host_wins_over_patch_at_equal_specificity() {
        // Tie between host and patch goes to host — same precedence the loader applies.
        let ft =
            classify_added_file(Path::new("/ws/shared/foo.php"), &bases(&["/ws/shared"]), &bases(&["/ws/shared"]), &[]);
        assert_eq!(ft, FileType::Host);
    }

    #[test]
    fn patch_wins_over_include_when_both_match_without_host() {
        // No host match; patch and include both match. Patch beats vendored — matches the
        // loader's USER_DEFINED > PATCH > BUILT_IN > VENDORED tier order.
        let ft = classify_added_file(
            Path::new("/ws/overlap/foo.php"),
            &[],
            &bases(&["/ws/overlap"]),
            &bases(&["/ws/overlap"]),
        );
        assert_eq!(ft, FileType::Patch);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::PoisonError;
    use std::time::SystemTime;

    use notify::WatcherKind;
    use notify::event::CreateKind;
    use notify::event::Flag;

    use super::*;
    use crate::DatabaseConfiguration;

    /// A project folder with an `app` and a `vendor` folder, at its real path.
    fn project() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("a temporary folder should be made");
        let root = directory.path().canonicalize().expect("the temporary folder should have a real path");
        for folder in ["app", "vendor"] {
            std::fs::create_dir_all(root.join(folder)).expect("the project's folder should be made");
        }

        (directory, root)
    }

    /// Writes `contents` to the file `name` in `root`.
    fn write(root: &Path, name: &str, contents: &str) {
        std::fs::write(root.join(name), contents).expect("the file should be written");
    }

    /// Whether `path` names a cookie file.
    fn is_cookie(path: &Path) -> bool {
        path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(COOKIE_PREFIX))
    }

    /// The paths of each event `received` gives, up to the first that names `file`, or of each within a minute when
    /// none does.
    fn paths_until(received: &Receiver<notify::Result<Event>>, file: &Path) -> Vec<PathBuf> {
        let deadline = Instant::now() + Duration::from_mins(1);
        let mut paths = Vec::new();
        while !paths.iter().any(|path| path == file)
            && let Ok(event) = received.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            paths.extend(event.expect("the watcher should report no error").paths);
        }

        paths
    }

    /// No handler sees a cookie: not its watcher's own, nor the one a second watcher of the same folder writes once the
    /// first is live, and each is gone from the folder.
    #[test]
    fn a_cookie_never_reaches_a_handler() {
        let (_directory, root) = project();
        let app = [(root.join("app"), RecursiveMode::Recursive)];
        let (events, received) = mpsc::channel();

        let _watcher = start_watcher(events, &app, Duration::from_millis(50)).expect("the watcher should start");
        let _second = start_watcher(|_: notify::Result<Event>| {}, &app, Duration::from_millis(50))
            .expect("the second watcher should start");
        write(&root, "app/a.php", "<?php\n\necho 1;\n");

        let file = root.join("app/a.php");
        let paths = paths_until(&received, &file);
        assert!(paths.contains(&file), "no event names {}: {paths:?}", file.display());
        assert!(!paths.iter().any(|path| is_cookie(path)), "{paths:?}");
        let names: Vec<PathBuf> = std::fs::read_dir(root.join("app"))
            .expect("the folder should be read")
            .map(|entry| entry.expect("the folder's entry should be read").path())
            .collect();
        assert!(!names.iter().any(|path| is_cookie(path)), "{names:?}");
    }

    /// The log lines a test's subscriber writes.
    #[derive(Clone, Default)]
    struct Log(Arc<Mutex<Vec<u8>>>);

    impl Log {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
        }
    }

    impl std::io::Write for Log {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs `start` with a subscriber that keeps the log, and returns what `start` returns with the log.
    fn logged<T>(start: impl FnOnce() -> T) -> (T, String) {
        let log = Log::default();
        let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer({
            let log = log.clone();
            move || log.clone()
        });
        let started = tracing::subscriber::with_default(subscriber.finish(), start);

        (started, log.text())
    }

    /// The name each cookie of the tests' own watchers starts with.
    const TEST_COOKIE: &str = ".mago-watch-cookie-test-";

    /// A watcher that reports the cookie of each of the first `FOLDERS` folders it watches, as it starts on the folder,
    /// and nothing else, as a native watcher whose stream of each later folder never starts.
    struct ReportsCookies<const FOLDERS: usize> {
        handler: Box<dyn EventHandler>,
        watched: usize,
    }

    impl<const FOLDERS: usize> NotifyWatcher for ReportsCookies<FOLDERS> {
        fn new<F>(handler: F, _config: Config) -> notify::Result<Self>
        where
            F: EventHandler,
        {
            Ok(Self { handler: Box::new(handler), watched: 0 })
        }

        fn watch(&mut self, path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
            if self.watched < FOLDERS {
                let cookie = path.join(format!("{TEST_COOKIE}{}", self.watched));
                self.handler.handle_event(Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(cookie)));
            }
            self.watched += 1;

            Ok(())
        }

        fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> WatcherKind {
            WatcherKind::NullWatcher
        }
    }

    /// Starts a watcher of type `W` of `folders`, which has 50 ms to report its cookies and then polls every 50 ms, and
    /// returns the log of its start.
    fn start_logged<W>(folders: &[(PathBuf, RecursiveMode)]) -> String
    where
        W: NotifyWatcher + Send + 'static,
    {
        let (_watcher, log) = logged(|| {
            start_watcher_of::<W, _>(
                |_: notify::Result<Event>| {},
                folders,
                TEST_COOKIE,
                Duration::from_millis(50),
                Duration::from_millis(50),
            )
            .expect("the watcher should start")
        });

        log
    }

    /// A watcher that reports the cookie of each folder starts with no warning.
    #[test]
    fn a_watcher_that_reports_each_cookie_warns_of_nothing() {
        let (_directory, root) = project();
        let folders = [(root.join("app"), RecursiveMode::Recursive), (root.join("vendor"), RecursiveMode::Recursive)];

        let log = start_logged::<ReportsCookies<2>>(&folders);

        assert!(!log.contains("WARN"), "{log}");
    }

    /// A watcher that reports the cookie of one folder but not of another polls, and its warning names only the
    /// other.
    #[test]
    fn a_watcher_that_misses_one_folder_s_cookie_polls_and_names_that_folder() {
        let (_directory, root) = project();
        let folders = [(root.join("app"), RecursiveMode::Recursive), (root.join("vendor"), RecursiveMode::Recursive)];

        let log = start_logged::<ReportsCookies<1>>(&folders);

        assert!(
            log.contains(&format!(
                "The file watcher reported no change in {} within 50ms, so this run checks the watched files every 50ms instead.",
                root.join("vendor").display()
            )),
            "{log}"
        );
    }

    /// A folder the cookie cannot be written to gets a warning that names it and the error, and the watcher of the
    /// other folders starts without polling.
    #[cfg(unix)]
    #[test]
    fn a_folder_that_takes_no_cookie_is_named_with_the_error() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, root) = project();
        let vendor = root.join("vendor");
        let folders = [(root.join("app"), RecursiveMode::Recursive), (vendor.clone(), RecursiveMode::Recursive)];
        std::fs::set_permissions(&vendor, std::fs::Permissions::from_mode(0o555)).expect("vendor should be read-only");

        let log = start_logged::<ReportsCookies<1>>(&folders);
        std::fs::set_permissions(&vendor, std::fs::Permissions::from_mode(0o755)).expect("vendor should be writable");

        assert!(
            log.contains(&format!(
                "Could not write the file watcher's cookie in {}, so a change made there as the watch starts can be missed: Permission denied",
                vendor.display()
            )),
            "{log}"
        );
        assert!(!log.contains("checks the watched files every"), "{log}");
    }

    /// The write time of every file the poll tests save: whole seconds, so one second later is the next second.
    const SAVED_AT: Duration = Duration::from_secs(1_700_000_000);

    /// Replaces `app/a.php` in `root` with `contents` written at `modified` seconds past [`SAVED_AT`], in one rename, as
    /// an editor that saves by renaming does, so no poll sees the contents without the write time.
    fn save(root: &Path, contents: &str, modified: u64) {
        let draft = root.join("a.php.draft");
        std::fs::write(&draft, contents).expect("the draft should be written");
        std::fs::File::options()
            .write(true)
            .open(&draft)
            .and_then(|draft| draft.set_modified(SystemTime::UNIX_EPOCH + SAVED_AT + Duration::from_secs(modified)))
            .expect("the draft's write time should be set");
        std::fs::rename(&draft, root.join("app/a.php")).expect("the draft should replace the file");
    }

    /// Starts a watcher of `app` in `root` that never reports its cookie, so it polls every 50 ms, and gives the changes
    /// [`DatabaseWatcher::handle_event`] makes of each event, with the log of its start.
    fn poll_app(root: &Path) -> (Box<dyn NotifyWatcher + Send>, Receiver<Vec<ChangedFile>>, String) {
        let (changes, received) = mpsc::channel();
        let workspace = root.to_path_buf();
        let extensions = HashSet::from([b"php".to_vec()]);
        let handler = move |result: notify::Result<Event>| {
            if let Ok(event) = result
                && let Some(Report::Changed(changed)) = DatabaseWatcher::handle_event(
                    event,
                    &workspace,
                    &GlobSet::empty(),
                    &HashSet::new(),
                    &extensions,
                    &[],
                )
            {
                let _ = changes.send(changed);
            }
        };
        let (watcher, log) = logged(|| {
            start_watcher_of::<notify::NullWatcher, _>(
                handler,
                &[(root.join("app"), RecursiveMode::Recursive)],
                TEST_COOKIE,
                Duration::from_millis(50),
                Duration::from_millis(50),
            )
            .expect("the poll watcher should start")
        });

        (watcher, received, log)
    }

    /// Whether `received` gives a change of `app/a.php` in `root` within a minute.
    fn reports_a_change(received: &Receiver<Vec<ChangedFile>>, root: &Path) -> bool {
        let expected = ChangedFile { id: FileId::new(b"app/a.php"), path: root.join("app/a.php") };
        let deadline = Instant::now() + Duration::from_mins(1);

        std::iter::from_fn(|| received.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok())
            .any(|changes| changes.contains(&expected))
    }

    /// A watcher that never reports its cookie, as one on a volume FSEvents does not cover, gives way to polling: it
    /// warns, naming the folder, and an edit to a file after it returns reaches the database as a change, which the
    /// poller reports as a later write time.
    #[test]
    fn a_watcher_that_reports_no_cookie_polls_and_says_so() {
        let (_directory, root) = project();
        save(&root, "<?php echo 1;\n", 0);

        let (_watcher, received, log) = poll_app(&root);
        save(&root, "<?php echo 2;\n", 1);

        assert!(reports_a_change(&received, &root), "no change of app/a.php");
        assert!(
            log.contains(&format!(
                "The file watcher reported no change in {} within 50ms, so this run checks the watched files every 50ms instead.",
                root.join("app").display()
            )),
            "{log}"
        );
    }

    /// The poller reports a second edit within the second of the first, which leaves the file's write time as it was.
    #[test]
    fn a_poll_watcher_reports_a_second_edit_within_the_same_second() {
        let (_directory, root) = project();
        save(&root, "<?php echo 1;\n", 0);

        let (_watcher, received, _) = poll_app(&root);
        save(&root, "<?php echo 2;\n", 1);
        assert!(reports_a_change(&received, &root), "no change after the first edit");
        save(&root, "<?php echo 3;\n", 1);

        assert!(reports_a_change(&received, &root), "no change after the second edit");
    }

    /// A watcher that reports the cookie of each folder it watches, as it starts on the folder, and then only that the
    /// operating system dropped events, as inotify does when its queue overflows.
    struct DropsEvents(ReportsCookies<{ usize::MAX }>);

    impl NotifyWatcher for DropsEvents {
        fn new<F>(handler: F, config: Config) -> notify::Result<Self>
        where
            F: EventHandler,
        {
            ReportsCookies::<{ usize::MAX }>::new(handler, config).map(Self)
        }

        fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
            self.0.watch(path, recursive_mode)?;
            self.0.handler.handle_event(Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)));

            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            self.0.unwatch(path)
        }

        fn kind() -> WatcherKind {
            ReportsCookies::<{ usize::MAX }>::kind()
        }
    }

    /// Reads the files of `app` in `root` into a database, runs `change` on the files and the database, then watches
    /// `app` with that database by a [`DropsEvents`], and returns the watcher, the files its first wait reports as
    /// changed, and the log of both.
    fn wait_after_dropped_events(
        root: &Path,
        change: impl FnOnce(&mut Database<'static>),
    ) -> (DatabaseWatcher<'static>, Vec<FileId>, String) {
        let configuration = DatabaseConfiguration::new(root, vec![b"app"], vec![], vec![], vec![b"php"]).into_static();
        let mut database = DatabaseLoader::new(configuration).load().expect("the database should read the files");
        change(&mut database);

        let mut watcher = DatabaseWatcher::new(database);
        let (changed, log) = logged(|| {
            watcher
                .watch_of::<DropsEvents>(WatchOptions::default(), TEST_COOKIE, Duration::from_millis(50))
                .expect("the watcher should start");

            watcher.wait().expect("the watcher should report its changes")
        });

        (watcher, changed, log)
    }

    /// An edit whose event the operating system dropped reaches the database as a change, as the watcher reads the
    /// watched files again, with a line that says so.
    #[test]
    fn an_edit_the_operating_system_dropped_reaches_the_database() {
        let (_directory, root) = project();
        write(&root, "app/a.php", "<?php echo 1;\n");

        let (watcher, changed, log) =
            wait_after_dropped_events(&root, |_| write(&root, "app/a.php", "<?php echo 2;\n"));

        assert_eq!(changed, vec![FileId::new(b"app/a.php")]);
        let file = watcher.database().get_by_name(b"app/a.php").expect("the database should hold app/a.php");
        assert_eq!(file.contents.as_ref(), b"<?php echo 2;\n");
        let line = format!(
            "The file watcher dropped events in {}, so this run reads the watched files again.",
            root.join("app").display()
        );
        assert_eq!(log.matches(&line).count(), 1, "{log}");
    }

    /// A file added and one removed whose events the operating system dropped each reach the database as a change.
    #[test]
    fn a_file_added_and_one_removed_the_operating_system_dropped_reach_the_database() {
        let (_directory, root) = project();
        write(&root, "app/a.php", "<?php echo 1;\n");
        write(&root, "app/b.php", "<?php echo 2;\n");

        let (watcher, changed, _) = wait_after_dropped_events(&root, |_| {
            std::fs::remove_file(root.join("app/b.php")).expect("app/b.php should be removed");
            write(&root, "app/c.php", "<?php echo 3;\n");
        });

        assert_eq!(
            changed.into_iter().collect::<HashSet<_>>(),
            HashSet::from([FileId::new(b"app/b.php"), FileId::new(b"app/c.php")])
        );
        assert!(watcher.database().get_by_name(b"app/b.php").is_err(), "the database should not hold app/b.php");
        assert!(watcher.database().get_by_name(b"app/c.php").is_ok(), "the database should hold app/c.php");
    }

    /// A file with no path, as a built-in stub is, stays in the database when the operating system drops events, and is
    /// not reported as changed.
    #[test]
    fn a_file_with_no_path_stays_when_the_operating_system_drops_events() {
        let (_directory, root) = project();
        write(&root, "app/a.php", "<?php echo 1;\n");
        let (watcher, changed, _) = wait_after_dropped_events(&root, |database| {
            database.add(File::new(
                Cow::Borrowed(b"stubs/core.php"),
                FileType::Builtin,
                None,
                Cow::Borrowed(b"<?php\n"),
            ));
        });

        assert_eq!(changed, Vec::new());
        assert!(watcher.database().get_by_name(b"stubs/core.php").is_ok(), "the database should hold the stub");
    }
}
