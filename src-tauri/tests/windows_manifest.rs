//! Smoke test that proves Cargo test executables receive the Common Controls
//! v6 activation context. Without the manifest, loading this executable fails
//! before `main` with STATUS_ENTRYPOINT_NOT_FOUND because `TaskDialogIndirect`
//! is not exported by the legacy Common Controls assembly.

#[cfg(windows)]
#[link(name = "comctl32")]
unsafe extern "system" {
    fn TaskDialogIndirect(
        config: *const core::ffi::c_void,
        button: *mut i32,
        radio_button: *mut i32,
        verification_checked: *mut i32,
    ) -> i32;
}

#[test]
fn windows_test_executable_loads_common_controls_v6() {
    #[cfg(windows)]
    unsafe {
        // A null configuration is intentional: the call must return an
        // argument error, but reaching it proves the imported entry point was
        // resolved by the loader under the v6 activation context.
        let _ = TaskDialogIndirect(
            core::ptr::null(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
    }
}
