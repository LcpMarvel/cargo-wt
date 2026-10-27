# cargo-wt 设计

日期：2026-10-27

## 背景

Rust 项目开多个 git worktree 时，每个 worktree 的 `target/` 都要完整重编依赖，磁盘占用和编译时间成倍增加。

macOS 的 APFS 支持 `clonefile(2)`：克隆文件瞬间完成，克隆前后共享物理块，只有被改写的部分才占新空间。cargo-wt 用它让新 worktree 继承已有 worktree 的构建产物。

## 前期实验（ripgrep，cargo 1.96，APFS 系统盘）

| 做法 | 重编 crate 数 | 耗时 | 磁盘新增 |
|---|---|---|---|
| 从零构建 | 34 | 16s | 280 MB |
| `cp -c` 克隆 target | 0 | 0s | ≈0 MB |
| 克隆后构建 | 9（workspace 成员） | 10s | 154 MB |
| 克隆 + 对齐源文件 mtime 后构建 | **0** | **0s** | **0 MB** |

正确性验证：
- 克隆得到的二进制可以运行。
- 修改源 worktree 的代码，不会触发新 worktree 重编。
- 修改新 worktree 的代码，会正确重编对应 crate 及其下游。

结论：
- 第三方依赖与路径无关，克隆后直接复用。
- workspace 成员重编是因为 checkout 刷新了 mtime，路径本身不是原因。cargo 自己的 dep-info 存的是相对路径。
- 残留的绝对路径只出现在 rustc 的 `deps/*.d`（cargo 判断新旧时不用它）和 build script 的 `root-output`（cargo 用它在目录移动后重写路径）中，不影响正确性。

**关键约束**：只有内容完全相同的文件才能对齐 mtime。如果给内容不同的文件改成旧的 mtime，cargo 会误判为没有变化，静默编出错误的二进制。

## 范围

第一版做以下几项：
- `cargo wt add`
- `cargo wt seed`
- `post-checkout` hook，以及安装它的 `cargo wt install-hook`
- `cargo wt du`

不做：
- `rm` / `gc`：产物放在 worktree 内，`git worktree remove` 会一起删掉。
- `dedup`：对已有产物做事后去重，以后再考虑。

平台只支持 macOS + APFS。目标不是 APFS 时直接报错，不做降级。

## 产物位置

产物放在每个 worktree 自己的 `<worktree>/target`，也就是 cargo 的默认位置，不需要额外配置。

前提条件：不能设置全局 `CARGO_TARGET_DIR`。它会覆盖默认位置，而且克隆只能在同一个 APFS 卷内进行。

## 结构

一个 Rust 二进制 `cargo-wt`，放进 PATH 后以 `cargo wt ...` 调用。两个入口共用核心函数 `seed(new_wt)`：

- `cargo wt add <path> [<branch>] [--from <wt>]`：先执行 `git worktree add`，再执行 seed。
- `post-checkout` hook：满足以下条件时执行 `cargo wt seed <path>`：
  - 上一个 HEAD 是全零 SHA，也就是这是新建的 worktree；
  - 仓库里有 `Cargo.toml`。

### seed 流程

1. **选源**
   - 指定了 `--from` 就直接用它。
   - 否则从 `git worktree list` 里筛出存在 `target/` 的 worktree。在每个候选里执行 `git diff --name-only <新 HEAD>`，选差异文件最少的那个。
   - 没有候选时提示后退出，退出码为 0。
2. **克隆**：对整个 `target` 调用 `clonefile(2)`。如果新 worktree 已经有 `target`，报错，不覆盖。
3. **对齐 mtime**：遍历新 worktree 的所有已跟踪文件，跳过第 1 步得到的差异文件。其余文件用源 worktree 中对应文件的 mtime 覆盖（相当于 `touch -r`）。
4. **输出摘要**：源 worktree、对齐的文件数、跳过的差异文件数。

为什么复制源文件的 mtime，而不是用 commit 时间：这样新 worktree 和源 worktree 的新旧状态完全一致。源 worktree 自己要重编的部分，新 worktree 也会重编，不多也不少。

`git diff --name-only <新 HEAD>` 比较的是源 worktree 的工作区内容，未提交的修改也算作差异，所以**不要求源 worktree 是干净的**。源 worktree 中被忽略、但构建时会读到的文件，在新 worktree 里不存在，cargo 会因为文件缺失而判定需要重编，同样安全。

### 失败处理

第 2、3 步中任何一步失败，就删掉已经克隆过来的 `target`，然后以非零退出码退出。绝不留下 mtime 只对齐了一部分的状态。

### hook 安装

`cargo wt install-hook` 把 hook 写入仓库公共的 `.git/hooks/post-checkout`，所有 worktree 共用。

- 如果已有 hook，拒绝覆盖并报错，给出手动串联的方法。
- 不使用全局的 `core.hooksPath`，因为它会让所有仓库自带的 hook 失效。

## cargo wt du

遍历当前仓库所有 worktree 的 `target/`，对每个文件取两个值：
- 逻辑大小；
- 独占大小：用 `getattrlist` 读 `ATTR_CMNEXT_PRIVATESIZE`，表示不和任何克隆共享的字节数。

输出每个 worktree 的逻辑大小，以及独占大小（即删掉这个 worktree 能释放的空间）。共享部分只给上下界：
- 下界：独占合计 + 最大的单个共享量；
- 上界：独占合计 + 共享量之和。

要算精确值需要读 extent 布局，第一版不做。

支持 `--json` 输出。

实现的第一步：先写一个小程序，验证 `ATTR_CMNEXT_PRIVATESIZE` 对克隆文件的计算语义。如果不符合预期，`du` 直接报错，不输出可能误导人的数字。

## 测试

核心风险是静默编出错误的二进制，因此测试的标准是**产物和从零构建的结果一致**。

测试夹具是一个离线、无外部依赖的 workspace：crate `a`，以及依赖它的 `b`，`b` 的二进制打印 `a` 里的一个常量。每个场景都实际运行二进制、检查输出。

| 场景 | 预期 |
|---|---|
| 新 worktree 与源 worktree 在同一个 commit | 重编 0 个 |
| 新分支修改了 `a` | `a`、`b` 重编，输出新值 |
| 源 worktree 有未提交的修改 | 新 worktree 按自己 commit 的内容重编，输出正确 |
| 源 worktree 的产物已过期 | 重编的集合和源 worktree 自己会重编的集合相同 |
| 新 worktree 已有 `target` | 报错，不覆盖 |
| 在对齐 mtime 阶段注入失败 | `target` 被删除，非零退出 |
| `git worktree add` 触发 hook | 自动 seed |
| 非 Rust 仓库触发 hook | 什么都不做 |
| 已有 hook 时执行 `install-hook` | 拒绝覆盖 |

单元测试覆盖三块：选源逻辑、hook 参数解析、差异文件集合的计算。

测试的临时目录必须在 APFS 上，不满足时直接失败，不跳过。

发版前，在 ripgrep 上重跑一遍前期实验，作为冒烟测试。
