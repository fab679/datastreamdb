//! Stable C ABI: instance handles for any language.
//!
//! Every other language binds through these symbols (see `docs/EMBEDDING.md`).
//! Status codes are `i32`: `0` ok, `1` invalid argument, `2` bad config,
//! `3` engine error, `4` internal panic (caught). [`datastreamdb_last_error`]
//! describes the most recent failure on the calling thread.
//!
//! ```c
//! // datastreamdb.h (checked in next to this file; xtask codegen verifies it)
//! typedef struct datastreamdb_handle datastreamdb_handle_t;
//! int32_t datastreamdb_open(const char *config_json, datastreamdb_handle_t **out);
//! int32_t datastreamdb_execute(const datastreamdb_handle_t *h, const uint8_t *req,
//!                        size_t req_len, uint8_t **out, size_t *out_len);
//! int32_t datastreamdb_close(datastreamdb_handle_t *h);
//! void datastreamdb_free_bytes(uint8_t *ptr, size_t len);
//! const char *datastreamdb_last_error(void);
//! const char *datastreamdb_version(void);
//! ```

#![warn(missing_docs)]

use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char, c_int},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, slice,
    sync::Arc,
};

/// Success.
pub const DATASTREAMDB_OK: c_int = 0;
/// Null pointer, invalid UTF-8, or malformed envelope.
pub const DATASTREAMDB_EINVAL: c_int = 1;
/// Config JSON rejected.
pub const DATASTREAMDB_ECONFIG: c_int = 2;
/// Engine failure (storage, backend).
pub const DATASTREAMDB_EENGINE: c_int = 3;
/// Caught a Rust panic; instance state is unchanged.
pub const DATASTREAMDB_EPANIC: c_int = 4;

/// Opaque handle owning one [`datastreamdb_core::Instance`].
pub struct DataStreamDBHandle {
    inner: Arc<datastreamdb_core::Instance>,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: impl Into<String>) {
    let text = message.into();
    LAST_ERROR.with(|slot| *slot.borrow_mut() = CString::new(text).ok());
}

/// Runs `op`, converting panics into [`DATASTREAMDB_EPANIC`].
fn guard(op: impl FnOnce() -> c_int) -> c_int {
    catch_unwind(AssertUnwindSafe(op)).unwrap_or_else(|_| {
        set_last_error("internal panic (caught at FFI boundary)");
        DATASTREAMDB_EPANIC
    })
}

/// Reads a NUL-terminated string argument.
fn read_c_str<'a>(ptr: *const c_char, what: &str) -> Result<&'a str, c_int> {
    if ptr.is_null() {
        set_last_error(format!("{what}: null pointer"));
        return Err(DATASTREAMDB_EINVAL);
    }
    // SAFETY: non-null per check above; caller guarantees a valid C string.
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str().map_err(|_| {
        set_last_error(format!("{what}: invalid UTF-8"));
        DATASTREAMDB_EINVAL
    })
}

fn open_inner(config_json: *const c_char, out: *mut *mut DataStreamDBHandle) -> c_int {
    if out.is_null() {
        set_last_error("datastreamdb_open: null out pointer");
        return DATASTREAMDB_EINVAL;
    }
    let Ok(json) = read_c_str(config_json, "datastreamdb_open config") else {
        return DATASTREAMDB_EINVAL;
    };
    let Ok(config) = datastreamdb_core::InstanceConfig::from_json(json) else {
        set_last_error("datastreamdb_open: invalid config JSON");
        return DATASTREAMDB_ECONFIG;
    };
    match datastreamdb_core::Instance::open(config) {
        Ok(instance) => {
            // SAFETY: `out` is non-null; caller provides a writable pointer slot.
            unsafe {
                *out = Box::into_raw(Box::new(DataStreamDBHandle { inner: instance }));
            }
            DATASTREAMDB_OK
        }
        Err(err) => {
            set_last_error(format!("datastreamdb_open: {err}"));
            DATASTREAMDB_EENGINE
        }
    }
}

/// Opens an instance from JSON config (see [`datastreamdb_core::InstanceConfig`]).
///
/// # Safety
///
/// `config_json` must be a valid NUL-terminated string and `out` a writable
/// pointer slot. On success the caller owns `*out` and must pass it to
/// [`datastreamdb_close`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn datastreamdb_open(
    config_json: *const c_char,
    out: *mut *mut DataStreamDBHandle,
) -> c_int {
    guard(|| open_inner(config_json, out))
}

