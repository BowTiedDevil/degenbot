# Architecture Deepening Plans

Plans are numbered sequentially in a single `0xx` series, grouped by domain.

See the [skill vocabulary](https://github.com/user/skills/improve-codebase-architecture) for terms: **module**, **interface**, **depth**, **seam**, **adapter**, **leverage**, **locality**.

## Writing Plans

New plans **must** follow the [template](TEMPLATE.md). The template is derived from the clearest completed plans (035, 039, 040, 045). Key requirements:

1. **Deletion test** — state what happens if you delete the code; this distinguishes reorganizing from removing
2. **Specific friction table** — concrete, falsifiable rows (not vague complaints)
3. **Vertical slices** — each slice ships independently with a green test suite
4. **Design decisions** — record non-obvious choices with rationale so reviewers don't infer them
5. **Relationship to other plans** — list every intersecting plan with its relationship (prerequisite / complementary / orthogonal / superseded)
6. **Status checklist** — `[ ]` unchecked items; mark `[x]` and note results when complete


