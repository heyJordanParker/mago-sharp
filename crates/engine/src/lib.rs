//! Runs PHP inside Mago on the PHP# engine linked into the binary.
//!
//! Each thread that calls into an [`Engine`] owns one interpreter. The thread starts it on its first call, runs the
//! engine's scripts in it once, and serves every later call from it. A call passes [`Value`]s and receives one, and
//! the PHP it runs asks Mago questions through `\Mago\Engine\ask(...$arguments)`, which the call's `answer` answers.
//! `\Mago\Engine\class_count()` returns how many class-likes the interpreter has declared, which grows with each one.

use std::cell::Cell;
use std::cell::RefCell;
use std::ffi::CStr;
use std::ffi::CString;
use std::ffi::c_char;
use std::ffi::c_void;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::Weak;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

// The engine's ext/sharp calls the native functions this Rust library defines, so mago links it.
use sharp as _;

mod value;

pub use value::Value;

use value::Zval;
use value::engine_value_release;

/// What a call's `answer` gives the PHP that asked: a value, or a message the PHP receives as a `\RuntimeException`.
pub type Answer = Result<Value, String>;

/// The scripts each thread's interpreter runs when it starts, before it serves any call, the environment variables it
/// reads, and the PHP settings it runs with.
#[derive(Debug)]
pub struct Engine {
    id: u64,
    scripts: Vec<PathBuf>,
    variables: Arc<[(CString, CString)]>,
    settings: Vec<(CString, CString)>,
    reset: Mutex<Vec<u64>>,
}

/// Why a call into the engine failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The engine could not start in this process.
    Start,
    /// A thread's interpreter could not start a request.
    Interpreter,
    /// PHP refused the setting `name` of `value`: it knows no such setting, or the value is not one it takes.
    Setting { name: String, value: String },
    /// A script failed while the interpreter ran it at its start, with what it threw or printed.
    Script { script: PathBuf, message: String },
    /// The called function threw `Thrown`.
    Threw(Thrown),
    /// The call stopped on a fatal error, with what the engine printed. The interpreter is reset.
    Failed(String),
    /// The call ran past its deadline `duration`, stopped in `frames`, the functions it was running, innermost first.
    /// The interpreter is reset.
    Deadline { duration: Duration, frames: Vec<Frame> },
    /// The call returned a value of this engine type, which no [`Value`] holds.
    Value(u8),
    /// An argument or an answer holds a [`Value::Object`] or a [`Value::Case`] the interpreter can't build, and why.
    Build(String),
    /// The call was made from inside another call's `answer`, on the same thread.
    Nested,
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => write!(formatter, "the PHP# engine could not start"),
            Self::Interpreter => write!(formatter, "a PHP# interpreter could not start a request"),
            Self::Setting { name, value } => write!(formatter, "PHP refused the setting `{name}` = `{value}`"),
            Self::Script { script, message } => write!(formatter, "{} failed: {message}", script.display()),
            Self::Threw(thrown) => write!(formatter, "{thrown}"),
            Self::Failed(message) => write!(formatter, "{}", message.trim()),
            Self::Deadline { duration, .. } => {
                write!(formatter, "the call ran past its deadline of {} ms", duration.as_millis())
            }
            Self::Value(kind) => {
                write!(formatter, "the call returned a value of engine type {kind}, which Mago can't read")
            }
            Self::Nested => write!(formatter, "a call into the engine ran while the same thread answered another"),
            Self::Build(reason) => write!(formatter, "a value Mago gave the engine can't be built: {reason}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// An exception a call or a script threw: its class, its message, and the file and line it was thrown at.
///
/// A `CompileError` the engine throws for a PHP# file it refuses to load names that file. It displays as
/// `Class: message in file:line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thrown {
    pub class: String,
    pub message: String,
    pub file: PathBuf,
    pub line: i64,
}

impl Thrown {
    /// The exception the engine wrote as `read`, the list of its class, message, file and line, or else, when the
    /// engine could write none, an exception of no class whose message is what the interpreter printed.
    fn of(read: Result<Value, EngineError>) -> Self {
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        match read {
            Ok(Value::List(fields)) => match fields.as_slice() {
                [Value::String(class), Value::String(message), Value::String(file), Value::Int(line)] => {
                    Self { class: text(class), message: text(message), file: PathBuf::from(text(file)), line: *line }
                }
                _ => Self::printed(),
            },
            _ => Self::printed(),
        }
    }

