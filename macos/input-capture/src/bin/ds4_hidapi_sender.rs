use clap::Parser;
use std::{
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    process::ExitCode,
    time::{Duration, Instant},
};

use hidapi::{HidApi, HidDevice};
use lanplay_input_capture::{INPUT_PORT, ds4::parse_bluetooth_input};
use lanplay_input_protocol::{
    Datagram, EventId, GamepadStateV1, MAX_DATAGRAM, Message, Sequence, SessionId, decode, encode,
};

const SONY_VENDOR: u16 = 0x054c;
const DS4_BLUETOOTH_PRODUCT: u16 = 0x09cc;
const CONTROL_RETRY: Duration = Duration::from_millis(50);
const CONTROL_DEADLINE: Duration = Duration::from_secs(10);

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
    #[arg(long, default_value = "0.0.0.0:5007")]
    feedback_listen: String,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let target = match resolve(&cli.target) {
        Ok(target) => target,
        Err(error) => return fail(error, 2),
    };
    let mut device = match open_ds4(Duration::from_secs(30)) {
        Ok(device) => device,
        Err(error) => return fail(error, 3),
    };
    let mut sequence = 0u32;
    let mut attach_id = EventId(1);
    let attach = Message::GamepadAttach {
        id: attach_id,
        controller_slot: cli.slot,
        session_generation: cli.generation,
    };
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(socket) => socket,
        Err(error) => return fail(error.to_string(), 2),
    };
    let feedback_socket = match UdpSocket::bind(&cli.feedback_listen) {
        Ok(socket) => socket,
        Err(error) => return fail(error.to_string(), 2),
    };
    if let Err(error) = feedback_socket.set_nonblocking(true) {
        return fail(error.to_string(), 2);
    }
    if !send_control_until_ack(&socket, target, cli.session, &mut sequence, attach) {
        return fail("initial attach was not acknowledged".to_owned(), 3);
    }
    let deadline = Instant::now() + Duration::from_secs(cli.seconds);
    let mut reports = 0u64;
    let mut sent = 0u64;
    let mut recoveries = 0u64;
    let mut buffer = [0; 128];
    let mut feedback_sequence = None;
    let mut rumble_reports = 0u64;
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
                    if let Err(error) = drain_feedback(
                        &feedback_socket,
                        &device,
                        cli.session,
                        cli.generation,
                        cli.slot,
                        &mut feedback_sequence,
                        &mut rumble_reports,
                    ) {
                        return fail(error, 3);
                    }
                }
            }
            Err(error) => {
                eprintln!("ds4-hidapi-sender: read failed, reconnecting: {error}");
                recoveries += 1;
                device = match open_ds4(Duration::from_secs(30)) {
                    Ok(device) => device,
                    Err(error) => return fail(error, 3),
                };
                attach_id = attach_id.next();
                let attach = Message::GamepadAttach {
                    id: attach_id,
                    controller_slot: cli.slot,
                    session_generation: cli.generation,
                };
                if !send_control_until_ack(&socket, target, cli.session, &mut sequence, attach) {
                    return fail("reconnect attach was not acknowledged".to_owned(), 3);
                }
            }
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
    println!(
        "raw_reports {reports} states_sent {sent} recoveries {recoveries} rumble_reports {rumble_reports}"
    );
    if sent == 0 {
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}

fn drain_feedback(
    socket: &UdpSocket,
    device: &HidDevice,
    session: u32,
    generation: u32,
    slot: u8,
    last_sequence: &mut Option<u32>,
    applied: &mut u64,
) -> Result<(), String> {
    let mut buffer = [0; MAX_DATAGRAM];
    loop {
        let (length, _) = match socket.recv_from(&mut buffer) {
            Ok(packet) => packet,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) => return Err(format!("rumble feedback receive failed: {error}")),
        };
        let Ok(datagram) = decode(&buffer[..length]) else {
            continue;
        };
        if datagram.session != SessionId(session) {
            continue;
        }
        let Message::GamepadFeedback(feedback) = datagram.message else {
            continue;
        };
        if feedback.session_generation != generation || feedback.controller_slot != slot {
            continue;
        }
        if last_sequence.is_some_and(|previous| !is_newer(feedback.sequence, previous)) {
            continue;
        }
        apply_usb_rumble(device, feedback.low_frequency, feedback.high_frequency)?;
        *last_sequence = Some(feedback.sequence);
        *applied += 1;
    }
}

fn is_newer(candidate: u32, previous: u32) -> bool {
    let distance = candidate.wrapping_sub(previous);
    distance != 0 && distance < (1 << 31)
}

fn apply_usb_rumble(
    device: &HidDevice,
    low_frequency: u16,
    high_frequency: u16,
) -> Result<(), String> {
    let mut report = [0u8; 32];
    report[0] = 0x05;
    report[1] = 0xFF;
    report[4] = (low_frequency / 257) as u8;
    report[5] = (high_frequency / 257) as u8;
    device
        .write(&report)
        .map(|_| ())
        .map_err(|error| format!("DS4 USB rumble write failed: {error}"))
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
