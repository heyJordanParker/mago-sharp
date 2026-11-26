#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::panic_in_result_fn)]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use mago_engine::Answer;
use mago_engine::Clock;
use mago_engine::Engine;
use mago_engine::EngineError;
use mago_engine::Frame;
use mago_engine::Value;

const DEADLINE: Duration = Duration::from_secs(10);

/// The backstop of wall time of a call whose test checks no backstop: long past any test, so only its deadline of CPU
/// time can stop it, however starved of CPU a loaded machine leaves its thread.
const BACKSTOP: Duration = Duration::from_secs(120);

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn engine() -> Engine {
    Engine::new(vec![fixture("functions.php")], Vec::new(), Vec::new()).expect("the engine starts")
}

fn no_questions(arguments: Vec<Value>) -> Answer {
    panic!("the PHP asked {arguments:?} where it asks nothing")
}

fn call(engine: &Engine, function: &str) -> Result<Value, EngineError> {
    engine.call(&format!("Mago\\Tests\\Engine\\{function}"), &[], DEADLINE, BACKSTOP, &mut no_questions)
}

#[test]
fn a_value_goes_to_php_and_comes_back_as_it_went() {
    let engine = engine();
    let values = vec![
        Value::Null,
        Value::Bool(true),
        Value::Int(-7),
        Value::Float(1.5),
        Value::String(b"bytes \0 kept".to_vec()),
        Value::List(vec![Value::Int(1), Value::List(Vec::new())]),
        Value::Map(vec![(b"name".to_vec(), Value::String(b"Order".to_vec())), (b"7".to_vec(), Value::Bool(false))]),
    ];

    let echoed = engine.call("Mago\\Tests\\Engine\\echo_back", &values, DEADLINE, BACKSTOP, &mut no_questions);

    assert_eq!(echoed, Ok(Value::List(values)));
}

#[test]
fn each_thread_runs_an_interpreter_of_its_own() {
    let engine = engine();
    let counts: Vec<i64> = std::thread::scope(|scope| {
        let threads: Vec<_> = std::iter::repeat_with(|| {
            scope.spawn(|| {
                std::iter::repeat_with(|| call(&engine, "count_call"))
                    .take(100)
                    .last()
                    .unwrap()
                    .expect("the call returns the count")
            })
        })
        .take(4)
        .collect();
        threads
            .into_iter()
            .map(|thread| match thread.join().unwrap() {
                Value::Int(count) => count,
                other => panic!("count_call returned {other:?}"),
            })
            .collect()
    });

    assert_eq!(counts, [100, 100, 100, 100]);
}

#[test]
fn php_asks_mago_and_the_call_answers() {
    let engine = engine();
    let mut asked = Vec::new();

    let answered = engine.call(
        "Mago\\Tests\\Engine\\ask_mago",
        &[Value::String(b"the type of Order::total".to_vec())],
        DEADLINE,
        BACKSTOP,
        &mut |arguments| {
            asked.push(arguments);
            Ok(Value::String(b"int".to_vec()))
        },
    );

    assert_eq!(answered, Ok(Value::String(b"int".to_vec())));
    assert_eq!(asked, [vec![Value::String(b"the type of Order::total".to_vec()), Value::Int(2)]]);
}

#[test]
fn an_answer_that_fails_throws_in_the_php_that_asked() {
    let engine = engine();

    let answered = engine.call(
        "Mago\\Tests\\Engine\\ask_mago_and_catch",
        &[Value::String(b"a class Mago doesn't know".to_vec())],
        DEADLINE,
        BACKSTOP,
        &mut |_| Err("no class `Missing`".to_owned()),
    );

    assert_eq!(answered, Ok(Value::String(b"caught: no class `Missing`".to_vec())));
}

#[test]
fn a_reset_interpreter_starts_again_with_nothing_of_the_last() {
    let engine = engine();
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)));
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(2)));
    let before = engine.interpreter().expect("the thread runs an interpreter");

    engine.reset(before);

    assert_eq!(engine.interpreter(), None, "the calling thread's interpreter ends at once");
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)));
    assert_ne!(engine.interpreter(), Some(before));
}

