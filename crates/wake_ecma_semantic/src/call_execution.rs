//! Owned call-path facts from bounded, per-execution-region native graphs. Graph nodes are an
//! internal implementation detail, not an external CFG ABI. Uncaught throws are not normal exits.

use std::fmt;
use wake_common::{FxHashMap, FxHashSet, Span};
use wake_ecma_ast::*;

mod build;
use build::Builder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionRegionKind {
    Module,
    Function,
    Arrow,
    ClassInitializer,
    StaticBlock,
}

#[derive(Clone, Debug)]
pub struct CallExecution {
    pub span: Span,
    pub region: Span,
    pub region_kind: ExecutionRegionKind,
    pub reachable: bool,
    pub on_normal_path: bool,
    /// Every entry-to-return/fallthrough path executes this original call at least once.
    pub unconditional: bool,
    pub may_repeat: bool,
    pub inside_loop: bool,
    pub inside_exception: bool,
    pub in_parameters: bool,
    pub in_class: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CallExecutionFacts {
    pub calls: Vec<CallExecution>,
}

#[derive(Clone, Copy, Debug)]
pub struct CallExecutionLimits {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_steps: usize,
}

impl Default for CallExecutionLimits {
    fn default() -> Self {
        Self {
            max_nodes: 100_000,
            max_edges: 400_000,
            max_steps: 10_000_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallExecutionError(pub &'static str);
impl fmt::Display for CallExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Call execution analysis exceeded the {} limit", self.0)
    }
}
impl std::error::Error for CallExecutionError {}

pub fn analyze_call_execution(
    program: &Program<'_>,
) -> Result<CallExecutionFacts, CallExecutionError> {
    analyze_call_execution_with_limits(program, CallExecutionLimits::default())
}

pub fn analyze_call_execution_with_limits(
    program: &Program<'_>,
    limits: CallExecutionLimits,
) -> Result<CallExecutionFacts, CallExecutionError> {
    let mut analyzer = Analyzer {
        budget: Budget {
            limits,
            nodes: 0,
            edges: 0,
            steps: 0,
        },
        facts: CallExecutionFacts::default(),
        error: None,
    };
    analyzer.visit_program(program);
    if let Some(error) = analyzer.error {
        return Err(error);
    }
    analyzer
        .facts
        .calls
        .sort_by_key(|call| (call.span.lo, call.span.hi, call.region.lo, call.region.hi));
    Ok(analyzer.facts)
}

struct Budget {
    limits: CallExecutionLimits,
    nodes: usize,
    edges: usize,
    steps: usize,
}
impl Budget {
    fn nodes(&mut self) -> Result<(), CallExecutionError> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > self.limits.max_nodes {
            Err(CallExecutionError("node"))
        } else {
            Ok(())
        }
    }
    fn edges(&mut self, count: usize) -> Result<(), CallExecutionError> {
        self.edges = self.edges.saturating_add(count);
        if self.edges > self.limits.max_edges {
            Err(CallExecutionError("edge"))
        } else {
            Ok(())
        }
    }
    fn step(&mut self) -> Result<(), CallExecutionError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.limits.max_steps {
            Err(CallExecutionError("work"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Flags {
    inside_loop: bool,
    inside_exception: bool,
    in_parameters: bool,
    in_class: bool,
    suppress_calls: bool,
}
#[derive(Clone, Copy)]
struct Site {
    span: Span,
    flags: Flags,
}
type NodeId = usize;
struct Node {
    edges: Vec<NodeId>,
    site: Option<Site>,
}
struct Graph {
    nodes: Vec<Node>,
    entry: NodeId,
    normal: NodeId,
}

impl Graph {
    fn solve(
        &self,
        region: Span,
        kind: ExecutionRegionKind,
        budget: &mut Budget,
    ) -> Result<Vec<CallExecution>, CallExecutionError> {
        let count = self.nodes.len();
        let mut predecessors = vec![Vec::new(); count];
        for (from, node) in self.nodes.iter().enumerate() {
            for &to in &node.edges {
                budget.step()?;
                predecessors[to].push(from);
            }
        }
        let mut reached = vec![false; count];
        let mut postorder = Vec::new();
        let mut stack = vec![(self.entry, 0)];
        reached[self.entry] = true;
        while let Some((node, edge)) = stack.last_mut() {
            budget.step()?;
            if let Some(&next) = self.nodes[*node].edges.get(*edge) {
                *edge += 1;
                if !reached[next] {
                    reached[next] = true;
                    stack.push((next, 0));
                }
            } else {
                postorder.push(*node);
                stack.pop();
            }
        }
        let mut on_normal = vec![false; count];
        let mut work = vec![self.normal];
        on_normal[self.normal] = true;
        while let Some(node) = work.pop() {
            for &previous in &predecessors[node] {
                budget.step()?;
                if !on_normal[previous] {
                    on_normal[previous] = true;
                    work.push(previous);
                }
            }
        }
        // Kosaraju using the same iterative finishing order; unreachable cycles are irrelevant.
        let mut component = vec![usize::MAX; count];
        let mut sizes = Vec::new();
        for &root in postorder.iter().rev() {
            if component[root] != usize::MAX {
                continue;
            }
            let id = sizes.len();
            let mut size = 0;
            component[root] = id;
            work.push(root);
            while let Some(node) = work.pop() {
                size += 1;
                for &previous in &predecessors[node] {
                    budget.step()?;
                    if reached[previous] && component[previous] == usize::MAX {
                        component[previous] = id;
                        work.push(previous);
                    }
                }
            }
            sizes.push(size);
        }
        // Reverse-postorder immediate dominators. A straight-line region converges in one
        // forward pass; the global work budget also bounds adversarial irreducible graphs.
        let mut rank = vec![0; count];
        for (index, &node) in postorder.iter().rev().enumerate() {
            rank[node] = index;
        }
        let mut dominator = vec![None; count];
        dominator[self.entry] = Some(self.entry);
        let mut changed = true;
        while changed {
            changed = false;
            for &node in postorder.iter().rev().skip(1) {
                budget.step()?;
                let mut candidate: Option<NodeId> = None;
                for &previous in &predecessors[node] {
                    budget.step()?;
                    if dominator[previous].is_none() {
                        continue;
                    }
                    candidate = Some(match candidate {
                        None => previous,
                        Some(mut left) => {
                            let mut right = previous;
                            while left != right {
                                budget.step()?;
                                if rank[left] > rank[right] {
                                    left = dominator[left].unwrap();
                                } else {
                                    right = dominator[right].unwrap();
                                }
                            }
                            left
                        }
                    });
                }
                if candidate != dominator[node] {
                    dominator[node] = candidate;
                    changed = true;
                }
            }
        }
        let mut required = vec![false; count];
        if reached[self.normal] {
            let mut node = self.normal;
            loop {
                budget.step()?;
                required[node] = true;
                if node == self.entry {
                    break;
                }
                node = dominator[node].expect("a reachable node has an immediate dominator");
            }
        }
        let mut sites: FxHashMap<Span, Vec<NodeId>> = FxHashMap::default();
        for (node, value) in self.nodes.iter().enumerate() {
            if let Some(site) = value.site {
                sites.entry(site.span).or_default().push(node);
            }
        }
        let mut calls = Vec::with_capacity(sites.len());
        for (span, instances) in sites {
            let reachable = instances.iter().any(|&node| reached[node]);
            let on_normal_path = instances
                .iter()
                .any(|&node| reached[node] && on_normal[node]);
            let mut unconditional = instances.iter().any(|&node| required[node]);
            if !unconditional && reached[self.normal] && instances.len() > 1 && on_normal_path {
                // Finally clones share one source identity. Removing every instance tests the
                // combined cut, rather than incorrectly demanding one clone dominate the exit.
                let removed: FxHashSet<_> = instances.iter().copied().collect();
                let mut seen = vec![false; count];
                work.push(self.entry);
                while let Some(node) = work.pop() {
                    budget.step()?;
                    if seen[node] || removed.contains(&node) {
                        continue;
                    }
                    seen[node] = true;
                    if node == self.normal {
                        break;
                    }
                    work.extend(self.nodes[node].edges.iter().copied());
                }
                work.clear();
                unconditional = !seen[self.normal];
            }
            let mut flags = Flags::default();
            for &node in &instances {
                let observed = self.nodes[node].site.unwrap().flags;
                flags.inside_loop |= observed.inside_loop;
                flags.inside_exception |= observed.inside_exception;
                flags.in_parameters |= observed.in_parameters;
                flags.in_class |= observed.in_class;
            }
            let may_repeat = instances.iter().any(|&node| {
                reached[node]
                    && (sizes[component[node]] > 1 || self.nodes[node].edges.contains(&node))
            });
            calls.push(CallExecution {
                span,
                region,
                region_kind: kind,
                reachable,
                on_normal_path,
                unconditional,
                may_repeat,
                inside_loop: flags.inside_loop,
                inside_exception: flags.inside_exception,
                in_parameters: flags.in_parameters,
                in_class: flags.in_class,
            });
        }
        Ok(calls)
    }
}

enum Body<'a, 'b> {
    Statements(&'b [Statement<'a>]),
    Expression(Expression<'a>),
}
struct Analyzer {
    budget: Budget,
    facts: CallExecutionFacts,
    error: Option<CallExecutionError>,
}
impl Analyzer {
    fn region<'a>(
        &mut self,
        span: Span,
        kind: ExecutionRegionKind,
        parameters: &[Pattern<'a>],
        body: Body<'a, '_>,
    ) {
        if self.error.is_some() {
            return;
        }
        let result = Builder::region(&mut self.budget, kind, parameters, body)
            .and_then(|graph| graph.solve(span, kind, &mut self.budget));
        match result {
            Ok(calls) => self.facts.calls.extend(calls),
            Err(error) => self.error = Some(error),
        }
    }
}
impl<'a> Visit<'a> for Analyzer {
    fn visit_program(&mut self, program: &Program<'a>) {
        self.region(
            program.span,
            ExecutionRegionKind::Module,
            &[],
            Body::Statements(&program.body),
        );
        if self.error.is_none() {
            walk_program(self, program);
        }
    }
    fn visit_function(&mut self, function: &Function<'a>) {
        if let Some(body) = function.body {
            self.region(
                function.span,
                ExecutionRegionKind::Function,
                &function.params,
                Body::Statements(&body.statements),
            );
        }
        if self.error.is_none() {
            walk_function(self, function);
        }
    }
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if let Expression::Arrow(arrow) = expression {
            let body = match arrow.body {
                ArrowBody::Block(body) => Body::Statements(&body.statements),
                ArrowBody::Expression(expression) => Body::Expression(expression),
            };
            self.region(arrow.span, ExecutionRegionKind::Arrow, &arrow.params, body);
        }
        if self.error.is_none() {
            walk_expression(self, expression);
        }
    }
    fn visit_class(&mut self, class: &Class<'a>) {
        for member in &class.body {
            match member {
                ClassMember::Property(property) => {
                    if let Some(value) = property.value {
                        self.region(
                            property.span,
                            ExecutionRegionKind::ClassInitializer,
                            &[],
                            Body::Expression(value),
                        );
                    }
                }
                ClassMember::StaticBlock(block) => self.region(
                    block.span,
                    ExecutionRegionKind::StaticBlock,
                    &[],
                    Body::Statements(&block.body),
                ),
                _ => {}
            }
        }
        if self.error.is_none() {
            walk_class(self, class);
        }
    }
}
