#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::panic_in_result_fn)]

use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use mago_engine::Answer;
use mago_engine::Engine;
use mago_engine::EngineError;
use mago_engine::Value;

const DEADLINE: Duration = Duration::from_secs(10);

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn engine() -> Engine {
    Engine::new(vec![fixture("functions.php")], Vec::new()).expect("the engine starts")
}

fn no_questions(arguments: Vec<Value>) -> Answer {
    panic!("the PHP asked {arguments:?} where it asks nothing")
}

fn call(engine: &Engine, function: &str) -> Result<Value, EngineError> {
    engine.call(&format!("Mago\\Tests\\Engine\\{function}"), &[], DEADLINE, &mut no_questions)
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

    let echoed = engine.call("Mago\\Tests\\Engine\\echo_back", &values, DEADLINE, &mut no_questions);

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

    let Err(EngineError::Threw(message)) = thrown else { panic!("the call threw, and returned {thrown:?}") };
    assert!(message.starts_with("RuntimeException: the provider refused in "), "{message}");
    assert!(message.ends_with("functions.php:42"), "{message}");
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

    let stopped = engine.call("Mago\\Tests\\Engine\\spin", &[], deadline, &mut no_questions);

    assert_eq!(stopped, Err(EngineError::Deadline(deadline)));
    assert!(start.elapsed() < Duration::from_secs(5), "the call stopped {:?} after it started", start.elapsed());
    assert_eq!(call(&engine, "count_call"), Ok(Value::Int(1)), "the interpreter starts again");
}

#[test]
fn a_call_that_ends_before_its_deadline_leaves_the_next_call_alone() {
    let engine = engine();
    for _ in 0..50 {
        let short = engine.call("Mago\\Tests\\Engine\\count_call", &[], Duration::from_millis(1), &mut no_questions);
        assert!(matches!(short, Ok(Value::Int(_)) | Err(EngineError::Deadline(_))), "{short:?}");
    }

    assert!(matches!(call(&engine, "count_call"), Ok(Value::Int(_))));
}

#[test]
fn a_script_that_throws_at_the_start_fails_the_call_and_names_the_script() {
    let script = fixture("throws.php");
    let engine = Engine::new(vec![script.clone()], Vec::new()).expect("the engine starts");

    let failed = engine.call("count", &[], DEADLINE, &mut no_questions);

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

#[test]
fn a_value_no_value_holds_fails_the_call() {
    let engine = engine();

    assert!(matches!(call(&engine, "an_object"), Err(EngineError::Value(_))));
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

    let answered =
        engine.call("Mago\\Tests\\Engine\\ask_mago", &[Value::String(b"again".to_vec())], DEADLINE, &mut |_| {
            nested = Some(call(&engine, "count_call"));
            Ok(Value::Null)
        });

    assert_eq!(answered, Ok(Value::Null));
    assert_eq!(nested, Some(Err(EngineError::Nested)));
}

#[test]
fn an_answer_that_restarts_the_deadline_keeps_a_call_that_progresses_running() {
    let engine = engine();
    let deadline = Duration::from_millis(100);
    let rounds = [Value::Int(5)];

    let progressed = engine.call("Mago\\Tests\\Engine\\spin_asking", &rounds, deadline, &mut |_| {
        engine.restart_deadline();
        Ok(Value::Null)
    });
    let stalled = engine.call("Mago\\Tests\\Engine\\spin_asking", &rounds, deadline, &mut |_| Ok(Value::Null));

    assert_eq!(progressed, Ok(Value::Int(5)));
    assert_eq!(stalled, Err(EngineError::Deadline(deadline)));
}

#[test]
fn an_interpreter_reads_the_engine_variables_through_getenv_env_and_server() {
    let engine = Engine::new(vec![fixture("functions.php")], vec![(c"MAGO_ENGINE_STAGE".into(), c"analysis".into())])
        .expect("the engine starts");
    let stage = Value::String(b"analysis".to_vec());

    let read = engine.call(
        "Mago\\Tests\\Engine\\variable",
        &[Value::String(b"MAGO_ENGINE_STAGE".to_vec())],
        DEADLINE,
        &mut no_questions,
    );

    assert_eq!(read, Ok(Value::List(vec![stage.clone(), stage.clone(), stage])));
}

#[test]
fn a_shutdown_function_reads_the_engine_variables_as_its_thread_ends() {
    let engine = Engine::new(vec![fixture("functions.php")], vec![(c"MAGO_ENGINE_STAGE".into(), c"shutdown".into())])
        .expect("the engine starts");
    let file = std::env::temp_dir().join(format!("mago-engine-shutdown-{}", std::process::id()));
    let arguments =
        [Value::String(b"MAGO_ENGINE_STAGE".to_vec()), Value::String(file.as_os_str().as_encoded_bytes().to_vec())];

    let registered = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                engine.call("Mago\\Tests\\Engine\\write_variable_at_shutdown", &arguments, DEADLINE, &mut no_questions)
            })
            .join()
            .unwrap()
    });

    assert_eq!(registered, Ok(Value::Null));
    let written = std::fs::read_to_string(&file).expect("the shutdown function wrote its file as the thread ended");
    std::fs::remove_file(&file).unwrap();
    assert_eq!(written, "shutdown");
}
