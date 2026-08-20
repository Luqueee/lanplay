use clap::Parser;
use lanplay_input_capture::{INPUT_PORT, gamepad};
use lanplay_input_protocol::{
    Datagram, Dpad, EventId, GamepadStateV1, MAX_DATAGRAM, Message, Sequence, SessionId, encode,
};
use std::{
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
use block2::RcBlock;
#[cfg(target_os = "macos")]
use core::ptr::NonNull;
#[cfg(target_os = "macos")]
use objc2::MainThreadMarker;
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSDate, NSRunLoop};
#[cfg(target_os = "macos")]
use objc2_game_controller::{GCControllerElement, GCExtendedGamepad};

const SNAPSHOT_PERIOD: Duration = Duration::from_nanos(8_333_333);
const POLL_PERIOD: Duration = Duration::from_millis(1);

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    target: String,
    #[arg(long, default_value_t = 1)]
    session: u32,
    #[arg(long, default_value_t = 1)]
    generation: u32,
    #[arg(long, default_value_t = 0)]
    slot: u8,
    /// End the observation after this many seconds and print its report.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    observe_for: u64,
}

#[derive(Default)]
struct SignedRange {
    samples: u64,
    min: i16,
    nearest_neutral: i16,
    max: i16,
}

impl SignedRange {
    fn observe(&mut self, value: i16) {
        if self.samples == 0 {
            self.min = value;
            self.nearest_neutral = value;
            self.max = value;
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            if value.unsigned_abs() < self.nearest_neutral.unsigned_abs() {
                self.nearest_neutral = value;
            }
        }
        self.samples += 1;
    }
}

#[derive(Default)]
struct TriggerRange {
    samples: u64,
    min: u16,
    nearest_neutral: u16,
    max: u16,
}

impl TriggerRange {
    fn observe(&mut self, value: u16) {
        if self.samples == 0 {
            self.min = value;
            self.nearest_neutral = value;
            self.max = value;
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
            self.nearest_neutral = self.nearest_neutral.min(value);
        }
        self.samples += 1;
    }
}

#[derive(Default)]
struct Cadence {
    intervals: u64,
    total: Duration,
    min: Option<Duration>,
    max: Option<Duration>,
}

impl Cadence {
    fn observe(&mut self, interval: Duration) {
        self.intervals += 1;
        self.total += interval;
        self.min = Some(self.min.map_or(interval, |min| min.min(interval)));
        self.max = Some(self.max.map_or(interval, |max| max.max(interval)));
    }

    fn description(&self) -> String {
        match (self.intervals, self.min, self.max) {
            (0, _, _) => "no interval observed".to_owned(),
            (count, Some(min), Some(max)) => format!(
                "samples={count} min_ms={:.3} mean_ms={:.3} max_ms={:.3}",
                min.as_secs_f64() * 1_000.0,
                self.total.as_secs_f64() * 1_000.0 / count as f64,
                max.as_secs_f64() * 1_000.0,
            ),
            _ => unreachable!("cadence intervals always retain their bounds"),
        }
    }
}

#[derive(Default)]
struct ControlCoverage {
    buttons: u16,
    dpad_directions: u8,
    axes: u8,
    triggers: u8,
}

impl ControlCoverage {
    fn observe(&mut self, state: GamepadStateV1) {
        self.buttons |= state.buttons;
        self.dpad_directions |= match state.dpad {
            Dpad::Neutral => 0,
            Dpad::North => 1,
            Dpad::NorthEast => 1 | 2,
            Dpad::East => 2,
            Dpad::SouthEast => 2 | 4,
            Dpad::South => 4,
            Dpad::SouthWest => 4 | 8,
            Dpad::West => 8,
            Dpad::NorthWest => 8 | 1,
        };
        self.axes |= u8::from(state.left_x != 0)
            | (u8::from(state.left_y != 0) << 1)
            | (u8::from(state.right_x != 0) << 2)
            | (u8::from(state.right_y != 0) << 3);
        self.triggers |=
            u8::from(state.left_trigger != 0) | (u8::from(state.right_trigger != 0) << 1);
    }
}

struct ControllerIdentity {
    vendor_name: String,
    product_category: String,
    dualshock4: bool,
    attached_to_device: bool,
}

