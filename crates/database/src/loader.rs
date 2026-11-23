//! Database loader for scanning and loading project files.

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::ffi::OsString;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use globset::GlobSet;
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::Database;
use crate::DatabaseConfiguration;
use crate::error::DatabaseError;
use crate::error::IOAction;
use crate::exclusion::Exclusion;
use crate::file::DevPackage;
use crate::file::File;
use crate::file::FileId;
use crate::file::FileType;
use crate::file::FolderNamespace;
use crate::file::nanos_since_epoch;
use crate::matcher::build_glob_set;
use crate::utils::bytes_to_os_str;
use crate::utils::bytes_to_path;
use crate::utils::bytes_to_string_lossy;
use crate::utils::file_from;

/// Holds a file along with the specificity of the pattern that matched it.
///
/// Specificity is used to resolve conflicts when a file matches both `paths` and `includes`.
/// Higher specificity values indicate more specific matches (e.g., exact file paths have higher
/// specificity than directory patterns).
#[derive(Debug)]
struct FileWithSpecificity {
    file: File,
    specificity: usize,
}

/// Builder for loading files into a Database from the filesystem and memory.
pub struct DatabaseLoader<'config> {
    database: Option<Database<'config>>,
    configuration: DatabaseConfiguration<'config>,
    memory_sources: Vec<(&'static [u8], &'static [u8], FileType)>,
    stdin_override: Option<(Cow<'config, [u8]>, Vec<u8>)>,
}

impl<'config> DatabaseLoader<'config> {
    #[inline]
    #[must_use]
    pub fn new(configuration: DatabaseConfiguration<'config>) -> Self {
        Self { configuration, memory_sources: vec![], database: None, stdin_override: None }
    }

    #[inline]
    #[must_use]
    pub fn with_database(mut self, database: Database<'config>) -> Self {
        self.database = Some(database);
        self
    }

    /// When set, the file with this logical name (workspace-relative path) will use the given
    /// content instead of being read from disk. The logical name is used for baseline and reporting.
    ///
    /// `content` is raw bytes: PHP source is binary-safe, so a buffer piped in via `--stdin-input`
    /// may not be valid UTF-8.
    #[inline]
    #[must_use]
    pub fn with_stdin_override(mut self, logical_name: impl AsRef<[u8]>, content: Vec<u8>) -> Self {
        self.stdin_override = Some((Cow::Owned(logical_name.as_ref().to_vec()), content));
        self
    }

    #[inline]
    pub fn add_memory_source(&mut self, name: &'static str, contents: &'static str, file_type: FileType) {
        self.memory_sources.push((name.as_bytes(), contents.as_bytes(), file_type));
    }

    /// Loads files from disk into the database: the files [`Self::list`] finds, read as [`Self::load_listed`] reads
    /// them.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if:
    /// - A glob pattern is invalid
    /// - File system operations fail (reading directories, files)
    /// - A file exceeds the maximum supported size
    #[inline]
    pub fn load(self) -> Result<Database<'config>, DatabaseError> {
        let listing = self.list()?;

