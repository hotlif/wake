use wake_turbo::{Engine, Vc, task};

#[task]
fn double(value: Vc<i64>) -> i64 {
    *value.read() * 2
}

#[test]
fn fork_reuses_memos_and_isolates_inputs_and_revisions() {
    let mut accepted = Engine::new();
    let input = accepted.new_input(21_i64);
    let result = accepted.enter(|| double(input));
    assert_eq!(*accepted.enter(|| result.read()), 42);
    let executions = accepted.exec_count();
    let mut candidate = accepted.fork();
    assert_eq!(*candidate.enter(|| result.read()), 42);
    assert_eq!(candidate.exec_count(), 0);
    candidate.set_input(input, 7);
    assert_eq!(*candidate.enter(|| result.read()), 14);
    assert_eq!(*accepted.enter(|| result.read()), 42);
    assert_eq!(accepted.exec_count(), executions);
    let successor = candidate.fork();
    successor.set_input(input, 9);
    assert_eq!(*successor.enter(|| result.read()), 18);
    assert_eq!(*candidate.enter(|| result.read()), 14);
}