fn execute_inner(
    handle: *const DataStreamDBHandle,
    req: *const u8,
    req_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    if handle.is_null() || req.is_null() || out_ptr.is_null() || out_len.is_null() {
        set_last_error("datastreamdb_execute: null pointer");
        return DATASTREAMDB_EINVAL;
    }
    // SAFETY: non-null per check; caller guarantees `req_len` readable bytes.
    let bytes = unsafe { slice::from_raw_parts(req, req_len) };
    let Ok(request) = std::str::from_utf8(bytes) else {
        set_last_error("datastreamdb_execute: request is not UTF-8");
        return DATASTREAMDB_EINVAL;
    };
    // SAFETY: non-null per check; caller guarantees a live handle from `datastreamdb_open`.
    let instance = unsafe { &(*handle).inner };
    match instance.execute(request) {
        Ok(response) => {
            let boxed: Box<[u8]> = response.into_bytes().into_boxed_slice();
            // SAFETY: all out-pointers non-null; caller frees via `datastreamdb_free_bytes`.
            unsafe {
                *out_len = boxed.len();
                *out_ptr = Box::into_raw(boxed).cast::<u8>();
            }
            DATASTREAMDB_OK
        }
        Err(err) => {
            set_last_error(format!("datastreamdb_execute: {err}"));
            DATASTREAMDB_EINVAL
        }
    }
}

/// Executes a JSON request; response bytes go to `*out_ptr`/`*out_len`.
///
/// # Safety
///
/// `handle` must come from [`datastreamdb_open`]; `req` must span `req_len` readable
/// bytes; `out_ptr`/`out_len` must be writable. On success the caller owns the
/// response buffer and must free it with [`datastreamdb_free_bytes`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn datastreamdb_execute(
    handle: *const DataStreamDBHandle,
    req: *const u8,
    req_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
) -> c_int {
    guard(|| execute_inner(handle, req, req_len, out_ptr, out_len))
}

/// Closes a handle opened by [`datastreamdb_open`]. Closing the last handle flushes.
///
/// # Safety
///
/// `handle` must be a live handle from [`datastreamdb_open`] and must not be used after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn datastreamdb_close(handle: *mut DataStreamDBHandle) -> c_int {
    guard(|| {
        if handle.is_null() {
            set_last_error("datastreamdb_close: null pointer");
            return DATASTREAMDB_EINVAL;
        }
        // SAFETY: guaranteed live and unique by the caller contract.
        drop(unsafe { Box::from_raw(handle) });
        DATASTREAMDB_OK
    })
}

/// Frees a response buffer from [`datastreamdb_execute`].
///
/// # Safety
///
/// `ptr`/`len` must be exactly what [`datastreamdb_execute`] wrote.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn datastreamdb_free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: caller contract; capacity equals len via `into_boxed_slice`.
    drop(unsafe { Box::from_raw(ptr::slice_from_raw_parts_mut(ptr, len)) });
}

/// Describes the last failure on this thread (never null, thread-local).
#[unsafe(no_mangle)]
pub extern "C" fn datastreamdb_last_error() -> *const c_char {
    LAST_ERROR.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(c"no error".as_ptr(), |s| s.as_ptr())
    })
}

