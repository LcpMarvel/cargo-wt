//! 端到端测试：判据是继承产物后的二进制与从零构建的输出一致。
//! 夹具是离线可编译的 workspace：b 打印 a::VALUE。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_cargo-wt");

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    main: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let main = root.join("main");
        write(&main, "Cargo.toml", "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"3\"\n");
        write(&main, "a/Cargo.toml", "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2024\"\n");
        write(&main, "a/src/lib.rs", "pub const VALUE: u32 = 1;\n");
        write(
            &main,
            "b/Cargo.toml",
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        );
        write(&main, "b/src/main.rs", "fn main() { println!(\"{}\", a::VALUE); }\n");
        write(&main, ".gitignore", "/target\n");
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@example.com"]);
        git(&main, &["config", "user.name", "t"]);
        cargo(&main, &["generate-lockfile", "--offline"]);
        commit_all(&main, "init");
        Fixture { _tmp: tmp, root, main }
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

fn write(dir: &Path, rel: &str, content: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn check(out: Output, what: &str) -> Output {
    assert!(
        out.status.success(),
        "{what} 失败:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn git(dir: &Path, args: &[&str]) -> Output {
    check(Command::new("git").arg("-C").arg(dir).args(args).output().unwrap(), &format!("git {args:?}"))
}

fn commit_all(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn cargo(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new("cargo").current_dir(dir).args(args).env_remove("CARGO_TARGET_DIR").output().unwrap();
    check(out, &format!("cargo {args:?}"))
}

/// 构建并返回被重新编译的 crate 名
fn build(dir: &Path) -> Vec<String> {
    let out = cargo(dir, &["build", "-v", "--offline"]);
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("Compiling "))
        .map(|l| l.split(' ').next().unwrap().to_owned())
        .collect()
}

fn run_b(dir: &Path) -> String {
    let out = check(Command::new(dir.join("target/debug/b")).output().unwrap(), "运行 b");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn path_with_bin() -> String {
    let bin_dir = Path::new(BIN).parent().unwrap();
    format!("{}:{}", bin_dir.display(), std::env::var("PATH").unwrap())
}

fn cargo_wt(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(dir)
        .args(args)
        .env_remove("CARGO_TARGET_DIR")
        .env("PATH", path_with_bin())
        .output()
        .unwrap()
}

fn seed(f: &Fixture, wt: &Path) -> Output {
    cargo_wt(&f.main, &["seed", wt.to_str().unwrap()])
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn same_commit_needs_no_rebuild() {
    let f = Fixture::new();
    assert_eq!(build(&f.main).len(), 2);
    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    check(seed(&f, &wt), "seed");
    assert_eq!(build(&wt), Vec::<String>::new());
    assert_eq!(run_b(&wt), "1");
}

#[test]
fn preserves_hardlinks() {
    let f = Fixture::new();
    build(&f.main);
    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    check(seed(&f, &wt), "seed");
    // rustc 增量编译把 deps/*.rcgu.o 与 incremental/ 下的文件硬链接
    let src = hardlink_groups(&f.main.join("target"));
    assert!(!src.is_empty(), "夹具应产生硬链接");
    assert_eq!(hardlink_groups(&wt.join("target")), src);
}

/// 按 inode 分组的硬链接相对路径集合
fn hardlink_groups(root: &Path) -> std::collections::BTreeSet<Vec<PathBuf>> {
    use std::os::unix::fs::MetadataExt;
    fn walk(root: &Path, dir: &Path, m: &mut std::collections::HashMap<u64, Vec<PathBuf>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                walk(root, &p, m);
            } else if meta.nlink() > 1 {
                m.entry(meta.ino()).or_default().push(p.strip_prefix(root).unwrap().to_owned());
            }
        }
    }
    let mut m = std::collections::HashMap::new();
    walk(root, root, &mut m);
    m.into_values()
        .map(|mut v| {
            v.sort();
            v
        })
        .collect()
}

#[test]
fn branch_with_changed_crate_rebuilds_it_and_downstream() {
    let f = Fixture::new();
    git(&f.main, &["checkout", "-q", "-b", "feat"]);
    write(&f.main, "a/src/lib.rs", "pub const VALUE: u32 = 2;\n");
    commit_all(&f.main, "feat");
    git(&f.main, &["checkout", "-q", "main"]);
    build(&f.main);

    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"]);
    check(seed(&f, &wt), "seed");
    assert_eq!(build(&wt), ["a", "b"]);
    assert_eq!(run_b(&wt), "2");
    // 源 worktree 不受影响
    assert_eq!(build(&f.main), Vec::<String>::new());
    assert_eq!(run_b(&f.main), "1");
}

#[test]
fn uncommitted_change_in_source_is_not_inherited() {
    let f = Fixture::new();
    build(&f.main);
    write(&f.main, "a/src/lib.rs", "pub const VALUE: u32 = 3;\n");
    build(&f.main);
    assert_eq!(run_b(&f.main), "3");

    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    check(seed(&f, &wt), "seed");
    assert_eq!(build(&wt), ["a", "b"]);
    assert_eq!(run_b(&wt), "1");
}

#[test]
fn stale_source_artifacts_rebuild_like_source_would() {
    let f = Fixture::new();
    build(&f.main);
    write(&f.main, "a/src/lib.rs", "pub const VALUE: u32 = 5;\n");
    commit_all(&f.main, "bump");

    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    check(seed(&f, &wt), "seed");
    assert_eq!(build(&wt), ["a", "b"]);
    assert_eq!(run_b(&wt), "5");
    assert_eq!(build(&f.main), ["a", "b"]);
}

#[test]
fn refuses_existing_target() {
    let f = Fixture::new();
    build(&f.main);
    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    std::fs::create_dir(wt.join("target")).unwrap();
    let out = seed(&f, &wt);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("已存在"), "{}", stderr(&out));
}

#[test]
fn failure_while_aligning_removes_cloned_target() {
    let f = Fixture::new();
    build(&f.main);
    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let locked = wt.join("b/src/main.rs");
    check(Command::new("chflags").arg("uchg").arg(&locked).output().unwrap(), "chflags");
    let out = seed(&f, &wt);
    check(Command::new("chflags").arg("nouchg").arg(&locked).output().unwrap(), "chflags");
    assert!(!out.status.success());
    assert!(!wt.join("target").exists(), "失败后不应留下产物");
}

#[test]
fn refuses_target_dir_outside_worktree() {
    let f = Fixture::new();
    build(&f.main);
    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    let out = Command::new(BIN)
        .current_dir(&f.main)
        .args(["seed", wt.to_str().unwrap()])
        .env("CARGO_TARGET_DIR", f.root.join("shared"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("CARGO_TARGET_DIR"), "{}", stderr(&out));
}

#[test]
fn hook_seeds_worktrees_created_by_plain_git() {
    let f = Fixture::new();
    build(&f.main);
    check(cargo_wt(&f.main, &["install-hook"]), "install-hook");
    let wt = f.wt("wt");
    let out = Command::new("git")
        .arg("-C")
        .arg(&f.main)
        .args(["worktree", "add", "-q", wt.to_str().unwrap()])
        .env_remove("CARGO_TARGET_DIR")
        .env("PATH", path_with_bin())
        .output()
        .unwrap();
    check(out, "git worktree add");
    assert_eq!(build(&wt), Vec::<String>::new());
    assert_eq!(run_b(&wt), "1");
}

#[test]
fn hook_ignores_non_rust_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    write(&repo, "README", "hi\n");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    commit_all(&repo, "init");
    check(cargo_wt(&repo, &["install-hook"]), "install-hook");
    let wt = repo.with_file_name("wt");
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["worktree", "add", "-q", wt.to_str().unwrap()])
        .env("PATH", path_with_bin())
        .output()
        .unwrap();
    check(out, "git worktree add");
    assert!(!wt.join("target").exists());
}