#[test]
fn a_reset_of_another_threads_interpreter_ends_it_on_that_threads_next_call() {
    let engine = &engine();
    let (sends_interpreter, receives_interpreter) = std::sync::mpsc::channel();
    let (sends_reset, receives_reset) = std::sync::mpsc::channel::<()>();

    let counted = std::thread::scope(|scope| {
        let running = scope.spawn(move || {
            assert_eq!(call(engine, "count_call"), Ok(Value::Int(1)));
            sends_interpreter.send(engine.interpreter()).expect("the test waits for the interpreter");
            receives_reset.recv().expect("the test resets the interpreter");
            call(engine, "count_call")
        });
        let held = receives_interpreter.recv().expect("the thread runs an interpreter").expect("the thread runs one");
        engine.reset(held);
        sends_reset.send(()).expect("the thread waits for the reset");
        running.join().expect("the thread ends")
    });

    assert_eq!(counted, Ok(Value::Int(1)));
}

#[test]
fn a_thrown_exception_names_its_class_message_and_place() {
    let engine = engine();

    let thrown = call(&engine, "throw_exception");

    let Err(EngineError::Threw(thrown)) = thrown else { panic!("the call threw, and returned {thrown:?}") };
    assert_eq!(thrown.class, "RuntimeException");
    assert_eq!(thrown.message, "the provider refused");
    assert!(thrown.file.ends_with("fixtures/functions.php"), "{}", thrown.file.display());
    assert_eq!(thrown.line, 69);
    assert_eq!(thrown.to_string(), format!("RuntimeException: the provider refused in {}:69", thrown.file.display()));
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)), "an exception keeps the interpreter");
}

#[test]
fn a_fatal_error_fails_the_call_with_what_php_printed_and_resets_the_interpreter() {
    let engine = engine();
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)));

    let failed = call(&engine, "fail");

    let Err(EngineError::Failed(message)) = failed else { panic!("the call failed, and returned {failed:?}") };
    assert!(message.contains("Cannot redeclare function Mago\\Tests\\Engine\\count_call()"), "{message}");
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)), "the interpreter starts again");
}

#[test]
fn a_call_past_its_deadline_stops_and_resets_the_interpreter() {
    let engine = engine();
    let deadline = Duration::from_millis(200);
    let start = Instant::now();

    let stopped = engine.call("Mago\\Tests\\Engine\\spin", &[], deadline, BACKSTOP, &mut no_questions);

    let Err(EngineError::Deadline { duration, clock, frames }) = stopped else {
        panic!("the call ran on: {stopped:?}")
    };
    assert_eq!((duration, clock), (deadline, Clock::Cpu));
    assert_eq!(frames.first(), Some(&Frame { class: String::new(), function: "Mago\\Tests\\Engine\\spin".to_owned() }));
    assert!(start.elapsed() < Duration::from_secs(5), "the call stopped {:?} after it started", start.elapsed());
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)), "the interpreter starts again");
}

