# Prompt DSL / PostDSL parity against real NeuroMita prompts

Validated on **2026-11-05** against the user-supplied `Prompts.zip`.
The full original directory is kept byte-for-byte under
`tests/fixtures/prompt-corpus/Prompts/` (538 files, 31 prompt sets, 9 characters).
The uploaded archive SHA-256 was:
`0e5cae2801096d47be4eef3230a15c5f0c914bcb93b4eee16430eb17384073c6`.
`Legacy` files are preserved as fixtures; they were not modified or integrated.

## Reference and method

Python baseline: `Atm4x/NeuroMita`, branch
`bridge/session-timeline-export-20261101`; reference source tree at
`5f3d061f523d9abefa6a264bb2953048a935fa65`.
The harness invokes **the actual Python** `DSL.dsl_engine.DslInterpreter`
and `DSL.post_dsl_engine.PostDslInterpreter` with a deterministic synthetic
Character. Its `set_variable` mutates a Python dictionary; memory is absent.
The harness loads `config.json` initial values, disables seed flags, and
resolves case-insensitive file names to emulate the normal Windows deployment.
The Rust test uses the same supplied resources, character defaults and seed
flags. Output block SHA-256 values are computed after CRLF normalization and
removing trailing whitespace (the Python file loader uses `rstrip`).
The check compares individual stable/dynamic blocks and their ordering, not
just whether parsing succeeded.

**Results:**

| Test | Observed | Scope |
| --- | --- | --- |
| Main DSL loads/rendering | 31/31 sets | Includes nested templates and shared `Common/` and `Structural/` resources |
| Exact Python stable/dynamic output | 21/31 sets | Same synthetic inputs and normalized block boundaries |
| Documented behavior differences | 10/31 sets | Below; not asserted as exact parity |
| Seed-flag branches | 31/31 sets | Rust interpretation and declaration building with all six flags true; *not* Python/Rust memory side-effect parity |
| Real PostDSL scripts | 9/9 cases | One fixed test response plus a synthetic initial variable state; text and resulting variables match Python |
| Real `custom_params` | 2 sets | `Love` formula and boolean field handling |

### The ten differences

For nine sets, Python automatically initializes an unknown **local** variable
`attitude_band` as `None`. The Python expression evaluator continues to prefer
that stale `None` over a later `Character.set_variable("attitude_band", ...)`
assignment. Rust reads the newly assigned value. This difference affects:

- `Cappie/Default`, `Cappie/Lite`
- `Crazy/Lite`
- `Creepy/Default`, `Creepy/Lite`
- `Ghost/Default`, `Ghost/Lite`
- `Mila/Default`, `Mila/Lite`

`Kind/LiteJson` has the same Python-local shadowing error for `Love`:
Python emits a `[DSL ERROR ...]` marker when comparing `None >= 5`, whereas
Rust uses the `Love = 0` value set by the script. These differences are
intentionally retained and explicitly annotated in the reference manifest;
the Rust test does **not** silently count them as exact matches. If exact
compatibility with the Python bug is required, it needs a separate decision.

### Bugs fixed by the real corpus

- Snapshot loading includes `Common/` and `Structural/` shared folders; all
  included resources are still frozen at request admission.
- Unambiguous case-insensitive paths in captured resources match Windows
  behavior; ambiguous names fail instead of selecting an arbitrary file.
- The `[{var}]` substitution matches an identifier only; JSON examples like
  `[{"type": "intent.type", "payload": {}}]` are preserved verbatim.
- Source line endings and trailing-space handling match the reference loader.
- `SET LOCAL` is script-scoped; it does not leak into other scripts.
- `round()` preserves integer formatting when its input and result are integral.

### Limits of the evidence

The harness is a deterministic DSL evaluation comparison, **not** a live
LLM/GUI integration run or a proof that all possible variable values and
control-flow branches match Python. The fixed PostDSL scenario does not cover
every input or error case. Python memory seeding side effects, live Timeline,
provider protocols, GUI `DEBUG_DISPLAY`, filesystem change races and external
custom plugins are outside this corpus test. Existing Rust integration tests
cover the application-level paths separately. Python error recovery and Rust's
strict error handling still differ by policy.

## Reproduce

With the Python NeuroMita source checkout and its supported interpreter:

```bash
python3 tools/prompt_parity/python_reference.py \
  --python-src /path/to/NeuroMita/src \
  --prompts tests/fixtures/prompt-corpus/Prompts \
  --output /tmp/prompt-reference
cargo test --offline --test prompt_corpus
```

Compare the two generated `python_*.json` manifests **as parsed JSON** with
those in `tests/fixtures/prompt-corpus/`. The Rust integration test asserts
that all 31 sets are present, all 21 exact matches remain exact, all
10 exceptions are explicitly recorded, and all 9 sampled PostDSL behaviors
match. To promote a documented exception to an exact match, delete its key
from `known_python_semantics_differences`; the test will then verify its
block hashes against Python.
