# cargo-wt

Let new git worktrees inherit Rust build artifacts from an existing worktree, using APFS `clonefile` on macOS.

When a new worktree matches its source, it needs no rebuilds and takes no extra disk space. Files that differ are rebuilt as usual.

On ripgrep, a fresh worktree normally needs 34 crates rebuilt and 280 MB of new disk space. With cargo-wt it needs **0 crates and ~0 MB**.

[中文说明](#中文说明)

## Install

```sh
cargo install --git https://github.com/LcpMarvel/cargo-wt
```

Requirements:

- macOS with the repository on an APFS volume.
- Build artifacts must live inside the worktree (the default `target/`). Do not set `CARGO_TARGET_DIR` or `build.target-dir`.

## Usage

Install the hook once per repository:

```sh
cargo wt install-hook
```

After that, every new worktree inherits artifacts automatically, however it was created: plain `git worktree add`, your editor, or AI agents.

Other commands:

```sh
cargo wt add <git worktree add args...> [--from <worktree>]
cargo wt seed <worktree> [--from <worktree>]   # seed an existing worktree
cargo wt du [--json]                           # logical vs. private (unshared) size per worktree
```

By default, artifacts come from the worktree whose working tree differs least from the new commit.

## How it stays correct

1. The source `target/` is cloned with `clonefile`. Cloning takes no time, and blocks stay shared until they are rewritten.
2. Hardlinks that rustc creates for incremental compilation are restored in the clone.
3. A tracked file gets its source counterpart's mtime only if its content is identical, as determined by `git diff` against the new HEAD. Uncommitted changes in the source count as differences. Files that differ keep a fresh mtime, so cargo rebuilds them.

As a result, the new worktree is exactly as fresh or stale as its source: cargo rebuilds whatever it would have rebuilt in the source.

If any step after cloning fails, the cloned `target/` is removed before the error is reported.

The design notes and experiment data are in [docs/plans](docs/plans/2026-10-27-cargo-wt-design.md).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.

---

## 中文说明

在 macOS APFS 上用 `clonefile`，让新建的 git worktree 继承已有 worktree 的 Rust 构建产物。内容相同的部分零重编，也不占额外空间。

```sh
cargo install --git https://github.com/LcpMarvel/cargo-wt
cargo wt install-hook    # 每个仓库执行一次，之后任何方式新建的 worktree 都会自动继承产物
```

前提：产物在 worktree 内，不要设置 `CARGO_TARGET_DIR` / `build.target-dir`。设计与实验数据见 [docs/plans](docs/plans/2026-10-27-cargo-wt-design.md)。