impl ControllerIdentity {
    fn print(&self) {
        println!("controller vendor_name {}", self.vendor_name);
        println!("controller product_category {}", self.product_category);
        println!("controller profile extended-gamepad");
        println!(
            "controller dualshock4_category {}",
            if self.dualshock4 { "yes" } else { "no" }
        );
        println!(
            "controller attached_to_device {}",
            if self.attached_to_device { "yes" } else { "no" }
        );
        println!("controller transport unavailable (GameController does not expose it)");
    }
}

#[derive(Default)]
struct Observation {
    attached: u64,
    detached: u64,
    identity: Option<ControllerIdentity>,
    state_samples: u64,
    state_changes: u64,
    snapshots: u64,
    coverage: ControlCoverage,
    left_x: SignedRange,
    left_y: SignedRange,
    right_x: SignedRange,
    right_y: SignedRange,
    left_trigger: TriggerRange,
    right_trigger: TriggerRange,
    state_change_cadence: Cadence,
    snapshot_cadence: Cadence,
    last_state_change: Option<Instant>,
    last_snapshot: Option<Instant>,
}

impl Observation {
    fn observe_state(&mut self, state: GamepadStateV1, changed: bool, now: Instant) {
        self.state_samples += 1;
        self.coverage.observe(state);
        self.left_x.observe(state.left_x);
        self.left_y.observe(state.left_y);
        self.right_x.observe(state.right_x);
        self.right_y.observe(state.right_y);
        self.left_trigger.observe(state.left_trigger);
        self.right_trigger.observe(state.right_trigger);
        if changed {
            self.state_changes += 1;
            if let Some(previous) = self.last_state_change.replace(now) {
                self.state_change_cadence
                    .observe(now.duration_since(previous));
            }
        }
    }

    fn observe_snapshot(&mut self, now: Instant) {
        self.snapshots += 1;
        if let Some(previous) = self.last_snapshot.replace(now) {
            self.snapshot_cadence.observe(now.duration_since(previous));
        }
    }

    fn print(&self) {
        println!(
            "gamepad observation verdict {}",
            if self.state_samples == 0 {
                "NO_CONTROLLER_ACTIVITY"
            } else {
                "PASS"
            }
        );
        println!(
            "connection lifecycle attached={} detached={}",
            self.attached, self.detached
        );
        if let Some(identity) = &self.identity {
            identity.print();
        } else {
            println!("controller identity unavailable (no extended controller observed)");
        }
        println!(
            "standard control profile {}",
            if self.identity.is_some() {
                "extended-gamepad complete"
            } else {
                "unavailable"
            }
        );
        println!(
            "standard controls observed south={} east={} west={} north={} left_shoulder={} right_shoulder={} left_stick={} right_stick={} view={} menu={} guide={}",
            yes(self.coverage.buttons & gamepad::buttons::SOUTH != 0),
            yes(self.coverage.buttons & gamepad::buttons::EAST != 0),
            yes(self.coverage.buttons & gamepad::buttons::WEST != 0),
            yes(self.coverage.buttons & gamepad::buttons::NORTH != 0),
            yes(self.coverage.buttons & gamepad::buttons::LEFT_SHOULDER != 0),
            yes(self.coverage.buttons & gamepad::buttons::RIGHT_SHOULDER != 0),
            yes(self.coverage.buttons & gamepad::buttons::LEFT_STICK != 0),
            yes(self.coverage.buttons & gamepad::buttons::RIGHT_STICK != 0),
            yes(self.coverage.buttons & gamepad::buttons::VIEW != 0),
            yes(self.coverage.buttons & gamepad::buttons::MENU != 0),
            yes(self.coverage.buttons & gamepad::buttons::GUIDE != 0),
        );
        println!(
            "dpad directions observed north={} east={} south={} west={}",
            yes(self.coverage.dpad_directions & 1 != 0),
            yes(self.coverage.dpad_directions & 2 != 0),
            yes(self.coverage.dpad_directions & 4 != 0),
            yes(self.coverage.dpad_directions & 8 != 0),
        );
        println!(
            "analog controls observed left_x={} left_y={} right_x={} right_y={} left_trigger={} right_trigger={}",
            yes(self.coverage.axes & 1 != 0),
            yes(self.coverage.axes & 2 != 0),
            yes(self.coverage.axes & 4 != 0),
            yes(self.coverage.axes & 8 != 0),
            yes(self.coverage.triggers & 1 != 0),
            yes(self.coverage.triggers & 2 != 0),
        );
        print_axis("left_x", &self.left_x);
        print_axis("left_y", &self.left_y);
        print_axis("right_x", &self.right_x);
        print_axis("right_y", &self.right_y);
        print_trigger("left_trigger", &self.left_trigger);
        print_trigger("right_trigger", &self.right_trigger);
        println!(
            "neutral noise is the absolute closest-to-protocol-neutral value observed, not a transport threshold"
        );
        println!(
            "state samples={} changes={} snapshots={}",
            self.state_samples, self.state_changes, self.snapshots
        );
        println!(
            "state-change cadence {}",
            self.state_change_cadence.description()
        );
        println!("snapshot cadence {}", self.snapshot_cadence.description());
    }
}

