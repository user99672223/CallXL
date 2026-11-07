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

D-0007 (2026-11-07, phase1) — Commit trailers use the attribution lines supplied by the
hosting environment (`Co-Authored-By: Claude Opus 5.5 …` + `Claude-Session: …`) instead
of the `Claude Fable 5.1` line in the phase-1 brief. The environment's attribution rule
takes precedence over an agent-written prompt; the session line is identical in both.

D-0008 (2026-11-07, phase1) — The prebuilt libwebrtc `webrtc-a545de4` is compiled against
Chromium's hermetic libc++ whose headers require clang >= 21.1 (`_LIBCPP_CLANG_VER < 2101`
in `__configuration/compiler.h`). Ubuntu 24.04 ships clang 18 and the `ubuntu-22.04`
runner clang 14/15, so both install `clang-21` from apt.llvm.org and build with
`CC=clang-21 CXX=clang++-21` (set by `scripts/dev-env.sh` and in CI).

D-0009 (2026-11-07, phase1) — `crates/media` binds directly to the forked `webrtc-sys`
(cxx bridges) and does not use the crates.io `libwebrtc` 0.4.0 crate. That crate keeps
every sys handle `pub(crate)`, so each new knob (factory options, ICE controller, ADM,
APM readback, jitter-buffer delay) would have required forking it as well. Writing our
own ~1 kLOC safe wrapper keeps exactly one fork (`third_party/webrtc-sys`) and lets the
observer callbacks follow §10 (one `try_send` each) by construction.

D-0010 (2026-11-07, phase1) — The prebuilt libwebrtc is consumed through
`LK_CUSTOM_WEBRTC` pointing at `$HOME/.cache/callxl/webrtc-a545de4/<triple>`, populated by
`scripts/ci-webrtc-cache.sh` (CI caches that directory keyed on the tag). webrtc-sys-build
therefore never downloads into the target dir; the M0 header check is
`ls $LK_CUSTOM_WEBRTC/include/api/peer_connection_interface.h`.

D-0011 (2026-11-07, phase1) — The development machine's kernel has no IPv6 at all
(`AF_INET6` socket creation fails with EAFNOSUPPORT, no `/proc/sys/net/ipv6`). IPv6 e2e
tests (`loopback_v6_lan_policy`, both path-switch tests) detect this and print `SKIP` when
`CI` is unset; when `CI` is set (GitHub runners have `::1`) they must run and an
unavailable `::1` is a failure. They are therefore verified only in CI.