        self.load_listed(&listing)
    }

    /// Walks the configured roots and lists every file [`Self::load`] reads, each with its metadata, taken before any
    /// file is read, and reads none of them. A caller that keeps a record of each file's status, as `mago compile`
    /// does, reads it from the listing, so the walk that finds the files is the one the load reads them from.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if an `excludes` or a `paths` glob pattern is invalid.
    pub fn list(&self) -> Result<Listing, DatabaseError> {
        let extensions_set: HashSet<OsString> =
            self.configuration.extensions.iter().map(|s| bytes_to_os_str(s.as_ref()).into_owned()).collect();
        let glob_excludes = self.glob_excludes()?;
        let excluded_folders = ExcludedFolders::from_configuration(&self.configuration)?;
        let list =
            |roots, file_type| self.list_paths(roots, file_type, &extensions_set, &glob_excludes, &excluded_folders);
        let (host, (vendored, patch)) = rayon::join(
            || list(&self.configuration.paths, FileType::Host),
            || {
                rayon::join(
                    || list(&self.configuration.includes, FileType::Vendored),
                    || list(&self.configuration.patches, FileType::Patch),
                )
            },
        );
        let mut files = host?;
        files.extend(vendored?);
        files.extend(patch?);

        Ok(Listing { workspace: excluded_folders.workspace, files })
    }

    /// Reads the files `listing` names, as [`Self::list`] gave it, into the database, with the files in memory.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if an `excludes` glob pattern is invalid, a listed file can't be read, or one
    /// exceeds the maximum supported size.
    pub fn load_listed(mut self, listing: &Listing) -> Result<Database<'config>, DatabaseError> {
        let mut db = self.database.take().unwrap_or_else(|| Database::new(self.configuration.clone()));

        // Update database configuration to use the loader's configuration
        // (fixes workspace path when merging with prelude database)
        db.configuration = self.configuration.clone();

        let glob_excludes = self.glob_excludes()?;
        let path_excludes: HashSet<_> = self
            .configuration
            .excludes
            .iter()
            .filter_map(|ex| match ex {
                Exclusion::Path(p) => Some(p),
                Exclusion::Pattern(_) => None,
            })
            .collect();
        let read = listing
            .files
            .par_iter()
            .filter_map(|listed| self.read(&listing.workspace, listed))
            .collect::<Result<Vec<FileWithSpecificity>, DatabaseError>>()?;

        let mut all_files: HashMap<FileId, File> = HashMap::default();
        // Per-file maximum specificity for each tier the file matched. `None` in a slot means
        // the file did not match any configured pattern in that tier; otherwise it carries the
        // best specificity score that tier could offer, scored by `calculate_pattern_specificity`.
        type TierSpecs = (Option<usize>, Option<usize>, Option<usize>);
        let mut tier_specs: HashMap<FileId, TierSpecs> = HashMap::default();
        let (host_files_with_spec, other_files_with_spec): (Vec<_>, Vec<_>) =
            read.into_iter().partition(|file_with_spec| file_with_spec.file.file_type == FileType::Host);

        // Process host files (from paths)
        for file_with_spec in host_files_with_spec {
            let file_id = file_with_spec.file.id;
            let specificity = file_with_spec.specificity;

            all_files.insert(file_id, file_with_spec.file);
            bump_spec(&mut tier_specs.entry(file_id).or_insert((None, None, None)).0, specificity);
        }

        // When stdin override is set, ensure that the file is in the database
        // (covers new/unsaved files, not on disk). Excluded paths are skipped
        // so that editor integrations using `--stdin-input` honor the same
        // exclude rules as a regular filesystem scan.
        if let Some((name, content)) = &self.stdin_override {
            let virtual_path = self.configuration.workspace.join(bytes_to_path(name.as_ref()).as_ref());
            let virtual_path_canonical = virtual_path.canonicalize().unwrap_or_else(|_| virtual_path.clone());
            let virtual_path_str = virtual_path_canonical.to_string_lossy();

            let matched_glob = !glob_excludes.is_empty()
                && (glob_excludes.is_match(virtual_path_canonical.as_path())
                    || glob_excludes.is_match(bytes_to_path(name.as_ref()).as_ref()));

            let matched_path = path_excludes.iter().any(|excl| {
                let canonical = if Path::new(excl.as_ref()).is_absolute() {
                    excl.as_ref().to_path_buf()
                } else {
                    self.configuration.workspace.join(excl.as_ref())
                };
                let canonical = canonical.canonicalize().unwrap_or(canonical);
                let canonical_str = canonical.to_string_lossy();

                virtual_path_str.starts_with(canonical_str.as_ref())
                    && matches!(virtual_path_str.as_bytes().get(canonical_str.len()), None | Some(&b'/' | &b'\\'))
            });

            if !matched_glob && !matched_path {
                let file = File::ephemeral(Cow::Owned(name.as_ref().to_vec()), Cow::Owned(content.clone()));
                let file_id = file.id;
                if let Entry::Vacant(e) = all_files.entry(file_id) {
                    e.insert(file);

                    bump_spec(&mut tier_specs.entry(file_id).or_insert((None, None, None)).0, usize::MAX);
                }
            }
        }

        // Process vendored files (from includes), then patch files, in the listing's order.
        for file_with_spec in other_files_with_spec {
            let file_id = file_with_spec.file.id;
            let specificity = file_with_spec.specificity;
            let specs = tier_specs.entry(file_id).or_insert((None, None, None));
            let slot = if file_with_spec.file.file_type == FileType::Patch { &mut specs.2 } else { &mut specs.1 };
            bump_spec(slot, specificity);
            all_files.entry(file_id).or_insert(file_with_spec.file);
        }

        db.reserve(tier_specs.len() + self.memory_sources.len());

        let mut manifests = Manifests::default();
        for (file_id, (host_spec, vendored_spec, patch_spec)) in tier_specs {
            if let Some(mut file) = all_files.remove(&file_id) {
                file.file_type = resolve_file_type(host_spec, vendored_spec, patch_spec);
                mark_package(&mut file, &self.configuration.workspace, &mut manifests);
                db.add(file);
            }
        }

        for (name, contents, file_type) in self.memory_sources {
            let file = File::new(Cow::Borrowed(name), file_type, None, Cow::Borrowed(contents));

            db.add(file);
        }

        Ok(db)
    }

    /// The set of the configuration's `excludes` glob patterns, which exclude each file they match.
    fn glob_excludes(&self) -> Result<GlobSet, DatabaseError> {
        let patterns = self.configuration.excludes.iter().filter_map(|exclusion| match exclusion {
            Exclusion::Pattern(pattern) => Some(pattern.as_ref()),
            Exclusion::Path(_) => None,
        });

        build_glob_set(patterns, self.configuration.glob)
    }

    /// The file `listed` names, read as a file of the workspace at `workspace`, or the stdin override's contents when
    /// its name is the override's. None when the file is gone since the listing.
    fn read(&self, workspace: &Path, listed: &Listed) -> Option<Result<FileWithSpecificity, DatabaseError>> {
        let Listed { path, file_type, specificity, metadata, exact, .. } = listed;
        if let Some((override_name, override_content)) = &self.stdin_override
            && override_name.as_ref() == File::logical_name(workspace, path).as_slice()
        {
            let name = Cow::Owned(File::logical_name(workspace, path));
            let file = File::new(name, *file_type, Some(path.clone()), Cow::Owned(override_content.clone()));

            return Some(Ok(FileWithSpecificity { file, specificity: *specificity }));
        }
        let modified = if *exact { None } else { metadata.modified().ok() };

        read_listed_file(workspace, path, *file_type, modified)
            .map(|file| file.map(|file| FileWithSpecificity { file, specificity: *specificity }))
    }

    /// Lists every file under a set of root paths or glob patterns, each as a file of `file_type`, in parallel: a
    /// root that names a file, whatever its extension, each file of one of `extensions` a root folder holds below a
    /// folder `excluded_folders` doesn't hold, and each file a glob pattern matches. A file `glob_excludes` matches or
    /// an `excludes` path holds is left out. Each listed file carries the specificity of its root, for conflict
    /// resolution, and its metadata, read before any file is.
    fn list_paths(
        &self,
        roots: &[Cow<'config, [u8]>],
        file_type: FileType,
        extensions: &HashSet<OsString>,
        glob_excludes: &GlobSet,
        excluded_folders: &ExcludedFolders,
    ) -> Result<Vec<Listed>, DatabaseError> {
        // Canonicalize the workspace once.  All WalkDir roots are canonicalized
        // before traversal so their paths inherit the canonical prefix without
        // any per-file syscalls.
        let canonical_workspace = &excluded_folders.workspace;
        let canonical_excludes = &excluded_folders.paths;

        // The bool flags a path that was named exactly (a literal file on disk) rather than
        // discovered by walking a configured directory. Such paths bypass the extension filter.
        let mut paths_to_process: Vec<(PathBuf, PathBuf, usize, bool)> = Vec::new();
        let mut directory_roots: Vec<(PathBuf, PathBuf, usize)> = Vec::new();

        for root in roots {
            // Check if this is a glob pattern (contains glob metacharacters).
            // First check if it's an actual file/directory on disk. if so, treat it
            // as a literal path even if the name contains glob metacharacters like `[]`.
            let root_path = bytes_to_path(root.as_ref());
            let resolved_path = if root_path.is_absolute() {
                root_path.as_ref().to_path_buf()
            } else {
                self.configuration.workspace.join(root_path.as_ref())
            };

            let is_glob_pattern = !resolved_path.exists()
                && (root.contains(&b'*') || root.contains(&b'?') || root.contains(&b'[') || root.contains(&b'{'));

            let specificity = calculate_pattern_specificity(root.as_ref());
            if is_glob_pattern {
                // Handle as glob pattern
                let pattern = if root_path.is_absolute() {
                    bytes_to_string_lossy(root.as_ref()).into_owned()
                } else {
                    // Make relative patterns absolute by prepending workspace
                    self.configuration.workspace.join(root_path.as_ref()).to_string_lossy().to_string()
                };

                match glob::glob(&pattern) {
                    Ok(entries) => {
                        for entry in entries {
                            match entry {
                                Ok(path) => {
                                    if path.is_file() {
                                        // Canonicalize so the path shares the same prefix as
                                        // `canonical_workspace` (important on macOS where
                                        // TempDir / glob return /var/… but canonicalize gives
                                        // /private/var/…).  Fall back to the original on error.
                                        let canonical = path.canonicalize().unwrap_or(path);
                                        paths_to_process.push((canonical.clone(), canonical, specificity, false));
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Failed to read glob entry: {}", e);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        return Err(DatabaseError::Glob(e.to_string()));
                    }
                }
            } else {
                let canonical_root = resolved_path.canonicalize().unwrap_or(resolved_path);

                // A path that resolves to a regular file was named explicitly rather than
                // discovered by walking a directory. Honor it verbatim, bypassing the extension
                // filter so extensionless PHP files (e.g. `bin/console`) can be loaded.
                if canonical_root.is_file() {
                    paths_to_process.push((canonical_root.clone(), canonical_root, specificity, true));
                    continue;
                }

                for entry in WalkDir::new(&canonical_root).follow_links(true).max_depth(1) {
                    match entry {
                        Ok(entry) => {
                            if entry.depth() == 0 {
                                continue;
                            }

                            let real = real_path(&entry, &[]);
                            if entry.file_type().is_dir() {
                                if !excluded_folders.contains(entry.path()) {
                                    directory_roots.push((entry.into_path(), real, specificity));
                                }
                            } else {
                                paths_to_process.push((entry.into_path(), real, specificity, false));
                            }
                        }
                        Err(err) => warn_walk_error(&err, canonical_root.as_path()),
                    }
                }
            }
        }

        let has_path_excludes = !canonical_excludes.is_empty();
        let has_glob_excludes = !glob_excludes.is_empty();
        // Whether the file at `path` is listed, by its path alone, so a file the listing leaves out costs no metadata
        // read: one of `extensions`, or one a root names exactly, that no exclude leaves out.
        let is_listed = |path: &Path, exact: bool| {
            if has_glob_excludes
                && (glob_excludes.is_match(path)
                    || glob_excludes.is_match(workspace_relative_string(path, canonical_workspace.as_path())))
            {
                return false;
            }

            if !exact && !path.extension().is_some_and(|extension| extensions.contains(extension)) {
                return false;
            }

            !has_path_excludes
                || !path.to_str().is_some_and(|s| {
                    canonical_excludes.iter().any(|excl| {
                        s.starts_with(excl.as_str())
                            && matches!(s.as_bytes().get(excl.len()), None | Some(&b'/' | &b'\\'))
                    })
                })
        };
        let list_path = |(path, real, specificity, exact): (PathBuf, PathBuf, usize, bool),
                         metadata: Option<std::fs::Metadata>| {
            // A file gone since the walk found it is no file, as a read that finds it gone leaves it out.
            let metadata = metadata.or_else(|| std::fs::metadata(&path).ok())?;

            Some(Listed { path, real, metadata, file_type, specificity, exact })
        };

        let (direct_files, directory_files) = rayon::join(
            || {
                paths_to_process
                    .into_par_iter()
                    .filter(|(path, _, _, exact)| is_listed(path, *exact))
                    .filter_map(|path| list_path(path, None))
                    .collect::<Vec<Listed>>()
            },
            || {
                directory_roots
                    .into_par_iter()
                    .map(|(root, real_root, specificity)| {
                        let walker = WalkDir::new(&root).follow_links(true).into_iter().filter_entry(|entry| {
                            entry.depth() == 0
                                || !entry.file_type().is_dir()
                                || !excluded_folders.contains(entry.path())
                        });
                        // Each linked folder of the walk with its real path, so a file below one names its real path.
                        let mut links: Vec<(PathBuf, PathBuf)> = Vec::new();
                        if real_root != root {
                            links.push((root.clone(), real_root));
                        }

                        let mut files = Vec::new();
                        for entry in walker {
                            match entry {
                                Ok(entry) if entry.file_type().is_dir() => {
                                    if entry.depth() > 0 && entry.path_is_symlink() {
                                        links.push((entry.path().to_path_buf(), real_path(&entry, &links)));
                                    }
                                }
                                Ok(entry) if is_listed(entry.path(), false) => {
                                    let real = real_path(&entry, &links);
                                    let metadata = entry.metadata().ok();
                                    if let Some(listed) =
                                        list_path((entry.into_path(), real, specificity, false), metadata)
                                    {
                                        files.push(listed);
                                    }
                                }
                                Ok(_) => {}
                                Err(err) => warn_walk_error(&err, root.as_path()),
                            }
                        }

                        files
                    })
                    .collect::<Vec<Vec<Listed>>>()
            },
        );

        let mut files = direct_files;
        files.extend(directory_files.into_iter().flatten());

        Ok(files)
    }
}

/// The real path of the file or folder `entry` names, from the real path of the deepest of `links`, each linked folder
/// above it with its real path, that holds it: the entry's own target when it is a link, as the disk resolves it.
fn real_path(entry: &walkdir::DirEntry, links: &[(PathBuf, PathBuf)]) -> PathBuf {
    let path = entry.path();
    if entry.path_is_symlink() {
        return path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    }

    links
        .iter()
        .rev()
        .find_map(|(link, real)| path.strip_prefix(link).ok().map(|below| real.join(below)))
        .unwrap_or_else(|| path.to_path_buf())
}

/// Every file a walk of a [`DatabaseConfiguration`]'s roots found, as [`DatabaseLoader::list`] gives them, which
/// [`DatabaseLoader::load_listed`] reads.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    /// The canonical workspace the files are named relative to.
    workspace: PathBuf,
    files: Vec<Listed>,
}

impl Listing {
    /// Each file the walk found, once for each root tier that found it.
    #[must_use]
    pub fn files(&self) -> &[Listed] {
        &self.files
    }
}

/// A file a walk found, before any file was read: the path it is named by, its real path, and its metadata, with the
/// tier and the specificity of the root that found it.
#[derive(Debug, Clone)]
pub struct Listed {
    /// The path the database names the file by: through a linked folder as the walk reached it, and its real path
    /// for a file a glob pattern matches.
    pub path: PathBuf,
    /// The file's real path, with every link resolved.
    pub real: PathBuf,
    /// The file's metadata, its link followed, read before any file was read.
    pub metadata: std::fs::Metadata,
    file_type: FileType,
    specificity: usize,
    /// Whether a root names the file itself, which keeps no modification time.
    exact: bool,
}

fn workspace_relative_string(path: &Path, workspace: &Path) -> String {
    let relative = path.strip_prefix(workspace).unwrap_or(path).to_string_lossy();
    #[cfg(windows)]
    {
        relative.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        relative.into_owned()
    }
}

/// Reads the file at `path`, or none when [`read_listed`] finds it gone. The file keeps `modified`, the modification
/// time its walk read before the read, which a file a root names by itself has none of.
fn read_listed_file(
    workspace: &Path,
    path: &Path,
    file_type: FileType,
    modified: Option<std::time::SystemTime>,
) -> Option<Result<File, DatabaseError>> {
    let bytes = match read_listed(path) {
        Ok(bytes) => bytes?,
        Err(error) => return Some(Err(error)),
    };

    Some(file_from(workspace, path, file_type, bytes).map(|mut file| {
        file.modified_ns = modified.map(nanos_since_epoch);
        file
    }))
}

/// The bytes of the file a listing named at `path`, or none when no file is there any more.
///
/// A file deleted or moved since the listing is not part of the run's tree, as if the run started after the delete.
///
/// # Errors
///
/// Returns a [`DatabaseError::IOError`] naming `path` when a file is there that can't be read.
pub fn read_listed(path: &Path) -> Result<Option<Vec<u8>>, DatabaseError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(DatabaseError::io(IOAction::ReadFile, path, error)),
    }
}

/// The folders a walk of the workspace skips, as [`DatabaseLoader::load`] skips them.
///
/// Each one an `excludes` path names, and each one an `excludes` pattern names before a trailing `/**/*`, `/**` or
/// `/*`. Built once from a [`DatabaseConfiguration`]; cheap to query per folder.
#[derive(Debug, Clone)]
pub struct ExcludedFolders {
    /// The canonical workspace, which a pattern is matched against relative to.
    workspace: PathBuf,
    /// The canonical path of each `excludes` path that exists, as a string, so a plain byte prefix check matches it.
    paths: Vec<String>,
    /// The `excludes` patterns that name a folder's contents, each without its trailing wildcard.
    globs: GlobSet,
}

impl ExcludedFolders {
    /// Builds the folders a walk skips from a database configuration.
    ///
    /// # Errors
    ///
    /// Returns a [`DatabaseError`] if an exclude glob pattern fails to compile.
    pub fn from_configuration(configuration: &DatabaseConfiguration<'_>) -> Result<Self, DatabaseError> {
        let workspace =
            configuration.workspace.canonicalize().unwrap_or_else(|_| configuration.workspace.to_path_buf());
        let mut paths = Vec::new();
        let mut patterns = Vec::new();
        for exclusion in &configuration.excludes {
            match exclusion {
                Exclusion::Path(path) => {
                    let path = if path.is_absolute() {
                        path.as_ref().to_path_buf()
                    } else {
                        configuration.workspace.join(path.as_ref())
                    };
                    if let Some(path) =
                        path.canonicalize().ok().and_then(|path| path.into_os_string().into_string().ok())
                    {
                        paths.push(path);
                    }
                }
                Exclusion::Pattern(pattern) => {
                    let folder = pattern
                        .strip_suffix("/**/*")
                        .or_else(|| pattern.strip_suffix("/**"))
                        .or_else(|| pattern.strip_suffix("/*"));
                    if let Some(folder) =
                        folder.filter(|folder| !folder.is_empty() && *folder != "*" && *folder != "**")
                    {
                        patterns.push(folder);
                    }
                }
            }
        }
        let globs = build_glob_set(patterns, configuration.glob)?;

        Ok(Self { workspace, paths, globs })
    }

    /// Returns `true` if a walk of the workspace skips the folder `path`, which is canonical, and everything in it.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        if let Some(path) = path.to_str()
            && self.paths.iter().any(|excluded| {
                path.starts_with(excluded.as_str())
                    && matches!(path.as_bytes().get(excluded.len()), None | Some(&b'/' | &b'\\'))
            })
        {
            return true;
        }

        !self.globs.is_empty()
            && (self.globs.is_match(path) || self.globs.is_match(workspace_relative_string(path, &self.workspace)))
    }
}

