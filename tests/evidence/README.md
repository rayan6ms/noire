# Evidence templates

`requirements.toml` maps every current requirement to the named templates in
this directory. A template describes the evidence that must eventually exist; it
does not claim that a planned requirement has passed.

Template status has two values:

- `active`: the cited automated source exists and runs today;
- `planned`: the stable test/evidence contract exists, but qualifying evidence is
  not yet produced by the implementation.

Automated templates use `AT-`, manual templates use `ME-`, and mixed automated
plus human-review templates use `MX-`. Manual work is permitted only where the
template records why automation cannot establish the complete result.

Every completed evidence record derived from a template must include:

- covered requirement and task IDs;
- exact commit and package version;
- commands, fixtures, and environment identifiers;
- machine-readable results where available;
- manual author and date when applicable;
- failures, waivers, and defect links.

Evidence must assert observable samples, graph nodes, files, process state, exit
codes, or API replies. A method call or successful compilation is not behavioral
evidence unless the requirement is specifically a build boundary.

Run `python3 .github/scripts/validate_traceability.py --self-test` after changing
the manifest or templates. The validator checks the fixed current ID set,
priorities, reciprocal mappings, unique evidence/test IDs, manual justifications,
and active source locators.
