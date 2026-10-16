//! Operating-system calls: file timestamps (FR-20), sleep prevention
//! (FR-36), directory fsync. The only module allowed `unsafe`.

#![allow(unsafe_code)]

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use filetime_creation::FileTime;

/// Set access, modification and (where the platform allows) creation time.
pub fn set_file_times(path: &Path, t: SystemTime) -> io::Result<()> {
    let ft = FileTime::from_system_time(t);
    // Windows: SetFileTime sets creation too. Elsewhere ctime is ignored.
    filetime_creation::set_file_times(path, ft, ft, ft)?;
    #[cfg(target_os = "macos")]
    set_birthtime(path, t)?;
    Ok(())
}

/// macOS: `setattrlist(ATTR_CMN_CRTIME)`.
#[cfg(target_os = "macos")]
fn set_birthtime(path: &Path, t: SystemTime) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let since = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| (d.as_secs() as i64, d.subsec_nanos() as i64))
        .unwrap_or((0, 0));
    let mut spec = libc::timespec {
        tv_sec: since.0 as libc::time_t,
        tv_nsec: since.1 as libc::c_long,
    };
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_CRTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: valid C string, a correctly sized attrlist and timespec buffer.
    let rc = unsafe {
        libc::setattrlist(
            c_path.as_ptr(),
            (&mut attrs as *mut libc::attrlist).cast(),
            (&mut spec as *mut libc::timespec).cast(),
            std::mem::size_of::<libc::timespec>(),
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Peak resident memory of this process so far, in bytes (NFR-PERF-02):
/// the peak working set on Windows, `ru_maxrss` on macOS. For logs and
/// benchmarks; `None` where unknown.
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: a zeroed counters struct of the size passed; the pseudo
        // handle of the current process needs no closing.
        unsafe {
            let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            c.cb = size;
            if GetProcessMemoryInfo(GetCurrentProcess(), &mut c, size) != 0 {
                return Some(c.PeakWorkingSetSize as u64);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: getrusage fills the zeroed struct it is given.
        unsafe {
            let mut u: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_SELF, &mut u) == 0 {
                return Some(u.ru_maxrss as u64); // bytes on macOS
            }
        }
        None
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Flush a directory entry to disk after a rename (no-op on Windows, where
/// directories cannot be opened for sync).
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    if cfg!(windows) {
        return Ok(());
    }
    File::open(dir)?.sync_all()
}

/// Keeps the system awake while alive (FR-36). Display sleep stays allowed.
/// Create and drop it on the same thread.
pub struct KeepAwake {
    active: bool,
    #[cfg(target_os = "macos")]
    assertion: u32,
}

impl KeepAwake {
    pub fn new(reason: &str) -> KeepAwake {
        let _ = reason;
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::{
                ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
            };
            // SAFETY: plain Win32 call with flag constants.
            let prev = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
            KeepAwake { active: prev != 0 }
        }
        #[cfg(target_os = "macos")]
        {
            match mac::prevent_idle_sleep(reason) {
                Some(id) => KeepAwake {
                    active: true,
                    assertion: id,
                },
                None => KeepAwake {
                    active: false,
                    assertion: 0,
                },
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            KeepAwake { active: false }
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Power::{ES_CONTINUOUS, SetThreadExecutionState};
            // SAFETY: restores the default execution state.
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
        #[cfg(target_os = "macos")]
        mac::release(self.assertion);
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CString, c_char, c_void};

    type CFStringRef = *const c_void;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    fn cfstring(s: &str) -> Option<CFStringRef> {
        let c = CString::new(s).ok()?;
        // SAFETY: valid NUL-terminated UTF-8; the result is released by the caller.
        let r = unsafe {
            CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        };
        (!r.is_null()).then_some(r)
    }

    pub fn prevent_idle_sleep(reason: &str) -> Option<u32> {
        let kind = cfstring("PreventUserIdleSystemSleep")?;
        let name = cfstring(reason)?;
        let mut id = 0u32;
        // SAFETY: both CFStrings are valid; `id` receives the assertion.
        let rc =
            unsafe { IOPMAssertionCreateWithName(kind, K_IOPM_ASSERTION_LEVEL_ON, name, &mut id) };
        // SAFETY: we own both strings.
        unsafe {
            CFRelease(kind);
            CFRelease(name);
        }
        (rc == 0).then_some(id)
    }

    pub fn release(id: u32) {
        // SAFETY: `id` came from IOPMAssertionCreateWithName.
        unsafe {
            IOPMAssertionRelease(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn file_times_are_set_to_the_capture_instant() {
        let dir = std::env::temp_dir().join(format!("mr-job-platform-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.bin");
        std::fs::write(&p, b"x").unwrap();
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_623_767_400);
        set_file_times(&p, t).unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        assert_eq!(meta.modified().unwrap(), t);
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(meta.created().unwrap(), t, "creation time (FR-20)");
        }
    }

    #[test]
    fn keep_awake_is_released_on_drop() {
        let guard = KeepAwake::new("Memories Rescue test");
        if cfg!(any(windows, target_os = "macos")) {
            assert!(guard.active());
        }
        drop(guard);
    }
}