#[test]
fn install_hook_refuses_foreign_hook_and_is_idempotent() {
    let f = Fixture::new();
    check(cargo_wt(&f.main, &["install-hook"]), "install-hook");
    check(cargo_wt(&f.main, &["install-hook"]), "再次 install-hook");

    let g = Fixture::new();
    write(&g.main, ".git/hooks/post-checkout", "#!/bin/sh\necho mine\n");
    let out = cargo_wt(&g.main, &["install-hook"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("拒绝覆盖"), "{}", stderr(&out));
}

#[test]
fn add_seeds_once_even_with_hook_installed() {
    let f = Fixture::new();
    build(&f.main);
    check(cargo_wt(&f.main, &["install-hook"]), "install-hook");
    let wt = f.wt("wt");
    // 模拟 `cargo wt add ...`：cargo 会把 "wt" 作为第一个参数传入
    check(cargo_wt(&f.main, &["wt", "add", "-q", wt.to_str().unwrap()]), "add");
    assert_eq!(build(&wt), Vec::<String>::new());
    assert_eq!(run_b(&wt), "1");
}

#[test]
fn du_reports_shared_artifacts() {
    let f = Fixture::new();
    build(&f.main);
    let wt = f.wt("wt");
    git(&f.main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    check(seed(&f, &wt), "seed");
    let out = check(cargo_wt(&f.main, &["du", "--json"]), "du");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let wts = v["worktrees"].as_array().unwrap();
    assert_eq!(wts.len(), 2);
    for w in wts {
        let logical = w["logical_bytes"].as_u64().unwrap();
        let private = w["private_bytes"].as_u64().unwrap();
        assert!(logical > 0);
        // 刚克隆完、未构建：几乎全部共享
        assert!(private * 10 < logical, "独占 {private} 应远小于逻辑 {logical}");
    }
}
