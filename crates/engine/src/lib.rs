//! Runs PHP inside Mago on the PHP# engine linked into the binary.
//!
//! Each thread that calls into an [`Engine`] owns one interpreter. The thread starts it on its first call, runs the
//! engine's scripts in it once, and serves every later call from it. A call passes [`Value`]s and receives one, and
//! the PHP it runs asks Mago questions through `\Mago\Engine\ask(...$arguments)`, which the call's `answer` answers.

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
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

mod value;

pub use value::Value;

use value::Zval;
use value::engine_value_release;

/// What a call's `answer` gives the PHP that asked: a value, or a message the PHP receives as a `\RuntimeException`.
pub type Answer = Result<Value, String>;

/// The scripts each thread's interpreter runs when it starts, before it serves any call, and the environment
/// variables it reads.
#[derive(Debug)]
pub struct Engine {
    id: u64,
    scripts: Vec<PathBuf>,
    variables: Arc<[(CString, CString)]>,
    reset: Mutex<Vec<u64>>,
}

/// Why a call into the engine failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The engine could not start in this process.
    Start,
    /// A thread's interpreter could not start a request.
    Interpreter,
    /// A script failed while the interpreter ran it at its start, with what it threw or printed.
    Script { script: PathBuf, message: String },
    /// The called function threw, as `Class: message in file:line`.
    Threw(String),
    /// The call stopped on a fatal error, with what the engine printed. The interpreter is reset.
    Failed(String),
    /// The call ran past its deadline. The interpreter is reset.
    Deadline(Duration),
    /// The call returned a value of this engine type, which no [`Value`] holds.
    Value(u8),
    /// The call was made from inside another call's `answer`, on the same thread.
    Nested,
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start => write!(formatter, "the PHP# engine could not start"),
            Self::Interpreter => write!(formatter, "a PHP# interpreter could not start a request"),
            Self::Script { script, message } => write!(formatter, "{} failed: {message}", script.display()),
            Self::Threw(message) => write!(formatter, "{message}"),
            Self::Failed(message) => write!(formatter, "{}", message.trim()),
            Self::Deadline(deadline) => {
                write!(formatter, "the call ran past its deadline of {} ms", deadline.as_millis())
            }
            Self::Value(kind) => {
                write!(formatter, "the call returned a value of engine type {kind}, which Mago can't read")
            }
            Self::Nested => write!(formatter, "a call into the engine ran while the same thread answered another"),
        }
    }
}

impl std::error::Error for EngineError {}

const DONE: i32 = 0;
const THREW: i32 = 1;

static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);
static NEXT_INTERPRETER: AtomicU64 = AtomicU64::new(1);

unsafe extern "C" {
    fn engine_start() -> i32;
    fn engine_interpreter_start() -> i32;
    fn engine_interpreter_stop();
    fn engine_thread_end();
    fn engine_run_script(path: *const c_char, result: *mut Zval) -> i32;
    fn engine_call(function: *const c_char, length: usize, arguments: *mut Zval, count: u32, result: *mut Zval) -> i32;
    fn engine_executor_globals() -> *mut c_void;
    fn engine_interrupt(globals: *mut c_void);
    fn engine_clear_interrupt(globals: *mut c_void);
}

/// One thread's interpreter: the engine whose scripts it ran, its id, and the engine globals a deadline interrupts.
struct Interpreter {
    engine: u64,
    id: u64,
    globals: *mut c_void,
}

impl Drop for Interpreter {
    fn drop(&mut self) {
        // SAFETY: an interpreter is dropped on the thread that started it, which no call runs on meanwhile.
        unsafe { engine_interpreter_stop() };
    }
}

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
    static VARIABLES: RefCell<Arc<[(CString, CString)]>> = RefCell::default();
    static WATCHING: Cell<Option<(u64, Duration)>> = const { Cell::new(None) };
}

