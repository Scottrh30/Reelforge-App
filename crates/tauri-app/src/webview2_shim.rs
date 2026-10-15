//! The WebView2 loader, carried inside the exe.
//!
//! With the GNU toolchain WebView2 is reached through Microsoft's small
//! `WebView2Loader.dll`, normally shipped beside the exe. Instead, these
//! functions stand in for the DLL's five exports: on first use the DLL is
//! written to the user's app-data folder and loaded from there, and each call
//! is passed on to it. So the download stays a single exe.

use std::ffi::c_void;
use std::sync::OnceLock;

type Hresult = i32;
const E_FAIL: Hresult = 0x8000_4005_u32 as i32;

static DLL: &[u8] = include_bytes!("WebView2Loader.dll");

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

fn module() -> *mut c_void {
    static MODULE: OnceLock<usize> = OnceLock::new();
    *MODULE.get_or_init(|| {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in DLL {
            hash = (hash ^ *b as u64).wrapping_mul(0x100000001b3);
        }
        let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join("Reelforge");
        let path = dir.join(format!("WebView2Loader-{hash:016x}.dll"));
        if std::fs::metadata(&path).map(|m| m.len() as usize != DLL.len()).unwrap_or(true) {
            let _ = std::fs::create_dir_all(&dir);
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, DLL).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        unsafe { LoadLibraryW(wide.as_ptr()) as usize }
    }) as *mut c_void
}

fn proc(name: &[u8]) -> Option<*mut c_void> {
    let m = module();
    if m.is_null() {
        return None;
    }
    let p = unsafe { GetProcAddress(m, name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

macro_rules! forward {
    ($name:ident, $imp:ident, ($($a:ident : $t:ty),*)) => {
        #[no_mangle]
        pub unsafe extern "system" fn $name($($a: $t),*) -> Hresult {
            match proc(concat!(stringify!($name), "\0").as_bytes()) {
                Some(p) => {
                    let f: unsafe extern "system" fn($($t),*) -> Hresult = std::mem::transmute(p);
                    f($($a),*)
                }
                None => E_FAIL,
            }
        }
        /// The import-table slot the linker would otherwise fill from the DLL.
        #[no_mangle]
        #[used]
        pub static $imp: unsafe extern "system" fn($($t),*) -> Hresult = $name;
    };
}

forward!(CompareBrowserVersions, __imp_CompareBrowserVersions, (a: *const u16, b: *const u16, r: *mut i32));
forward!(CreateCoreWebView2Environment, __imp_CreateCoreWebView2Environment, (h: *mut c_void));
forward!(CreateCoreWebView2EnvironmentWithOptions, __imp_CreateCoreWebView2EnvironmentWithOptions, (folder: *const u16, data: *const u16, opts: *mut c_void, h: *mut c_void));
forward!(GetAvailableCoreWebView2BrowserVersionString, __imp_GetAvailableCoreWebView2BrowserVersionString, (folder: *const u16, v: *mut *mut u16));
forward!(GetAvailableCoreWebView2BrowserVersionStringWithOptions, __imp_GetAvailableCoreWebView2BrowserVersionStringWithOptions, (folder: *const u16, opts: *mut c_void, v: *mut *mut u16));

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: *mut c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(p: *mut c_void);
}

/// The installed WebView2 runtime's version, or `None` if there isn't one.
pub fn runtime_version() -> Option<String> {
    let mut out: *mut u16 = std::ptr::null_mut();
    let hr = unsafe { GetAvailableCoreWebView2BrowserVersionString(std::ptr::null(), &mut out) };
    if hr < 0 || out.is_null() {
        return None;
    }
    let mut n = 0;
    while unsafe { *out.add(n) } != 0 {
        n += 1;
    }
    let v = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, n) });
    unsafe { CoTaskMemFree(out as *mut c_void) };
    Some(v)
}

/// Tell the user something went wrong before the window could open.
pub fn message(text: &str) {
    let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    unsafe { MessageBoxW(std::ptr::null_mut(), w(text).as_ptr(), w("Reelforge").as_ptr(), 0x10) };
}
