//! Windows helpers shared by the hard scan and the duplicate-file reader (docs/native.md).

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_BASIC_INFORMATION, FileBasicInformation, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

/// What is done through `h` no longer updates the file's or directory's last access time
/// (SetFileTime's -1; FILE_BASIC_INFORMATION's zero fields change nothing). It writes no USN
/// record and leaves ChangeTime as it was. Needs FILE_WRITE_ATTRIBUTES on the handle; best effort
/// (returns whether the call succeeded).
pub fn keep_access_time(h: HANDLE) -> bool {
    let info = FILE_BASIC_INFORMATION {
        CreationTime: 0,
        LastAccessTime: -1,
        LastWriteTime: 0,
        ChangeTime: 0,
        FileAttributes: 0,
    };
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: `info` is a valid FILE_BASIC_INFORMATION for the call.
    let status = unsafe {
        NtSetInformationFile(
            h,
            &mut iosb,
            (&raw const info).cast(),
            size_of::<FILE_BASIC_INFORMATION>() as u32,
            FileBasicInformation,
        )
    };
    status >= 0
}
