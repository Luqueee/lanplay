use std::{
    process::ExitCode,
    time::{Duration, Instant},
};

use clap::Parser;
use hidapi::HidApi;
use lanplay_input_capture::ds4::parse_bluetooth_input;

const SONY_VENDOR: u16 = 0x054c;
const DS4_BLUETOOTH_PRODUCT: u16 = 0x09cc;

#[derive(Parser)]
struct Cli {
    #[arg(long, default_value_t = 15)]
    seconds: u64,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            eprintln!("ds4-hidapi-probe: cannot enumerate HID: {error}");
            return ExitCode::from(2);
        }
    };
    let Some(info) = api.device_list().find(|device| {
        device.vendor_id() == SONY_VENDOR && device.product_id() == DS4_BLUETOOTH_PRODUCT
    }) else {
        eprintln!("ds4-hidapi-probe: DS4 Bluetooth device 054c:09cc not found");
        return ExitCode::from(3);
    };
    let device = match info.open_device(&api) {
        Ok(device) => device,
        Err(error) => {
            eprintln!("ds4-hidapi-probe: cannot open DS4: {error}");
            return ExitCode::from(3);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(cli.seconds);
    let mut reports = 0u64;
    let mut parsed = 0u64;
    let mut changing = 0u64;
    let mut previous = None;
    let mut buffer = [0u8; 128];
    while Instant::now() < deadline {
        match device.read_timeout(&mut buffer, 100) {
            Ok(0) => {}
            Ok(length) => {
                reports += 1;
                if let Some(state) = parse_bluetooth_input(&buffer[..length], 1, 0, reports as u32)
                {
                    parsed += 1;
                    let mut comparable = state;
                    comparable.sequence = 0;
                    if previous.is_some_and(|prior| prior != comparable) {
                        changing += 1;
                    }
                    previous = Some(comparable);
                }
            }
            Err(error) => {
                eprintln!("ds4-hidapi-probe: read failed: {error}");
                return ExitCode::from(3);
            }
        }
    }
    println!("reports {reports} parsed {parsed} changing {changing}");
    if reports == 0 || parsed == 0 {
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}
