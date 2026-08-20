use std::{
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use clap::Parser;
use lanplay_input_capture::{INPUT_PORT, gamepad};
use lanplay_input_protocol::{
    Datagram, EventId, GamepadStateV1, MAX_DATAGRAM, Message, Sequence, SessionId, encode,
};

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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
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
    let mut sequence = 0u32;
    let mut event = EventId(0);
    let mut attached = false;
    let mut previous = GamepadStateV1::neutral_for(cli.generation, cli.slot, 0);
    let mut last_snapshot = Instant::now() - SNAPSHOT_PERIOD;

    loop {
        let now = Instant::now();
        #[cfg(target_os = "macos")]
        let current = unsafe {
            objc2_game_controller::GCController::current()
                .and_then(|controller| controller.extendedGamepad())
        };
        #[cfg(not(target_os = "macos"))]
        let current: Option<()> = None;

        if let Some(profile) = current {
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
                println!(
                    "controller attached slot {} generation {}",
                    cli.slot, cli.generation
                );
            }
            let state = gamepad::snapshot(&profile, cli.generation, cli.slot, sequence);
            if state != previous || now.duration_since(last_snapshot) >= SNAPSHOT_PERIOD {
                if !send(
                    &socket,
                    target,
                    cli.session,
                    &mut sequence,
                    Message::GamepadState(state),
                ) {
                    return ExitCode::from(3);
                }
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
        thread::sleep(POLL_PERIOD);
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
