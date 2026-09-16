use super::{ModuleGraph, ModuleResolution, invalid};
use crate::{LintError, ModuleRequest, ModuleRequestKind};
use std::collections::VecDeque;

pub(super) struct Cycles {
    pub component: Vec<usize>,
    pub incomplete: Vec<bool>,
}

#[derive(Clone, Copy)]
pub(super) struct Policy {
    pub commonjs: bool,
    pub dynamic: bool,
    pub types: bool,
    pub ignore_external: bool,
}

impl Policy {
    pub fn key(self) -> usize {
        self.commonjs as usize
            | (self.dynamic as usize) << 1
            | (self.types as usize) << 2
            | (self.ignore_external as usize) << 3
    }

    pub fn includes(self, request: &ModuleRequest, resolution: &ModuleResolution) -> bool {
        if request.parent.is_some()
            || request.type_only && !self.types
            || self.ignore_external && resolution.external()
        {
            return false;
        }
        match request.kind {
            ModuleRequestKind::Require | ModuleRequestKind::ImportEquals => self.commonjs,
            ModuleRequestKind::DynamicImport => self.dynamic,
            ModuleRequestKind::TypeImport => self.types,
            _ => true,
        }
    }
}

struct Work(usize);
impl Work {
    fn step(&mut self) -> Result<(), LintError> {
        self.0 = self
            .0
            .checked_sub(1)
            .ok_or_else(|| invalid("Module topology work budget exceeded"))?;
        Ok(())
    }
}

pub(super) fn build(graph: &ModuleGraph, policy: Policy) -> Result<Cycles, LintError> {
    let mut work = Work(graph.limits.work);
    let count = graph.files.len();
    let mut edges = vec![Vec::new(); count];
    let mut reverse = vec![Vec::new(); count];
    let mut incomplete: Vec<_> = graph.files.iter().map(|file| file.incomplete).collect();
    for (index, file) in graph.files.iter().enumerate() {
        work.step()?;
        for (request, resolution) in file.edges() {
            work.step()?;
            if !policy.includes(request, resolution) {
                continue;
            }
            if !request.attributes_known {
                incomplete[index] = true;
                continue;
            }
            match resolution {
                ModuleResolution::File { target, .. } => {
                    edges[index].push(target.0);
                    reverse[target.0].push(index);
                }
                ModuleResolution::Unresolved { .. }
                | ModuleResolution::Unknown(_)
                | ModuleResolution::Opaque { .. } => incomplete[index] = true,
                _ => (),
            }
        }
    }
    // Iterative Kosaraju: no dependency depth uses the process stack.
    let mut visited = vec![false; count];
    let mut postorder = Vec::with_capacity(count);
    for root in 0..count {
        work.step()?;
        if std::mem::replace(&mut visited[root], true) {
            continue;
        }
        let mut stack = vec![(root, 0)];
        while let Some((node, next)) = stack.last_mut() {
            work.step()?;
            if let Some(&target) = edges[*node].get(*next) {
                *next += 1;
                if !std::mem::replace(&mut visited[target], true) {
                    stack.push((target, 0));
                }
            } else {
                postorder.push(*node);
                stack.pop();
            }
        }
    }
    let mut component = vec![usize::MAX; count];
    for root in postorder.into_iter().rev() {
        work.step()?;
        if component[root] != usize::MAX {
            continue;
        }
        component[root] = root;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            work.step()?;
            for &parent in &reverse[node] {
                work.step()?;
                if component[parent] == usize::MAX {
                    component[parent] = root;
                    stack.push(parent);
                }
            }
        }
    }
    // A reverse work queue propagates incomplete dependency facts once per edge.
    let mut queue: VecDeque<_> = incomplete
        .iter()
        .enumerate()
        .filter_map(|(id, &bad)| bad.then_some(id))
        .collect();
    while let Some(node) = queue.pop_front() {
        work.step()?;
        for &parent in &reverse[node] {
            work.step()?;
            if !std::mem::replace(&mut incomplete[parent], true) {
                queue.push_back(parent);
            }
        }
    }
    Ok(Cycles {
        component,
        incomplete,
    })
}
