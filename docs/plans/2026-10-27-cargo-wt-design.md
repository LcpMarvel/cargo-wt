# cargo-wt design

Date: 2026-10-27

## Background

With several git worktrees of a Rust project, every worktree's `target/` rebuilds all dependencies from scratch, so disk usage and build time grow with each worktree.

On macOS, APFS supports `clonefile(2)`. A clone is created instantly and shares physical blocks with the original until either side rewrites them. cargo-wt uses it to let a new worktree inherit build artifacts from an existing one.

## Preliminary experiment (ripgrep, cargo 1.96, APFS system volume)

| Approach | Crates recompiled | Time | New disk usage |
|---|---|---|---|
| Clean build | 34 | 16s | 280 MB |
| `cp -c` clone of target | 0 | 0s | ≈0 MB |
| Build after clone | 9 (workspace members) | 10s | 154 MB |
| Build after clone + aligning source mtimes | **0** | **0s** | **0 MB** |

Correctness checks:
- The cloned binary runs.
- Editing the source worktree does not trigger rebuilds in the new one.
- Editing the new worktree rebuilds the affected crate and its dependents.

Conclusions:
- Third-party dependencies do not depend on the worktree path and are reused directly.
- Workspace members were rebuilt because checkout refreshed their mtimes, not because of the path. Cargo's own dep-info stores relative paths.
- The remaining absolute paths appear only in two places, and neither affects correctness:
  - rustc's `deps/*.d`, which cargo does not use for freshness;
  - build scripts' `root-output`, which cargo uses to rewrite paths after the target directory moves.

**Key constraint**: mtimes may be aligned only for files with identical content. If a file whose content differs is given an older mtime, cargo considers it unchanged and silently produces a wrong binary.

## Scope

Version 1 includes:
- `cargo wt add`
- `cargo wt seed`
- a `post-checkout` hook, plus `cargo wt install-hook` to install it
- `cargo wt du`

Out of scope:
- `rm` / `gc`: artifacts live inside the worktree, so `git worktree remove` deletes them too.
- `dedup`: deduplicating existing artifacts after the fact; maybe later.

Platform: macOS + APFS only. A non-APFS target is an error; there is no fallback.

## Artifact location

Artifacts live in each worktree's own `<worktree>/target`. This is cargo's default and needs no configuration.

Prerequisite: no global `CARGO_TARGET_DIR`. It would override the default, and cloning only works within one APFS volume.

## Structure

A single Rust binary, `cargo-wt`, invoked as `cargo wt ...` once it is on `PATH`. Two entry points share the core function `seed(new_wt)`:

- `cargo wt add <git worktree add args...> [--from <wt>]` runs `git worktree add`, then seeds.
- The `post-checkout` hook runs `cargo wt seed` when both of these hold:
  - the previous HEAD is the all-zero SHA, i.e. the worktree was just created;
  - the repository root has a `Cargo.toml`.

### seed

1. **Pick a source.**
   - With `--from`, use the given worktree.
   - Otherwise, consider every worktree from `git worktree list` that has a target directory. Run `git diff --name-only <new HEAD>` in each and pick the one with the fewest differing files.
   - If there is no candidate, report it and exit with status 0.
2. **Clone.** Call `clonefile(2)` on the whole target directory. If the new worktree already has one, fail rather than overwrite it.
3. **Restore hardlinks.** See "Findings during implementation" below.
4. **Align mtimes.** For every tracked file in the new worktree that is not in the diff set from step 1, copy the mtime of the matching file in the source worktree (like `touch -r`).
5. **Report.** Print the source worktree, how many files were aligned and how many differing files were skipped.

**Why copy the source file's mtime rather than use the commit time**: this leaves the new worktree exactly as fresh or stale as its source. Cargo rebuilds what it would have rebuilt in the source, no more and no less.

**Why the source does not need to be clean**: `git diff --name-only <new HEAD>` compares the source's working tree, so uncommitted changes count as differences. Files that are ignored in the source but read by the build do not exist in the new worktree; cargo sees them as missing and rebuilds, which is also safe.

**The new worktree must be clean**: when seeding an existing worktree by hand, modified tracked files would break the "same content as the source" assumption, so seed fails instead.

**Target directory lookup**: seed asks `cargo metadata` for the target directory, so `build.target-dir` and similar settings are respected. If the directory is outside the worktree (for example, `CARGO_TARGET_DIR` is set), seed fails and says why.

### Failure handling

If any step after cloning fails, seed deletes the cloned target directory and exits with a non-zero status. It never leaves artifacts with only some mtimes aligned.

### Hook installation

`cargo wt install-hook` writes the hook to the repository's shared `.git/hooks/post-checkout`, which every worktree uses.

- If a hook already exists there, it refuses to overwrite it and explains how to chain it by hand.
- It does not use a global `core.hooksPath`, which would disable every repository's own hooks.

## cargo wt du

`du` walks the target directory of every worktree in the repository and records two values per file:
- logical size;
- private size: `ATTR_CMNEXT_PRIVATESIZE` from `getattrlist`, the bytes not shared with any clone.

It prints each worktree's logical size and private size (what deleting that worktree's artifacts would free). The totals include only two numbers that always hold:
- total private size, which is exact;
- total logical size, an upper bound on physical usage.

There is no lower bound. The original design used "total private + largest single shared amount", but that does not hold because clones also exist inside one worktree: on macOS, cargo 1.96 clones `deps/x-<hash>` to `debug/x` rather than hardlinking it. An exact figure would need the extent layout; version 1 does not do that.

Each inode is counted once. `--json` is supported.

The semantics of `ATTR_CMNEXT_PRIVATESIZE` were verified: after cloning a 50 MB file twice, all three copies report a private size of 0. After overwriting 4 MB of one copy, that copy reports 4 MB and the other two still report 0.

## Findings during implementation

**clonefile splits hardlinks.** Cloning a directory turns its hardlinks into independent clones. rustc's incremental compilation hardlinks `deps/*.rcgu.o` with files under `incremental/`, and splitting them inflates logical size; on ripgrep the difference was about 40 MB. After cloning and before aligning mtimes, seed recreates the hardlinks of the source directory. A failure in this step also triggers cleanup.

## Testing

The main risk is silently producing a wrong binary, so the criterion is that **artifacts behave exactly like a clean build**.

The fixture is an offline workspace with no external dependencies: crate `a`, and crate `b`, which depends on `a` and prints one of its constants. Every scenario runs the binary and checks its output.

| Scenario | Expected |
|---|---|
| New worktree at the same commit as the source | 0 crates recompiled |
| New branch changes `a` | `a` and `b` recompiled; new value printed |
| Source has uncommitted changes | New worktree rebuilds from its own commit's content; correct output |
| Source artifacts are stale | Same set recompiled as the source itself would |
| Source has hardlinks | Same hardlink groups in the new worktree |
| New worktree already has a target directory | Error; nothing overwritten |
| Failure injected while aligning mtimes | Target directory removed; non-zero exit |
| `CARGO_TARGET_DIR` points outside the worktree | Error with a hint |
| `git worktree add` with the hook installed | Seeded automatically |
| `cargo wt add` with the hook installed | Seeded exactly once |
| Hook in a non-Rust repository | Nothing happens |
| `install-hook` with an existing foreign hook | Refuses to overwrite |

Unit tests cover worktree list parsing, null-SHA detection, totals and size formatting.

The test temp directory must be on APFS; otherwise the tests fail instead of being skipped.

Before each release, rerun the experiment on ripgrep as a smoke test.
