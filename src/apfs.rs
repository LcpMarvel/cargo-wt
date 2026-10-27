//! macOS / APFS 系统调用：clonefile、独占大小、mtime。

use anyhow::{Context, Result, bail};
use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

fn cpath(p: &Path) -> Result<CString> {
    CString::new(p.as_os_str().as_bytes()).with_context(|| format!("路径含 NUL: {}", p.display()))
}

pub fn ensure_apfs(p: &Path) -> Result<()> {
    let c = cpath(p)?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("statfs {}", p.display()));
    }
    let fs = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) }.to_string_lossy();
    if fs != "apfs" {
        bail!("{} 位于 {fs} 文件系统上，cargo-wt 只支持 APFS", p.display());
    }
    Ok(())
}

pub fn ensure_same_volume(a: &Path, b: &Path) -> Result<()> {
    let da = std::fs::metadata(a).with_context(|| format!("stat {}", a.display()))?.dev();
    let db = std::fs::metadata(b).with_context(|| format!("stat {}", b.display()))?.dev();
    if da != db {
        bail!("{} 与 {} 不在同一个卷上，无法 clone", a.display(), b.display());
    }
    Ok(())
}

/// 递归克隆整个目录树；`dst` 必须不存在。
pub fn clone_tree(src: &Path, dst: &Path) -> Result<()> {
    let (s, d) = (cpath(src)?, cpath(dst)?);
    if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("clonefile {} -> {}", src.display(), dst.display()));
    }
    Ok(())
}

/// 把 `dst` 的 mtime 设为 `src_meta` 的 mtime（纳秒精度），不跟随符号链接，atime 不变。
pub fn copy_mtime(src_meta: &std::fs::Metadata, dst: &Path) -> Result<()> {
    let d = cpath(dst)?;
    let times = [
        libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
        libc::timespec { tv_sec: src_meta.mtime(), tv_nsec: src_meta.mtime_nsec() },
    ];
    if unsafe { libc::utimensat(libc::AT_FDCWD, d.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("设置 mtime {}", dst.display()));
    }
    Ok(())
}

#[repr(C)]
struct AttributeSet {
    common: u32,
    vol: u32,
    dir: u32,
    file: u32,
    fork: u32,
}

#[repr(C)]
struct PrivateSizeReply {
    length: u32,
    returned: AttributeSet,
    private_size: libc::off_t,
}

/// 文件中不与任何 clone 共享的字节数（ATTR_CMNEXT_PRIVATESIZE）。
pub fn private_size(p: &Path) -> Result<u64> {
    let c = cpath(p)?;
    let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
    al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    al.commonattr = libc::ATTR_CMN_RETURNED_ATTRS;
    al.forkattr = libc::ATTR_CMNEXT_PRIVATESIZE;
    let mut reply: PrivateSizeReply = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::getattrlist(
            c.as_ptr(),
            (&raw mut al).cast(),
            (&raw mut reply).cast(),
            size_of::<PrivateSizeReply>(),
            libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("getattrlist {}", p.display()));
    }
    if reply.returned.fork & libc::ATTR_CMNEXT_PRIVATESIZE == 0 {
        bail!("{} 未返回 PRIVATESIZE", p.display());
    }
    Ok(u64::try_from(reply.private_size)?)
}
