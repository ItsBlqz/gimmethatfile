use std::fs::File;
use std::io;

/// Positional write: many connections write into the same file at different
/// offsets without sharing a cursor.
pub fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        while !buf.is_empty() {
            let n = file.seek_write(buf, offset)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            buf = &buf[n..];
            offset += n as u64;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        let _ = &mut buf;
        let _ = &mut offset;
        file.write_all_at(buf, offset)
    }
}

/// On NTFS, writing far into a freshly sized file makes Windows zero-fill
/// everything before it, which stalls large downloads. Marking the file sparse
/// avoids that. A no-op elsewhere; failures are ignored.
pub fn set_sparse(file: &File, sparse: bool) {
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Ioctl::{FILE_SET_SPARSE_BUFFER, FSCTL_SET_SPARSE};
        use windows_sys::Win32::System::IO::DeviceIoControl;

        let input = FILE_SET_SPARSE_BUFFER { SetSparse: sparse };
        let mut returned = 0u32;
        DeviceIoControl(
            file.as_raw_handle() as _,
            FSCTL_SET_SPARSE,
            &input as *const _ as *const _,
            std::mem::size_of::<FILE_SET_SPARSE_BUFFER>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        );
    }
    #[cfg(not(windows))]
    {
        let _ = (file, sparse);
    }
}