fn warn_walk_error(error: &walkdir::Error, fallback: &Path) {
    let path = error.path().unwrap_or(fallback).display();
    if let Some(ancestor) = error.loop_ancestor() {
        tracing::warn!("Skipping symlink loop at `{path}`: link cycles back to `{}`.", ancestor.display());
    } else {
        tracing::warn!("Failed to walk `{path}`: {error}. Entry will be skipped.");
    }
}

fn bump_spec(slot: &mut Option<usize>, s: usize) {
    *slot = Some(slot.map_or(s, |e| e.max(s)));
}

/// The package name in the standard library's `composer.json`.
const STANDARD_LIBRARY_PACKAGE: &str = "heyjordanparker/php-sharp-composer";

/// The nearest `composer.json` of each directory already looked up, so each one is read once.
pub type Manifests = HashMap<PathBuf, Option<Arc<Manifest>>>;

/// What a file's nearest `composer.json` says about it.
#[derive(Debug, Default)]
pub struct Manifest {
    /// The folder that holds it.
    directory: PathBuf,
    /// The package it names.
    name: Option<String>,
    /// Whether it names the standard library's package.
    is_standard_library: bool,
    /// Whether its package is a project: it does not say `"type": "library"`, and it is not the standard library.
    is_project: bool,
    /// Its PSR-4 folders, `autoload`'s and `autoload-dev`'s.
    folders: Vec<Psr4Folder>,
    /// Its dev-only packages, each by name and the folder Composer installs it in, from `directory`.
    dev_packages: Vec<(String, PathBuf)>,
}

impl Manifest {
    /// The package it names, as `acme/format`.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether its package is a project, an application, whose `composer.json` does not say `"type": "library"`, so
    /// leaving the type out turns none of spec section 5's strict rules off. The standard library is a library
    /// whatever type Composer needs it to give.
    #[must_use]
    pub fn is_project(&self) -> bool {
        self.is_project
    }