/// Crate version string (never null, static lifetime).
#[unsafe(no_mangle)]
pub extern "C" fn datastreamdb_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0")
        .as_ptr()
        .cast::<c_char>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_memory() -> *mut DataStreamDBHandle {
        let config = CString::new("{}").expect("config");
        let mut handle: *mut DataStreamDBHandle = ptr::null_mut();
        // SAFETY: test-owned valid pointers.
        let code = unsafe { datastreamdb_open(config.as_ptr(), &raw mut handle) };
        assert_eq!(code, DATASTREAMDB_OK);
        assert!(!handle.is_null());
        handle
    }

    fn last_error_text() -> String {
        // SAFETY: `datastreamdb_last_error` never returns null.
        unsafe { CStr::from_ptr(datastreamdb_last_error()) }
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn lifecycle_roundtrips() {
        let handle = open_memory();
        let req = br#"{"op":"put","key":"n","value":"Ada"}"#;
        let (mut out, mut len) = (ptr::null_mut(), 0usize);
        // SAFETY: test-owned valid pointers and request bytes.
        let code = unsafe {
            datastreamdb_execute(handle, req.as_ptr(), req.len(), &raw mut out, &raw mut len)
        };
        assert_eq!(code, DATASTREAMDB_OK);
        // SAFETY: exactly the buffer `datastreamdb_execute` wrote.
        unsafe { datastreamdb_free_bytes(out, len) };
        // SAFETY: live handle from `datastreamdb_open`.
        assert_eq!(unsafe { datastreamdb_close(handle) }, DATASTREAMDB_OK);
    }

    /// Executes `req` and returns (code, response text), freeing the buffer.
    fn exec_to_string(handle: *mut DataStreamDBHandle, req: &[u8]) -> (c_int, String) {
        let (mut out, mut len) = (ptr::null_mut(), 0usize);
        // SAFETY: test-owned live handle, request bytes, and out-slots.
        let code = unsafe {
            datastreamdb_execute(handle, req.as_ptr(), req.len(), &raw mut out, &raw mut len)
        };
        if code != DATASTREAMDB_OK {
            return (code, String::new());
        }
        // SAFETY: exactly the buffer `datastreamdb_execute` wrote.
        let text = unsafe { slice::from_raw_parts(out.cast_const(), len) };
        let text = String::from_utf8_lossy(text).into_owned();
        // SAFETY: exactly the buffer `datastreamdb_execute` wrote.
        unsafe {
            datastreamdb_free_bytes(out, len);
        }
        (code, text)
    }

    #[test]
    fn two_handles_are_isolated() {
        let h1 = open_memory();
        let h2 = open_memory();
        let (code, _) = exec_to_string(h1, br#"{"op":"put","key":"n","value":"Ada"}"#);
        assert_eq!(code, DATASTREAMDB_OK);
        let (_, r1) = exec_to_string(h1, br#"{"op":"get","key":"n"}"#);
        let (_, r2) = exec_to_string(h2, br#"{"op":"get","key":"n"}"#);
        assert!(r1.contains("Ada"), "{r1}");
        assert!(r2.contains("null"), "{r2}");
        // SAFETY: live handles from `datastreamdb_open`.
        assert_eq!(unsafe { datastreamdb_close(h1) }, DATASTREAMDB_OK);
        assert_eq!(unsafe { datastreamdb_close(h2) }, DATASTREAMDB_OK);
    }

    #[test]
    fn last_error_is_thread_local() {
        let handle = open_memory();
        let (code, _) = exec_to_string(handle, br#"{"op":"nope"}"#);
        assert_eq!(code, DATASTREAMDB_EINVAL);
        let main_err = last_error_text();
        let other = std::thread::spawn(|| {
            let bad = CString::new("{oops").expect("config");
            let mut other: *mut DataStreamDBHandle = ptr::null_mut();
            // SAFETY: test-owned valid pointers.
            let code = unsafe { datastreamdb_open(bad.as_ptr(), &raw mut other) };
            assert_eq!(code, DATASTREAMDB_ECONFIG);
            last_error_text()
        })
        .join()
        .expect("join");
        assert_ne!(main_err, other);
        assert_eq!(last_error_text(), main_err);
        // SAFETY: live handle from `datastreamdb_open`.
        assert_eq!(unsafe { datastreamdb_close(handle) }, DATASTREAMDB_OK);
    }

    #[test]
    fn nulls_and_bad_json_fail_cleanly() {
        let mut handle: *mut DataStreamDBHandle = ptr::null_mut();
        // SAFETY: intentionally null config to test the guard.
        assert_eq!(
            unsafe { datastreamdb_open(ptr::null(), &raw mut handle) },
            DATASTREAMDB_EINVAL
        );
        // SAFETY: intentionally null out-slot.
        let config = CString::new("{}").expect("config");
        assert_eq!(
            unsafe { datastreamdb_open(config.as_ptr(), ptr::null_mut()) },
            DATASTREAMDB_EINVAL
        );
        let bad = CString::new("{oops").expect("config");
        // SAFETY: test-owned valid pointers.
        assert_eq!(
            unsafe { datastreamdb_open(bad.as_ptr(), &raw mut handle) },
            DATASTREAMDB_ECONFIG
        );
        assert!(last_error_text().contains("datastreamdb_open"));

        let handle = open_memory();
        // SAFETY: live handle; null request.
        let (mut out, mut len) = (ptr::null_mut(), 0usize);
        assert_eq!(
            unsafe { datastreamdb_execute(handle, ptr::null(), 3, &raw mut out, &raw mut len) },
            DATASTREAMDB_EINVAL
        );
        let req = br#"{"op":"nope"}"#;
        // SAFETY: test-owned valid pointers and request bytes.
        assert_eq!(
            unsafe {
                datastreamdb_execute(handle, req.as_ptr(), req.len(), &raw mut out, &raw mut len)
            },
            DATASTREAMDB_EINVAL
        );
        // SAFETY: live handle from `datastreamdb_open`.
        assert_eq!(unsafe { datastreamdb_close(handle) }, DATASTREAMDB_OK);
        // SAFETY: intentionally null close.
        assert_eq!(
            unsafe { datastreamdb_close(ptr::null_mut()) },
            DATASTREAMDB_EINVAL
        );
    }

    #[test]
    fn version_is_nonempty() {
        // SAFETY: `datastreamdb_version` never returns null.
        let version = unsafe { CStr::from_ptr(datastreamdb_version()) }.to_string_lossy();
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
    }
}
