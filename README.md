# cargo-wt

在 macOS APFS 上，用 `clonefile` 让新建的 git worktree 继承已有 worktree 的 Rust 构建产物：
- 内容与源 worktree 相同的部分零重编，并且不占额外空间；
- 有差异的文件照常重编。

设计与实验数据见 [docs/plans/2026-10-27-cargo-wt-design.md](docs/plans/2026-10-27-cargo-wt-design.md)。

## 安装

```sh
cargo install --path .
```

前提条件：
- 产物必须放在 worktree 内；
- 不要设置 `CARGO_TARGET_DIR` 或 `build.target-dir`。

## 用法

```sh
cargo wt install-hook            # 在仓库中安装一次；此后任何方式创建的 worktree 都会自动继承产物
cargo wt add <git worktree add 的参数...> [--from <worktree>]
cargo wt seed <worktree> [--from <worktree>]   # 对已建好的 worktree 手动继承
cargo wt du [--json]             # 各 worktree 产物的逻辑大小与独占大小
```

默认的产物来源是工作区内容与新 commit 差异最小的那个 worktree。

## 正确性

- 只有内容与源完全相同的已跟踪文件，才会复制源文件的 mtime；有差异的文件保留新的 mtime，交给 cargo 重编。
- 新 worktree 的新旧状态与源 worktree 完全一致：源 worktree 自己需要重编的部分，新 worktree 同样会重编。
- 克隆后的任一步骤失败，都会删除克隆出来的产物再报错。
