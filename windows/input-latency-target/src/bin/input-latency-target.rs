//! A window that flashes on input and times how long that took.
//!
//! The work lives in the library so that the argument parsing, the flash state
//! machine and the report can be compiled and tested off Windows while the
//! window itself is compiled away; a binary cannot make that distinction for
//! itself, because a crate-level `cfg` would leave it with no `main`.

fn main() -> std::process::ExitCode {
    lanplay_input_latency_target::run::main()
}
