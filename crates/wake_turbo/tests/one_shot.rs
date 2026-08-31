//! One-shot engine contract: values cross stage barriers through `Vc`, while red-green metadata
//! and fingerprints stay cold because the input graph freezes at the first derived query.

use std::hash::{Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use wake_turbo::{Engine, Executor, TaskArg, TaskId, query};

#[derive(Clone)]
struct HashCounted {
    value: i64,
    hash_calls: Arc<AtomicUsize>,
}

impl Hash for HashCounted {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash_calls.fetch_add(1, Ordering::Relaxed);
        self.value.hash(state);
    }
}

#[test]
fn one_shot_skips_input_and_output_fingerprints() {
    let engine = Engine::new_one_shot();
    let input_hashes = Arc::new(AtomicUsize::new(0));
    let output_hashes = Arc::new(AtomicUsize::new(0));
    let input = engine.new_input(HashCounted {
        value: 20,
        hash_calls: input_hashes.clone(),
    });

    // One-shot callers may finish assembling inputs before the first derived query. This update
    // must remain fingerprint-free just like initial input creation.
    engine.set_input(
        input,
        HashCounted {
            value: 21,
            hash_calls: input_hashes.clone(),
        },
    );

    let id = TaskId::of("wake_turbo_test", "one_shot_double", &[input.arg_ref()]);
    let task_hashes = output_hashes.clone();
    let output = engine.enter(|| {
        query(id, move || HashCounted {
            value: input.read().value * 2,
            hash_calls: task_hashes.clone(),
        })
    });

    assert_eq!(engine.enter(|| output.read().value), 42);
    assert_eq!(input_hashes.load(Ordering::Relaxed), 0);
    assert_eq!(output_hashes.load(Ordering::Relaxed), 0);
    assert_eq!(engine.exec_count(), 1);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "uses the crossbeam work-stealing executor; single-flight has a separate loom gate"
)]
fn one_shot_keeps_concurrent_single_flight() {
    let engine = Arc::new(Engine::new_one_shot());
    let executor = Executor::new(8);
    let input = engine.new_input(41_i64);
    let id = TaskId::of(
        "wake_turbo_test",
        "one_shot_single_flight",
        &[input.arg_ref()],
    );
    let computations = Arc::new(AtomicUsize::new(0));
    let requests = (0..128)
        .map(|_| {
            let computations = computations.clone();
            move || {
                let output = query(id, move || {
                    computations.fetch_add(1, Ordering::Relaxed);
                    *input.read() + 1
                });
                *output.read()
            }
        })
        .collect();

    let results = engine.par_request(&executor, requests);
    assert!(results.iter().all(|value| *value == 42));
    assert_eq!(computations.load(Ordering::Relaxed), 1);
    assert_eq!(engine.exec_count(), 1);
}

#[test]
fn one_shot_values_cross_separate_vc_stage_barriers() {
    let engine = Engine::new_one_shot();
    let input = engine.new_input(20_i64);
    let first_id = TaskId::of("wake_turbo_test", "one_shot_first", &[input.arg_ref()]);
    let first = engine.enter(|| query(first_id, move || *input.read() + 1));

    // A staged pipeline may create fresh linker/layout inputs after parse tasks have already
    // frozen earlier cells. New cells are safe because no materialized task could have read them;
    // only updates to an existing cell are rejected.
    let layout = engine.new_input(2_i64);
    let second_id = TaskId::of(
        "wake_turbo_test",
        "one_shot_second",
        &[first.arg_ref(), layout.arg_ref()],
    );
    let second = engine.enter(|| query(second_id, move || *first.read() * *layout.read()));

    assert_eq!(engine.enter(|| *second.read()), 42);
    assert_eq!(engine.exec_count(), 2);
}

#[test]
fn one_shot_rejects_input_updates_after_first_query_without_poisoning_inputs() {
    let engine = Engine::new_one_shot();
    let input = engine.new_input(21_i64);
    let id = TaskId::of("wake_turbo_test", "one_shot_freeze", &[input.arg_ref()]);
    let output = engine.enter(|| query(id, move || *input.read() * 2));
    assert_eq!(engine.enter(|| *output.read()), 42);

    let rejected = catch_unwind(AssertUnwindSafe(|| engine.set_input(input, 99_i64)));
    assert!(
        rejected.is_err(),
        "a materialized one-shot graph must be frozen"
    );
    assert_eq!(
        engine.enter(|| *input.read()),
        21,
        "the rejected update must neither change nor poison the input slot"
    );
}

#[test]
#[should_panic(expected = "循环依赖")]
fn one_shot_preserves_cycle_detection() {
    let engine = Engine::new_one_shot();
    let input = engine.new_input(0_i64);
    let id = TaskId::of("wake_turbo_test", "one_shot_cycle", &[input.arg_ref()]);
    engine.enter(|| {
        let _ = query(id, move || *query(id, || 1_i64).read()).read();
    });
}
