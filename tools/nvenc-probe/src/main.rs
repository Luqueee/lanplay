//! Opens an NVENC session on the capture device and prints concrete support.
//!
//! This is deliberately separate from the COM-free capability probe: a driver
//! DLL and API version are not enough to prove that the selected D3D11 device
//! can open an encoder session.

fn main() {
    #[cfg(windows)]
    run();
    #[cfg(not(windows))]
    {
        eprintln!("lanplay-nvenc-probe requires Windows");
        std::process::exit(2);
    }
}

#[cfg(windows)]
fn run() {
    let device = match lanplay_capture::CaptureDevice::open(0) {
        Ok(device) => device,
        Err(error) => {
            eprintln!("device: {error}");
            std::process::exit(1);
        }
    };
    println!("device {}", device.identity());

    let session = match lanplay_encoder_nvenc::NvencSession::open(device.device()) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("nvenc session: {error}");
            std::process::exit(1);
        }
    };

    let guids = match session.encode_guids() {
        Ok(guids) => guids,
        Err(error) => {
            eprintln!("nvenc codec query: {error}");
            std::process::exit(1);
        }
    };
    println!("codec GUIDs: {}", guids.len());
    for codec in guids {
        println!("  codec {codec:?}");
        match session.input_formats(codec) {
            Ok(formats) => println!("    input formats: {formats:?}"),
            Err(error) => {
                eprintln!("    input formats: {error}");
                std::process::exit(1);
            }
        }
    }
}
