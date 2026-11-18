use std::hash::BuildHasher;
use std::hash::Hasher;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicPtr;
use std::sync::atomic::Ordering;

use foldhash::fast::FixedState;

use crate::bumpalloc::LeakyBumpAlloc;

/// Header that precedes the bytes of an interned [`Word`](crate::Word) entry.
///
/// Memory layout, written contiguously by the arena, with no padding between the header
/// and the bytes (entry alignment is the natural alignment of [`Entry`] itself, and the
/// bytes follow immediately):
///
/// ```text
/// | hash: u64 | len: u32 | _pad: u32 | lowercase: AtomicPtr<Entry> | bytes: [u8; len] |
///   ^ Entry                                                           ^ Entry::bytes()
/// ```
///
/// `lowercase` is the entry of the bytes' ASCII lowercase form, the entry itself when the bytes hold no ASCII uppercase
/// letter, or null until [`lowercase`] is first asked for it.
#[repr(C)]
pub(crate) struct Entry {
    pub(crate) hash: u64,
    pub(crate) len: u32,
    _pad: u32,
    lowercase: AtomicPtr<Entry>,
}

impl Entry {
    /// Returns the interned byte slice that follows an entry's header in the arena.
    ///
    /// Takes a raw pointer rather than `&self` on purpose: a `&Entry` carries provenance
    /// over only the header, so reading the trailing bytes through it is out of bounds
    /// under Stacked Borrows. `ptr` comes straight from the arena allocation and carries
    /// provenance over the whole header-plus-bytes block.
    ///
    /// # Safety
    ///
    /// `ptr` must point at an `Entry` produced by the interner: a header followed by `len`
    /// initialized bytes inside the leaked arena, with provenance over that whole block.
    #[allow(clippy::multiple_unsafe_ops_per_block)]
    pub(crate) unsafe fn bytes(ptr: *const Entry) -> &'static [u8] {
        // SAFETY: `ptr` is a valid pointer to an `Entry` header, so reading the `len` field is safe.
        unsafe {
            let len = (*ptr).len as usize;
            let data = ptr.cast::<u8>().add(std::mem::size_of::<Entry>());
            std::slice::from_raw_parts(data, len)
        }
    }
}

/// Number of shards. Each shard owns its own table, arena and writer lock, so concurrent insertions rarely contend.
const NUM_SHARDS: usize = 64;

/// Initial table capacity (slots) per shard. Sized so the prelude warm-up doesn't
/// trigger many resizes.
const INITIAL_TABLE_CAPACITY: usize = 1 << 14; // 16K slots/shard, 1M total

/// Initial bytes per shard arena. Grows by doubling on overflow.
const INITIAL_ARENA_BYTES: usize = 64 * 1024;

/// An open-addressed table of pointers to entries in the arena. Null is the sentinel for an empty slot, and a slot
/// once filled never changes, so a reader probes it without a lock.
struct Table {
    slots: Box<[AtomicPtr<Entry>]>,
    mask: usize,
}

impl Table {
    fn new(capacity: usize) -> Table {
        Table {
            slots: std::iter::repeat_with(|| AtomicPtr::new(std::ptr::null_mut())).take(capacity).collect(),
            mask: capacity - 1,
        }
    }

    /// Finds the entry of `bytes` in this table, or the empty slot a new entry of them goes in.
    #[allow(clippy::multiple_unsafe_ops_per_block)]
    fn find(&self, bytes: &[u8], hash: u64) -> Result<NonNull<Entry>, usize> {
        let mut pos = (hash as usize) & self.mask;
        let mut probe: usize = 0;

        loop {
            // SAFETY: `pos` is masked with `self.mask = slots.len() - 1`, so it is always within bounds of `slots`.
            let slot = unsafe { self.slots.get_unchecked(pos) }.load(Ordering::Acquire);
            if slot.is_null() {
                return Err(pos);
            }

            // SAFETY: every non-null slot points at an `Entry` written in full before the release store that published
            // it, which the acquire load above synchronizes with; the arena outlives the program. Reading the header
            // fields and trailing bytes through the raw `slot` keeps its whole-block provenance (see `Entry::bytes`).
            let matched =
                unsafe { (*slot).hash == hash && (*slot).len as usize == bytes.len() && Entry::bytes(slot) == bytes };

            if matched {
                // SAFETY: `slot` was checked non-null above.
                return Ok(unsafe { NonNull::new_unchecked(slot) });
            }

            probe += 1;
            debug_assert!(probe <= self.mask, "interner probe walked the entire table");
            pos = (pos + probe) & self.mask;
        }
    }
}

