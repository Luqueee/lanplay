//! Moves the mouse on a schedule, so an input gate does not need a hand.
//!
//! A human moving the mouse gives an unrepeatable run and no ground truth: the
//! only totals available are the client's and the host's, and if they disagree
//! there is nothing to say which one is wrong. Posting the events instead
//! gives three numbers to compare, and the first of them is known exactly:
//!
//! ```text
//! posted    <- this program, exact by construction
//! captured  <- the Mac client's own reckoning
//! injected  <- the Windows host's
//! ```
//!
//! What this proves and what it does not. It exercises everything from the
//! window server's event delivery onward: the client's monitors, the rounding
//! residue, the wire format, the socket, and the host's injection. It does not
//! exercise the path from a physical mouse into the window server, because a
//! posted event does not travel it. A physical mouse remains the only way to
//! test that, and this exists so that everything after it can be tested
//! without one.
//!
//! Deltas are set explicitly rather than left to be inferred from successive
//! cursor positions. A client that captures relative movement reads the delta
//! fields, and a client whose cursor is detached - which is what capture does -
//! would otherwise see no movement at all.
//!
//! The absolute position carried alongside them is cosmetic, and is advanced
//! anyway so the cursor visibly moves. That matters for a reason that is not
//! cosmetic at all: a test nobody can see is a test nobody trusts, and the
//! first question asked of this program was why the pointer sat still. The
//! position is folded back inside a box in the middle of the screen when it
//! would leave it, which cannot corrupt anything, because the deltas are set
//! from the pattern and never derived from the position.
//!
//! Needs Accessibility permission, since posting events is exactly what that
//! permission governs. Without it the calls are silently dropped, which is why
//! the summary reports what the window server accepted rather than what was
//! attempted.
//!
//! `--capture-click` posts one left click before the motion begins. A client
//! that starts uncaptured, which is what the capture state machine requires,
//! sends nothing at all until something asks it to capture, and a motion arm
//! then measures a clean run of zero. Exercising the real path is better than a
//! flag that skips it, but it does mean a click lands wherever the cursor is,
//! so it is not the default.
//!
//! usage:
//!   mouse-mover [--seconds 30] [--hz 250] [--amplitude 6] [--pattern circle]
//!               [--capture-click]

#![cfg(target_os = "macos")]

use std::time::{Duration, Instant};

use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventTapLocation, CGEventType, CGMouseButton,
    CGWarpMouseCursorPosition,
};

/// How the deltas are shaped over time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pattern {
    /// Sum returns to zero every revolution, so a comparison of totals is a
    /// comparison against a known target rather than against a drift.
    Circle,
    /// Constant drift, which is what shows up a client that loses events: the
    /// total is proportional to how many arrived.
    Drift,
    /// Alternating single pixels, which is the smallest movement the rounding
    /// residue can mishandle.
    Jitter,
}

