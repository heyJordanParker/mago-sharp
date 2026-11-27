//! Database watcher for real-time file change monitoring.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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
use notify::RecommendedWatcher;
use notify::RecursiveMode;
use notify::Watcher as NotifyWatcher;
use notify::event::MetadataKind;
use notify::event::ModifyKind;

use crate::Database;
use crate::DatabaseConfiguration;
use crate::DatabaseReader;
use crate::ReadDatabase;
use crate::error::DatabaseError;
use crate::exclusion::Exclusion;
use crate::file::FileId;
use crate::file::FileType;
use crate::filesystem;
use crate::filesystem::Filesystem;
use crate::loader::DatabaseLoader;
use crate::loader::ExcludedFolders;
use crate::loader::Manifests;
use crate::loader::calculate_pattern_specificity;
use crate::loader::mark_package;
use crate::loader::read_listed;
use crate::loader::resolve_file_type;
use crate::utils::bytes_to_path;
use crate::utils::file_from;

const DEFAULT_POLL_INTERVAL_MS: u64 = 1000;
const WAIT_INTERNAL_MS: u64 = 100;
/// How long the window a round's first change opens stays open, as watchexec's `--debounce`: the changes reported
/// within it join the round, and every later one goes to the next round, so a stream of changes never holds a round back.
const WAIT_WINDOW_MS: u64 = 50;
const STABILITY_CHECK_MS: u64 = 10;

