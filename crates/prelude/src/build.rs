use std::borrow::Cow;

use mago_allocator::Arena;
use mago_allocator::LocalArena;
use rust_embed::RustEmbed;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::populator::populate_codebase;
use mago_codex::reference::SymbolReferences;
use mago_codex::scanner::scan_program;
use mago_database::DatabaseReader;
use mago_database::ReadDatabase;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_names::resolver::NameResolver;
use mago_syntax::parser::parse_file;

use crate::Prelude;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/assets"]
#[prefix = "mago_prelude_"]
#[include = "*.php"]
struct PHPAssets;

pub(crate) fn build_prelude_internal() -> Prelude {
    let mut arena = LocalArena::new();
    let database = get_prelude_database();
    let read_db = database.read_only();
    let mut metadata = get_prelude_metadata(&mut arena, &read_db);
    let mut symbol_references = SymbolReferences::default();

    // A test harness builds the prelude in a `LazyLock` that rayon jobs force, and a rayon thread that waits on a join
    // runs other jobs of its pool meanwhile: one that forces the lock on the thread populating it would wait on that
    // thread forever. So the population runs on a pool of its own, and this thread waits for it outside rayon.
    std::thread::scope(|scope| {
        let populated = scope.spawn(|| match rayon::ThreadPoolBuilder::new().build() {
            #[allow(clippy::default_trait_access)]
            Ok(pool) => pool.install(|| {
                populate_codebase(&mut metadata, &mut symbol_references, Default::default(), Default::default());
            }),
            Err(error) => panic!("the prelude's thread pool does not start: {error}"),
        });

        populated.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });

    // The embedded prelude is immutable between analyses. Persisting its top-level symbols as
    // safe lets the runtime population pass skip already-populated built-ins while still
    // repopulating every project symbol merged into this metadata.
    let mut safe_symbols = std::mem::take(&mut metadata.safe_symbols);
    safe_symbols.extend(metadata.class_likes.keys().copied());
    safe_symbols
        .extend(metadata.function_likes.keys().filter_map(|(scope, member)| member.is_empty().then_some(*scope)));
    safe_symbols.extend(metadata.constants.keys().copied());
    metadata.safe_symbols = safe_symbols;

    Prelude { database, metadata, symbol_references }
}

fn get_prelude_database() -> mago_database::Database<'static> {
    let configuration =
        mago_database::DatabaseConfiguration::new(std::path::Path::new("/"), vec![], vec![], vec![], vec![])
            .into_static();
    let mut db = mago_database::Database::new(configuration);
    for filename in PHPAssets::iter() {
        let Some(embedded) = PHPAssets::get(&filename) else { continue };

        let content = match embedded.data {
            Cow::Borrowed(slice) => {
                let Ok(string) = std::str::from_utf8(slice) else { continue };

                Cow::Borrowed(string)
            }
            Cow::Owned(vector) => {
                let Ok(string) = String::from_utf8(vector) else { continue };

                Cow::Owned(string)
            }
        };

        let name = match filename {
            Cow::Borrowed(str) => Cow::Borrowed(str.as_bytes()),
            Cow::Owned(str) => Cow::Owned(str.into_bytes()),
        };

        let content = match content {
            Cow::Borrowed(s) => Cow::Borrowed(s.as_bytes()),
            Cow::Owned(s) => Cow::Owned(s.into_bytes()),
        };

        db.add(File::new(name, FileType::Builtin, None, content));
    }

    db
}

fn get_prelude_metadata(arena: &mut LocalArena, database: &ReadDatabase) -> CodebaseMetadata {
    let mut metadata = CodebaseMetadata::default();
    for file in database.files() {
        arena.reset();

        let file_metadata = scan_file_for_metadata(&file, arena);

        metadata.extend(file_metadata);
    }

    metadata
}

fn scan_file_for_metadata<A>(source_file: &File, arena: &A) -> CodebaseMetadata
where
    A: Arena,
{
    let program = parse_file(arena, source_file);
    if program.has_errors() {
        panic!(
            "Prelude file '{}' has parse errors, which is not allowed.",
            String::from_utf8_lossy(source_file.name.as_ref())
        );
    }

    let resolver = NameResolver::new(arena);
    let resolved_names = resolver.resolve(program);

    scan_program(arena, source_file, program, &resolved_names, mago_php_version::PHPVersion::LATEST)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::Instant;

    use crate::Prelude;

    /// The other jobs the pool holds at most at once, so its threads always find one and the queue stays short.
    const QUEUED_AT_MOST: usize = 64;

    /// The builds the test runs. With the build on the caller's pool, another job ran on the building thread in 16 of 20
    /// runs of one build, and in 20 of 20 runs of three.
    const BUILDS: usize = 3;

    static BUILDER: AtomicUsize = AtomicUsize::new(usize::MAX);
    static BUILDING: AtomicBool = AtomicBool::new(false);
    static QUEUED: AtomicUsize = AtomicUsize::new(0);
    static DURING_THE_BUILD: AtomicUsize = AtomicUsize::new(0);
    static ON_THE_BUILDER: AtomicUsize = AtomicUsize::new(0);

    /// A job of the pool other than the build, which works for 50 microseconds and counts whether it ran during the
    /// build and on the building thread.
    fn other_job() {
        QUEUED.fetch_sub(1, Ordering::SeqCst);
        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(50) {
            std::hint::spin_loop();
        }
        if BUILDING.load(Ordering::SeqCst) {
            DURING_THE_BUILD.fetch_add(1, Ordering::SeqCst);
            if rayon::current_thread_index() == Some(BUILDER.load(Ordering::SeqCst)) {
                ON_THE_BUILDER.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// Each test harness builds its prelude in a `LazyLock` that its analyses force from rayon jobs. A job that forced
    /// it on the rayon thread building it would wait on that thread, which waits on the job: the harness hangs. So the
    /// thread that builds the prelude runs none of its pool's other jobs, while the pool's other threads run them.
    #[test]
    fn a_rayon_thread_that_builds_the_prelude_runs_no_other_job_of_its_pool_until_the_build_returns() {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        for _ in 0..BUILDS {
            std::thread::scope(|scope| {
                let build = scope.spawn(|| {
                    pool.install(|| {
                        BUILDER.store(rayon::current_thread_index().unwrap(), Ordering::SeqCst);
                        BUILDING.store(true, Ordering::SeqCst);
                        let prelude = Prelude::build();
                        BUILDING.store(false, Ordering::SeqCst);
                        prelude
                    })
                });
                while !build.is_finished() {
                    if QUEUED.load(Ordering::SeqCst) < QUEUED_AT_MOST {
                        QUEUED.fetch_add(1, Ordering::SeqCst);
                        pool.spawn(other_job);
                    } else {
                        std::thread::yield_now();
                    }
                }
            });
        }

        assert!(DURING_THE_BUILD.load(Ordering::SeqCst) > 0, "the pool ran no other job during the build");
        assert_eq!(ON_THE_BUILDER.load(Ordering::SeqCst), 0, "other jobs ran on the building thread during the build");
    }
}