fn main() {
    let mut seconds = 30.0f64;
    let mut hz = 250.0f64;
    let mut amplitude = 6.0f64;
    let mut pattern = Pattern::Circle;
    let mut capture_click = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value =
            |args: &mut dyn Iterator<Item = String>| args.next().and_then(|v| v.parse().ok());
        match arg.as_str() {
            "--seconds" => seconds = value(&mut args).unwrap_or(seconds),
            "--hz" => hz = value(&mut args).unwrap_or(hz),
            "--amplitude" => amplitude = value(&mut args).unwrap_or(amplitude),
            "--capture-click" => capture_click = true,
            "--pattern" => {
                pattern = match args.next().as_deref() {
                    Some("drift") => Pattern::Drift,
                    Some("jitter") => Pattern::Jitter,
                    _ => Pattern::Circle,
                }
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    let period = Duration::from_secs_f64(1.0 / hz.max(1.0));
    let deadline = Duration::from_secs_f64(seconds.max(0.0));
    println!(
        "mouse-mover: {seconds:.0} s at {hz:.0} Hz, amplitude {amplitude:.1} px, \
         pattern {}",
        match pattern {
            Pattern::Circle => "circle",
            Pattern::Drift => "drift",
            Pattern::Jitter => "jitter",
        }
    );

    // A box in the middle of the screen. The position is folded back into it
    // rather than allowed to reach an edge, because a clamped position is the
    // one case where the window server could stop reporting movement, and
    // that is the case relative capture exists to survive rather than one
    // this program should accidentally test.
    let home = CGPoint { x: 400.0, y: 400.0 };
    let (low, high) = (200.0f64, 600.0f64);
    let mut at = home;
    CGWarpMouseCursorPosition(home);

    if capture_click {
        // Before any motion, because a client that is not capturing yet would
        // refuse the motion and count it refused, which is correct behaviour
        // and a useless measurement.
        click(home);
        std::thread::sleep(Duration::from_millis(120));
        println!("posted one left click to request capture");
    }

    let start = Instant::now();
    let mut posted = 0u64;
    let mut accepted = 0u64;
    let mut total = (0i64, 0i64);
    let mut index = 0u64;

    while start.elapsed() < deadline {
        let (dx, dy) = match pattern {
            Pattern::Circle => {
                // One revolution every hundred events, so the sum returns to
                // about zero and any residual is the pipeline's, not the
                // pattern's.
                let angle = (index % 100) as f64 * core::f64::consts::TAU / 100.0;
                (
                    (amplitude * angle.cos()).round() as i64,
                    (amplitude * angle.sin()).round() as i64,
                )
            }
            Pattern::Drift => (amplitude.round() as i64, 0),
            Pattern::Jitter => {
                if index.is_multiple_of(2) {
                    (1, -1)
                } else {
                    (-1, 1)
                }
            }
        };

        // Advanced by the delta so the movement is visible, then folded. The
        // fold is a jump in position and not in delta, so a client reading
        // deltas cannot tell it happened.
        at.x += dx as f64;
        at.y += dy as f64;
        if at.x < low || at.x > high {
            at.x = home.x;
        }
        if at.y < low || at.y > high {
            at.y = home.y;
        }
        if post_motion(at, dx, dy) {
            accepted += 1;
            total.0 += dx;
            total.1 += dy;
        }
        posted += 1;
        index += 1;

        // Absolute pacing against the start, so a slow post does not push
        // every later one and the rate stays what was asked for.
        let next = period * (index as u32);
        let elapsed = start.elapsed();
        if next > elapsed {
            std::thread::sleep(next - elapsed);
        }
    }

    println!("posted {posted}  accepted {accepted}");
    println!("total dx {}  total dy {}", total.0, total.1);
    if accepted == 0 {
        // The only failure this program has, and it is silent at the API, so
        // it has to be spelled out here.
        eprintln!(
            "nothing was accepted: posting events needs Accessibility permission \
             for this binary in System Settings, Privacy and Security"
        );
        std::process::exit(1);
    }
    let rate = accepted as f64 / start.elapsed().as_secs_f64();
    println!("accepted rate {rate:.1}/s against {hz:.0} Hz asked for");
}

/// Posts a left button press and release at `at`.
fn click(at: CGPoint) {
    for kind in [CGEventType::LeftMouseDown, CGEventType::LeftMouseUp] {
        if let Some(event) = CGEvent::new_mouse_event(None, kind, at, CGMouseButton::Left) {
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        }
    }
}

/// Posts one mouse-moved event carrying an explicit delta.
///
/// Returns whether the event could be built at all. The window server does
/// not report whether it accepted a post, so a caller can only know that the
/// event existed and was handed over.
fn post_motion(at: CGPoint, dx: i64, dy: i64) -> bool {
    let Some(event) =
        CGEvent::new_mouse_event(None, CGEventType::MouseMoved, at, CGMouseButton::Left)
    else {
        return false;
    };
    CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaX, dx);
    CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventDeltaY, dy);
    // The HID tap rather than the session tap, so the event enters where a
    // real device's would and every monitor downstream sees it.
    CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
    true
}