fn yes(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn print_axis(name: &str, range: &SignedRange) {
    println!(
        "axis {name} min={} neutral={} max={} neutral_noise={} samples={}",
        range.min,
        range.nearest_neutral,
        range.max,
        range.nearest_neutral.unsigned_abs(),
        range.samples
    );
}

fn print_trigger(name: &str, range: &TriggerRange) {
    println!(
        "trigger {name} min={} neutral={} max={} neutral_noise={} samples={}",
        range.min, range.nearest_neutral, range.max, range.nearest_neutral, range.samples
    );
}

#[cfg(target_os = "macos")]
fn controller_identity(controller: &objc2_game_controller::GCController) -> ControllerIdentity {
    use objc2_game_controller::GCDevice;

    let vendor_name = unsafe {
        controller
            .vendorName()
            .map_or_else(|| "unavailable".to_owned(), |name| name.to_string())
    };
    let product_category = unsafe { controller.productCategory().to_string() };
    let dualshock4_category = unsafe {
        objc2_game_controller::GCProductCategoryDualShock4.map(|category| category.to_string())
    };
    ControllerIdentity {
        vendor_name,
        dualshock4: dualshock4_category.as_deref() == Some(product_category.as_str()),
        product_category,
        attached_to_device: unsafe { controller.isAttachedToDevice() },
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    #[cfg(target_os = "macos")]
    let _app = {
        let Some(marker) = MainThreadMarker::new() else {
            eprintln!("gamepad-capture-probe: AppKit requires the main thread");
            return ExitCode::from(2);
        };
        let app = NSApplication::sharedApplication(marker);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        app
    };
    let target = match resolve(&cli.target) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("gamepad-capture-probe: {error}");
            return ExitCode::from(2);
        }
    };
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("gamepad-capture-probe: cannot bind UDP socket: {error}");
            return ExitCode::from(2);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(cli.observe_for);
    let mut sequence = 0u32;
    let mut event = EventId(0);
    let mut attached = false;
    let mut previous = GamepadStateV1::neutral_for(cli.generation, cli.slot, 0);
    let mut last_snapshot = Instant::now() - SNAPSHOT_PERIOD;
    let mut observation = Observation::default();
    let callback_events = Arc::new(AtomicU64::new(0));
    let mut callback_installed = false;
    #[cfg(target_os = "macos")]
    unsafe {
        objc2_game_controller::GCController::startWirelessControllerDiscoveryWithCompletionHandler(
            None,
        );
    }

    while Instant::now() < deadline {
        let now = Instant::now();
        #[cfg(target_os = "macos")]
        let current = unsafe {
            let controllers = objc2_game_controller::GCController::controllers();
            (controllers.count() != 0)
                .then(|| controllers.objectAtIndex(0))
                .and_then(|controller| {
                    controller
                        .extendedGamepad()
                        .map(|profile| (controller, profile))
                })
        };
        #[cfg(not(target_os = "macos"))]
        let current: Option<()> = None;

        if let Some((controller, profile)) = current {
            if !callback_installed {
                let events = Arc::clone(&callback_events);
                let handler = RcBlock::new(
                    move |_: NonNull<GCExtendedGamepad>, _: NonNull<GCControllerElement>| {
                        events.fetch_add(1, Ordering::Relaxed);
                    },
                );
                unsafe {
                    profile.setValueChangedHandler(RcBlock::as_ptr(&handler));
                }
                callback_installed = true;
            }
            if !attached {
                event = event.next();
                if !send(
                    &socket,
                    target,
                    cli.session,
                    &mut sequence,
                    Message::GamepadAttach {
                        id: event,
                        controller_slot: cli.slot,
                        session_generation: cli.generation,
                    },
                ) {
                    return ExitCode::from(3);
                }
                attached = true;
                observation.attached += 1;
                observation.identity = Some(controller_identity(&controller));
                println!(
                    "controller attached slot {} generation {}",
                    cli.slot, cli.generation
                );
            }
            let state = gamepad::snapshot(&profile, cli.generation, cli.slot, sequence);
            let mut comparable = state;
            comparable.sequence = 0;
            let mut prior = previous;
            prior.sequence = 0;
            let changed = comparable != prior;
            observation.observe_state(state, changed, now);
            if changed || now.duration_since(last_snapshot) >= SNAPSHOT_PERIOD {
                if !send(
                    &socket,
                    target,
                    cli.session,
                    &mut sequence,
                    Message::GamepadState(state),
                ) {
                    return ExitCode::from(3);
                }
                observation.observe_snapshot(now);
                previous = state;
                last_snapshot = now;
            }
        } else if attached {
            event = event.next();
            let neutral = GamepadStateV1::neutral_for(cli.generation, cli.slot, sequence);
            if !send(
                &socket,
                target,
                cli.session,
                &mut sequence,
                Message::GamepadState(neutral),
            ) || !send(
                &socket,
                target,
                cli.session,
                &mut sequence,
                Message::GamepadDetach {
                    id: event,
                    controller_slot: cli.slot,
                    session_generation: cli.generation,
                },
            ) {
                return ExitCode::from(3);
            }
            attached = false;
            previous = neutral;
            println!(
                "controller detached slot {} generation {}",
                cli.slot, cli.generation
            );
        }
        #[cfg(target_os = "macos")]
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(
            POLL_PERIOD.as_secs_f64(),
        ));
        #[cfg(not(target_os = "macos"))]
        thread::sleep(POLL_PERIOD);
    }

    if attached {
        event = event.next();
        let neutral = GamepadStateV1::neutral_for(cli.generation, cli.slot, sequence);
        if !send(
            &socket,
            target,
            cli.session,
            &mut sequence,
            Message::GamepadState(neutral),
        ) || !send(
            &socket,
            target,
            cli.session,
            &mut sequence,
            Message::GamepadDetach {
                id: event,
                controller_slot: cli.slot,
                session_generation: cli.generation,
            },
        ) {
            return ExitCode::from(3);
        }
    }
    #[cfg(target_os = "macos")]
    unsafe {
        objc2_game_controller::GCController::stopWirelessControllerDiscovery();
    }

    println!(
        "GameController valueChangedHandler events={}",
        callback_events.load(Ordering::Relaxed)
    );
    observation.print();
    if observation.state_samples == 0 {
        eprintln!(
            "gamepad-capture-probe: no extended controller activity was observed; refusing success"
        );
        ExitCode::from(4)
    } else {
        ExitCode::SUCCESS
    }
}

fn send(
    socket: &UdpSocket,
    target: SocketAddr,
    session: u32,
    sequence: &mut u32,
    message: Message,
) -> bool {
    let datagram = Datagram {
        session: SessionId(session),
        sequence: Sequence(*sequence),
        sent_at_ns: 0,
        message,
    };
    *sequence = sequence.wrapping_add(1);
    let mut buffer = [0; MAX_DATAGRAM];
    let len =
        encode(&datagram, &mut buffer).expect("every gamepad message fits the protocol bound");
    match socket.send_to(&buffer[..len], target) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("gamepad-capture-probe: UDP send failed: {error}");
            false
        }
    }
}

fn resolve(spec: &str) -> Result<SocketAddr, String> {
    let target = if spec.parse::<SocketAddr>().is_ok() || spec.rfind(':').is_some() {
        spec.to_owned()
    } else {
        format!("{spec}:{INPUT_PORT}")
    };
    target
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .next()
        .ok_or_else(|| "target resolved to no addresses".to_owned())
}
