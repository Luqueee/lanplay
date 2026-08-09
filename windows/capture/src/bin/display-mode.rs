//! Lists and sets display modes from the interactive session.
//!
//! The work lives in the library so the module can be compiled away off
//! Windows the same way every other Windows-only module in this crate is; a
//! binary cannot do that for itself, because a crate-level `cfg` leaves it
//! with no `main`.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    lanplay_capture::display_mode::main()
}

/// There are no display modes to enumerate without a Windows desktop, and
/// pretending otherwise would put a plausible-looking mode list in front of
/// whoever ran this by mistake.
#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("display-mode: needs Windows; there is no display device here.");
    std::process::ExitCode::from(3)
}
