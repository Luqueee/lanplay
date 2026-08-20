use clap::Parser;
use lanplay_input_inject::{GamepadAction, GamepadHost, GamepadOutcome, deliver};
use lanplay_input_protocol::{MAX_DATAGRAM, Message, SessionId, decode};
use std::{
    io::{BufRead, BufReader, BufWriter, Write},
    net::UdpSocket,
    process::{Child, ChildStdin, Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:5006")]
    bind: String,
    #[arg(long, default_value_t = 1)]
    session: u32,
    #[arg(long)]
    bridge: String,
    #[arg(long, default_value_t = 10)]
    seconds: u64,
}

#[derive(Default)]
struct Counts {
    udp: u64,
    decode: u64,
    wrong_session: u64,
    attach: u64,
    state: u64,
    detach: u64,
    stale: u64,
    neutral: u64,
}
struct Bridge {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<std::process::ChildStdout>,
}

impl Bridge {
    fn start(path: &str) -> Result<Self, String> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot start bridge: {error}"))?;
        let input = BufWriter::new(child.stdin.take().ok_or("bridge has no stdin")?);
        let mut output = BufReader::new(child.stdout.take().ok_or("bridge has no stdout")?);
        let mut line = String::new();
        output
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if line.trim() != "ready" {
            return Err(format!("bridge did not become ready: {line:?}"));
        }
        Ok(Self {
            child,
            input,
            output,
        })
    }

    fn command(&mut self, command: &str) -> Result<(), String> {
        writeln!(self.input, "{command}").map_err(|error| error.to_string())?;
        self.input.flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        self.output
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if line.trim() == "ok" {
            Ok(())
        } else {
            Err(format!("bridge refused {command:?}: {line:?}"))
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.command("quit");
        let _ = self.child.wait();
    }
}

impl lanplay_input_inject::VirtualGamepadBackend for Bridge {
    type Error = String;

    fn create(&mut self, controller_slot: u8, _session_generation: u32) -> Result<(), Self::Error> {
        self.command(&format!("create {controller_slot}"))
    }

    fn submit_state(
        &mut self,
        state: lanplay_input_protocol::GamepadStateV1,
    ) -> Result<(), Self::Error> {
        self.command(&format!(
            "state {} {} {} {} {} {} {} {} {}",
            state.controller_slot,
            state.left_x,
            state.left_y,
            state.right_x,
            state.right_y,
            state.left_trigger,
            state.right_trigger,
            state.buttons,
            state.dpad as u8,
        ))
    }

    fn destroy(
        &mut self,
        controller_slot: u8,
        _session_generation: u32,
    ) -> Result<(), Self::Error> {
        self.command(&format!("destroy {controller_slot}"))
    }
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    let socket = UdpSocket::bind(&cli.bind).map_err(|error| error.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(cli.seconds);
    let mut bridge = Bridge::start(&cli.bridge)?;
    let mut host = GamepadHost::new();
    println!("ready");
    let mut buffer = [0; MAX_DATAGRAM];
    let mut counts = Counts::default();

    while Instant::now() < deadline {
        let (length, _) = match socket.recv_from(&mut buffer) {
            Ok(packet) => packet,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(error.to_string()),
        };
        counts.udp += 1;
        let datagram = match decode(&buffer[..length]) {
            Ok(datagram) if datagram.session == SessionId(cli.session) => datagram,
            Ok(_) => {
                counts.wrong_session += 1;
                continue;
            }
            Err(_) => {
                counts.decode += 1;
                continue;
            }
        };
        match &datagram.message {
            Message::GamepadAttach { .. } => counts.attach += 1,
            Message::GamepadDetach { .. } => counts.detach += 1,
            Message::GamepadState(_) => counts.state += 1,
            _ => continue,
        }
        let outcome = match datagram.message {
            Message::GamepadAttach {
                controller_slot,
                session_generation,
                ..
            } => host.attach(controller_slot, session_generation, |action| {
                deliver(&mut bridge, action).expect("bridge action")
            }),
            Message::GamepadDetach {
                controller_slot,
                session_generation,
                ..
            } => host.detach(controller_slot, session_generation, |action| {
                counts.neutral += matches!(action, GamepadAction::Submit(_)) as u64;
                deliver(&mut bridge, action).expect("bridge action")
            }),
            Message::GamepadState(state) => host.submit(state, |action| {
                deliver(&mut bridge, action).expect("bridge action")
            }),
            _ => continue,
        };
        if outcome == GamepadOutcome::Stale {
            counts.stale += 1;
            eprintln!("gamepad-inject-probe: stale controller state discarded");
        }
    }
    host.neutralize_all(|action| {
        counts.neutral += matches!(action, GamepadAction::Submit(_)) as u64;
        deliver(&mut bridge, action).expect("bridge neutralization")
    });
    println!(
        "udp {} decode {} wrong-session {} attach {} state {} detach {} stale {} neutral {}",
        counts.udp,
        counts.decode,
        counts.wrong_session,
        counts.attach,
        counts.state,
        counts.detach,
        counts.stale,
        counts.neutral,
    );
    if counts.attach == 0 || counts.state == 0 || counts.detach == 0 || counts.neutral == 0 {
        return Err("gamepad evidence was incomplete".to_owned());
    }
    Ok(())
}
