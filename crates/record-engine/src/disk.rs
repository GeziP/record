//! 磁盘剩余空间（设计文档 6.5 自动保护、9 磁盘写满）。

use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::PCWSTR;

/// `path` 所在卷对当前用户可用的字节数。取不到时返回 `None`，调用方不能当成“空间充足”也不能当成“空间不足”。
pub fn free_bytes(path: &Path) -> Option<u64> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temp_directory_has_free_space() {
        let free = free_bytes(&std::env::temp_dir()).expect("读取剩余空间");
        assert!(free > 1024 * 1024, "只有 {free} 字节？");
    }

    #[test]
    fn a_path_that_does_not_exist_gives_none() {
        assert_eq!(free_bytes(Path::new("Z:\\这个目录不存在\\x")), None);
    }
}
