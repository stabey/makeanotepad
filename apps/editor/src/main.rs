#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::process::ExitCode;
use std::time::Instant;

use editor::cli::{CliArgs, help_text};
use editor::run_editor_app;

#[cfg(target_os = "windows")]
mod win_console {
    use std::ffi::c_void;

    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
    const STD_ERROR_HANDLE: u32 = (-12i32) as u32;
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    const FILE_TYPE_UNKNOWN: u32 = 0x0000;
    const INVALID_HANDLE_VALUE: *mut c_void = (-1isize) as *mut c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> *mut c_void;
        fn SetStdHandle(n_std_handle: u32, h_handle: *mut c_void) -> i32;
        fn GetFileType(h_file: *mut c_void) -> u32;
        fn AttachConsole(dw_process_id: u32) -> i32;
    }

    fn is_valid_redirected_or_inherited_handle(h: *mut c_void) -> bool {
        !h.is_null() && h != INVALID_HANDLE_VALUE && unsafe { GetFileType(h) } != FILE_TYPE_UNKNOWN
    }

    pub fn attach_parent_console_preserving_std_handles() {
        let snapshot = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|id| {
            let h = unsafe { GetStdHandle(id) };
            (id, h, is_valid_redirected_or_inherited_handle(h))
        });
        if snapshot[1].2 && snapshot[2].2 {
            return;
        }
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0 {
            for (id, handle, was_valid) in snapshot {
                if was_valid {
                    unsafe {
                        let _ = SetStdHandle(id, handle);
                    }
                }
            }
        }
    }
}

fn main() -> ExitCode {
    #[cfg(target_os = "windows")]
    win_console::attach_parent_console_preserving_std_handles();

    let process_start = Instant::now();
    let cli = match CliArgs::parse_env() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("error: {err}\n\n{}", help_text());
            return ExitCode::from(2);
        }
    };

    if cli.show_help {
        print!("{}", help_text());
        return ExitCode::SUCCESS;
    }

    if cli.show_version {
        println!("editor {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    match run_editor_app(cli, process_start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("editor error: {err}");
            ExitCode::FAILURE
        }
    }
}