/// A call's deadline stops it while another thread's calls, each with a later deadline, start and end around it.
#[test]
fn a_call_past_its_deadline_stops_while_another_threads_calls_end_in_time() {
    let engine = engine();
    let deadline = Duration::from_millis(300);
    let stopped = AtomicBool::new(false);
    let (calling, called) = std::sync::mpsc::channel();

    std::thread::scope(|scope| {
        let other = scope.spawn(|| {
            let mut ended = 0;
            while !stopped.load(Ordering::SeqCst) {
                assert!(matches!(call(&engine, "count_call"), Ok(Value::Int(_))));
                ended += 1;
                if ended == 1 {
                    calling.send(()).unwrap();
                }
            }

            ended
        });
        called.recv().unwrap();
        let start = Instant::now();

        let spun = engine.call("Mago\\Tests\\Engine\\spin", &[], deadline, BACKSTOP, &mut no_questions);
        stopped.store(true, Ordering::SeqCst);

        assert!(
            matches!(spun, Err(EngineError::Deadline { duration, clock: Clock::Cpu, .. }) if duration == deadline),
            "{spun:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(5), "the call stopped {:?} after it started", start.elapsed());
        assert!(other.join().unwrap() > 1, "the other thread's calls kept ending in time");
    });
}

#[test]
fn a_calls_deadline_times_the_function_it_calls_and_never_the_interpreters_start() {
    let engine = Engine::new(vec![fixture("slow_start.php")], Vec::new(), Vec::new()).expect("the engine starts");

    let changed =
        engine.call("chdir", &[Value::String(b"/".to_vec())], Duration::from_secs(1), BACKSTOP, &mut no_questions);

    assert_eq!(changed, Ok(Value::Bool(true)));
}

#[test]
fn a_call_that_ends_before_its_deadline_leaves_the_next_call_alone() {
    let engine = engine();
    for _ in 0..50 {
        let short =
            engine.call("Mago\\Tests\\Engine\\count_call", &[], Duration::from_millis(1), BACKSTOP, &mut no_questions);
        assert!(matches!(short, Ok(Value::Int(_)) | Err(EngineError::Deadline { .. })), "{short:?}");
    }

    assert!(matches!(call(&engine, "count_call"), Ok(Value::Int(_))));
}

#[test]
fn a_script_that_throws_at_the_start_fails_the_call_and_names_the_script() {
    let script = fixture("throws.php");
    let engine = Engine::new(vec![script.clone()], Vec::new(), Vec::new()).expect("the engine starts");

    let failed = engine.call("count", &[], DEADLINE, BACKSTOP, &mut no_questions);

    let Err(EngineError::Script { script: failing, message }) = failed else { panic!("returned {failed:?}") };
    assert_eq!(failing, script);
    assert!(message.starts_with("LogicException: the host could not load in "), "{message}");
}

#[test]
fn two_engines_on_one_thread_each_get_their_own_interpreter() {
    let first = engine();
    let second = engine();
    assert_eq!(call(&first, "count_call"), Ok(Value::Int(1)));

    assert_eq!(call(&second, "count_call"), Ok(Value::Int(1)));
    assert_eq!(call(&first, "count_call"), Ok(Value::Int(1)));
}

#[test]
fn an_interpreter_reads_no_php_ini_and_sets_no_memory_limit() {
    let engine = engine();

    assert_eq!(
        call(&engine, "settings"),
        Ok(Value::Map(vec![
            (b"memory_limit".to_vec(), Value::String(b"-1".to_vec())),
            (b"php.ini".to_vec(), Value::Bool(false)),
            (b"max_execution_time".to_vec(), Value::String(b"0".to_vec())),
        ]))
    );
}

/// An interpreter runs with its engine's PHP settings, as `php -d` sets them: one a script may change, such as
/// `memory_limit`, and one only `php.ini` may, such as `max_input_time`.
#[test]
fn an_interpreter_runs_with_the_engine_settings() {
    let engine = Engine::new(
        vec![fixture("functions.php")],
        Vec::new(),
        vec![(c"memory_limit".into(), c"2G".into()), (c"max_input_time".into(), c"7".into())],
    )
    .expect("the engine starts");

    let read = engine.call(
        "Mago\\Tests\\Engine\\setting",
        &[Value::String(b"memory_limit".to_vec())],
        DEADLINE,
        BACKSTOP,
        &mut no_questions,
    );
    let system = engine.call(
        "Mago\\Tests\\Engine\\setting",
        &[Value::String(b"max_input_time".to_vec())],
        DEADLINE,
        BACKSTOP,
        &mut no_questions,
    );

    assert_eq!(read, Ok(Value::String(b"2G".to_vec())));
    assert_eq!(system, Ok(Value::String(b"7".to_vec())));
}

/// A setting PHP knows nothing of fails the interpreter's start, named, as a typo in `php -d` would go unseen.
#[test]
fn a_setting_php_knows_nothing_of_fails_the_call() {
    let engine =
        Engine::new(vec![fixture("functions.php")], Vec::new(), vec![(c"no_such_setting".into(), c"2G".into())])
            .expect("the engine starts");

    assert_eq!(
        call(&engine, "count_call"),
        Err(EngineError::Setting { name: "no_such_setting".to_owned(), value: "2G".to_owned() })
    );
}

#[test]
fn a_value_no_value_holds_fails_the_call() {
    let engine = engine();

    assert!(matches!(call(&engine, "an_object"), Err(EngineError::Value(_))));
}

fn parcel(properties: Vec<(&'static str, Value)>) -> Value {
    Value::Object { class: "Mago\\Tests\\Engine\\Parcel", properties }
}

fn post() -> Value {
    Value::Case { enumeration: "Mago\\Tests\\Engine\\Carrier", case: "Post" }
}

fn open_parcel(engine: &Engine, parcel: Value) -> Result<Value, EngineError> {
    engine.call("Mago\\Tests\\Engine\\open_parcel", &[parcel], DEADLINE, BACKSTOP, &mut no_questions)
}

#[test]
fn an_object_goes_to_php_with_its_properties_set_and_no_constructor_run() {
    let engine = engine();
    let parcel = parcel(vec![("grams", Value::Int(250)), ("label", Value::Null), ("carrier", post())]);

    let opened = open_parcel(&engine, parcel);

    assert_eq!(opened, Ok(Value::List(vec![Value::Int(250), Value::Null, Value::String(b"Post".to_vec())])));
}

#[test]
fn an_object_the_interpreter_cant_build_fails_the_call_and_the_next_call_runs() {
    let engine = engine();
    let refused = [
        Value::Object { class: "Mago\\Tests\\Engine\\Missing", properties: Vec::new() },
        parcel(vec![("grams", Value::Int(1)), ("label", Value::Null), ("carrier", post()), ("color", Value::Null)]),
        parcel(vec![("grams", Value::String(b"1".to_vec())), ("label", Value::Null), ("carrier", post())]),
        parcel(vec![("grams", Value::Int(1)), ("grams", Value::Int(2)), ("label", Value::Null), ("carrier", post())]),
        parcel(vec![("grams", Value::Int(1)), ("carrier", post())]),
        parcel(vec![
            ("grams", Value::Int(1)),
            ("label", Value::Null),
            ("carrier", Value::Case { enumeration: "Mago\\Tests\\Engine\\Carrier", case: "Drone" }),
        ]),
    ];

    for parcel in refused {
        let opened = open_parcel(&engine, parcel.clone());

        assert!(matches!(opened, Err(EngineError::Build(_))), "{parcel:?} opened as {opened:?}");
    }
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)), "the interpreter keeps serving calls");
}