/// The start of the name of each cookie file [`start_watcher`] writes, so every watcher leaves out every cookie.
const COOKIE_PREFIX: &str = ".mago-watch-cookie-";

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
        self.watch_of::<RecommendedWatcher>(options, &unique_cookie(), remote_filesystem)
    }

    /// [`Self::watch`] with a watcher of type `W`, which [`start_watcher_of`] starts with `cookie`, polling each path
    /// `remote` names a filesystem of.
    fn watch_of<W>(
        &mut self,
        options: WatchOptions,
        cookie: &str,
        remote: fn(&Path) -> Option<&'static str>,
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
        let polled = Self::polled(config, &all_exclusions, &extensions, &explicit_watch_paths)?;
        let watcher = start_watcher_of::<W, _, _>(
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
            remote,
            polled,
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

    /// Whether a poll of the watched folders reads a folder, given as `true`, or the file at a path: each folder under
    /// one of `explicit_watch_paths`, whose changes `handle_event` reports whatever the exclusions say, and no other
    /// folder the loader skips with `exclusions`, the watch's own among them; and only a file of one of `extensions`.
    fn polled<'scope>(
        config: &DatabaseConfiguration<'scope>,
        exclusions: &[Exclusion<'scope>],
        extensions: &HashSet<Vec<u8>>,
        explicit_watch_paths: &[PathBuf],
    ) -> Result<impl Fn(&Path, bool) -> bool + Send + Sync + 'static, DatabaseError> {
        let skipped = ExcludedFolders::from_configuration(&DatabaseConfiguration {
            excludes: exclusions.to_vec(),
            ..config.clone()
        })?;
        let extensions = extensions.clone();
        // The poll gives a folder at its real path.
        let explicit: Vec<PathBuf> =
            explicit_watch_paths.iter().map(|path| path.canonicalize().unwrap_or_else(|_| path.clone())).collect();

        Ok(move |path: &Path, is_folder: bool| {
            if is_folder {
                explicit.iter().any(|root| path.starts_with(root)) || !skipped.contains(path)
            } else {
                path.extension().is_some_and(|extension| extensions.contains(extension.as_encoded_bytes()))
            }
        })
    }

    /// Stops watching if currently active.
    #[inline]
    pub fn stop(&mut self) {
        // Dropping the watcher stops it once, where FSEvents stops and starts its stream again for each path unwatched.
        if self.watcher.take().is_some() {
            for path in &self.watched_paths {
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
    /// A changed file [`read_listed`] finds gone is removed from the database.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if:
    /// - The watcher is not currently active ([`DatabaseError::WatcherNotActive`])
    /// - A changed file is there but can't be read, or is too large
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
                let closes = Instant::now() + Duration::from_millis(WAIT_WINDOW_MS);
                while let Some(open) = closes.checked_duration_since(Instant::now()).filter(|open| !open.is_zero()) {
                    match receiver.recv_timeout(open) {
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

                    let path = &changed_file.path;
                    let held = self.database.get(&changed_file.id).is_ok();
                    let read = if held { Self::read_stable_contents(path)? } else { read_listed(path)? };
                    match (read, held) {
                        (None, true) => {
                            self.database.delete(changed_file.id);
                            tracing::trace!("Deleted file from database: {}", path.display());
                        }
                        (None, false) => {}
                        (Some(contents), true) => {
                            self.database.update(changed_file.id, Cow::Owned(contents));
                            tracing::trace!("Updated file in database: {}", path.display());
                        }
                        (Some(contents), false) => {
                            let file_type = classify_added_file(
                                path,
                                &self.host_base_paths,
                                &self.patch_base_paths,
                                &self.include_base_paths,
                            );
                            let mut file = file_from(&workspace, path, file_type, contents)?;
                            mark_package(&mut file, &workspace, &mut manifests);
                            self.database.add(file);
                            tracing::debug!("Added new file to database: {}", path.display());
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

    /// The contents of the changed file at `path` once they stop changing, or none when [`read_listed`] finds it gone.
    ///
    /// Some IDEs and formatters write files in multiple steps (save, then format), so this reads the file, waits
    /// briefly, and reads it again.
    ///
    /// # Errors
    ///
    /// Returns the error of [`read_listed`].
    fn read_stable_contents(path: &Path) -> Result<Option<Vec<u8>>, DatabaseError> {
        let Some(contents) = read_listed(path)? else {
            return Ok(None);
        };

        std::thread::sleep(Duration::from_millis(STABILITY_CHECK_MS));

        let reread = read_listed(path)?;
        if reread.as_ref() != Some(&contents) {
            tracing::debug!("File content changed during stability check: {}", path.display());
        }

        Ok(reread)
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
/// A path on a filesystem whose client caches what it reads, as [`remote_filesystem`] names it, is checked every
/// `poll_interval` by a walk that compares each file's write time and size, as `notify`'s `PollWatcher` does, since the
/// operating system reports a change there late or not at all: on a container's bind mount from macOS, inotify reports
/// an edit made on the Mac up to minutes later, while the edit is readable at once. Every other path has the operating
/// system watcher, and no poll.
///
/// An operating system watcher can start reporting some time after it is asked to: on a busy Mac, FSEvents reports a
/// stream's changes minutes late and never reports a change made before the stream started. So, as Watchman syncs its
/// clients, the watcher writes and deletes a cookie file, of a name no other watcher uses, in each watched folder once
/// it started, since it reports each later change in that folder after the cookie. The cookie is waited for however
/// late it comes, on a thread of its own, and written again at a longer interval each time while the watcher has not
/// reported it, since a watcher can miss one written as its stream starts. So this returns once the watcher started:
/// what the files were as it returned is walked first, and once a folder's cookie comes, a second walk of the folder
/// gives `handler` each change made there before it. A busy FSEvents can also report a change made before the watcher started, as the `composer.lock` a
/// project wrote just before `mago analyze --watch` ran, so `handler` gets no change the watcher reports in a folder
/// before that folder's cookie. A folder the cookie cannot be written to, such as a read-only one, gets a warning that
/// names it and the error.
///
/// # Errors
///
/// Returns the error of starting the operating system watcher, of watching one of `paths`, or of starting the poll's or
/// the sync's thread.
pub fn start_watcher<H>(
    handler: H,
    paths: &[(PathBuf, RecursiveMode)],
    poll_interval: Duration,
) -> Result<Box<dyn NotifyWatcher + Send>, DatabaseError>
where
    H: EventHandler + Clone,
{
    start_watcher_of::<RecommendedWatcher, H, _>(
        handler,
        paths,
        &unique_cookie(),
        poll_interval,
        remote_filesystem,
        |_: &Path, _| true,
    )
}

/// An operating system watcher that stops on a thread of its own when dropped. FSEvents can take minutes to stop a
/// stream while fseventsd is behind, and notify 8.2's FSEvents watcher can wait forever for a run loop that missed its
/// stop, so the watch goes on while the watcher stops.
struct StopsAside(Option<Box<dyn NotifyWatcher + Send>>);

impl NotifyWatcher for StopsAside {
    fn new<F>(event_handler: F, config: Config) -> notify::Result<Self>
    where
        F: EventHandler,
    {
        Ok(Self(Some(Box::new(RecommendedWatcher::new(event_handler, config)?))))
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.0.as_mut().map_or(Ok(()), |watcher| watcher.watch(path, recursive_mode))
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.0.as_mut().map_or(Ok(()), |watcher| watcher.unwatch(path))
    }

    fn kind() -> notify::WatcherKind {
        RecommendedWatcher::kind()
    }
}

impl Drop for StopsAside {
    fn drop(&mut self) {
        if let Some(watcher) = self.0.take() {
            std::thread::spawn(move || drop(watcher));
        }
    }
}

/// A start of cookie names that no other watcher of any `mago` uses.
fn unique_cookie() -> String {
    static WATCHERS: AtomicUsize = AtomicUsize::new(0);

    format!("{COOKIE_PREFIX}{}-{}-", std::process::id(), WATCHERS.fetch_add(1, Ordering::Relaxed))
}

/// What the thread that syncs the operating system watcher of [`start_watcher_of`] hears.
enum Syncing {
    /// The watcher reported the cookie of this name.
    Reported(String),
    /// The watch stopped.
    Stop,
}

/// [`start_watcher`] with a watcher of type `W`, whose cookie in the folder at index `n` of the watched folders is
/// named `cookie` then `n`, and which polls each path `remote` names a filesystem of. A walk reads only the folders and
/// files `polled` takes, as [`Walk`] says.
fn start_watcher_of<W, H, P>(
    handler: H,
    paths: &[(PathBuf, RecursiveMode)],
    cookie: &str,
    poll_interval: Duration,
    remote: fn(&Path) -> Option<&'static str>,
    polled: P,
) -> Result<Box<dyn NotifyWatcher + Send>, DatabaseError>
where
    W: NotifyWatcher + Send + 'static,
    H: EventHandler + Clone,
    P: Fn(&Path, bool) -> bool + Send + Sync + 'static,
{
    let polled = Arc::new(polled);
    let (mut polled_paths, mut native_paths) = (Vec::new(), Vec::new());
    let mut named = Vec::new();
    for (path, recursive_mode) in paths {
        match remote(path) {
            Some(filesystem) => {
                named.push(format!("{} ({filesystem})", path.display()));
                polled_paths.push((path.clone(), *recursive_mode));
            }
            None => native_paths.push((path.clone(), *recursive_mode)),
        }
    }

    let mut watching = Watching { syncing: None, polling: None };
    if !native_paths.is_empty() {
        watching.syncing = Some(start_synced::<W, H, P>(handler.clone(), &native_paths, cookie, Arc::clone(&polled))?);
    }
    if !polled_paths.is_empty() {
        tracing::info!(
            "The file watcher checks the files in {} every {poll_interval:?}, since that filesystem reports a change late or not at all.",
            named.join(", ")
        );
        let poll = Walk { folders: polled_paths, polled };
        // The poll reports only what changes after this first walk, which ends before the watcher is returned.
        let seen = poll.walk();
        let (stop, stopped) = mpsc::channel();
        spawn_logging("mago-watch-poll", move || poll.run(seen, handler, poll_interval, &stopped))?;
        watching.polling = Some(stop);
    }

    Ok(Box::new(watching))
}

/// Starts the operating system watcher of `paths`, and a thread that writes each folder's cookie, waits for each
/// however late it comes, and then gives `handler` each change a [`Walk`] of the folder finds since this returned, as
/// [`start_watcher`] says. Returns the sender that stops both.
///
/// # Errors
///
/// Returns the error of starting the watcher, of watching one of `paths`, or of starting the thread.
fn start_synced<W, H, P>(
    mut handler: H,
    paths: &[(PathBuf, RecursiveMode)],
    cookie: &str,
    polled: Arc<P>,
) -> Result<mpsc::Sender<Syncing>, DatabaseError>
where
    W: NotifyWatcher + Send + 'static,
    H: EventHandler + Clone,
    P: Fn(&Path, bool) -> bool + Send + Sync + 'static,
{
    let (syncing, heard) = mpsc::channel();
    // Whether the watcher reported every folder's cookie, from which on `handler` gets every change it reports.
    let started = Arc::new(AtomicBool::new(false));
    let native_handler = {
        let mut handler = handler.clone();
        let cookie = cookie.to_owned();
        let reported = syncing.clone();
        let started = Arc::clone(&started);
        // The folders whose cookie the watcher reported, at the real path it reports them under.
        let mut synced: Vec<PathBuf> = Vec::new();
        move |result: notify::Result<Event>| {
            let Ok(mut event) = result else {
                return handler.handle_event(result);
            };
            let named_paths = !event.paths.is_empty();
            event.paths.retain(|path| {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                if name.starts_with(&cookie) {
                    synced.extend(path.parent().map(Path::to_path_buf));
                    let _ = reported.send(Syncing::Reported(name.to_owned()));
                }

                !name.starts_with(COOKIE_PREFIX)
            });
            // A change the watcher reports in a folder before that folder's cookie was made before it started.
            let made_since = started.load(Ordering::SeqCst)
                || event.need_rescan()
                || event.paths.iter().all(|path| synced.iter().any(|folder| path.starts_with(folder)));
            if made_since && (!named_paths || !event.paths.is_empty()) {
                handler.handle_event(Ok(event));
            }
        }
    };

    // Each folder with the name of its cookie, as the watcher reports a path under the folder's real path.
    let folders: Vec<(PathBuf, RecursiveMode, String)> = paths
        .iter()
        .filter(|(path, _)| path.is_dir())
        .enumerate()
        .map(|(index, (path, recursive_mode))| (path.clone(), *recursive_mode, format!("{cookie}{index}")))
        .collect();
    // What each folder's files are as this returns, which its walk once its cookie comes is compared with.
    let before: Vec<HashMap<PathBuf, Seen>> = folders
        .iter()
        .map(|(folder, recursive_mode, _)| {
            Walk { folders: vec![(folder.clone(), *recursive_mode)], polled: Arc::clone(&polled) }.walk()
        })
        .collect();

    let starting = Instant::now();
    let mut watcher = W::new(native_handler, Config::default()).map_err(DatabaseError::WatcherInit)?;
    // All paths at once, as FSEvents stops and starts its stream again for each path watched on its own.
    let mut added = watcher.paths_mut();
    for (path, recursive_mode) in paths {
        added.add(path, *recursive_mode).map_err(DatabaseError::WatcherWatch)?;
    }
    added.commit().map_err(DatabaseError::WatcherWatch)?;
    let watcher = StopsAside(Some(Box::new(watcher)));

    spawn_logging("mago-watch-sync", move || {
        let mut unsynced: Vec<(PathBuf, RecursiveMode, String, HashMap<PathBuf, Seen>)> = Vec::new();
        for ((folder, recursive_mode, name), seen) in folders.into_iter().zip(before) {
            match write_cookie(&folder, &name) {
                Ok(()) => unsynced.push((folder, recursive_mode, name, seen)),
                Err(error) => tracing::warn!(
                    "Could not write the file watcher's cookie in {}, so a change made there as the watch starts can be missed: {error}",
                    folder.display()
                ),
            }
        }
        // A watcher can miss a cookie written as its stream starts, so each cookie it has not reported is written again,
        // a second after the last write and twice as long each time after, up to half a minute apart.
        let (mut written, mut rewrite) = (Instant::now(), Duration::from_secs(1));
        loop {
            if unsynced.is_empty() && !started.swap(true, Ordering::SeqCst) {
                tracing::debug!("The file watcher reported its cookies after {:?}.", starting.elapsed());
            }
            let message = if unsynced.is_empty() {
                heard.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                heard.recv_timeout(rewrite.saturating_sub(written.elapsed()))
            };
            match message {
                Ok(Syncing::Reported(name)) => {
                    if let Some(index) = unsynced.iter().position(|(_, _, cookie, _)| *cookie == name) {
                        let (folder, recursive_mode, _, seen) = unsynced.remove(index);
                        let walk = Walk { folders: vec![(folder, recursive_mode)], polled: Arc::clone(&polled) };
                        report_changes(&seen, &walk.walk(), &mut handler);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    for (folder, _, name, _) in &unsynced {
                        let _ = write_cookie(folder, name);
                    }
                    (written, rewrite) = (Instant::now(), (rewrite * 2).min(Duration::from_secs(30)));
                }
                Ok(Syncing::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(watcher);
    })?;

    Ok(syncing)
}

/// Runs `work` on a thread named `name`, which logs where its caller logs.
fn spawn_logging(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), DatabaseError> {
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || tracing::dispatcher::with_default(&dispatch, work))
        .map(drop)
        .map_err(|error| DatabaseError::WatcherInit(notify::Error::io(error)))
}

/// Writes and deletes the cookie `name` in `folder`, which the operating system watcher reports once it watches it.
fn write_cookie(folder: &Path, name: &str) -> std::io::Result<()> {
    let path = folder.join(name);
    std::fs::write(&path, b"")?;
    if let Err(error) = std::fs::remove_file(&path) {
        tracing::warn!("Could not delete the file watcher's cookie {}: {error}", path.display());
    }

    Ok(())
}

/// What a file was at a walk: its size and write time, the status a walk reads without the file's contents.
#[derive(Clone, PartialEq, Eq)]
struct Seen {
    size: u64,
    modified: Option<std::time::SystemTime>,
}

/// A walk of folders that reads each file's status, which the poll of a folder on a filesystem [`remote_filesystem`]
/// names repeats every poll interval, and a folder's sync walks once its cookie comes.
struct Walk<P> {
    /// Each walked folder, or file, and how deep the walk reads it.
    folders: Vec<(PathBuf, RecursiveMode)>,
    /// Whether a walk reads the folder, given as `true`, or the file at a path. A folder it does not read it does not
    /// go into either.
    polled: Arc<P>,
}

impl<P> Walk<P>
where
    P: Fn(&Path, bool) -> bool,
{
    /// Walks the folders every `poll_interval`, counted from the start of the last walk, until `stopped` ends, and gives
    /// `handler` each change since the walk that gave `seen`.
    fn run<H>(self, mut seen: HashMap<PathBuf, Seen>, mut handler: H, poll_interval: Duration, stopped: &Receiver<()>)
    where
        H: EventHandler,
    {
        let mut walked = Instant::now();
        while stopped.recv_timeout(poll_interval.saturating_sub(walked.elapsed())) == Err(RecvTimeoutError::Timeout) {
            walked = Instant::now();
            let now = self.walk();
            report_changes(&seen, &now, &mut handler);
            seen = now;
        }
    }

    /// Each file of the walked folders that `polled` reads, other than a cookie, with its status now.
    fn walk(&self) -> HashMap<PathBuf, Seen> {
        let mut seen = HashMap::new();
        for (folder, recursive_mode) in &self.folders {
            let real = folder.canonicalize().unwrap_or_else(|_| folder.clone());
            let depth = if *recursive_mode == RecursiveMode::Recursive { usize::MAX } else { 1 };
            let walk = walkdir::WalkDir::new(folder).follow_links(true).max_depth(depth).into_iter();
            for entry in walk.filter_entry(|entry| {
                // A folder is matched at its real path, as the loader matches it, the walked folder itself among them.
                let relative = entry.path().strip_prefix(folder).unwrap_or(entry.path());
                !entry.file_type().is_dir()
                    || (self.polled)(&if entry.depth() == 0 { real.clone() } else { real.join(relative) }, true)
            }) {
                let Ok(entry) = entry else { continue };
                let path = entry.path();
                if entry.file_type().is_dir()
                    || path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(COOKIE_PREFIX))
                    || !(self.polled)(path, false)
                {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else { continue };
                seen.insert(path.to_path_buf(), Seen { size: metadata.len(), modified: metadata.modified().ok() });
            }
        }

        seen
    }
}

/// Gives `handler` each file `now` holds that `before` lacks as made, each whose status changed as edited, and each
/// `before` holds that `now` lacks as removed.
fn report_changes<H>(before: &HashMap<PathBuf, Seen>, now: &HashMap<PathBuf, Seen>, handler: &mut H)
where
    H: EventHandler,
{
    for (path, seen) in now {
        let kind = match before.get(path) {
            None => EventKind::Create(notify::event::CreateKind::File),
            Some(earlier) if earlier != seen => EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Content)),
            Some(_) => continue,
        };
        handler.handle_event(Ok(Event::new(kind).add_path(path.clone())));
    }
    for path in before.keys().filter(|path| !now.contains_key(*path)) {
        let kind = EventKind::Remove(notify::event::RemoveKind::File);
        handler.handle_event(Ok(Event::new(kind).add_path(path.clone())));
    }
}

/// The watch [`start_watcher`] gives: the stops of its poll, when it polls, and of its operating system watcher, when
/// it has one. Dropping it stops both: the poll at its next check, without waiting for a walk, and the operating system
/// watcher aside.
struct Watching {
    syncing: Option<mpsc::Sender<Syncing>>,
    #[allow(dead_code, reason = "dropping it stops the poll")]
    polling: Option<mpsc::Sender<()>>,
}

impl Drop for Watching {
    fn drop(&mut self) {
        if let Some(syncing) = &self.syncing {
            let _ = syncing.send(Syncing::Stop);
        }
    }
}

impl NotifyWatcher for Watching {
    fn new<F>(_event_handler: F, _config: Config) -> notify::Result<Self>
    where
        F: EventHandler,
    {
        Err(notify::Error::generic("a watch starts only through start_watcher"))
    }

    fn watch(&mut self, _path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
        Err(notify::Error::generic("a watch watches only the paths start_watcher gave it"))
    }

    fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
        Err(notify::Error::generic("a watch watches only the paths start_watcher gave it"))
    }

    fn kind() -> notify::WatcherKind {
        RecommendedWatcher::kind()
    }
}

/// The name of the filesystem that holds `path` when its client caches a file's status and contents, so a change
/// there reaches the operating system watcher late or not at all, as [`filesystem::of`] names it.
fn remote_filesystem(path: &Path) -> Option<&'static str> {
    match filesystem::of(path) {
        Some(Filesystem::Remote(name)) => Some(name),
        _ => None,
    }
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

    use notify::WatcherKind;
    use notify::event::CreateKind;
    use notify::event::DataChange;
    use notify::event::Flag;

    use super::*;
    use crate::error::IOAction;
    use crate::file::File;

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

    /// An operating system watcher of `folder` that reports every change, cookies included, from the time it returns: it
    /// returns once it reported a file it wrote there.
    fn live_watcher(folder: &Path) -> (RecommendedWatcher, Receiver<notify::Result<Event>>) {
        let (events, received) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(events, Config::default()).expect("the watcher should start");
        watcher.watch(folder, RecursiveMode::Recursive).expect("the folder should be watched");
        let ready = folder.join("ready");
        let deadline = Instant::now() + Duration::from_mins(1);
        loop {
            std::fs::write(&ready, "").expect("the file should be written");
            let reported = received
                .recv_timeout(Duration::from_millis(100))
                .is_ok_and(|event| event.is_ok_and(|event| event.paths.contains(&ready)));
            if reported {
                return (watcher, received);
            }
            assert!(Instant::now() < deadline, "the watcher reported no write of {} within a minute", ready.display());
        }
    }

    /// The cookies whose delete `received` reports, once `count` of them, or each within a minute.
    fn removed_cookies(received: &Receiver<notify::Result<Event>>, count: usize) -> HashSet<PathBuf> {
        let deadline = Instant::now() + Duration::from_mins(1);
        let mut removed = HashSet::new();
        while removed.len() < count
            && let Ok(event) = received.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            let event = event.expect("the watcher should report no error");
            if event.kind.is_remove() {
                removed.extend(event.paths.into_iter().filter(|path| is_cookie(path)));
            }
        }

        removed
    }

    /// No handler sees a cookie: not its watcher's own, nor the one a second watcher of the same folder writes once the
    /// first is live, and each is gone from the folder once its delete is reported.
    #[test]
    fn a_cookie_never_reaches_a_handler() {
        let (_directory, root) = project();
        let app = [(root.join("app"), RecursiveMode::Recursive)];
        let (_live, reported) = live_watcher(&root.join("app"));
        let (events, received) = mpsc::channel();

        let _watcher = start_watcher(events, &app, Duration::from_millis(50)).expect("the watcher should start");
        let _second = start_watcher(|_: notify::Result<Event>| {}, &app, Duration::from_millis(50))
            .expect("the second watcher should start");
        let cookies = removed_cookies(&reported, 2);
        assert_eq!(cookies.len(), 2, "each watcher's cookie should be deleted: {cookies:?}");
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

        /// A dispatcher that writes each log line to this log. While one dispatcher exists, tracing keeps a log line's
        /// interest from the dispatcher of the first thread to reach it, so a test running beside this one with none
        /// would silence the line here: a second dispatcher, alive for the whole run, makes tracing ask every one.
        fn dispatch(&self) -> tracing::Dispatch {
            static SECOND: std::sync::LazyLock<tracing::Dispatch> =
                std::sync::LazyLock::new(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
            std::sync::LazyLock::force(&SECOND);
            let log = self.clone();

            tracing::Dispatch::new(
                tracing_subscriber::fmt()
                    .with_ansi(false)
                    .with_max_level(tracing::Level::DEBUG)
                    .with_writer(move || log.clone())
                    .finish(),
            )
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
        let started = tracing::dispatcher::with_default(&log.dispatch(), start);

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

    /// Starts a watcher of type `W` of `folders` on no polled filesystem, and returns it with the log its threads write.
    fn start_logged<W>(folders: &[(PathBuf, RecursiveMode)]) -> (Box<dyn NotifyWatcher + Send>, Log)
    where
        W: NotifyWatcher + Send + 'static,
    {
        let log = Log::default();
        let watcher = tracing::dispatcher::with_default(&log.dispatch(), || {
            start_watcher_of::<W, _, _>(
                |_: notify::Result<Event>| {},
                folders,
                TEST_COOKIE,
                Duration::from_millis(50),
                |_| None,
                |_: &Path, _| true,
            )
            .expect("the watcher should start")
        });

        (watcher, log)
    }

    /// Whether `log` holds `line` within a minute.
    fn logs(log: &Log, line: &str) -> bool {
        let deadline = Instant::now() + Duration::from_mins(1);
        while !log.text().contains(line) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        true
    }

    /// A watcher that reports, in each folder it watches, a `composer.lock` written before it started and then the
    /// folder's cookie, as a busy FSEvents does.
    struct ReportsAnEarlierChange {
        handler: Box<dyn EventHandler>,
        watched: usize,
    }

    impl NotifyWatcher for ReportsAnEarlierChange {
        fn new<F>(handler: F, _config: Config) -> notify::Result<Self>
        where
            F: EventHandler,
        {
            Ok(Self { handler: Box::new(handler), watched: 0 })
        }

        fn watch(&mut self, path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
            for file in [path.join("composer.lock"), path.join(format!("{TEST_COOKIE}{}", self.watched))] {
                self.handler.handle_event(Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(file)));
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

    /// A change the watcher reports before its cookies, made before it started, reaches no handler, so a watch that
    /// starts just after a project wrote its `composer.lock` does not restart for it.
    #[test]
    fn a_change_reported_before_the_cookies_reaches_no_handler() {
        let (_directory, root) = project();
        let folders = [(root.join("app"), RecursiveMode::Recursive), (root.join("vendor"), RecursiveMode::Recursive)];
        let (events, received) = mpsc::channel();
        let log = Log::default();

        let _watcher = tracing::dispatcher::with_default(&log.dispatch(), || {
            start_watcher_of::<ReportsAnEarlierChange, _, _>(
                events,
                &folders,
                TEST_COOKIE,
                Duration::from_millis(50),
                |_| None,
                |_: &Path, _| true,
            )
            .expect("the watcher should start")
        });

        assert!(logs(&log, "The file watcher reported its cookies after"), "{}", log.text());
        let reported: Vec<_> = received.try_iter().collect();
        assert!(reported.is_empty(), "{reported:?}");
    }

    /// A watcher that fails to start, as one whose operating system refuses another watch.
    struct FailsToStart;

    impl NotifyWatcher for FailsToStart {
        fn new<F>(_handler: F, _config: Config) -> notify::Result<Self>
        where
            F: EventHandler,
        {
            Err(notify::Error::generic("the operating system refused the watch"))
        }

        fn watch(&mut self, _path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
            Ok(())
        }

        fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> WatcherKind {
            WatcherKind::NullWatcher
        }
    }

    /// A watcher that fails to start fails the watch with its error, so a watch that can't see a change never runs.
    #[test]
    fn a_watcher_that_fails_to_start_fails_the_watch() {
        let (_directory, root) = project();
        let configuration = DatabaseConfiguration::new(&root, vec![b"app"], vec![], vec![], vec![b"php"]).into_static();
        let database = DatabaseLoader::new(configuration).load().expect("the database should read the files");
        let mut watcher = DatabaseWatcher::new(database);

        let started = watcher.watch_of::<FailsToStart>(WatchOptions::default(), TEST_COOKIE, |_| None);

        let Err(DatabaseError::WatcherInit(error)) = started else {
            panic!("a watcher that fails to start should fail the watch, not give {started:?}");
        };
        assert!(error.to_string().contains("the operating system refused the watch"), "{error}");
        assert!(!watcher.is_watching(), "the watch should not run");
    }

    /// A watcher that never finishes stopping, as notify's FSEvents watcher waits forever for a run loop that missed its
    /// stop.
    struct NeverStops;

    impl Drop for NeverStops {
        fn drop(&mut self) {
            // Nothing ever sends on the channel while its sender lives, so the stop waits for good.
            let (_sender, never) = mpsc::channel::<()>();
            let _ = never.recv();
        }
    }

    impl NotifyWatcher for NeverStops {
        fn new<F>(_handler: F, _config: Config) -> notify::Result<Self>
        where
            F: EventHandler,
        {
            Ok(Self)
        }

        fn watch(&mut self, _path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
            Ok(())
        }

        fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> WatcherKind {
            WatcherKind::NullWatcher
        }
    }

    /// Dropping a watcher that never finishes stopping returns, so a watch restarts while the old watcher stops.
    #[test]
    fn dropping_a_watcher_that_never_stops_returns() {
        let (dropped, returned) = mpsc::channel();

        std::thread::spawn(move || {
            drop(StopsAside(Some(Box::new(NeverStops))));
            let _ = dropped.send(());
        });

        assert!(returned.recv_timeout(Duration::from_mins(1)).is_ok(), "the drop waited for the stop");
    }

    /// A watcher that reports the cookie of each folder starts with no warning.
    #[test]
    fn a_watcher_that_reports_each_cookie_warns_of_nothing() {
        let (_directory, root) = project();
        let folders = [(root.join("app"), RecursiveMode::Recursive), (root.join("vendor"), RecursiveMode::Recursive)];

        let (_watcher, log) = start_logged::<ReportsCookies<2>>(&folders);

        assert!(logs(&log, "The file watcher reported its cookies after"), "{}", log.text());
        assert!(!log.text().contains("WARN"), "{}", log.text());
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

        let (_watcher, log) = start_logged::<ReportsCookies<1>>(&folders);
        let line = format!(
            "Could not write the file watcher's cookie in {}, so a change made there as the watch starts can be missed: Permission denied",
            vendor.display()
        );
        let warned = logs(&log, &line);
        std::fs::set_permissions(&vendor, std::fs::Permissions::from_mode(0o755)).expect("vendor should be writable");

        assert!(warned, "{}", log.text());
        assert!(!log.text().contains("checks the files"), "{}", log.text());
    }

    /// Starts a watch of `app` in `root` on a filesystem the watch polls every 50 ms, with no operating system watcher,
    /// and gives the changes [`DatabaseWatcher::handle_event`] makes of each event, with the log of its start.
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
            start_watcher_of::<notify::NullWatcher, _, _>(
                handler,
                &[(root.join("app"), RecursiveMode::Recursive)],
                TEST_COOKIE,
                Duration::from_millis(50),
                |_| Some("nfs"),
                |_: &Path, _| true,
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

    /// A folder on a filesystem whose client caches what it reads, as a container's bind mount from macOS, is polled:
    /// the log names it with its filesystem, and an edit after the watch returns reaches the handler as a change.
    #[test]
    fn a_folder_on_a_polled_filesystem_is_polled_and_the_log_says_so() {
        let (_directory, root) = project();
        write(&root, "app/a.php", "<?php echo 1;\n");

        let (_watcher, received, log) = poll_app(&root);
        write(&root, "app/a.php", "<?php echo 22;\n");

        assert!(reports_a_change(&received, &root), "no change of app/a.php");
        assert!(
            log.contains(&format!(
                "The file watcher checks the files in {} (nfs) every 50ms, since that filesystem reports a change late or not at all.",
                root.join("app").display()
            )),
            "{log}"
        );
    }

    /// An edit of a file on a polled filesystem that changes its contents and size reaches the database in the next
    /// round of the watch.
    #[test]
    fn an_edit_on_a_polled_filesystem_reaches_the_next_round() {
        let (_directory, root) = project();
        write(&root, "app/a.php", "<?php echo 1;\n");
        let configuration = DatabaseConfiguration::new(&root, vec![b"app"], vec![], vec![], vec![b"php"]).into_static();
        let database = DatabaseLoader::new(configuration).load().expect("the database should read the files");
        let mut watcher = DatabaseWatcher::new(database);
        let options = WatchOptions { poll_interval: Some(Duration::from_millis(50)), ..WatchOptions::default() };
        watcher.watch_of::<FailsToStart>(options, TEST_COOKIE, |_| Some("nfs")).expect("the watcher should start");

        write(&root, "app/a.php", "<?php echo 'edited';\n");
        let round = changes_within_a_minute(&mut watcher);

        assert_eq!(round, vec![FileId::new(b"app/a.php")]);
        let file = watcher.database().get_by_name(b"app/a.php").expect("the database should hold app/a.php");
        assert_eq!(file.contents.as_ref(), b"<?php echo 'edited';\n");
    }

    /// The first round of `watcher` that holds a change, within a minute.
    fn changes_within_a_minute(watcher: &mut DatabaseWatcher<'static>) -> Vec<FileId> {
        let deadline = Instant::now() + Duration::from_mins(1);
        loop {
            let changed = watcher.wait().expect("the watcher should report its changes");
            if !changed.is_empty() || Instant::now() >= deadline {
                return changed;
            }
        }
    }

    /// A poll reads each file's status alone, never its contents: a poll of 1,000 unchanged files that a read would
    /// wait on forever, as a named pipe with no writer, ends, and an edit beside them reaches the handler.
    #[cfg(unix)]
    #[test]
    fn a_poll_of_unchanged_files_reads_no_file_s_contents() {
        let (_directory, root) = project();
        let pipes: Vec<PathBuf> = (0..1000).map(|index| root.join(format!("app/p{index:04}.php"))).collect();
        let made =
            crate::process::status(std::process::Command::new("mkfifo").args(&pipes)).expect("mkfifo should run");
        assert!(made.success(), "mkfifo should make the pipes");
        let (events, received) = mpsc::channel();
        let (started, starts) = mpsc::channel();
        let app = root.join("app");
        std::thread::spawn(move || {
            let watcher = start_watcher_of::<notify::NullWatcher, _, _>(
                events,
                &[(app, RecursiveMode::Recursive)],
                TEST_COOKIE,
                Duration::from_millis(50),
                |_| Some("nfs"),
                |_: &Path, _| true,
            );
            let _ = started.send(watcher);
        });
        let _watcher = starts
            .recv_timeout(Duration::from_mins(1))
            .expect("the first poll should end")
            .expect("the watcher should start");

        let edited = root.join("app/a.php");
        write(&root, "app/a.php", "<?php echo 1;\n");
        let paths = paths_until(&received, &edited);

        assert!(paths.contains(&edited), "no poll reported app/a.php: {paths:?}");
        assert!(!paths.iter().any(|path| pipes.contains(path)), "{paths:?}");
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
    /// `app` with that database by a [`DropsEvents`], and returns the watcher, the files the round of the dropped events
    /// reports as changed, and the log of both.
    fn wait_after_dropped_events(
        root: &Path,
        change: impl FnOnce(&mut Database<'static>),
    ) -> (DatabaseWatcher<'static>, Vec<FileId>, String) {
        let configuration = DatabaseConfiguration::new(root, vec![b"app"], vec![], vec![], vec![b"php"]).into_static();
        let mut database = DatabaseLoader::new(configuration).load().expect("the database should read the files");
        change(&mut database);

        let mut watcher = DatabaseWatcher::new(database);
        let log = Log::default();
        let changed = tracing::dispatcher::with_default(&log.dispatch(), || {
            watcher
                .watch_of::<DropsEvents>(WatchOptions::default(), TEST_COOKIE, |_| None)
                .expect("the watcher should start");
            let deadline = Instant::now() + Duration::from_mins(1);
            loop {
                let changed = watcher.wait().expect("the watcher should report its changes");
                if log.text().contains("dropped events") || Instant::now() >= deadline {
                    break changed;
                }
            }
        });

        (watcher, changed, log.text())
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

    /// A watcher that reports an edit of the `changed.php` of each folder it watches, as it starts on the folder.
    struct EditsChangedPhp(ReportsCookies<{ usize::MAX }>);

    impl NotifyWatcher for EditsChangedPhp {
        fn new<F>(handler: F, config: Config) -> notify::Result<Self>
        where
            F: EventHandler,
        {
            ReportsCookies::<{ usize::MAX }>::new(handler, config).map(Self)
        }

        fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
            self.0.watch(path, recursive_mode)?;
            let edit = EventKind::Modify(ModifyKind::Data(DataChange::Content));
            self.0.handler.handle_event(Ok(Event::new(edit).add_path(path.join("changed.php"))));

            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            self.0.unwatch(path)
        }

        fn kind() -> WatcherKind {
            ReportsCookies::<{ usize::MAX }>::kind()
        }
    }

    /// Reads the files of `app` in `root` into a database, runs `change` on the files, then watches `app` with that
    /// database by an [`EditsChangedPhp`], and returns the watcher and what its first round with a change gives.
    fn wait_after_an_edit_of_changed_php(
        root: &Path,
        change: impl FnOnce(),
    ) -> (DatabaseWatcher<'static>, Result<Vec<FileId>, DatabaseError>) {
        let configuration = DatabaseConfiguration::new(root, vec![b"app"], vec![], vec![], vec![b"php"]).into_static();
        let database = DatabaseLoader::new(configuration).load().expect("the database should read the files");
        change();

        let mut watcher = DatabaseWatcher::new(database);
        watcher
            .watch_of::<EditsChangedPhp>(WatchOptions::default(), TEST_COOKIE, |_| None)
            .expect("the watcher should start");
        let deadline = Instant::now() + Duration::from_mins(1);
        let waited = loop {
            match watcher.wait() {
                Ok(changed) if changed.is_empty() && Instant::now() < deadline => {}
                waited => break waited,
            }
        };

        (watcher, waited)
    }

    /// A changed file the watcher finds gone when it reads it is removed from the database, as one deleted after the
    /// edit the watcher reported, and the wait goes on.
    #[test]
    fn a_changed_file_found_gone_is_removed_from_the_database() {
        let (_directory, root) = project();
        write(&root, "app/changed.php", "<?php echo 1;\n");
        write(&root, "app/other.php", "<?php echo 2;\n");

        let (watcher, waited) = wait_after_an_edit_of_changed_php(&root, || {
            std::fs::remove_file(root.join("app/changed.php")).expect("app/changed.php should be removed");
        });

        assert_eq!(waited.expect("the wait should go on"), vec![FileId::new(b"app/changed.php")]);
        assert!(watcher.database().get_by_name(b"app/changed.php").is_err(), "the database should not hold it");
        assert!(watcher.database().get_by_name(b"app/other.php").is_ok(), "the database should hold app/other.php");
    }

    /// A changed file the watcher can't read for any reason other than its absence fails the wait, naming the file.
    /// Root reads a file of mode 000, so the test has nothing to show there.
    #[cfg(unix)]
    #[test]
    fn a_changed_file_the_watcher_cannot_read_fails_the_wait() {
        use std::os::unix::fs::PermissionsExt;

        let (_directory, root) = project();
        write(&root, "app/changed.php", "<?php echo 1;\n");
        let file = root.join("app/changed.php");
        let mode = |mode| std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).expect("set the mode");

        let (_watcher, waited) = wait_after_an_edit_of_changed_php(&root, || mode(0o000));
        let readable = std::fs::read(&file).is_ok();
        mode(0o644);
        if readable {
            return;
        }

        let Err(DatabaseError::IOError { action: IOAction::ReadFile, path, .. }) = waited else {
            panic!("a changed file that can't be read should fail the wait, not give {waited:?}");
        };
        assert_eq!(path, file);
    }

    /// A walk of the watch reads no folder the loader skips, as one the watch's own `vendor` exclusion names, and no
    /// file of an extension the watch does not check.
    #[test]
    fn a_walk_reads_no_excluded_folder_and_no_file_of_another_extension() {
        let (_directory, root) = project();
        std::fs::create_dir_all(root.join("app/vendor")).expect("app/vendor should be made");
        write(&root, "app/a.php", "<?php echo 1;\n");
        write(&root, "app/notes.txt", "notes\n");
        write(&root, "app/vendor/b.php", "<?php echo 2;\n");
        let configuration = DatabaseConfiguration::new(&root, vec![b"app"], vec![], vec![], vec![b"php"]);
        let exclusions = [Exclusion::Pattern(Cow::Borrowed("**/vendor/**"))];
        let polled = DatabaseWatcher::polled(&configuration, &exclusions, &HashSet::from([b"php".to_vec()]), &[])
            .expect("the exclusions should build");
        let walk = Walk { folders: vec![(root.join("app"), RecursiveMode::Recursive)], polled: Arc::new(polled) };

        let read: HashSet<PathBuf> = walk.walk().into_keys().collect();

        assert_eq!(read, HashSet::from([root.join("app/a.php")]));
    }

    /// A walk reads nothing of a watched folder the exclusions name, as `vendor` when it is an `includes` root, which
    /// the watch's own `**/vendor/**` exclusion keeps out of what the watcher reports, and every folder of a watched
    /// folder the watch reports whatever the exclusions say, as an `includes` root inside a `vendor` folder, its own
    /// `vendor` folder among them.
    #[test]
    fn a_walk_reads_nothing_of_an_excluded_root_and_all_of_an_explicitly_watched_one() {
        let (_directory, root) = project();
        for folder in ["vendor/symfony/console", "plugins/vendor/acme/src", "plugins/vendor/acme/vendor"] {
            std::fs::create_dir_all(root.join(folder)).expect("the folder should be made");
        }
        write(&root, "vendor/symfony/console/c.php", "<?php echo 1;\n");
        write(&root, "plugins/vendor/acme/src/d.php", "<?php echo 2;\n");
        write(&root, "plugins/vendor/acme/vendor/e.php", "<?php echo 3;\n");
        let configuration = DatabaseConfiguration::new(&root, vec![], vec![b"vendor"], vec![], vec![b"php"]);
        let exclusions = [Exclusion::Pattern(Cow::Borrowed("**/vendor/**"))];
        let explicit = [root.join("plugins/vendor/acme")];
        let polled = DatabaseWatcher::polled(&configuration, &exclusions, &HashSet::from([b"php".to_vec()]), &explicit)
            .expect("the exclusions should build");
        let walk = Walk {
            folders: vec![
                (root.join("vendor"), RecursiveMode::Recursive),
                (root.join("plugins/vendor/acme"), RecursiveMode::Recursive),
            ],
            polled: Arc::new(polled),
        };

        let read: HashSet<PathBuf> = walk.walk().into_keys().collect();

        assert_eq!(
            read,
            HashSet::from([root.join("plugins/vendor/acme/src/d.php"), root.join("plugins/vendor/acme/vendor/e.php")])
        );
    }

    /// Only a folder on a polled filesystem is polled: once the watcher of `app` synced, an edit in `vendor`, which the
    /// watch polls, reaches the handler, and one in `app`, which the operating system watcher alone reports, does not.
    #[test]
    fn a_poll_reads_only_the_folders_on_a_polled_filesystem() {
        let (_directory, root) = project();
        let folders = [(root.join("app"), RecursiveMode::Recursive), (root.join("vendor"), RecursiveMode::Recursive)];
        let (events, received) = mpsc::channel();
        let log = Log::default();
        let _watcher = tracing::dispatcher::with_default(&log.dispatch(), || {
            start_watcher_of::<ReportsCookies<1>, _, _>(
                events,
                &folders,
                TEST_COOKIE,
                Duration::from_millis(50),
                |path: &Path| path.ends_with("vendor").then_some("nfs"),
                |_: &Path, _| true,
            )
            .expect("the watcher should start")
        });
        assert!(logs(&log, "The file watcher reported its cookies after"), "{}", log.text());

        write(&root, "app/a.php", "<?php echo 1;\n");
        write(&root, "vendor/b.php", "<?php echo 2;\n");

        let polled = root.join("vendor/b.php");
        let mut paths = paths_until(&received, &polled);
        // An edit of app made before the one of vendor would come from the same poll, or from an earlier one.
        while let Ok(event) = received.recv_timeout(Duration::from_millis(250)) {
            paths.extend(event.expect("the poll should report no error").paths);
        }
        assert!(paths.contains(&polled), "{paths:?}");
        assert!(!paths.contains(&root.join("app/a.php")), "{paths:?}");
    }

    /// The handler of each folder a [`ReportsOnRequest`] watcher watches, through which its test reports what the
    /// watcher sees there.
    type Requested = Arc<Mutex<Box<dyn EventHandler>>>;
    static ON_REQUEST: Mutex<Vec<(PathBuf, Requested)>> = Mutex::new(Vec::new());

    /// A watcher that reports what its test asks through [`report`], and, when `COOKIES`, each folder's cookie as it
    /// starts on the folder.
    struct ReportsOnRequest<const COOKIES: bool> {
        handler: Requested,
        watched: usize,
    }

    impl<const COOKIES: bool> NotifyWatcher for ReportsOnRequest<COOKIES> {
        fn new<F>(handler: F, _config: Config) -> notify::Result<Self>
        where
            F: EventHandler,
        {
            Ok(Self { handler: Arc::new(Mutex::new(Box::new(handler))), watched: 0 })
        }

        fn watch(&mut self, path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
            if COOKIES {
                let cookie = path.join(format!("{TEST_COOKIE}{}", self.watched));
                self.handler
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .handle_event(Ok(Event::new(EventKind::Create(CreateKind::File)).add_path(cookie)));
            }
            self.watched += 1;
            ON_REQUEST
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((path.to_path_buf(), Arc::clone(&self.handler)));

            Ok(())
        }

        fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> WatcherKind {
            WatcherKind::NullWatcher
        }
    }

    /// Has the [`ReportsOnRequest`] watcher of `folder` report that the file at `path` was edited, once it watches the
    /// folder.
    fn report(folder: &Path, path: PathBuf) {
        let deadline = Instant::now() + Duration::from_mins(1);
        let handler = loop {
            let watching = ON_REQUEST
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|(watched, _)| watched == folder)
                .map(|(_, handler)| Arc::clone(handler));
            match watching {
                Some(handler) => break handler,
                None => {
                    assert!(Instant::now() < deadline, "no watcher watches {}", folder.display());
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        let edit = EventKind::Modify(ModifyKind::Data(DataChange::Content));
        handler.lock().unwrap_or_else(PoisonError::into_inner).handle_event(Ok(Event::new(edit).add_path(path)));
    }

    /// A stream of edits to another file never holds back the round of an earlier edit: the round that holds
    /// `app/a.php` starts within 200 ms of its edit, while `app/b.php` changes every 100 ms for 2 s.
    #[test]
    fn a_stream_of_edits_to_another_file_never_holds_back_the_round_of_an_earlier_edit() {
        let (_directory, root) = project();
        write(&root, "app/a.php", "<?php echo 1;\n");
        write(&root, "app/b.php", "<?php echo 1;\n");
        let configuration = DatabaseConfiguration::new(&root, vec![b"app"], vec![], vec![], vec![b"php"]).into_static();
        let database = DatabaseLoader::new(configuration).load().expect("the database should read the files");
        let mut watcher = DatabaseWatcher::new(database);
        let log = Log::default();
        tracing::dispatcher::with_default(&log.dispatch(), || {
            watcher.watch_of::<ReportsOnRequest<true>>(WatchOptions::default(), TEST_COOKIE, |_| None)
        })
        .expect("the watcher should start");
        assert!(logs(&log, "The file watcher reported its cookies after"), "{}", log.text());
        let app = root.join("app");

        write(&root, "app/a.php", "<?php echo 2;\n");
        let edited = Instant::now();
        report(&app, app.join("a.php"));
        let stream = std::thread::spawn(move || {
            for count in 0..20 {
                std::thread::sleep(Duration::from_millis(100));
                std::fs::write(app.join("b.php"), format!("<?php echo {count};\n")).expect("b.php should be written");
                report(&app, app.join("b.php"));
            }
        });
        let round = watcher.wait().expect("the watcher should report its changes");
        let started = edited.elapsed();
        stream.join().expect("the stream of edits should end");

        assert!(round.contains(&FileId::new(b"app/a.php")), "{round:?}");
        assert!(started <= Duration::from_millis(200), "the round of app/a.php started {started:?} after its edit");
    }

    /// A cookie the watcher has not reported is written again, as one a watcher missed while its stream started would
    /// otherwise keep its folder's changes from the handler for good.
    #[test]
    fn a_cookie_the_watcher_has_not_reported_is_written_again() {
        let (_directory, root) = project();
        let app = root.join("app");
        let modified = || std::fs::metadata(&app).and_then(|metadata| metadata.modified()).expect("app should be read");
        let deadline = Instant::now() + Duration::from_mins(1);
        let changed_from = |earlier| {
            while modified() == earlier && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            modified()
        };
        let before = modified();
        let _watcher = start_watcher_of::<ReportsOnRequest<false>, _, _>(
            |_: notify::Result<Event>| {},
            &[(app.clone(), RecursiveMode::Recursive)],
            TEST_COOKIE,
            Duration::from_millis(50),
            |_| None,
            |_: &Path, _| true,
        )
        .expect("the watcher should start");

        let first = changed_from(before);
        let second = changed_from(first);

        assert_ne!(first, before, "the cookie was not written");
        assert_ne!(second, first, "the cookie was not written again");
    }

    /// A watcher whose cookie comes late is waited for, never replaced by a poll: an edit made before the cookie
    /// reaches no handler until the cookie comes, and then reaches it from the walk of the folder.
    #[test]
    fn a_late_cookie_starts_no_poll_and_the_edit_made_before_it_is_reported_once_it_comes() {
        let (_directory, root) = project();
        let app = root.join("app");
        let (events, received) = mpsc::channel();
        let log = Log::default();
        let _watcher = tracing::dispatcher::with_default(&log.dispatch(), || {
            start_watcher_of::<ReportsOnRequest<false>, _, _>(
                events,
                &[(app.clone(), RecursiveMode::Recursive)],
                TEST_COOKIE,
                Duration::from_millis(50),
                |_| None,
                |_: &Path, _| true,
            )
            .expect("the watcher should start")
        });

        let edited = app.join("a.php");
        write(&root, "app/a.php", "<?php echo 1;\n");
        let before_the_cookie = received.recv_timeout(Duration::from_millis(500));
        report(&app, app.join(format!("{TEST_COOKIE}0")));
        let paths = paths_until(&received, &edited);

        assert!(before_the_cookie.is_err(), "a change reached the handler before the cookie: {before_the_cookie:?}");
        assert!(!log.text().contains("checks the files"), "{}", log.text());
        assert!(paths.contains(&edited), "the walk at the cookie reported no edit of app/a.php: {paths:?}");
    }
}