    fn printed() -> Self {
        Self { class: String::new(), message: output(), file: PathBuf::new(), line: 0 }
    }
}

impl fmt::Display for Thrown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.class.is_empty() {
            return write!(formatter, "{}", self.message);
        }

        write!(formatter, "{}: {} in {}:{}", self.class, self.message, self.file.display(), self.line)
    }
}

/// A function a call was running: the class it belongs to, empty for a function of no class, and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub class: String,
    pub function: String,
}

impl Frame {
    /// The frames the engine wrote as `read`, each the list of its class and function, or none when it wrote none.
    fn all(read: Result<Value, EngineError>) -> Vec<Self> {
        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        let Ok(Value::List(frames)) = read else {
            return Vec::new();
        };

        frames
            .iter()
            .filter_map(|frame| match frame {
                Value::List(fields) => match fields.as_slice() {
                    [Value::String(class), Value::String(function)] => {
                        Some(Self { class: text(class), function: text(function) })
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
}

const DONE: i32 = 0;
const THREW: i32 = 1;

static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);
static NEXT_INTERPRETER: AtomicU64 = AtomicU64::new(1);

unsafe extern "C" {
    fn engine_start() -> i32;
    fn engine_interpreter_start() -> i32;
    fn engine_set(name: *const c_char, name_length: usize, value: *const c_char, value_length: usize) -> i32;
    fn engine_interpreter_stop();
    fn engine_thread_end();
    fn engine_run_script(path: *const c_char, result: *mut Zval) -> i32;
    fn engine_call(function: *const c_char, length: usize, arguments: *mut Zval, count: u32, result: *mut Zval) -> i32;
    fn engine_executor_globals() -> *mut c_void;
    fn engine_interrupt(globals: *mut c_void);
    fn engine_clear_interrupt(globals: *mut c_void);
}

/// One thread's interpreter: the engine whose scripts it ran, its id, the engine globals a deadline interrupts, and the
/// variables it reads, which it holds until it has stopped, so its shutdown functions read them as it stops.
struct Interpreter {
    engine: u64,
    id: u64,
    globals: *mut c_void,
    #[allow(
        dead_code,
        reason = "holding it keeps the variables `VARIABLES` points to until the interpreter has stopped"
    )]
    variables: Arc<[(CString, CString)]>,
}

impl Drop for Interpreter {
    fn drop(&mut self) {
        // SAFETY: an interpreter is dropped on the thread that started it, which no call runs on meanwhile.
        unsafe { engine_interpreter_stop() };
        VARIABLES.set(NO_VARIABLES);
    }
}

/// The variables the calling thread's interpreter reads, which point into its own [`Interpreter::variables`].
type Variables = *const [(CString, CString)];

const NO_VARIABLES: Variables = &[];

/// The calling thread's place in the engine: its interpreter while one runs, and whether the engine knows the thread.
#[derive(Default)]
struct Thread {
    interpreter: Option<Interpreter>,
    known: bool,
}

impl Drop for Thread {
    fn drop(&mut self) {
        drop(self.interpreter.take());
        if self.known {
            // SAFETY: the thread ends, and its interpreter stopped just above.
            unsafe { engine_thread_end() };
        }
    }
}

type Answering = *mut (dyn FnMut(Vec<Value>) -> Answer + 'static);

thread_local! {
    static THREAD: RefCell<Thread> = RefCell::default();
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static ANSWERING: Cell<Option<Answering>> = const { Cell::new(None) };
    /// A pointer with no destructor, so the engine reads it while the thread ends too, as its interpreter stops then.
    static VARIABLES: Cell<Variables> = const { Cell::new(NO_VARIABLES) };
    static SLOT: Arc<Slot> = Deadlines::get().register();
}

impl Engine {
    /// An engine whose interpreters each run with the PHP `settings`, by name, as `php -d` sets them, then run
    /// `scripts`, in order, when they start, and read `variables` as environment variables, through `getenv()`, `$_ENV`
    /// and `$_SERVER`, over the process's own. The engine starts in this process on the first one.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine can't start in this process.
    pub fn new(
        scripts: Vec<PathBuf>,
        variables: Vec<(CString, CString)>,
        settings: Vec<(CString, CString)>,
    ) -> Result<Self, EngineError> {
        static STARTED: OnceLock<bool> = OnceLock::new();
        // SAFETY: the engine starts once per process, here.
        if !*STARTED.get_or_init(|| unsafe { engine_start() } == 0) {
            return Err(EngineError::Start);
        }

        Ok(Self {
            id: NEXT_ENGINE.fetch_add(1, Ordering::Relaxed),
            scripts,
            variables: Arc::from(variables),
            settings,
            reset: Mutex::default(),
        })
    }

    /// The id of the calling thread's interpreter, while it runs this engine's scripts.
    #[must_use]
    pub fn interpreter(&self) -> Option<u64> {
        THREAD.with_borrow(|thread| {
            thread
                .interpreter
                .as_ref()
                .filter(|interpreter| interpreter.engine == self.id)
                .map(|interpreter| interpreter.id)
        })
    }

    /// Resets the interpreter `interpreter`: it ends now when it is the calling thread's, outside a call, and otherwise
    /// the thread that runs it starts a new one on its next call.
    pub fn reset(&self, interpreter: u64) {
        let ended = THREAD.with(|thread| {
            let Ok(mut thread) = thread.try_borrow_mut() else {
                return false;
            };
            let own = thread.interpreter.as_ref().is_some_and(|own| own.engine == self.id && own.id == interpreter);
            if own {
                drop(thread.interpreter.take());
            }

            own
        });
        if !ended {
            self.reset.lock().unwrap_or_else(PoisonError::into_inner).push(interpreter);
        }
    }

    /// Starts the deadline of the call that runs on this thread again, as when the call started, so a call that
    /// reports its progress through its `answer` runs as long as each part keeps within the deadline.
    pub fn restart_deadline(&self) {
        Deadlines::get().restart();
    }

    /// Calls the PHP function `function`, such as `name` or `Class::method`, with `arguments` in the calling thread's
    /// interpreter, and returns what it returns. While it runs, `answer` answers each `\Mago\Engine\ask()` it makes. A
    /// thread that runs no interpreter starts one before the call, outside `deadline`. A call that runs past `deadline`
    /// is interrupted.
    ///
    /// # Errors
    ///
    /// Returns an error when the interpreter can't start, the function throws or fails, the call runs past its
    /// deadline, or it returns a value no [`Value`] holds.
    pub fn call(
        &self,
        function: &str,
        arguments: &[Value],
        deadline: Duration,
        answer: &mut dyn FnMut(Vec<Value>) -> Answer,
    ) -> Result<Value, EngineError> {
        if ANSWERING.get().is_some() {
            return Err(EngineError::Nested);
        }

        THREAD.with_borrow_mut(|thread| {
            thread.known = true;
            OUTPUT.with_borrow_mut(Vec::clear);
            let reset = thread
                .interpreter
                .as_ref()
                .is_some_and(|interpreter| interpreter.engine != self.id || self.take_reset(interpreter.id));
            if reset {
                drop(thread.interpreter.take());
            }

            let globals = match &thread.interpreter {
                Some(interpreter) => interpreter.globals,
                None => thread.interpreter.insert(self.start()?).globals,
            };

            let deadlines = Deadlines::get();
            deadlines.watch(deadline, globals);
            let mut frames = Vec::new();
            let result = self.run(function, arguments, answer, &mut frames);
            let fired = deadlines.finish(globals);
            match result {
                Err(EngineError::Failed(_)) if fired => {
                    drop(thread.interpreter.take());
                    Err(EngineError::Deadline { duration: deadline, frames })
                }
                Err(EngineError::Failed(message)) => {
                    drop(thread.interpreter.take());
                    Err(EngineError::Failed(message))
                }
                result => result,
            }
        })
    }

    fn take_reset(&self, interpreter: u64) -> bool {
        let mut reset = self.reset.lock().unwrap_or_else(PoisonError::into_inner);
        let before = reset.len();
        reset.retain(|id| *id != interpreter);

        reset.len() != before
    }

    /// Calls `function` with `arguments` in the thread's interpreter, answering what it asks with `answer`. A call that
    /// fails on a fatal error gives `frames` the functions it was running, innermost first.
    #[allow(
        clippy::multiple_unsafe_ops_per_block,
        clippy::transmute_ptr_to_ptr,
        reason = "each operation is one step of the same engine API call sequence, which the block's SAFETY note covers"
    )]
    fn run(
        &self,
        function: &str,
        arguments: &[Value],
        answer: &mut dyn FnMut(Vec<Value>) -> Answer,
        frames: &mut Vec<Frame>,
    ) -> Result<Value, EngineError> {
        let mut slots: Vec<Zval> = arguments.iter().map(|_| Zval::new()).collect();
        let mut result = Zval::new();
        // SAFETY: the thread runs an interpreter, each slot is empty when written, `answer` outlives the call it is
        // reachable from, and every value the engine wrote is released before the slots go.
        unsafe {
            let written = arguments.iter().zip(&mut slots).try_for_each(|(argument, slot)| argument.write(slot));
            if let Err(error) = written {
                for slot in &mut slots {
                    engine_value_release(slot);
                }

                return Err(error);
            }

            let answering: *mut (dyn FnMut(Vec<Value>) -> Answer + '_) = answer;
            ANSWERING
                .set(Some(std::mem::transmute::<*mut (dyn FnMut(Vec<Value>) -> Answer + '_), Answering>(answering)));
            let status = engine_call(
                function.as_ptr().cast(),
                function.len(),
                slots.as_mut_ptr(),
                slots.len() as u32,
                &raw mut result,
            );
            ANSWERING.set(None);
            for slot in &mut slots {
                engine_value_release(slot);
            }

            let read = Value::read(&raw const result);
            engine_value_release(&raw mut result);
            match status {
                DONE => read,
                THREW => Err(EngineError::Threw(Thrown::of(read))),
                _ => {
                    *frames = Frame::all(read);

                    Err(EngineError::Failed(output()))
                }
            }
        }
    }

    /// Starts the calling thread's interpreter, sets this engine's settings in it, and runs its scripts.
    #[allow(
        clippy::multiple_unsafe_ops_per_block,
        reason = "each operation is one step of the same engine API call sequence, which the block's SAFETY note covers"
    )]
    fn start(&self) -> Result<Interpreter, EngineError> {
        // SAFETY: the thread runs no interpreter, so it starts one, and it stops it when the start fails.
        unsafe {
            let variables = Arc::clone(&self.variables);
            VARIABLES.set(Arc::as_ptr(&variables));
            if engine_interpreter_start() != 0 {
                VARIABLES.set(NO_VARIABLES);
                return Err(EngineError::Interpreter);
            }

            let interpreter = Interpreter {
                engine: self.id,
                id: NEXT_INTERPRETER.fetch_add(1, Ordering::Relaxed),
                globals: engine_executor_globals(),
                variables,
            };
            for (name, value) in &self.settings {
                let (name, value) = (name.as_bytes(), value.as_bytes());
                if engine_set(name.as_ptr().cast(), name.len(), value.as_ptr().cast(), value.len()) != 0 {
                    return Err(EngineError::Setting {
                        name: String::from_utf8_lossy(name).into_owned(),
                        value: String::from_utf8_lossy(value).into_owned(),
                    });
                }
            }
            for script in &self.scripts {
                let path = std::ffi::CString::new(script.as_os_str().as_encoded_bytes()).map_err(|_| {
                    EngineError::Script { script: script.clone(), message: "the path holds a NUL byte".into() }
                })?;
                let mut result = Zval::new();
                let status = engine_run_script(path.as_ptr(), &raw mut result);
                let read = Value::read(&raw const result);
                engine_value_release(&raw mut result);
                let message = match status {
                    DONE => continue,
                    THREW => Thrown::of(read).to_string(),
                    _ => output(),
                };

                return Err(EngineError::Script { script: script.clone(), message });
            }

            Ok(interpreter)
        }
    }
}

/// What the calling thread's interpreter printed since its call started.
fn output() -> String {
    OUTPUT.with_borrow(|output| String::from_utf8_lossy(output).into_owned())
}

/// Answers a `\Mago\Engine\ask()` with the `answer` of the call that runs on this thread.
///
/// # Safety
///
/// The engine calls it with `count` live arguments and an empty `result`, on a thread whose call set its answer.
#[allow(
    clippy::multiple_unsafe_ops_per_block,
    reason = "each operation is one step of the same engine API call sequence, which the block's SAFETY note covers"
)]
#[unsafe(no_mangle)]
unsafe extern "C" fn mago_engine_answer(arguments: *mut Zval, count: u32, result: *mut Zval) -> i32 {
    // SAFETY: the engine gives `count` live arguments and an empty result, and the answer pointer stays valid for the
    // call that set it, which runs below on this thread.
    unsafe {
        let answer = (0..count as usize)
            .map(|index| Value::read(arguments.add(index)).map_err(|error| error.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|arguments| match ANSWERING.get() {
                Some(answering) => (*answering)(arguments),
                None => Err("\\Mago\\Engine\\ask() was called outside a call from Mago".to_owned()),
            });
        let written = answer.and_then(|value| {
            value.write(result).map_err(|error| {
                engine_value_release(result);
                error.to_string()
            })
        });
        match written {
            Ok(()) => 0,
            Err(message) => {
                let _ = Value::String(message.into_bytes()).write(result);
                1
            }
        }
    }
}

/// Keeps what the calling thread's interpreter prints, which a failed call reports.
///
/// # Safety
///
/// The engine calls it with `length` readable bytes at `bytes`.
#[unsafe(no_mangle)]
unsafe extern "C" fn mago_engine_output(bytes: *const c_char, length: usize) {
    // SAFETY: the engine gives `length` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), length) };
    let _ = OUTPUT.try_with(|output| output.borrow_mut().extend_from_slice(bytes));
}

/// The value of the environment variable `name`, of `length` bytes, that the calling thread's engine sets, or null.
///
/// # Safety
///
/// The engine calls it with `length` readable bytes at `name`, and copies the value before the thread's next call.
#[unsafe(no_mangle)]
unsafe extern "C" fn mago_engine_variable(name: *const c_char, length: usize) -> *const c_char {
    // SAFETY: the engine gives `length` readable bytes.
    let name = unsafe { std::slice::from_raw_parts(name.cast::<u8>(), length) };
    // SAFETY: the engine asks while the calling thread's interpreter runs, which holds the variables until it stops.
    let variables = unsafe { &*VARIABLES.get() };
    variables.iter().find(|(set, _)| set.as_bytes() == name).map_or(std::ptr::null(), |(_, value)| value.as_ptr())
}

/// Gives the name and value of the environment variable at `index` that the calling thread's engine sets, and returns
/// whether there is one.
///
/// # Safety
///
/// The engine calls it with two writable pointers, and copies both strings before the thread's next call.
#[unsafe(no_mangle)]
unsafe extern "C" fn mago_engine_variable_at(
    index: usize,
    name: *mut *const c_char,
    value: *mut *const c_char,
) -> bool {
    // SAFETY: the engine asks while the calling thread's interpreter runs, which holds the variables until it stops.
    let variables = unsafe { &*VARIABLES.get() };
    let Some((set, held)) = variables.get(index) else {
        return false;
    };
    // SAFETY: the engine gives a writable pointer for the name.
    unsafe { *name = CStr::as_ptr(set) };
    // SAFETY: the engine gives a writable pointer for the value.
    unsafe { *value = CStr::as_ptr(held) };

    true
}

/// The deadline of the call the calling thread runs, if it runs one. Only the thread and the deadlines thread lock it,
/// so a call's lock waits for nothing but the deadlines thread's look at it.
#[derive(Default)]
struct Slot {
    call: Mutex<Option<Deadline>>,
}

/// One call's deadline, which the deadlines thread enforces by setting its interpreter's timeout flag and VM
/// interrupt, so the engine stops the PHP at its next instruction boundary.
struct Deadline {
    at: Instant,
    after: Duration,
    globals: usize,
    fired: bool,
}

/// Every thread's [`Slot`], and when the deadlines thread wakes next. A call wakes it only when the call's deadline
/// comes before that, so a stream of calls that each end in time wakes it once per deadline, not twice per call.
struct Deadlines {
    slots: Mutex<Vec<Weak<Slot>>>,
    /// Held by the deadlines thread except while it sleeps, so a call that wakes it can't wake it before it sleeps.
    sleeping: Mutex<()>,
    changed: Condvar,
    /// When the deadlines thread wakes next, in nanoseconds since `epoch`, or `u64::MAX` while it waits for no
    /// deadline or looks at the slots.
    wake: AtomicU64,
    epoch: Instant,
}

impl Deadlines {
    fn get() -> &'static Self {
        static DEADLINES: OnceLock<&'static Deadlines> = OnceLock::new();
        DEADLINES.get_or_init(|| {
            let deadlines: &'static Self = Box::leak(Box::new(Self {
                slots: Mutex::default(),
                sleeping: Mutex::default(),
                changed: Condvar::new(),
                wake: AtomicU64::new(u64::MAX),
                epoch: Instant::now(),
            }));
            std::thread::Builder::new()
                .name("mago-engine-deadlines".to_owned())
                .spawn(|| deadlines.enforce())
                .map_or_else(|error| panic!("the engine's deadlines thread could not start: {error}"), drop);
            deadlines
        })
    }

    /// The calling thread's slot, which the deadlines thread looks at from now on.
    fn register(&self) -> Arc<Slot> {
        let slot = Arc::new(Slot::default());
        lock(&self.slots).push(Arc::downgrade(&slot));
        slot
    }

    /// Starts the deadline of the call the calling thread starts, `after` from now, which interrupts `globals`, the
    /// thread's interpreter.
    fn watch(&self, after: Duration, globals: *mut c_void) {
        let at = Instant::now() + after;
        SLOT.with(|slot| {
            *lock(&slot.call) = Some(Deadline { at, after, globals: globals as usize, fired: false });
        });
        self.wake_by(at);
    }

    /// Moves the deadline of the calling thread's call to its `after` from now, unless it already passed.
    fn restart(&self) {
        SLOT.with(|slot| {
            if let Some(deadline) = lock(&slot.call).as_mut().filter(|deadline| !deadline.fired) {
                deadline.at = Instant::now() + deadline.after;
            }
        });
    }

    /// Ends the deadline of the calling thread's call and returns whether it passed, clearing an interrupt of
    /// `globals` that came after the call ended.
    fn finish(&self, globals: *mut c_void) -> bool {
        let fired = SLOT.with(|slot| lock(&slot.call).take().is_some_and(|deadline| deadline.fired));
        if fired {
            // SAFETY: the interpreter still runs on this thread, and no call runs on it now.
            unsafe { engine_clear_interrupt(globals) };
        }

        fired
    }

    /// Wakes the deadlines thread when a deadline `at` comes before the thread wakes on its own.
    fn wake_by(&self, at: Instant) {
        if self.nanos(at) < self.wake.load(Ordering::SeqCst) {
            let _sleeping = lock(&self.sleeping);
            self.changed.notify_one();
        }
    }

    fn nanos(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_nanos()).unwrap_or(u64::MAX - 1)
    }

    /// Interrupts each interpreter whose deadline passed, and sleeps until the next deadline.
    fn enforce(&self) -> ! {
        let mut sleeping = lock(&self.sleeping);
        loop {
            self.wake.store(u64::MAX, Ordering::SeqCst);
            let now = Instant::now();
            let mut next: Option<Instant> = None;
            lock(&self.slots).retain(|slot| {
                let Some(slot) = slot.upgrade() else {
                    return false;
                };
                if let Some(deadline) = lock(&slot.call).as_mut().filter(|deadline| !deadline.fired) {
                    if deadline.at <= now {
                        // SAFETY: a deadline's interpreter runs until its call finishes the deadline, which takes the
                        // slot's lock first, and setting its flags is what the engine's own timer does.
                        unsafe { engine_interrupt(deadline.globals as *mut c_void) };
                        deadline.fired = true;
                    } else {
                        next = Some(next.map_or(deadline.at, |next| next.min(deadline.at)));
                    }
                }

                true
            });

            sleeping = match next {
                Some(at) => {
                    self.wake.store(self.nanos(at), Ordering::SeqCst);
                    self.changed
                        .wait_timeout(sleeping, at.saturating_duration_since(Instant::now()))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.changed.wait(sleeping).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