    /// The namespace of each PSR-4 folder `autoload` maps, dot-separated, in the order `composer.json` writes them.
    pub fn namespaces(&self) -> impl Iterator<Item = &str> {
        self.folders.iter().filter(|folder| !folder.is_test).map(|folder| folder.namespace.as_str())
    }
}

/// A PSR-4 folder, the namespace it holds, dot-separated, and whether `autoload-dev` maps it.
#[derive(Debug)]
struct Psr4Folder {
    namespace: String,
    directory: PathBuf,
    is_test: bool,
}

/// Marks a `.sharp` file a caller makes itself, as the loader marks each one it reads, so a file converted into PHP#
/// in memory is checked as the same file on disk would be.
pub fn mark_sharp_file(file: &mut File, workspace: &Path) {
    mark_package(file, workspace, &mut Manifests::default());
}

/// Marks a `.sharp` file from its nearest `composer.json`: whether it is a PHP# source of the standard library, which
/// names the standard library's package, vendored or in the package's own repository, whether it is a project's, and
/// what its PSR-4 folders say about its namespace, from the folder that holds it most closely. A file with no
/// `composer.json` above it is a project's, and one installed in `vendor/` never is, since its project is the one that
/// installs it.
pub(crate) fn mark_package(file: &mut File, workspace: &Path, manifests: &mut Manifests) {
    if !file.name.ends_with(b".sharp") {
        return;
    }

    let path = file.path.clone().unwrap_or_else(|| workspace.join(bytes_to_path(&file.name)));
    let manifest = path.parent().and_then(|directory| nearest_manifest(directory, manifests));
    file.is_project =
        file.file_type != FileType::Vendored && manifest.as_ref().is_none_or(|manifest| manifest.is_project);
    let Some(manifest) = manifest else {
        return;
    };

    file.is_standard_library = manifest.is_standard_library;
    file.folder_namespace = manifest
        .folders
        .iter()
        .filter_map(|folder| Some((folder, path.parent()?.strip_prefix(&folder.directory).ok()?)))
        .min_by_key(|(_, relative)| relative.components().count())
        .map(|(folder, relative)| {
            if folder.is_test {
                return FolderNamespace::Test;
            }

            let parts = std::iter::once(folder.namespace.clone())
                .filter(|namespace| !namespace.is_empty())
                .chain(relative.components().map(|part| part.as_os_str().to_string_lossy().into_owned()));

            FolderNamespace::Namespace(parts.collect::<Vec<_>>().join(".").into_boxed_str())
        });
    if file.file_type == FileType::Vendored || file.folder_namespace == Some(FolderNamespace::Test) {
        return;
    }

    let depth = path.strip_prefix(&manifest.directory).map_or(0, |relative| relative.components().count());
    let Some(package_root) = bytes_to_path(&file.name).ancestors().nth(depth).map(Path::to_path_buf) else {
        return;
    };
    file.dev_packages = manifest
        .dev_packages
        .iter()
        .map(|(name, folder)| DevPackage {
            name: name.as_str().into(),
            folder: folder_name(&package_root.join(folder)),
        })
        .collect();
}

/// The name of `folder`, a folder below the workspace, as a file's name starts with it: its parts, each followed by
/// `/`, as `vendor/phpunit/phpunit/`.
fn folder_name(folder: &Path) -> Box<[u8]> {
    let mut name = Vec::new();
    for part in folder.components() {
        if let Component::Normal(part) = part {
            name.extend_from_slice(part.as_encoded_bytes());
            name.push(b'/');
        }
    }

    name.into_boxed_slice()
}

/// The facts of the nearest `composer.json` at or above `directory`, each directory's looked up once in `manifests`.
/// `None` when no folder above it holds one.
pub fn nearest_manifest(directory: &Path, manifests: &mut Manifests) -> Option<Arc<Manifest>> {
    if let Some(manifest) = manifests.get(directory) {
        return manifest.clone();
    }

    let path = directory.join("composer.json");
    let manifest = match std::fs::read(&path) {
        Ok(contents) => match serde_json::from_slice::<serde_json::Value>(&contents) {
            Ok(package) => Some(Arc::new(read_manifest(directory, &package))),
            Err(error) => {
                tracing::warn!(
                    "Failed to parse `{}`: {error}. Its files are a project's, not the standard library.",
                    path.display()
                );

                Some(Arc::new(read_manifest(directory, &serde_json::Value::Null)))
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            directory.parent().and_then(|parent| nearest_manifest(parent, manifests))
        }
        Err(error) => {
            tracing::warn!(
                "Failed to read `{}`: {error}. Its files are a project's, not the standard library.",
                path.display()
            );

            Some(Arc::new(read_manifest(directory, &serde_json::Value::Null)))
        }
    };

    manifests.insert(directory.to_path_buf(), manifest.clone());

    manifest
}

/// The facts of the `composer.json` in `directory`, as `mago init` reads its PSR-4 folders.
fn read_manifest(directory: &Path, package: &serde_json::Value) -> Manifest {
    let mut folders = Vec::new();
    for (section, is_test) in [("autoload", false), ("autoload-dev", true)] {
        let Some(map) = package.get(section).and_then(|autoload| autoload.get("psr-4")).and_then(|map| map.as_object())
        else {
            continue;
        };

        for (prefix, directories) in map {
            let namespace = prefix.trim_matches('\\').replace('\\', ".");
            let directories = match directories {
                serde_json::Value::String(path) => vec![path.as_str()],
                serde_json::Value::Array(paths) => paths.iter().filter_map(serde_json::Value::as_str).collect(),
                _ => Vec::new(),
            };
            for path in directories {
                folders.push(Psr4Folder { namespace: namespace.clone(), directory: directory.join(path), is_test });
            }
        }
    }

    let name = package.get("name").and_then(serde_json::Value::as_str);
    let is_standard_library = name == Some(STANDARD_LIBRARY_PACKAGE);
    let vendor =
        Path::new(package.pointer("/config/vendor-dir").and_then(serde_json::Value::as_str).unwrap_or("vendor"));

    Manifest {
        directory: directory.to_path_buf(),
        name: name.map(str::to_owned),
        is_standard_library,
        is_project: !is_standard_library && package.get("type").and_then(serde_json::Value::as_str) != Some("library"),
        folders,
        dev_packages: dev_package_names(directory, package)
            .into_iter()
            .map(|name| {
                let folder = vendor.join(&name);

                (name, folder)
            })
            .collect(),
    }
}

/// The names of the dev-only packages of the `composer.json` `package` in `directory`: those its `composer.lock` lists
/// under `packages-dev`, a package another dev-only package requires among them, or, with no `composer.lock` it can
/// read, the packages it lists under `require-dev`, PHP and its extensions left out.
fn dev_package_names(directory: &Path, package: &serde_json::Value) -> Vec<String> {
    let lock = std::fs::read(directory.join("composer.lock"))
        .ok()
        .and_then(|contents| serde_json::from_slice::<serde_json::Value>(&contents).ok());
    if let Some(lock) = lock {
        return lock
            .get("packages-dev")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|locked| locked.get("name").and_then(serde_json::Value::as_str).map(str::to_owned))
            .collect();
    }

    package
        .get("require-dev")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(serde_json::Map::keys)
        .filter(|name| name.contains('/'))
        .cloned()
        .collect()
}

/// Picks the final [`FileType`] for a file that matched configured base paths in one or
/// more tiers.
///
/// Each argument carries the maximum specificity of any matching base path in that tier, or
/// `None` if no base path in that tier matched. Vendored wins over Host at equal-or-more
/// specificity; Host wins only when strictly more specific. Patch beats Vendored
/// unconditionally; Patch beats Host only when strictly more specific. When nothing matches
/// the result is `Host`.
pub(crate) fn resolve_file_type(
    host_spec: Option<usize>,
    vendored_spec: Option<usize>,
    patch_spec: Option<usize>,
) -> FileType {
    let mut decision: Option<(FileType, usize)> = host_spec.map(|s| (FileType::Host, s));

    if let Some(v) = vendored_spec {
        decision = match decision {
            Some((FileType::Host, h)) if v < h => decision,
            _ => Some((FileType::Vendored, v)),
        };
    }

    if let Some(p) = patch_spec {
        decision = match decision {
            Some((FileType::Host | FileType::Patch, e)) if p <= e => decision,
            _ => Some((FileType::Patch, p)),
        };
    }

    decision.map(|(ft, _)| ft).unwrap_or(FileType::Host)
}