#[test]
fn the_class_count_counts_each_class_like_the_interpreter_declares() {
    let engine = engine();

    let counted = call(&engine, "class_count_around_a_declaration");

    let Ok(Value::List(counts)) = counted else { panic!("class_count_around_a_declaration returned {counted:?}") };
    let [Value::Int(before), Value::Int(after)] = counts.as_slice() else { panic!("the counts are {counts:?}") };
    assert!(*before > 0, "the interpreter starts with PHP's own classes declared");
    assert_eq!(*after, before + 1);
}

#[test]
fn an_answer_that_calls_the_engine_again_is_refused() {
    let engine = engine();
    let mut nested = None;

    let answered = engine.call(
        "Mago\\Tests\\Engine\\ask_mago",
        &[Value::String(b"again".to_vec())],
        DEADLINE,
        BACKSTOP,
        &mut |_| {
            nested = Some(call(&engine, "count_call"));
            Ok(Value::Null)
        },
    );

    assert_eq!(answered, Ok(Value::Null));
    assert_eq!(nested, Some(Err(EngineError::Nested)));
}

#[test]
fn an_answer_that_restarts_the_deadline_keeps_a_call_that_progresses_running() {
    let engine = engine();
    let deadline = Duration::from_millis(100);
    let rounds = [Value::Int(5)];

    let round = Duration::from_millis(40);

    let progressed = engine.call("Mago\\Tests\\Engine\\ask_rounds", &rounds, deadline, BACKSTOP, &mut |_| {
        spend(round);
        engine.restart_deadline();
        Ok(Value::Null)
    });
    let stalled = engine.call("Mago\\Tests\\Engine\\ask_rounds", &rounds, deadline, BACKSTOP, &mut |_| {
        spend(round);
        Ok(Value::Null)
    });

    assert_eq!(progressed, Ok(Value::Int(5)));
    assert!(
        matches!(stalled, Err(EngineError::Deadline { duration, clock: Clock::Cpu, .. }) if duration == deadline),
        "{stalled:?}"
    );
}

