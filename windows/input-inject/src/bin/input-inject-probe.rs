//! Receives input datagrams and injects them.
//!
//! The work lives in the library so the receive-and-decode half can be run and
//! tested off Windows, where the injecting half does not compile; a binary
//! cannot make that distinction for itself, because a crate-level `cfg` would
//! leave it with no `main`.

fn main() -> std::process::ExitCode {
    lanplay_input_inject::probe::main()
}