/// Calculates how specific a configured base path or glob pattern is for conflict resolution.
///
/// Examples:
///
/// - "src/b.php" matching src/b.php: ~2000 (exact file, 2 components)
/// - "src/" matching src/b.php: ~100 (directory, 1 component)
/// - "src" matching src/b.php: ~100 (directory, 1 component)
pub(crate) fn calculate_pattern_specificity(pattern: &[u8]) -> usize {
    let pattern_path = bytes_to_path(pattern);

    let component_count = pattern_path.components().count();
    let is_glob =
        pattern.contains(&b'*') || pattern.contains(&b'?') || pattern.contains(&b'[') || pattern.contains(&b'{');

    if is_glob {
        let non_wildcard_components = pattern_path
            .components()
            .filter(|c| {
                let s = c.as_os_str().to_string_lossy();
                !s.contains('*') && !s.contains('?') && !s.contains('[') && !s.contains('{')
            })
            .count();
        non_wildcard_components * 10
    } else if pattern_path.is_file()
        || pattern_path.extension().is_some()
        || pattern.rsplit(|&b| b == b'.').next().is_some_and(|ext| ext.eq_ignore_ascii_case(b"php"))
    {
        component_count * 1000
    } else {
        component_count * 100
    }
}

#[cfg(test)]
mod resolution_tests {
    use super::*;

    #[test]
    fn defaults_to_host_when_nothing_matches() {
        assert_eq!(resolve_file_type(None, None, None), FileType::Host);
    }

    #[test]
    fn host_only_match_yields_host() {
        assert_eq!(resolve_file_type(Some(100), None, None), FileType::Host);
    }

    #[test]
    fn vendored_only_match_yields_vendored() {
        assert_eq!(resolve_file_type(None, Some(100), None), FileType::Vendored);
    }

    #[test]
    fn patch_only_match_yields_patch() {
        assert_eq!(resolve_file_type(None, None, Some(100)), FileType::Patch);
    }

    #[test]
    fn vendored_beats_host_at_equal_specificity() {
        assert_eq!(resolve_file_type(Some(100), Some(100), None), FileType::Vendored);
    }

    #[test]
    fn vendored_beats_host_when_more_specific() {
        assert_eq!(resolve_file_type(Some(100), Some(2000), None), FileType::Vendored);
    }

    #[test]
    fn host_beats_vendored_only_when_strictly_more_specific() {
        assert_eq!(resolve_file_type(Some(2000), Some(100), None), FileType::Host);
    }

    #[test]
    fn patch_beats_vendored_unconditionally() {
        assert_eq!(resolve_file_type(None, Some(2000), Some(100)), FileType::Patch);
    }

    #[test]
    fn host_beats_patch_at_equal_specificity() {
        assert_eq!(resolve_file_type(Some(100), None, Some(100)), FileType::Host);
    }

    #[test]
    fn patch_beats_host_when_strictly_more_specific() {
        assert_eq!(resolve_file_type(Some(100), None, Some(2000)), FileType::Patch);
    }

    #[test]
    fn patch_beats_host_that_won_over_vendored() {
        assert_eq!(resolve_file_type(Some(100), Some(2000), Some(50)), FileType::Patch);
    }

    #[test]
    fn exact_file_path_beats_directory_at_same_component_count() {
        assert!(calculate_pattern_specificity(b"src/foo.php") > calculate_pattern_specificity(b"src/foo"));
    }

    #[test]
    fn directory_beats_glob_at_same_non_wildcard_count() {
        assert!(calculate_pattern_specificity(b"src/") > calculate_pattern_specificity(b"src/**/*.php"));
    }

    #[test]
    fn deeper_path_beats_shallower_at_same_kind() {
        assert!(calculate_pattern_specificity(b"src/inner/") > calculate_pattern_specificity(b"src/"));
    }

    #[test]
    fn extensionless_phpish_pattern_treated_as_file() {
        assert_eq!(calculate_pattern_specificity(b"src/foo.PHP"), calculate_pattern_specificity(b"src/foo.php"),);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::DatabaseReader;
    use crate::GlobSettings;
    use std::borrow::Cow;
    use tempfile::TempDir;

    fn create_test_config(temp_dir: &TempDir, paths: Vec<&str>, includes: Vec<&str>) -> DatabaseConfiguration<'static> {
        create_test_config_with_patches(temp_dir, paths, includes, vec![])
    }

    fn create_test_config_with_patches(
        temp_dir: &TempDir,
        paths: Vec<&str>,
        includes: Vec<&str>,
        patches: Vec<&str>,
    ) -> DatabaseConfiguration<'static> {
        // Normalize path separators to platform-specific separators
        let normalize = |s: &str| s.replace('/', std::path::MAIN_SEPARATOR_STR);

