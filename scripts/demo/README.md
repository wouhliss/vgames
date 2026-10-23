# Milestone evidence

Run `scripts/demo/m1.sh` (or `m2.sh`, `m3.sh`, `m3b.sh`, `m4.sh`) with an authenticated
read-only `gh` session. The default commit is HEAD; `--sha <40-character SHA>` collects
another commit. These commands read Actions metadata and test logs; they do not deploy,
publish, generate keys, or run against a user's server. Raw logs are never printed or saved.

Exit codes: 0 means reached, 1 means an existing proof failed or evidence could not be
read, and 2 means not reached because evidence/tasks are pending. `--report` gives pending
a zero exit code so nightly summaries can describe unfinished work; its table still says
**not reached**, and any existing failed, skipped or cancelled test job still exits 1.
The collector passing means evidence collection completed. A separate `Milestone M1`
(through M4, including M3B) job turns green only for a `reached` row; pending milestone
jobs stay gray. All existing proof jobs still run and their failures fail collection.

The collector reads workflow definitions from the selected Git commit, ignoring uncommitted worktree changes. Fetch a historical commit before using `--sha`; unavailable commit objects fail closed. It uses the newest run/attempt for the exact commit. It never substitutes an
older green run. Passing a whole job is insufficient for M1–M3: required steps and named
test receipts must exist within the named step, so ignored tests, zero matches and skipped Node scenarios fail. A later backend step running the same Rust test name cannot satisfy an earlier step. Unscoped or empty logs fail closed.
Nightly E2E collects M1–M3 and M4; the scheduled/manual desktop matrix collects each OS
for M3b. A run in progress is pending until its required job completes. Re-run the entry
point after concurrent CI/matrix jobs finish. Actions jobs receive only contents/actions read.
The last-green column searches the latest thirty completed main runs for a successful
named milestone job. It is history only and never changes the current status.

M3's two-profile real-app invite, M3b's Windows overlay and sequential cloud-save scenario,
and M4's full acceptance adapters are explicitly pending. Existing compatibility, dry-run,
soak and transfer-budget workflow failures are still collected and fail the report. Add
named test/metric receipts when those scenarios land; remove the corresponding pending row
only after its real proof is collected. A green diagnostic soak cannot stand in for a
5.5-hour measurement. Hardware-only observations remain in human-checklist §B.

`python3 -m unittest discover -s scripts/demo -p test_collect.py` checks failure precedence,
zero/skipped tests, attempt selection, exact commits, pagination and unreadable evidence.
