# Architecture evolution loop

Read this file completely for every L1 or L2 task.

## Evidence labels

- **Fact**: directly supported by current source, tests, manifest, artifact, or command output.
- **Inference**: a conclusion drawn from facts; describe what could falsify it.
- **Decision**: the chosen target architecture or tradeoff.
- **Hypothesis**: not yet verified; pair it with an experiment and expected observation.
- **Rejected**: an alternative considered and the invariant it violates or cost it adds.

Do not describe roadmap items as facts. Do not preserve an invalidated hypothesis as background prose.

## Architecture Brief template

```markdown
Architecture Brief vN

Level: L1 | L2
Product goal:
Success conditions:
Current architecture and evidence:
Target architecture:
Facts:
Inferences:
Decisions:
Hypotheses and experiments:
Responsibilities and data flow:
Invariants:
Public and artifact changes:
Deletion/replacement plan:
Validation matrix:
Convergence conditions:
```

## Revision loop

When evidence contradicts the brief:

1. preserve the reproducible evidence;
2. identify the first invalid assumption, not only the final failure;
3. determine whether ownership, model, data flow, or implementation is wrong;
4. revise the brief and ADR before broadening the code change;
5. remove experiments or temporary paths that no longer represent the target model;
6. rerun earlier evidence after the new slice lands.

Do not use “tests now pass” as the sole revision justification.

## ADR lifecycle

Use `engineering/decisions/0000-template.md` for L2 decisions that outlive a single patch.

- `proposed`: target design is under validation.
- `accepted`: evidence supports adoption and the repository follows the decision.
- `superseded`: a newer ADR replaces the decision.
- `rejected`: evaluated but not adopted.

Never edit an old ADR to conceal a superseded decision. The new ADR names every replaced ADR; the old ADR changes status to `superseded` and points to the replacement.

## Deletion and complexity audit

Record removed types, entrypoints, branches, adapters, flags, and caches; remaining sources of truth; new abstractions and their consumers; temporary bridges and their removal conditions; and any duplicate behavior path left behind.

Prefer the smallest complete vertical slice. Reject both a local patch that deepens the wrong boundary and a broad rewrite whose extra surface is not required by the target model.
