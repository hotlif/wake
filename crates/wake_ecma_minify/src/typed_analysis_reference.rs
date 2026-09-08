//! Test-only snapshot of the original full-module initialization solver.
use super::*;
pub(super) fn solve_definite_initialization(
    cfg: &TypedControlFlowGraph,
    name_count: usize,
    symbol_count: usize,
) -> Vec<Option<bool>> {
    let mut all_symbols = DenseSymbolSet::empty(symbol_count);
    for initialized in cfg.roots.values() {
        for &symbol in initialized {
            all_symbols.insert(symbol);
        }
    }
    for block in &cfg.blocks {
        for event in &block.events {
            match *event {
                FlowEvent::Read { symbol, .. } | FlowEvent::Initialize(symbol) => {
                    all_symbols.insert(symbol);
                }
            }
        }
    }
    let mut predecessors = vec![Vec::new(); cfg.blocks.len()];
    for edge in &cfg.edges {
        predecessors[edge.to.index()].push(edge.from);
    }
    let mut root_sets = vec![None; cfg.blocks.len()];
    for (&root, initialized) in &cfg.roots {
        root_sets[root.index()] = Some(DenseSymbolSet::from_symbols(
            symbol_count,
            initialized.iter(),
        ));
    }
    let mut incoming = vec![all_symbols.clone(); cfg.blocks.len()];
    let mut outgoing = vec![all_symbols; cfg.blocks.len()];
    for (root, initialized) in root_sets.iter().enumerate() {
        let Some(initialized) = initialized else {
            continue;
        };
        incoming[root] = initialized.clone();
        outgoing[root] = transfer(initialized, &cfg.blocks[root]);
    }

    loop {
        let mut changed = false;
        for block in &cfg.blocks {
            let next_in = if let Some(initialized) = &root_sets[block.id.index()] {
                initialized.clone()
            } else if let Some((&first, rest)) = predecessors[block.id.index()].split_first() {
                let mut intersection = outgoing[first.index()].clone();
                for predecessor in rest {
                    intersection.intersect_with(&outgoing[predecessor.index()]);
                }
                intersection
            } else {
                DenseSymbolSet::empty(symbol_count)
            };
            let next_out = transfer(&next_in, block);
            if incoming[block.id.index()] != next_in || outgoing[block.id.index()] != next_out {
                incoming[block.id.index()] = next_in;
                outgoing[block.id.index()] = next_out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut reads = vec![None; name_count];
    for block in &cfg.blocks {
        let mut initialized = incoming[block.id.index()].clone();
        for event in &block.events {
            match *event {
                FlowEvent::Read { name, symbol } => {
                    let current = initialized.contains(symbol);
                    reads[name.index()] = Some(reads[name.index()].unwrap_or(true) && current);
                }
                FlowEvent::Initialize(symbol) => {
                    initialized.insert(symbol);
                }
            }
        }
    }
    reads
}

fn transfer(input: &DenseSymbolSet, block: &TypedCfgBlock) -> DenseSymbolSet {
    let mut output = input.clone();
    for event in &block.events {
        if let FlowEvent::Initialize(symbol) = *event {
            output.insert(symbol);
        }
    }
    output
}
