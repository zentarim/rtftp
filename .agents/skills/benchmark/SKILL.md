---
name: benchmark
description: >-
  Runs and interprets the cold-guest download-time benchmark
  (make bench-download) and compares builds or commits for performance
  regressions. Use when the user asks to benchmark, measure download time,
  compare performance between commits, bisect a slowdown, or investigate
  stalls or retransmits.
---

# Benchmark

Run only on explicit request. Never add it to `make test` or CI.

## Run

- Run `make bench-download`. It builds cloud-hypervisor and the initramfs
  if missing, then runs the ignored test `tests/test_download_time.rs`.
- Expect about 15 minutes for the full matrix of 6 sizes x 2 modes x
  (1 warm-up + 3 runs).
- Copy `target/bench/download_time.json` aside before rerunning; every run
  overwrites it and wipes `target/bench/logs/`.

## Read the results

- Compare transfer time, `total_s - first_response_s`. Treat
  `first_response_s` as guest boot (about 1.5 s); it hides transfer changes.
- Check `retransmits` per run. The server logs each retransmit, and the
  count comes from `target/bench/logs/<case>.log`.
- Read `max_gap_s` and `max_gap_block`:
  - 2-5 ms at block 2048*k+1 is the host fetching the next 1 MiB chunk.
  - About 15 ms at block 32770 is the client's buffer growing at 16 MiB.
    It is a measurement artifact.
  - Second-long gaps with no retransmits point to a host event, not code.
- A uniform slowdown with unchanged gaps is a per-block cost. Look for
  state left behind by setup code.

## Control noise

- Distrust a single run, a single session, and any comparison across
  sessions. Drift between sessions is as large as a 5% regression.
- Record `/proc/loadavg` at each step. Distrust steps that ran under load.
- Collect at least 20 runs per variant, in interleaved rounds with a fresh
  shuffled order each round.
- Report medians and a permutation test on the median difference. Accept
  p < 0.01, and check that the sign holds in every round.

## Compare builds

1. Create one `git worktree add --detach` per ref in the scratchpad.
2. Copy the same `tests/test_download_time.rs` and
   `tests/common/client.rs` into every worktree, so only `src/` differs.
3. Narrow `SIZES_MIB`, `BLOCK_SIZES` and `MEASURED_RUNS` in the worktree
   copies only. 32 MiB plain with 5 runs is the most sensitive case.
4. Copy `tests/*.qcow2` into each worktree and `touch` them, to skip the
   guestfish rebuild.
5. Share one hypervisor, guest and script by exporting
   `RTFTP_CLOUD_HYPERVISOR`, `RTFTP_STOREKEEPER` and `RTFTP_MK_INITRAMFS`.
6. Build every worktree before measuring:
   `cargo test --release --test test_download_time --no-run`.
7. Loop over rounds and variants, run
   `cargo test --release --test test_download_time -- --ignored --test-threads 1 --nocapture`
   in each worktree, and copy its JSON to `res/s<step>-<variant>.json`.
8. Run long loops in the background and report interim medians on request.

## Bisect a slowdown

- Measure every commit in the range in interleaved rounds. Do not use a
  binary bisect; noise misleads a single comparison.
- Confirm the culprit against its parent in every round.
- Split the culprit into variants that each revert one change on top of the
  culprit, and measure them against the parent the same way. Repeat until
  one line is left.
- Do not trust a cause read from the diff. Prove it by measurement.

## Clean up

- Remove the worktrees with `git worktree remove --force`.
- Never commit narrowed constants.

## Known findings

Read [references/known-findings.md](references/known-findings.md) before
investigating a slowdown, and add each confirmed cause to it.