#[test]
fn a_call_whose_thread_sleeps_past_its_deadline_runs_on() {
    let engine = engine();

    let (deadline, backstop) = (Duration::from_millis(100), Duration::from_secs(1));

    let napped = engine.call("Mago\\Tests\\Engine\\nap", &[Value::Int(400_000)], deadline, backstop, &mut no_questions);

    assert_eq!(napped, Ok(Value::Int(400_000)));
}

#[test]
fn a_call_stops_once_its_thread_has_used_its_deadline_of_cpu_time() {
    let engine = engine();
    let deadline = Duration::from_millis(200);
    let nap = Duration::from_millis(300);
    let (start, cpu) = (Instant::now(), cpu_time());

    let stopped = engine.call(
        "Mago\\Tests\\Engine\\nap_then_spin",
        &[Value::Int(300_000)],
        deadline,
        BACKSTOP,
        &mut no_questions,
    );

    assert!(
        matches!(stopped, Err(EngineError::Deadline { duration, clock: Clock::Cpu, .. }) if duration == deadline),
        "{stopped:?}"
    );
    assert!(cpu_time() - cpu >= deadline, "the call stopped after {:?} of CPU time", cpu_time() - cpu);
    assert!(start.elapsed() >= nap + deadline, "the call stopped {:?} after it started", start.elapsed());
}

#[test]
fn a_call_that_sleeps_past_its_backstop_stops_on_wall_time() {
    let engine = engine();
    let (deadline, backstop) = (Duration::from_millis(20), Duration::from_millis(200));
    let start = Instant::now();

    let stopped =
        engine.call("Mago\\Tests\\Engine\\nap", &[Value::Int(400_000)], deadline, backstop, &mut no_questions);

    assert!(
        matches!(stopped, Err(EngineError::Deadline { duration, clock: Clock::Wall, .. }) if duration == backstop),
        "{stopped:?}"
    );
    assert!(start.elapsed() >= backstop, "the call stopped {:?} after it started", start.elapsed());
}

/// The CPU time the calling thread has used.
fn cpu_time() -> Duration {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `time` is writable.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut time) };
    assert_eq!(result, 0, "the thread's CPU time could not be read: error {result}");

    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

/// Spends `cpu` of the calling thread's CPU time.
fn spend(cpu: Duration) {
    let until = cpu_time() + cpu;
    while cpu_time() < until {
        std::hint::spin_loop();
    }
}

#[test]
fn an_interpreter_reads_the_engine_variables_through_getenv_env_and_server() {
    let engine = Engine::new(
        vec![fixture("functions.php")],
        vec![(c"MAGO_ENGINE_STAGE".into(), c"analysis".into())],
        Vec::new(),
    )
    .expect("the engine starts");
    let stage = Value::String(b"analysis".to_vec());

    let read = engine.call(
        "Mago\\Tests\\Engine\\variable",
        &[Value::String(b"MAGO_ENGINE_STAGE".to_vec())],
        DEADLINE,
        BACKSTOP,
        &mut no_questions,
    );

    assert_eq!(read, Ok(Value::List(vec![stage.clone(), stage.clone(), stage])));
}

#[test]
fn a_shutdown_function_reads_the_engine_variables_as_its_thread_ends() {
    let engine = Engine::new(
        vec![fixture("functions.php")],
        vec![(c"MAGO_ENGINE_STAGE".into(), c"shutdown".into())],
        Vec::new(),
    )
    .expect("the engine starts");
    let file = std::env::temp_dir().join(format!("mago-engine-shutdown-{}", std::process::id()));
    let arguments =
        [Value::String(b"MAGO_ENGINE_STAGE".to_vec()), Value::String(file.as_os_str().as_encoded_bytes().to_vec())];

    let registered = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                engine.call(
                    "Mago\\Tests\\Engine\\write_variable_at_shutdown",
                    &arguments,
                    DEADLINE,
                    BACKSTOP,
                    &mut no_questions,
                )
            })
            .join()
            .unwrap()
    });

    assert_eq!(registered, Ok(Value::Null));
    let written = std::fs::read_to_string(&file).expect("the shutdown function wrote its file as the thread ended");
    std::fs::remove_file(&file).unwrap();
    assert_eq!(written, "shutdown");
}
