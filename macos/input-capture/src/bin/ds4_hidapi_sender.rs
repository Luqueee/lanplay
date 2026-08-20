use clap::Parser;
use std::{
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    process::ExitCode,
    time::{Duration, Instant},
};

use hidapi::{HidApi, HidDevice};
use lanplay_input_capture::{INPUT_PORT, ds4::parse_bluetooth_input};
use lanplay_input_protocol::{
    Datagram, EventId, GamepadStateV1, MAX_DATAGRAM, Message, Sequence, SessionId, encode,
};

const SONY_VENDOR: u16 = 0x054c;
const DS4_BLUETOOTH_PRODUCT: u16 = 0x09cc;
const CONTROL_RETRY: Duration = Duration::from_millis(20);
const CONTROL_DEADLINE: Duration = Duration::from_secs(2);

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
    #[arg(long, default_value_t = 60)]
    seconds: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let target = match resolve(&cli.target) {
        Ok(target) => target,
        Err(error) => return fail(error, 2),
    };
    let device = match open_ds4(Duration::from_secs(30)) {
        Ok(device) => device,
        Err(error) => return fail(error, 3),
    };
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(socket) => socket,
        Err(error) => return fail(error.to_string(), 2),
    };
    let mut sequence = 0u32;
    let attach = Message::GamepadAttach {
        id: EventId(1),
        controller_slot: cli.slot,
        session_generation: cli.generation,
    };
    if !send_control_until_ack(&socket, target, cli.session, &mut sequence, attach) {
        return ExitCode::from(3);
    }
    let deadline = Instant::now() + Duration::from_secs(cli.seconds);
    let mut reports = 0u64;
    let mut sent = 0u64;
    let mut buffer = [0; 128];
    while Instant::now() < deadline {
        match device.read_timeout(&mut buffer, 100) {
            Ok(0) => {}
            Ok(length) => {
                reports += 1;
                if let Some(state) =
                    parse_bluetooth_input(&buffer[..length], cli.generation, cli.slot, sequence)
                {
                    if !send(
                        &socket,
                        target,
                        cli.session,
                        &mut sequence,
                        Message::GamepadState(state),
                    ) {
                        return ExitCode::from(3);
                    }
                    sent += 1;
                }
            }
            Err(error) => return fail(error.to_string(), 3),
        }
    }
    let neutral = GamepadStateV1::neutral_for(cli.generation, cli.slot, sequence);
    let detach = Message::GamepadDetach {
        id: EventId(2),
        controller_slot: cli.slot,
        session_generation: cli.generation,
    };
    if !send(
        &socket,
        target,
        cli.session,
        &mut sequence,
        Message::GamepadState(neutral),
    ) || !send_control_until_ack(&socket, target, cli.session, &mut sequence, detach)
    {
        return ExitCode::from(3);
    }
    println!("raw_reports {reports} states_sent {sent}");
    if sent == 0 {
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}

fn send_control_until_ack(
    socket: &UdpSocket,
    target: SocketAddr,
    session: u32,
    sequence: &mut u32,
    message: Message,
) -> bool {
    let Some(id) = message.event_id() else {
        return false;
    };
    let deadline = Instant::now() + CONTROL_DEADLINE;
    let mut buffer = [0; MAX_DATAGRAM];
    while Instant::now() < deadline {
        if !send(socket, target, session, sequence, message) {
            return false;
        }
        let wait = CONTROL_RETRY.min(deadline.saturating_duration_since(Instant::now()));
        if socket.set_read_timeout(Some(wait)).is_err() {
            return false;
        }
        if let Ok((length, from)) = socket.recv_from(&mut buffer)
            && from == target
            && let Ok(reply) = lanplay_input_protocol::decode(&buffer[..length])
            && reply.session == SessionId(session)
            && matches!(reply.message, Message::Ack { top, missing: 0 } if top == id)
        {
            return true;
        }
    }
    false
}
fn open_ds4(wait: Duration) -> Result<HidDevice, String> {
    let deadline = Instant::now() + wait;
    loop {
        let api = HidApi::new().map_err(|error| error.to_string())?;
        let device = api
            .device_list()
            .find(|device| {
                device.vendor_id() == SONY_VENDOR && device.product_id() == DS4_BLUETOOTH_PRODUCT
            })
            .and_then(|info| info.open_device(&api).ok());
        if let Some(device) = device {
            return Ok(device);
        }
        if Instant::now() >= deadline {
            return Err("DS4 Bluetooth 054c:09cc not found after retry window".to_owned());
        }
        std::thread::sleep(Duration::from_millis(250));
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
    let length = encode(&datagram, &mut buffer).expect("gamepad message fits input datagram bound");
    socket.send_to(&buffer[..length], target).is_ok()
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

fn fail(error: String, code: u8) -> ExitCode {
    eprintln!("ds4-hidapi-sender: {error}");
    ExitCode::from(code)
}
