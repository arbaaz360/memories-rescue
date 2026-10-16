//! FR-51: can this Windows machine play HEVC?
//!
//! Windows decodes HEVC only through a Media Foundation decoder MFT, which
//! comes either from Microsoft's "HEVC Video Extensions" Store package (paid
//! `Microsoft.HEVCVideoExtension`, or the free OEM `Microsoft.HEVCVideoExtensions`)
//! or, rarely, from a vendor driver that registers its own MFT. Two checks,
//! either of which is enough:
//!
//! 1. **Package**: a subkey starting with `Microsoft.HEVCVideoExtension`
//!    under `HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\
//!    CurrentVersion\AppModel\Repository\Packages`, which lists the packages
//!    registered for the current user (what `Get-AppxPackage` reads).
//! 2. **Decoder MFT**: `MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, …)` with input
//!    type `MFVideoFormat_HEVC`, the same enumeration the Photos and Films &
//!    TV apps rely on. `mfplat.dll` is loaded at run time with
//!    `LoadLibraryExW(…, LOAD_LIBRARY_SEARCH_SYSTEM32)`, never linked: on
//!    Windows N/KN without the Media Feature Pack the DLL is absent, and a
//!    link-time import would stop the whole app from starting.
//!
//! On macOS (High Sierra and later) HEVC playback is part of the system, so
//! the answer is always yes; elsewhere it is reported as available because
//! FR-51 is a Windows notice.

use serde::{Deserialize, Serialize};

/// The detail behind [`hevc_playback_available`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HevcPlayback {
    pub available: bool,
    /// Full name of the HEVC Video Extensions package, when registered.
    pub package: Option<String>,
    /// Number of HEVC decoder MFTs `MFTEnumEx` returned; `None` when
    /// Media Foundation could not be loaded or the call failed.
    pub decoder_mfts: Option<u32>,
}

/// FR-51: `false` means the "HEVC playback" notice should be shown when the
/// export contains HEVC videos.
pub fn hevc_playback_available() -> bool {
    hevc_playback().available
}

#[cfg(not(windows))]
pub fn hevc_playback() -> HevcPlayback {
    HevcPlayback {
        available: true,
        package: None,
        decoder_mfts: None,
    }
}

#[cfg(windows)]
pub fn hevc_playback() -> HevcPlayback {
    let package = win::hevc_package();
    let decoder_mfts = win::hevc_decoder_mfts();
    HevcPlayback {
        available: package.is_some() || decoder_mfts.unwrap_or(0) > 0,
        package,
        decoder_mfts,
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, FreeLibrary};
    use windows_sys::Win32::System::Com::{
        COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW,
    };
    use windows_sys::core::GUID;

    const PACKAGES: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";
    const PACKAGE_PREFIX: &str = "Microsoft.HEVCVideoExtension";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn hevc_package() -> Option<String> {
        let path = wide(PACKAGES);
        let mut key: HKEY = null_mut();
        // SAFETY: valid NUL-terminated path, out-pointer to a local.
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) }
            != ERROR_SUCCESS
        {
            return None;
        }
        let mut found = None;
        let mut buf = [0u16; 512];
        for index in 0.. {
            let mut len = buf.len() as u32;
            // SAFETY: buffer and length describe `buf`; the other outputs are optional.
            let rc = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    buf.as_mut_ptr(),
                    &mut len,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                )
            };
            if rc != ERROR_SUCCESS {
                break;
            }
            let name = String::from_utf16_lossy(&buf[..len as usize]);
            if name.starts_with(PACKAGE_PREFIX) {
                found = Some(name);
                break;
            }
        }
        // SAFETY: `key` was opened above.
        unsafe { RegCloseKey(key) };
        found
    }

    #[repr(C)]
    struct RegisterTypeInfo {
        major: GUID,
        sub: GUID,
    }

    #[repr(C)]
    struct IUnknownVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }

    type MftEnumEx = unsafe extern "system" fn(
        GUID,
        u32,
        *const RegisterTypeInfo,
        *const RegisterTypeInfo,
        *mut *mut *mut c_void,
        *mut u32,
    ) -> i32;

    const MFT_CATEGORY_VIDEO_DECODER: GUID =
        GUID::from_u128(0xd6c02d4b_6833_45b4_971a_05a4b04bab91);
    const MF_MEDIA_TYPE_VIDEO: GUID = GUID::from_u128(0x73646976_0000_0010_8000_00aa00389b71);
    const MF_VIDEO_FORMAT_HEVC: GUID = GUID::from_u128(0x43564548_0000_0010_8000_00aa00389b71);
    /// SYNCMFT | ASYNCMFT | HARDWARE | LOCALMFT | SORTANDFILTER.
    const MFT_ENUM_FLAGS: u32 = 0x01 | 0x02 | 0x04 | 0x10 | 0x40;

    pub(super) fn hevc_decoder_mfts() -> Option<u32> {
        let dll = wide("mfplat.dll");
        // SAFETY: valid NUL-terminated name; System32 only, so no DLL planting.
        let lib = unsafe { LoadLibraryExW(dll.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if lib.is_null() {
            return None;
        }
        // SAFETY: `lib` is a loaded module and the name is NUL-terminated.
        let proc = unsafe { GetProcAddress(lib, c"MFTEnumEx".as_ptr().cast()) };
        let result = proc.and_then(|p| {
            // SAFETY: MFTEnumEx has exactly this signature (mfapi.h).
            let mft_enum_ex: MftEnumEx = unsafe { std::mem::transmute(p) };
            // SAFETY: plain COM initialisation for this thread; undone below if it succeeded.
            let hr = unsafe { CoInitializeEx(null(), COINIT_MULTITHREADED as u32) };
            let input = RegisterTypeInfo {
                major: MF_MEDIA_TYPE_VIDEO,
                sub: MF_VIDEO_FORMAT_HEVC,
            };
            let mut activates: *mut *mut c_void = null_mut();
            let mut count = 0u32;
            // SAFETY: arguments follow the documented contract; outputs are locals.
            let rc = unsafe {
                mft_enum_ex(
                    MFT_CATEGORY_VIDEO_DECODER,
                    MFT_ENUM_FLAGS,
                    &input,
                    null(),
                    &mut activates,
                    &mut count,
                )
            };
            let out = if rc >= 0 {
                if !activates.is_null() {
                    for i in 0..count as usize {
                        // SAFETY: MFTEnumEx returned `count` IMFActivate pointers,
                        // each owning one reference we must release, then the array.
                        unsafe {
                            let obj = *activates.add(i);
                            if !obj.is_null() {
                                let vtbl = *(obj as *mut *const IUnknownVtbl);
                                ((*vtbl).release)(obj);
                            }
                        }
                    }
                    // SAFETY: the array was allocated with CoTaskMemAlloc by MFTEnumEx.
                    unsafe { CoTaskMemFree(activates as *const c_void) };
                }
                Some(count)
            } else {
                None
            };
            if hr >= 0 {
                // SAFETY: balances the successful CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
            out
        });
        // SAFETY: `lib` came from LoadLibraryExW.
        unsafe { FreeLibrary(lib) };
        result
    }
}