/// What only the shard's writer, under its lock, changes: the count of entries and the arenas.
struct Writer {
    num_entries: usize,
    /// Current arena. Old arenas are kept alive in [`Writer::old_allocs`] so previously-handed-out pointers remain valid
    /// for the lifetime of the process.
    alloc: LeakyBumpAlloc,
    old_allocs: Vec<LeakyBumpAlloc>,
}

// SAFETY: A `Writer` owns its arenas exclusively, and the surrounding `Mutex` ensures only one thread mutates them at a
// time. The arenas are leaked, so the pointers into them outlive every thread.
unsafe impl Send for Writer {}

struct Shard {
    /// The current table. Only the writer, under its lock, replaces it, and a table it replaces is leaked, as the arena
    /// is, since a reader may still be probing it.
    table: AtomicPtr<Table>,
    writer: Mutex<Writer>,
}

impl Shard {
    fn new() -> Shard {
        Shard {
            table: AtomicPtr::new(Box::into_raw(Box::new(Table::new(INITIAL_TABLE_CAPACITY)))),
            writer: Mutex::new(Writer {
                num_entries: 0,
                alloc: LeakyBumpAlloc::new(INITIAL_ARENA_BYTES, std::mem::align_of::<Entry>()),
                old_allocs: Vec::new(),
            }),
        }
    }

    fn table(&self) -> &Table {
        // SAFETY: the table pointer always holds a table this shard allocated and never frees.
        unsafe { &*self.table.load(Ordering::Acquire) }
    }

    /// Finds the entry of `bytes`, or inserts one and returns its pointer. The returned pointer is stable for the
    /// lifetime of the process.
    #[allow(clippy::expect_used)]
    fn intern(&self, bytes: &[u8], hash: u64) -> NonNull<Entry> {
        if let Ok(entry) = self.table().find(bytes, hash) {
            return entry;
        }

        // `expect` is acceptable here: a poisoned mutex means another thread panicked while holding the writer lock,
        // which is an unrecoverable invariant violation.
        let mut writer = self.writer.lock().expect("interner shard mutex poisoned");
        // Under the lock the table is the writer's own, so a second find sees every entry and gives the slot to fill.
        let table = self.table();
        let pos = match table.find(bytes, hash) {
            Ok(entry) => return entry,
            Err(pos) => pos,
        };

        let entry = writer.allocate(bytes, hash);
        // SAFETY: `pos` is within `slots` bounds, as `find` gave it.
        unsafe { table.slots.get_unchecked(pos) }.store(entry.as_ptr(), Ordering::Release);
        writer.num_entries += 1;
        if writer.num_entries * 2 > table.mask {
            self.grow(table);
        }

        entry
    }

    /// Doubles the table and re-inserts every existing pointer into the new one, which it then publishes. Existing
    /// arena pointers are unchanged: only the index table grows. Called with the writer lock held.
    fn grow(&self, table: &Table) {
        let grown = Table::new((table.mask + 1) * 2);
        for slot in &table.slots {
            let ptr = slot.load(Ordering::Relaxed);
            if ptr.is_null() {
                continue;
            }

            // SAFETY: every non-null slot is a pointer to an `Entry` previously written by `allocate`; the arena is
            // leaky so the pointer is still valid.
            let hash = unsafe { (*ptr).hash };
            let mut pos = (hash as usize) & grown.mask;
            let mut probe: usize = 0;
            while !grown.slots[pos].load(Ordering::Relaxed).is_null() {
                probe += 1;
                debug_assert!(probe <= grown.mask, "grow probe walked the entire table");
                pos = (pos + probe) & grown.mask;
            }

            grown.slots[pos].store(ptr, Ordering::Relaxed);
        }

        self.table.store(Box::into_raw(Box::new(grown)), Ordering::Release);
    }
}

