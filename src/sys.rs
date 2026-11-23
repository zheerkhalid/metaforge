//! Small Windows helpers: the console for command-line use, opening things
//! with their default program, showing a file in Explorer, and the
//! single-instance guard.

use std::path::Path;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE, HANDLE, HWND,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_HANDLE,
    STD_OUTPUT_HANDLE,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// NUL-terminated UTF-16 copy of a string.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn std_handle_usable(which: STD_HANDLE) -> bool {
    match GetStdHandle(which) {
        Ok(h) => !h.0.is_null() && !h.is_invalid(),
        Err(_) => false,
    }
}

/// The release build is a windowed program without a console. When it is
/// started from a terminal for a command (`--show`, `--clean`, `--selftest`),
/// this connects its output to that terminal. Redirected output is left alone.
pub fn attach_parent_console() {
    // SAFETY: console API calls with constant arguments; a handle opened here
    // is handed over to the process's standard handle table.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if std_handle_usable(which) {
                continue;
            }
            if let Ok(console) = CreateFileW(
                w!("CONOUT$"),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                HANDLE::default(),
            ) {
                let _ = SetStdHandle(which, console);
            }
        }
    }
}

fn shell_open(file: &str, params: &str) -> Result<(), String> {
    let f = wide(file);
    let p = wide(params);
    // SAFETY: NUL-terminated strings that outlive the call.
    let r = unsafe {
        ShellExecuteW(
            HWND::default(),
            w!("open"),
            PCWSTR(f.as_ptr()),
            if params.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(p.as_ptr())
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r.0 as usize > 32 {
        Ok(())
    } else {
        Err(format!("Windows could not open \"{file}\"."))
    }
}

/// Opens a file, folder or web address with its default program. Web
/// addresses are only ever opened when the user clicks something.
pub fn open(target: &str) -> Result<(), String> {
    shell_open(target, "")
}

/// Shows a file selected in an Explorer window.
pub fn show_in_explorer(path: &Path) -> Result<(), String> {
    shell_open("explorer.exe", &format!("/select,\"{}\"", path.display()))
}

/// Holds the named mutex that marks the running window.
pub struct InstanceGuard(HANDLE);

// SAFETY: a mutex handle may be closed from any thread.
unsafe impl Send for InstanceGuard {}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: the handle was created by `single_instance` and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Claims a named mutex. `None` means another MetaForge window holds it.
pub fn single_instance(name: &str) -> Option<InstanceGuard> {
    let name = wide(name);
    // SAFETY: creating a named mutex; the name outlives the call.
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return None;
        }
        Some(InstanceGuard(handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_instance_guard() {
        let name = format!("MetaForge.Test.{}", std::process::id());
        let first = single_instance(&name);
        assert!(first.is_some());
        assert!(single_instance(&name).is_none());
        drop(first);
        assert!(single_instance(&name).is_some());
    }
}
