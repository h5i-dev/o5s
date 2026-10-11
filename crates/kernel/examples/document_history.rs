//! Measure actual document admission and materialization, including ancestry cost.
use o5s_kernel::model::{Anchor, DocumentGraph, NodeId, Operation, OperationId, Patch, PatchId};
use std::time::Instant;

fn identity(sequence: usize) -> PatchId {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&(sequence as u64).to_le_bytes());
    PatchId(bytes)
}

fn node(sequence: usize) -> NodeId {
    NodeId {
        insertion: OperationId {
            patch: identity(sequence),
            ordinal: 0,
        },
        ordinal: 0,
    }
}

fn measure(count: usize, sequential: bool, clone_wrapper: bool) {
    let mut graph = DocumentGraph::default();
    graph
        .apply(Patch {
            id: identity(0),
            dependencies: vec![],
            operations: vec![Operation::Insert {
                left: Anchor::Start,
                right: Anchor::End,
                atoms: vec![b"a\n".to_vec()],
            }],
        })
        .unwrap();
    let start = Instant::now();
    for sequence in 1..=count {
        let previous = if sequential { sequence - 1 } else { 0 };
        let patch = Patch {
            id: identity(sequence),
            dependencies: vec![identity(previous)],
            operations: vec![Operation::Insert {
                left: Anchor::Node(node(previous)),
                right: Anchor::End,
                atoms: vec![b"x\n".to_vec()],
            }],
        };
        if clone_wrapper {
            // Same production apply, with the former whole-document copy added
            // back outside it. This isolates copying overhead, not old binaries.
            let mut next = graph.clone();
            next.apply(patch).unwrap();
            graph = next;
        } else {
            graph.apply(patch).unwrap();
        }
    }
    let admission_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let materialized = graph.materialize();
    let materialization_ms = start.elapsed().as_secs_f64() * 1000.0;
    if sequential {
        assert_eq!(materialized.bytes.as_ref().unwrap().len(), (count + 1) * 2);
        assert!(materialized.conflicts.is_empty());
    } else if count > 1 {
        assert!(materialized.bytes.is_none());
        assert_eq!(materialized.conflicts.len(), 1);
    }
    println!(
        "{{\"history\":\"{}\",\"strategy\":\"{}\",\"patches\":{},\"admission_ms\":{:.3},\"materialization_ms\":{:.3},\"conflicts\":{}}}",
        if sequential {
            "sequential"
        } else {
            "independent"
        },
        if clone_wrapper {
            "clone_wrapper"
        } else {
            "journal"
        },
        count,
        admission_ms,
        materialization_ms,
        materialized.conflicts.len(),
    );
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let count = match args.as_slice() {
        [] => 2_000,
        [count] => count.parse::<usize>().expect("expected a patch count"),
        _ => panic!("usage: document_history [patch-count]"),
    };
    assert!(
        count > 0 && count <= 100_000,
        "patch count must be 1..=100000"
    );
    for sequential in [false, true] {
        for clone_wrapper in [false, true] {
            measure(count, sequential, clone_wrapper);
        }
    }
}
