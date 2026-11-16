//! Private synchronous ABI used only by the bundled Worker adapter.
//!
//! One instance owns one context. `call` consumes an allocation from `alloc`; the result slice
//! remains valid until the next call. No host filesystem, thread, network or clock imports exist.

use crate::{BrowserBuildContext, BrowserBuildError, BrowserBuildOptions};
use serde::Deserialize;
use std::sync::Mutex;

static CONTEXT: Mutex<Option<BrowserBuildContext>> = Mutex::new(None);
static RESULT: Mutex<Vec<u8>> = Mutex::new(Vec::new());

#[unsafe(no_mangle)]
pub extern "C" fn wake_browser_abi_version() -> u32 {
    1
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    Initialize {
        options: BrowserBuildOptions,
    },
    Build {
        files: std::collections::BTreeMap<String, Vec<u8>>,
    },
    Dispose,
}

#[unsafe(no_mangle)]
pub extern "C" fn wake_browser_alloc(length: u32) -> *mut u8 {
    if length as usize > 512 * 1024 * 1024 {
        return std::ptr::null_mut();
    }
    Box::into_raw(vec![0u8; length as usize].into_boxed_slice()).cast::<u8>()
}

/// # Safety
/// `pointer` and `length` must identify the exact live allocation returned by `wake_browser_alloc`.
/// This call consumes that allocation. The Worker never calls this ABI concurrently or reentrantly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wake_browser_call(pointer: *mut u8, length: u32) -> *const u8 {
    let bytes =
        unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(pointer, length as usize)) };
    let result = execute(&bytes);
    let response = match result {
        Ok(value) => serde_json::json!({"ok":true,"value":value}),
        Err(error) => serde_json::json!({"ok":false,"error":error}),
    };
    let mut output = RESULT.lock().expect("single-thread result");
    *output = serde_json::to_vec(&response).expect("serializable browser result");
    output.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn wake_browser_result_len() -> u32 {
    RESULT.lock().expect("single-thread result").len() as u32
}

fn execute(bytes: &[u8]) -> Result<serde_json::Value, BrowserBuildError> {
    let request: Request = serde_json::from_slice(bytes)
        .map_err(|error| BrowserBuildError::new("WAKE_CONFIG", error.to_string()))?;
    let mut context = CONTEXT.lock().expect("single-thread context");
    match request {
        Request::Initialize { options } => {
            *context = Some(BrowserBuildContext::new(options)?);
            Ok(serde_json::Value::Null)
        }
        Request::Build { files } => {
            let context = context.as_mut().ok_or_else(|| {
                BrowserBuildError::new("WAKE_CONFIG", "browser context is not initialized")
            })?;
            serde_json::to_value(context.build(files)?)
                .map_err(|error| BrowserBuildError::new("WAKE_INTERNAL", error.to_string()))
        }
        Request::Dispose => {
            *context = None;
            Ok(serde_json::Value::Null)
        }
    }
}
