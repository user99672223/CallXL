# DECISIONS.md — append-only decision log

Format: `D-NNNN (date, author) — decision. Why.` Never edit or delete earlier entries;
append a new entry that supersedes an old one if needed.

D-0001 (2026-11-07, orchestrator) — The integration branch for this run is
`claude/new-session-opnv54`, not `main`. The hosting environment mandates that branch as
the only one the orchestrator may push to; the repository was empty, so this branch is also
the repository's default branch. Everywhere `CLAUDE.md` or a prompt says "PR to `main`",
read "PR to `claude/new-session-opnv54`". Feature branches (`phase1/core`, `p2/desktop`,
`p2/ios`, `p2/infra-qa`) are pushed so CI runs on them. The user can fast-forward `main`
from the integration branch when they take delivery.

D-0002 (2026-11-07, orchestrator) — The `gh` CLI is not usable in this environment (its
token is rejected). Agents check CI through the GitHub MCP tools (`actions_list`,
`actions_get`, `get_job_logs`) or leave CI triage to the orchestrator. Any instruction
mentioning `gh run watch` / `gh run view --log-failed` means "inspect the run via those
tools".

D-0003 (2026-11-07, orchestrator) — `tc netem` is unavailable on the development machine
(the kernel lacks `sch_netem`: "Specified qdisc kind is unknown"). It *is* available on
GitHub's `ubuntu-22.04` runners. The e2e harness therefore has two impairment backends
selected by `CALLXL_NETEM=tc|proxy`: real `tc netem` on `lo` (CI), and a userspace UDP
impairment relay in `tests/e2e` that rewrites ICE candidates to go through a relay port
that adds delay/jitter/loss (local development). Every netem scenario must pass under
both backends; CI runs the `tc` backend.

D-0004 (2026-11-07, orchestrator) — Rust toolchain pinned to 1.97.0 (current stable on
the development machine), edition 2024.

D-0005 (2026-11-07, orchestrator) — Media binding baseline: crates.io `libwebrtc` 0.4.0 /
`webrtc-sys` 0.3.53 / `webrtc-sys-build` 0.3.21, which download LiveKit's prebuilt
libwebrtc `webrtc-a545de4`. Inspection shows `set_bitrate`, `create_device_audio_track`
(platform ADM capture), an APM config path and `on_ice_selected_candidate_pair_changed`
exist, but arbitrary field trials, a custom ICE controller factory, port-allocator flags,
Opus fmtp/ptime control and a WAV-playing test ADM do not. A minimal fork in
`third_party/webrtc-sys` (vendored 0.3.53 + cxx glue) is therefore expected in Phase 1.
The prebuilt libwebrtc download is reused unchanged (keyed on its tag in CI caches).

D-0006 (2026-11-07, orchestrator) — Phase 2 agents run concurrently on one 4-core
machine. To avoid three independent libwebrtc links, all worktrees share
`CARGO_TARGET_DIR=/home/user/target-shared`; cargo's own lock serialises concurrent
builds. CI keeps the default per-checkout target directory.
