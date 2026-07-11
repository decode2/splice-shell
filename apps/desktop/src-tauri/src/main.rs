// Release builds are GUI-subsystem apps: without this, Windows also gives the
// process a console, which both flashes a stray console window behind the GUI
// and -- because a process can be attached to at most one console -- makes the
// `AttachConsole` in `splice-pty`'s Ctrl+C path fail with ERROR_ACCESS_DENIED.
// Debug builds keep the console on purpose, so `log`/`eprintln!` diagnostics
// stay visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    splice_shell_desktop_lib::run();
}