        DatabaseConfiguration {
            workspace: Cow::Owned(temp_dir.path().to_path_buf()),
            paths: paths.into_iter().map(|s| Cow::Owned(normalize(s).into_bytes())).collect(),
            includes: includes.into_iter().map(|s| Cow::Owned(normalize(s).into_bytes())).collect(),
            patches: patches.into_iter().map(|s| Cow::Owned(normalize(s).into_bytes())).collect(),
            excludes: vec![],
            extensions: vec![Cow::Borrowed(b"php")],
            glob: GlobSettings::default(),
        }
    }

    /// Returns the file's logical name as a lossy UTF-8 string for assertion matching.
    fn name_str(name: &[u8]) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(name)
    }

    fn create_test_file(temp_dir: &TempDir, relative_path: &str, content: &str) {
        let file_path = temp_dir.path().join(relative_path);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(file_path, content).unwrap();
    }

    #[test]
    fn test_exact_file_vs_directory() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "src/b.php", "<?php");
        create_test_file(&temp_dir, "src/a.php", "<?php");

        let config = create_test_config(&temp_dir, vec!["src/b.php"], vec!["src/"]);
        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let b_file = db.files().find(|f| name_str(&f.name).contains("b.php")).unwrap();
        assert_eq!(b_file.file_type, FileType::Host, "src/b.php should be Host (exact file beats directory)");

        let a_file = db.files().find(|f| name_str(&f.name).contains("a.php")).unwrap();
        assert_eq!(a_file.file_type, FileType::Vendored, "src/a.php should be Vendored");
    }

    #[test]
    fn test_deeper_vs_shallower_directory() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "src/foo/bar.php", "<?php");

        let config = create_test_config(&temp_dir, vec!["src/foo/"], vec!["src/"]);
        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("bar.php")).unwrap();
        assert_eq!(file.file_type, FileType::Host, "Deeper directory pattern should win");
    }

    #[test]
    fn test_exact_file_vs_glob() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "src/b.php", "<?php");

        let config = create_test_config(&temp_dir, vec!["src/b.php"], vec!["src/*.php"]);
        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("b.php")).unwrap();
        assert_eq!(file.file_type, FileType::Host, "Exact file should beat glob pattern");
    }

    #[test]
    fn test_equal_specificity_includes_wins() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "src/a.php", "<?php");

        let config = create_test_config(&temp_dir, vec!["src/"], vec!["src/"]);
        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("a.php")).unwrap();
        assert_eq!(file.file_type, FileType::Vendored, "Equal specificity: includes should win");
    }

    #[test]
    fn test_complex_scenario_from_bug_report() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "src/a.php", "<?php");
        create_test_file(&temp_dir, "src/b.php", "<?php");
        create_test_file(&temp_dir, "src/c/d.php", "<?php");
        create_test_file(&temp_dir, "src/c/e.php", "<?php");
        create_test_file(&temp_dir, "vendor/lib1.php", "<?php");
        create_test_file(&temp_dir, "vendor/lib2.php", "<?php");

        let config = create_test_config(&temp_dir, vec!["src/b.php"], vec!["vendor", "src/c", "src/"]);
        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let b_file = db
            .files()
            .find(|f| name_str(&f.name).contains("src/b.php") || name_str(&f.name).ends_with("b.php"))
            .unwrap();
        assert_eq!(b_file.file_type, FileType::Host, "src/b.php should be Host in bug scenario");

        let d_file = db.files().find(|f| name_str(&f.name).contains("d.php")).unwrap();
        assert_eq!(d_file.file_type, FileType::Vendored, "src/c/d.php should be Vendored");

        let lib_file = db.files().find(|f| name_str(&f.name).contains("lib1.php")).unwrap();
        assert_eq!(lib_file.file_type, FileType::Vendored, "vendor/lib1.php should be Vendored");
    }

    #[test]
    fn test_files_only_in_paths() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "src/a.php", "<?php");

        let config = create_test_config(&temp_dir, vec!["src/"], vec![]);
        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("a.php")).unwrap();
        assert_eq!(file.file_type, FileType::Host, "File only in paths should be Host");
    }

    #[test]
    fn test_files_only_in_includes() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "vendor/lib.php", "<?php");

        let config = create_test_config(&temp_dir, vec![], vec!["vendor/"]);
        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("lib.php")).unwrap();
        assert_eq!(file.file_type, FileType::Vendored, "File only in includes should be Vendored");
    }

    #[test]
    fn test_stdin_override_replaces_file_content() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/foo.php", "<?php\n// on disk");

        let config = create_test_config(&temp_dir, vec!["src/"], vec![]);
        let loader = DatabaseLoader::new(config).with_stdin_override("src/foo.php", b"<?php\n// from stdin".to_vec());
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("foo.php")).unwrap();
        assert_eq!(
            file.contents.as_ref(),
            b"<?php\n// from stdin",
            "stdin override content should be used instead of disk"
        );
    }

    #[test]
    fn test_glob_excludes_match_workspace_relative_paths() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "src/Absences/Foo/Foo.php", "<?php");
        create_test_file(&temp_dir, "src/Absences/Test/Faker/Provider/AbsencesProvider.php", "<?php");
        create_test_file(&temp_dir, "src/Calendar/Test/Helper.php", "<?php");

        let mut config = create_test_config(&temp_dir, vec!["src"], vec![]);
        config.excludes = vec![Exclusion::Pattern(Cow::Borrowed("src/*/Test/**"))];

        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let names: Vec<String> = db.files().map(|f| name_str(&f.name).into_owned()).collect();
        assert!(names.iter().any(|n| n.ends_with("src/Absences/Foo/Foo.php")), "non-Test file should be loaded");
        assert!(
            !names.iter().any(|n| n.contains("src/Absences/Test/")),
            "files under src/*/Test/** should be excluded, got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.contains("src/Calendar/Test/")),
            "files under src/*/Test/** should be excluded, got {names:?}"
        );
    }

    #[test]
    fn test_glob_excludes_match_legacy_absolute_prefix_patterns() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "packages/foo/src/main.php", "<?php");
        create_test_file(&temp_dir, "packages/foo/vendor/lib.php", "<?php");

        let mut config = create_test_config(&temp_dir, vec!["packages"], vec![]);
        config.excludes = vec![Exclusion::Pattern(Cow::Borrowed("*/packages/**/vendor/*"))];

        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let names: Vec<String> = db.files().map(|f| name_str(&f.name).into_owned()).collect();
        assert!(names.iter().any(|n| n.ends_with("packages/foo/src/main.php")));
        assert!(
            !names.iter().any(|n| n.contains("/vendor/")),
            "legacy `*/packages/**/vendor/*` style should still exclude vendor files, got {names:?}"
        );
    }

    #[test]
    fn test_glob_dir_prune_skips_relative_directories() {
        let temp_dir = TempDir::new().unwrap();

        create_test_file(&temp_dir, "vendor/slevomat/coding-standard/main.php", "<?php");
        create_test_file(&temp_dir, "vendor/slevomat/coding-standard/tests/Sniffs/Foo.php", "<?php");
        create_test_file(&temp_dir, "vendor/another/lib.php", "<?php");

        let mut config = create_test_config(&temp_dir, vec![], vec!["vendor"]);
        config.excludes = vec![Exclusion::Pattern(Cow::Borrowed("vendor/**/tests/**"))];

        let loader = DatabaseLoader::new(config);
        let db = loader.load().unwrap();

        let names: Vec<String> = db.files().map(|f| name_str(&f.name).into_owned()).collect();
        assert!(names.iter().any(|n| n.ends_with("vendor/slevomat/coding-standard/main.php")));
        assert!(names.iter().any(|n| n.ends_with("vendor/another/lib.php")));
        assert!(
            !names.iter().any(|n| n.contains("/tests/")),
            "files under vendor/**/tests/** should be pruned, got {names:?}"
        );
    }

    #[test]
    fn test_stdin_override_adds_file_when_not_on_disk() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/.gitkeep", "");

        let config = create_test_config(&temp_dir, vec!["src/"], vec![]);
        let loader =
            DatabaseLoader::new(config).with_stdin_override("src/unsaved.php", b"<?php\n// unsaved buffer".to_vec());
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("unsaved.php")).unwrap();
        assert_eq!(file.file_type, FileType::Host);
        assert_eq!(file.contents.as_ref(), b"<?php\n// unsaved buffer");
    }

    #[test]
    fn test_stdin_override_accepts_non_utf8_content() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/.gitkeep", "");

        let config = create_test_config(&temp_dir, vec!["src/"], vec![]);
        // PHP identifiers are binary-safe, so a buffer piped in via `--stdin-input` may not
        // be valid UTF-8. The loaded file must carry those bytes through verbatim.
        let content = b"<?php\n\nfunction f\xC9\xFF(): void {}\n".to_vec();
        assert!(std::str::from_utf8(&content).is_err(), "test buffer must contain non-UTF-8 bytes");

        let loader = DatabaseLoader::new(config).with_stdin_override("src/buffer.php", content.clone());
        let db = loader.load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).contains("buffer.php")).unwrap();
        assert_eq!(file.contents.as_ref(), content.as_slice());
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_file_under_include_is_loaded() {
        let temp_dir = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();

        create_test_file(&external, "Bar.php", "<?php class Bar {}\n");
        std::fs::create_dir_all(temp_dir.path().join("vendor")).unwrap();
        std::os::unix::fs::symlink(external.path().join("Bar.php"), temp_dir.path().join("vendor/Bar.php")).unwrap();

        let config = create_test_config(&temp_dir, vec![], vec!["vendor/"]);
        let db = DatabaseLoader::new(config).load().unwrap();

        let bar = db.files().find(|f| name_str(&f.name).contains("Bar.php"));
        assert!(bar.is_some(), "symlinked Bar.php should be loaded via include = ['vendor/']");
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinked_directory_under_include_is_descended() {
        let temp_dir = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();

        create_test_file(&external, "src/Foo.php", "<?php class Foo {}\n");
        create_test_file(&external, "src/Bar.php", "<?php class Bar {}\n");

        std::fs::create_dir_all(temp_dir.path().join("vendor")).unwrap();
        std::os::unix::fs::symlink(external.path(), temp_dir.path().join("vendor/example-package")).unwrap();

        let config = create_test_config(&temp_dir, vec![], vec!["vendor/"]);
        let db = DatabaseLoader::new(config).load().unwrap();

        assert!(db.files().any(|f| name_str(&f.name).contains("Foo.php")), "Foo.php inside symlinked dir not found");
        assert!(db.files().any(|f| name_str(&f.name).contains("Bar.php")), "Bar.php inside symlinked dir not found");
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_cycle_is_warned_and_skipped() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/Real.php", "<?php class Real {}\n");
        std::os::unix::fs::symlink(temp_dir.path().join("src"), temp_dir.path().join("src/loop")).unwrap();

        let config = create_test_config(&temp_dir, vec![], vec!["src/"]);
        let db = DatabaseLoader::new(config).load().expect("symlink cycle should not abort the load");

        assert!(
            db.files().any(|f| name_str(&f.name).contains("Real.php")),
            "Real.php still reachable despite the loop"
        );
    }

    #[test]
    fn test_exact_extensionless_file_is_loaded() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "bin/console", "<?php\n// entrypoint");

        // `bin/console` has no extension, so it would be filtered out when discovered by
        // walking a directory. Naming it exactly must bypass the extension requirement.
        let config = create_test_config(&temp_dir, vec!["bin/console"], vec![]);
        let db = DatabaseLoader::new(config).load().unwrap();

        let file = db.files().find(|f| name_str(&f.name).ends_with("bin/console")).unwrap();
        assert_eq!(file.file_type, FileType::Host);
        assert_eq!(file.contents.as_ref(), b"<?php\n// entrypoint");
    }

    #[test]
    fn test_extensionless_file_in_directory_is_skipped() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "bin/console", "<?php");
        create_test_file(&temp_dir, "bin/run.php", "<?php");

        // Walking the directory must still honor the extension filter: only `run.php` loads.
        let config = create_test_config(&temp_dir, vec!["bin"], vec![]);
        let db = DatabaseLoader::new(config).load().unwrap();

        let names: Vec<String> = db.files().map(|f| name_str(&f.name).into_owned()).collect();
        assert!(names.iter().any(|n| n.ends_with("bin/run.php")), "run.php should be loaded, got {names:?}");
        assert!(!names.iter().any(|n| n.ends_with("bin/console")), "extensionless console should be skipped");
    }

    /// A file a walk listed keeps the modification time taken before its read, which a compile stamps it with, and a
    /// file the configuration names exactly, whose name may differ in case from the one on disk, keeps none.
    #[test]
    fn a_listed_file_keeps_its_modification_time_and_an_exactly_named_one_none() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/a.php", "<?php");
        create_test_file(&temp_dir, "bin/console", "<?php");

        let config = create_test_config(&temp_dir, vec!["src", "bin/console"], vec![]);
        let db = DatabaseLoader::new(config).load().unwrap();
        let modified = |name: &str| db.files().find(|f| name_str(&f.name).ends_with(name)).unwrap().modified_ns;
        let on_disk = std::fs::metadata(temp_dir.path().join("src/a.php")).unwrap().modified().unwrap();

        assert_eq!(modified("src/a.php"), Some(nanos_since_epoch(on_disk)));
        assert_eq!(modified("bin/console"), None);
    }

    /// A file a walk listed that is gone by the time it is read, as one deleted during the walk, is left out, while any
    /// other failure to read one is an error.
    #[test]
    fn a_listed_file_that_is_gone_when_read_is_left_out() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/a.php", "<?php");
        let read = |name: &str| read_listed_file(temp_dir.path(), &temp_dir.path().join(name), FileType::Host, None);

        assert!(matches!(read("src/a.php"), Some(Ok(_))));
        assert!(read("src/gone.php").is_none());
        assert!(matches!(read("src"), Some(Err(DatabaseError::IOError { action: IOAction::ReadFile, .. }))));
    }

    /// A file the loader can't read fails the load with its path and what the loader was doing. Root reads a file of
    /// mode 000, so the test has nothing to show there.
    #[cfg(unix)]
    #[test]
    fn a_file_the_loader_cannot_read_is_named_in_the_error() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/a.php", "<?php");
        let file = temp_dir.path().canonicalize().unwrap().join("src/a.php");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&file).is_ok() {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let loaded = DatabaseLoader::new(create_test_config(&temp_dir, vec!["src"], vec![])).load();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let Err(error) = loaded else { panic!("a file that can't be read should fail the load") };
        assert_eq!(
            error.to_string(),
            format!("can't read the file `{}`: Permission denied (os error 13)", file.display())
        );
    }

    #[test]
    fn test_patch_beats_vendored_at_equal_specificity() {
        // A file covered by both patches and includes at the same directory-level specificity
        // should be classified as Patch, not Vendored.
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "lib/Foo.php", "<?php");

        let config = create_test_config_with_patches(&temp_dir, vec![], vec!["lib/"], vec!["lib/"]);
        let db = DatabaseLoader::new(config).load().unwrap();

        let file = db.files().find(|f| String::from_utf8_lossy(&f.name).contains("Foo.php")).unwrap();
        assert_eq!(file.file_type, FileType::Patch, "patch should beat vendored at equal specificity");
    }

    #[test]
    fn test_host_beats_patch_at_equal_specificity() {
        // When a file is covered by both paths and patches at the same directory-level specificity,
        // the host (paths) classification wins.  Patches only override host when strictly more specific.
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/Foo.php", "<?php");

        let config = create_test_config_with_patches(&temp_dir, vec!["src/"], vec![], vec!["src/"]);
        let db = DatabaseLoader::new(config).load().unwrap();

        let file = db.files().find(|f| String::from_utf8_lossy(&f.name).contains("Foo.php")).unwrap();
        assert_eq!(file.file_type, FileType::Host, "host should beat patch at equal specificity");
    }

    #[test]
    fn test_patch_beats_host_when_strictly_more_specific() {
        // An exact-file patch pattern has higher specificity than a directory paths pattern,
        // so the patch wins and the file is treated as Patch rather than Host.
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "src/Foo.php", "<?php");
        create_test_file(&temp_dir, "src/Bar.php", "<?php");

        // Patch covers only Foo.php exactly; paths covers the whole directory.
        let config = create_test_config_with_patches(&temp_dir, vec!["src/"], vec![], vec!["src/Foo.php"]);
        let db = DatabaseLoader::new(config).load().unwrap();

        let foo = db.files().find(|f| String::from_utf8_lossy(&f.name).contains("Foo.php")).unwrap();
        assert_eq!(foo.file_type, FileType::Patch, "exact-file patch should beat directory-level host pattern");

        let bar = db.files().find(|f| String::from_utf8_lossy(&f.name).contains("Bar.php")).unwrap();
        assert_eq!(bar.file_type, FileType::Host, "file not covered by patch should remain Host");
    }

    #[test]
    fn a_sharp_file_belongs_to_the_standard_library_when_its_nearest_composer_json_names_the_package() {
        let temp_dir = TempDir::new().unwrap();
        let library = "vendor/heyjordanparker/php-sharp-composer";
        create_test_file(&temp_dir, "composer.json", r#"{"name": "acme/app"}"#);
        create_test_file(&temp_dir, "src/App/Page.sharp", "");
        create_test_file(
            &temp_dir,
            &format!("{library}/composer.json"),
            r#"{"name": "heyjordanparker/php-sharp-composer"}"#,
        );
        create_test_file(&temp_dir, &format!("{library}/autoload.php"), "<?php");
        create_test_file(&temp_dir, &format!("{library}/library/Sharp/Text/Text.sharp"), "");
        create_test_file(&temp_dir, &format!("{library}/library/Sharp/Text/Slug.sharp"), "");
        create_test_file(&temp_dir, &format!("{library}/library/Sharp/Text.sharp"), "");
        create_test_file(&temp_dir, &format!("{library}/vendor/acme/tools/composer.json"), r#"{"name": "acme/tools"}"#);
        create_test_file(&temp_dir, &format!("{library}/vendor/acme/tools/Sharp/Tools.sharp"), "");

        let mut config = create_test_config(&temp_dir, vec!["src"], vec!["vendor"]);
        config.extensions = vec![Cow::Borrowed(b"php"), Cow::Borrowed(b"sharp")];
        let db = DatabaseLoader::new(config).load().unwrap();

        let mut in_library: Vec<String> =
            db.files().filter(|file| file.is_standard_library).map(|file| name_str(&file.name).into_owned()).collect();
        in_library.sort();

        assert_eq!(
            in_library,
            [
                format!("{library}/library/Sharp/Text.sharp"),
                format!("{library}/library/Sharp/Text/Slug.sharp"),
                format!("{library}/library/Sharp/Text/Text.sharp"),
            ]
        );
    }

    /// Spec section 5's strict rules run in a project: a `.sharp` file unless its nearest `composer.json` says
    /// `"type": "library"`. So a package that leaves its type out, a folder with no `composer.json` above it and one
    /// whose `composer.json` does not parse are projects, and no line left out turns a rule off. The standard library
    /// is a library whatever its type, `composer-plugin`, and so is a package installed in `vendor/`, whose project is
    /// the one that installs it.
    #[test]
    fn a_sharp_file_belongs_to_a_project_unless_its_nearest_composer_json_says_library() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(&temp_dir, "app/composer.json", r#"{"name": "acme/app"}"#);
        create_test_file(&temp_dir, "app/src/App/Page.sharp", "");
        create_test_file(&temp_dir, "app/src/App/Page.php", "<?php");
        create_test_file(&temp_dir, "loose/Tool.sharp", "");
        create_test_file(&temp_dir, "broken/composer.json", "{");
        create_test_file(&temp_dir, "broken/Probe.sharp", "");
        create_test_file(&temp_dir, "packages/format/composer.json", r#"{"name": "acme/format", "type": "library"}"#);
        create_test_file(&temp_dir, "packages/format/src/Format.sharp", "");
        create_test_file(
            &temp_dir,
            "vendor/heyjordanparker/php-sharp-composer/composer.json",
            &format!(r#"{{"name": "{STANDARD_LIBRARY_PACKAGE}", "type": "composer-plugin"}}"#),
        );
        create_test_file(&temp_dir, "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text.sharp", "");
        create_test_file(&temp_dir, "vendor/acme/tools/composer.json", r#"{"name": "acme/tools"}"#);
        create_test_file(&temp_dir, "vendor/acme/tools/src/Tools.sharp", "");
        create_test_file(&temp_dir, "vendor/acme/bare/Bare.sharp", "");

        let mut config = create_test_config(
            &temp_dir,
            vec!["app", "loose", "broken", "packages", "vendor/heyjordanparker"],
            vec!["vendor"],
        );
        config.extensions = vec![Cow::Borrowed(b"php"), Cow::Borrowed(b"sharp")];
        let db = DatabaseLoader::new(config).load().unwrap();

        let mut in_project: Vec<String> =
            db.files().filter(|file| file.is_project).map(|file| name_str(&file.name).into_owned()).collect();
        in_project.sort();

        assert_eq!(in_project, ["app/src/App/Page.sharp", "broken/Probe.sharp", "loose/Tool.sharp"]);
    }

    /// `mago convert` reads a folder's package as the loader reads a file's: from the nearest `composer.json` at or
    /// above it, its name, its kind and the namespaces of `autoload`'s PSR-4 folders, `autoload-dev`'s left out.
    #[test]
    fn the_nearest_composer_json_of_a_folder_gives_its_package() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(
            &temp_dir,
            "composer.json",
            r#"{"type": "project", "autoload": {"psr-4": {"": "lib/", "App\\Shop\\": "src/"}}, "autoload-dev": {"psr-4": {"Tests\\": "tests/"}}}"#,
        );
        create_test_file(
            &temp_dir,
            "vendor/acme/format/composer.json",
            r#"{"name": "acme/format", "type": "library"}"#,
        );
        let mut manifests = Manifests::default();

        let project = nearest_manifest(&temp_dir.path().join("src/Shop/Orders"), &mut manifests).unwrap();
        let library = nearest_manifest(&temp_dir.path().join("vendor/acme/format/src"), &mut manifests).unwrap();

        assert_eq!((project.name(), project.is_project()), (None, true));
        assert_eq!(project.namespaces().collect::<Vec<_>>(), ["", "App.Shop"]);
        assert_eq!((library.name(), library.is_project()), (Some("acme/format"), false));
    }

    /// Spec section 5: an `autoload` PSR-4 folder maps a `.sharp` file's folder to the namespace it declares, the most
    /// closely holding folder winning, and an `autoload-dev` folder is a test folder.
    #[test]
    fn a_sharp_file_takes_the_namespace_of_its_psr_4_folder() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(
            &temp_dir,
            "composer.json",
            r#"{"autoload": {"psr-4": {"App\\": "app/", "App\\Billing\\": ["billing/"], "": "root/"}}, "autoload-dev": {"psr-4": {"Tests\\": "tests/"}}}"#,
        );
        for name in [
            "app/Order.sharp",
            "app/Tenant/Store/Order.sharp",
            "billing/Invoice.sharp",
            "root/Plain/Tool.sharp",
            "tests/Tenant/OrderTest.sharp",
            "lib/Other.sharp",
            "app/Order.php",
        ] {
            create_test_file(&temp_dir, name, "");
        }

        let mut config = create_test_config(&temp_dir, vec!["app", "billing", "root", "tests", "lib"], vec![]);
        config.extensions = vec![Cow::Borrowed(b"php"), Cow::Borrowed(b"sharp")];
        let db = DatabaseLoader::new(config).load().unwrap();

        let mut namespaces: Vec<(String, Option<FolderNamespace>)> =
            db.files().map(|file| (name_str(&file.name).into_owned(), file.folder_namespace.clone())).collect();
        namespaces.sort_by(|(left, _), (right, _)| left.cmp(right));
        let namespace = |name: &str| Some(FolderNamespace::Namespace(name.into()));

        assert_eq!(
            namespaces,
            [
                ("app/Order.php".to_owned(), None),
                ("app/Order.sharp".to_owned(), namespace("App")),
                ("app/Tenant/Store/Order.sharp".to_owned(), namespace("App.Tenant.Store")),
                ("billing/Invoice.sharp".to_owned(), namespace("App.Billing")),
                ("lib/Other.sharp".to_owned(), None),
                ("root/Plain/Tool.sharp".to_owned(), namespace("Plain")),
                ("tests/Tenant/OrderTest.sharp".to_owned(), Some(FolderNamespace::Test)),
            ]
        );
    }

    /// Spec section 5: a production `.sharp` file can't use the dev-only packages, those `composer.lock` lists under
    /// `packages-dev`, PHPUnit that Pest requires included, or, with no `composer.lock`, those `composer.json` lists
    /// under `require-dev`, PHP and its extensions left out, each installed in the project's vendor folder. A test file
    /// may use them all.
    #[test]
    fn a_production_sharp_file_takes_the_dev_only_packages_of_its_project() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(
            &temp_dir,
            "locked/composer.json",
            r#"{"require-dev": {"pestphp/pest": "^3.0"}, "autoload-dev": {"psr-4": {"Tests\\": "tests/"}}}"#,
        );
        create_test_file(
            &temp_dir,
            "locked/composer.lock",
            r#"{"packages": [{"name": "acme/report"}], "packages-dev": [{"name": "pestphp/pest"}, {"name": "phpunit/phpunit"}]}"#,
        );
        create_test_file(
            &temp_dir,
            "unlocked/composer.json",
            r#"{"require-dev": {"php": "^8.4", "ext-json": "*", "mockery/mockery": "^1.6"}, "config": {"vendor-dir": "libs"}}"#,
        );
        for name in ["locked/app/Order.sharp", "locked/tests/OrderTest.sharp", "unlocked/src/Invoice.sharp"] {
            create_test_file(&temp_dir, name, "");
        }

        let mut config = create_test_config(&temp_dir, vec!["locked", "unlocked"], vec![]);
        config.extensions = vec![Cow::Borrowed(b"sharp")];
        let db = DatabaseLoader::new(config).load().unwrap();

        let mut packages: Vec<(String, Vec<(String, String)>)> = db
            .files()
            .map(|file| {
                let packages = file
                    .dev_packages
                    .iter()
                    .map(|package| (package.name.to_string(), name_str(&package.folder).into_owned()))
                    .collect();

                (name_str(&file.name).into_owned(), packages)
            })
            .collect();
        packages.sort();
        let package = |name: &str, folder: &str| (name.to_owned(), folder.to_owned());

        assert_eq!(
            packages,
            [
                (
                    "locked/app/Order.sharp".to_owned(),
                    vec![
                        package("pestphp/pest", "locked/vendor/pestphp/pest/"),
                        package("phpunit/phpunit", "locked/vendor/phpunit/phpunit/"),
                    ]
                ),
                ("locked/tests/OrderTest.sharp".to_owned(), vec![]),
                (
                    "unlocked/src/Invoice.sharp".to_owned(),
                    vec![package("mockery/mockery", "unlocked/libs/mockery/mockery/")]
                ),
            ]
        );
    }

    /// Two PSR-4 namespaces of one folder give a `.sharp` file in it the first one `composer.json` writes, as Composer's
    /// own map keeps them, and not the first in alphabetical order.
    #[test]
    fn a_folder_two_psr_4_namespaces_share_takes_the_first_composer_json_writes() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(
            &temp_dir,
            "composer.json",
            r#"{"autoload": {"psr-4": {"Zeta\\": "src/", "Alpha\\": "src/"}}}"#,
        );
        create_test_file(&temp_dir, "src/Order.sharp", "");

        let mut config = create_test_config(&temp_dir, vec!["src"], vec![]);
        config.extensions = vec![Cow::Borrowed(b"sharp")];
        let db = DatabaseLoader::new(config).load().unwrap();
        let namespaces: Vec<Option<FolderNamespace>> = db.files().map(|file| file.folder_namespace.clone()).collect();

        assert_eq!(namespaces, [Some(FolderNamespace::Namespace("Zeta".into()))]);
    }

    /// A `.sharp` file made in memory, as `mago convert` makes one before it writes it, is marked as the loader marks
    /// the same file on disk, and a `.php` file is left alone.
    #[test]
    fn a_sharp_file_made_in_memory_is_marked_as_one_on_disk() {
        let temp_dir = TempDir::new().unwrap();
        create_test_file(
            &temp_dir,
            "composer.json",
            r#"{"type": "project", "autoload": {"psr-4": {"App\\": "app/"}}}"#,
        );
        let made = |name: &str| {
            let mut file = File::new(
                Cow::Owned(name.as_bytes().to_vec()),
                FileType::Host,
                Some(temp_dir.path().join(name)),
                Cow::Borrowed(b""),
            );
            mark_sharp_file(&mut file, temp_dir.path());
            file
        };

        let sharp = made("app/Tenant/Order.sharp");
        assert_eq!(sharp.folder_namespace, Some(FolderNamespace::Namespace("App.Tenant".into())));
        assert!(sharp.is_project);
        assert!(!sharp.is_standard_library);
        let php = made("app/Tenant/Order.php");
        assert_eq!(php.folder_namespace, None);
        assert!(!php.is_project);
    }
}