impl Engine {
    /// An engine whose interpreters each run `scripts`, in order, when they start, and read `variables` as
    /// environment variables, through `getenv()`, `$_ENV` and `$_SERVER`, over the process's own. The engine starts
    /// in this process on the first one.
    ///
    /// # Errors
    ///
    /// Returns an error when the engine can't start in this process.
    pub fn new(scripts: Vec<PathBuf>, variables: Vec<(CString, CString)>) -> Result<Self, EngineError> {
        static STARTED: OnceLock<bool> = OnceLock::new();
        // SAFETY: the engine starts once per process, here.
        if !*STARTED.get_or_init(|| unsafe { engine_start() } == 0) {
            return Err(EngineError::Start);
        }

        Ok(Self {
            id: NEXT_ENGINE.fetch_add(1, Ordering::Relaxed),
            scripts,
            variables: Arc::from(variables),
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
        if let Some((token, after)) = WATCHING.get() {
            Deadlines::get().restart(token, after);
        }
    }

    /// Calls the PHP function `function`, such as `name` or `Class::method`, with `arguments` in the calling thread's
    /// interpreter, and returns what it returns. While it runs, `answer` answers each `\Mago\Engine\ask()` it makes. A
    /// call that runs past `deadline` is interrupted.
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

            VARIABLES.with_borrow_mut(|variables| *variables = Arc::clone(&self.variables));
            let watch = Watch::start(deadline);
            WATCHING.set(Some((watch.token, deadline)));
            let result = self.run(thread, function, arguments, answer, &watch);
            WATCHING.set(None);
            let fired = watch.finish(thread.interpreter.as_ref().map(|interpreter| interpreter.globals));
            match result {
                Err(EngineError::Failed(_) | EngineError::Script { .. }) if fired => {
                    drop(thread.interpreter.take());
                    Err(EngineError::Deadline(deadline))
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

    #[allow(
        clippy::multiple_unsafe_ops_per_block,
        clippy::transmute_ptr_to_ptr,
        reason = "each operation is one step of the same engine API call sequence, which the block's SAFETY note covers"
    )]
    fn run(
        &self,
        thread: &mut Thread,
        function: &str,
        arguments: &[Value],
        answer: &mut dyn FnMut(Vec<Value>) -> Answer,
        watch: &Watch,
    ) -> Result<Value, EngineError> {
        match &thread.interpreter {
            Some(interpreter) => watch.arm(interpreter.globals),
            None => thread.interpreter = Some(self.start(watch)?),
        }

        let mut slots: Vec<Zval> = arguments.iter().map(|_| Zval::new()).collect();
        let mut result = Zval::new();
        // SAFETY: the thread runs an interpreter, each slot is empty when written, `answer` outlives the call it is
        // reachable from, and every value the engine wrote is released before the slots go.
        unsafe {
            for (argument, slot) in arguments.iter().zip(&mut slots) {
                argument.write(slot);
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
                THREW => match read {
                    Ok(Value::String(message)) => {
                        Err(EngineError::Threw(String::from_utf8_lossy(&message).into_owned()))
                    }
                    _ => Err(EngineError::Threw(output())),
                },
                _ => Err(EngineError::Failed(output())),
            }
        }
    }

    /// Starts the calling thread's interpreter and runs this engine's scripts in it.
    #[allow(
        clippy::multiple_unsafe_ops_per_block,
        reason = "each operation is one step of the same engine API call sequence, which the block's SAFETY note covers"
    )]
    fn start(&self, watch: &Watch) -> Result<Interpreter, EngineError> {
        // SAFETY: the thread runs no interpreter, so it starts one, and it stops it when the start fails.
        unsafe {
            if engine_interpreter_start() != 0 {
                return Err(EngineError::Interpreter);
            }

            let interpreter = Interpreter {
                engine: self.id,
                id: NEXT_INTERPRETER.fetch_add(1, Ordering::Relaxed),
                globals: engine_executor_globals(),
            };
            watch.arm(interpreter.globals);
            for script in &self.scripts {
                let path = std::ffi::CString::new(script.as_os_str().as_encoded_bytes()).map_err(|_| {
                    EngineError::Script { script: script.clone(), message: "the path holds a NUL byte".into() }
                })?;
                let mut result = Zval::new();
                let status = engine_run_script(path.as_ptr(), &raw mut result);
                let read = Value::read(&raw const result);
                engine_value_release(&raw mut result);
                let message = match (status, read) {
                    (DONE, _) => continue,
                    (THREW, Ok(Value::String(message))) => String::from_utf8_lossy(&message).into_owned(),
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
        match answer {
            Ok(value) => {
                value.write(result);
                0
            }
            Err(message) => {
                Value::String(message.into_bytes()).write(result);
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
    VARIABLES.with_borrow(|variables| {
        variables.iter().find(|(set, _)| set.as_bytes() == name).map_or(std::ptr::null(), |(_, value)| value.as_ptr())
    })
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
    VARIABLES.with_borrow(|variables| {
        let Some((set, held)) = variables.get(index) else {
            return false;
        };
        // SAFETY: the engine gives a writable pointer for the name.
        unsafe { *name = CStr::as_ptr(set) };
        // SAFETY: the engine gives a writable pointer for the value.
        unsafe { *value = CStr::as_ptr(held) };

        true
    })
}

/// One call's deadline, which the deadlines thread enforces by setting the interpreter's timeout flag and VM
/// interrupt, so the engine stops the PHP at its next instruction boundary.
struct Watch {
    token: u64,
}

struct Deadline {
    token: u64,
    at: Instant,
    globals: Option<usize>,
    fired: bool,
}

#[derive(Default)]
struct Deadlines {
    pending: Mutex<Vec<Deadline>>,
    changed: Condvar,
}

impl Watch {
    fn start(after: Duration) -> Self {
        static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        let deadlines = Deadlines::get();
        deadlines.pending.lock().unwrap_or_else(PoisonError::into_inner).push(Deadline {
            token,
            at: Instant::now() + after,
            globals: None,
            fired: false,
        });
        deadlines.changed.notify_one();

        Self { token }
    }

    /// Names the interpreter this deadline interrupts, once the call has one.
    fn arm(&self, globals: *mut c_void) {
        let deadlines = Deadlines::get();
        let mut pending = deadlines.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(deadline) = pending.iter_mut().find(|deadline| deadline.token == self.token) {
            deadline.globals = Some(globals as usize);
        }
        drop(pending);
        deadlines.changed.notify_one();
    }

    /// Ends the watch and returns whether the deadline passed, clearing an interrupt that came after the call ended.
    fn finish(self, globals: Option<*mut c_void>) -> bool {
        let mut pending = Deadlines::get().pending.lock().unwrap_or_else(PoisonError::into_inner);
        let fired = pending
            .iter()
            .position(|deadline| deadline.token == self.token)
            .is_some_and(|index| pending.swap_remove(index).fired);
        drop(pending);
        if let (true, Some(globals)) = (fired, globals) {
            // SAFETY: the interpreter still runs on this thread, and no call runs on it now.
            unsafe { engine_clear_interrupt(globals) };
        }

        fired
    }
}

impl Deadlines {
    /// Moves the deadline `token` to `after` from now, unless it already passed.
    fn restart(&self, token: u64, after: Duration) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(deadline) = pending.iter_mut().find(|deadline| deadline.token == token && !deadline.fired) {
            deadline.at = Instant::now() + after;
        }
        drop(pending);
        self.changed.notify_one();
    }

    fn get() -> &'static Self {
        static DEADLINES: OnceLock<&'static Deadlines> = OnceLock::new();
        DEADLINES.get_or_init(|| {
            let deadlines: &'static Self = Box::leak(Box::default());
            std::thread::Builder::new()
                .name("mago-engine-deadlines".to_owned())
                .spawn(|| deadlines.enforce())
                .map_or_else(|error| panic!("the engine's deadlines thread could not start: {error}"), drop);
            deadlines
        })
    }

    /// Interrupts each armed interpreter whose deadline passed, and sleeps until the next deadline.
    fn enforce(&self) -> ! {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            let now = Instant::now();
            for deadline in pending.iter_mut().filter(|deadline| !deadline.fired && deadline.at <= now) {
                if let Some(globals) = deadline.globals {
                    // SAFETY: an armed deadline's interpreter runs until its call finishes the watch, which takes this
                    // lock first, and setting its flags is what the engine's own timer does.
                    unsafe { engine_interrupt(globals as *mut c_void) };
                    deadline.fired = true;
                }
            }

            let next = pending.iter().filter(|deadline| !deadline.fired).map(|deadline| deadline.at).min();
            pending = match next {
                Some(at) => {
                    self.changed
                        .wait_timeout(pending, at.saturating_duration_since(now))
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.changed.wait(pending).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}