impl Writer {
    /// Writes a new entry of `bytes` into the arena and returns its pointer.
    #[allow(clippy::multiple_unsafe_ops_per_block)]
    #[allow(clippy::cast_ptr_alignment)]
    #[allow(clippy::expect_used)]
    fn allocate(&mut self, bytes: &[u8], hash: u64) -> NonNull<Entry> {
        let entry_size = std::mem::size_of::<Entry>() + bytes.len();
        let aligned_size = round_up(entry_size, std::mem::align_of::<Entry>());
        if self.alloc.allocated() + aligned_size > self.alloc.capacity() {
            // `expect` here: doubling a usize past `usize::MAX` is impossible while the
            // process has free virtual memory; this is a programmer-bug guard, not runtime.
            let new_capacity =
                self.alloc.capacity().checked_mul(2).expect("arena capacity doubled overflow").max(aligned_size);
            let new_alloc = LeakyBumpAlloc::new(new_capacity, std::mem::align_of::<Entry>());
            let old_alloc = std::mem::replace(&mut self.alloc, new_alloc);
            self.old_allocs.push(old_alloc);
        }

        // SAFETY: `allocate` returns a pointer to a freshly-reserved `aligned_size`-byte block
        // inside the current arena. The block is aligned to `align_of::<Entry>()` (matching
        // the arena's construction), so casting to `*mut Entry` is sound.
        let ptr = unsafe { self.alloc.allocate(aligned_size) }.cast::<Entry>();
        // SAFETY: `ptr` is the freshly-reserved aligned block. We write the header followed
        // by exactly `bytes.len()` bytes; the offset `size_of::<Entry>()` stays inside the
        // `aligned_size` region we just reserved.
        unsafe {
            std::ptr::write(
                ptr,
                Entry {
                    hash,
                    len: u32::try_from(bytes.len()).expect("word length exceeds u32::MAX"),
                    _pad: 0,
                    lowercase: AtomicPtr::new(std::ptr::null_mut()),
                },
            );

            let dest_ptr = ptr.cast::<u8>().add(std::mem::size_of::<Entry>());
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest_ptr, bytes.len());
        }

        // SAFETY: `ptr` was just written-to and is non-null (allocation didn't return null —
        // OOM aborts inside `LeakyBumpAlloc::allocate`).
        unsafe { NonNull::new_unchecked(ptr) }
    }
}

/// Global interner. Shards are lazily initialized on first use through the
/// surrounding [`OnceLock`].
struct Interner {
    shards: [Shard; NUM_SHARDS],
    hasher: FixedState,
}

impl Interner {
    fn new() -> Interner {
        Interner { shards: std::array::from_fn(|_| Shard::new()), hasher: FixedState::default() }
    }
}

static INTERNER: OnceLock<Interner> = OnceLock::new();

fn interner() -> &'static Interner {
    INTERNER.get_or_init(Interner::new)
}

/// Hashes `bytes` and returns the canonical entry pointer for them. Subsequent calls
/// with the same bytes return the same pointer.
pub(crate) fn intern(bytes: &[u8]) -> NonNull<Entry> {
    let interner = interner();

    let mut hasher = interner.hasher.build_hasher();
    hasher.write(bytes);
    let hash = hasher.finish();

    let shard_idx = (hash >> (u64::BITS as usize - SHARD_BITS)) as usize;
    interner.shards[shard_idx].intern(bytes, hash)
}

/// Returns the entry of the ASCII lowercase form of the entry `entry`'s bytes. The first call for an entry finds or
/// interns that form and links the entry to it, and every later call reads the link. Two threads that race on the first
/// call link the same pointer, as interning is canonical.
pub(crate) fn lowercase(entry: NonNull<Entry>) -> NonNull<Entry> {
    // SAFETY: `entry` points at an `Entry` the interner wrote, which lives for the process.
    let link = unsafe { &entry.as_ref().lowercase };
    if let Some(lowercase) = NonNull::new(link.load(Ordering::Acquire)) {
        return lowercase;
    }

    // SAFETY: as above, the entry's trailing bytes are initialized and live for the process.
    let bytes = unsafe { Entry::bytes(entry.as_ptr()) };
    let lowercase = if bytes.iter().any(u8::is_ascii_uppercase) { intern_lowercase(bytes) } else { entry };
    link.store(lowercase.as_ptr(), Ordering::Release);

    lowercase
}

/// Interns the ASCII lowercase form of `bytes`.
fn intern_lowercase(bytes: &[u8]) -> NonNull<Entry> {
    const STACK_BUFFER: usize = 256;

    if bytes.len() <= STACK_BUFFER {
        let mut buffer = [0u8; STACK_BUFFER];
        for (lowercase, byte) in buffer.iter_mut().zip(bytes) {
            *lowercase = byte.to_ascii_lowercase();
        }

        return intern(&buffer[..bytes.len()]);
    }

    intern(&bytes.to_ascii_lowercase())
}

/// Top-bits shift count for shard selection. Top bits of the hash, not the low bits,
/// because the low bits are also used for in-shard table indexing.
const SHARD_BITS: usize = 6; // log2(NUM_SHARDS)

const _: () = assert!(1 << SHARD_BITS == NUM_SHARDS, "SHARD_BITS must match NUM_SHARDS");

#[inline]
fn round_up(n: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two(), "alignment must be a power of two");
    (n + align - 1) & !(align - 1)
}
