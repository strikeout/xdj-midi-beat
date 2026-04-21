use std::env;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

pub fn run_emulator(binary_path: &str) -> anyhow::Result<()> {
    println!("Starting ESP32 emulator...");

    if cfg!(target_os = "linux") {
        println!("Checking for QEMU with xtensa support...");
        let qemu_check = Command::new("qemu-system-xtensa").arg("--version").output();

        match qemu_check {
            Ok(out) if out.status.success() => {
                println!("QEMU with xtensa found");
            }
            _ => {
                println!("WARNING: QEMU with xtensa not found");
                println!("Install with: brew install qemu (macOS) or apt install qemu-system-xtensa (Linux)");
            }
        }
    }

    if cfg!(target_os = "macos") {
        let qemu_check = Command::new("qemu-system-xtensa").arg("--version").output();
        match qemu_check {
            Ok(out) if out.status.success() => {
                println!("QEMU found, starting emulation...");
                start_qemu_emu(binary_path)?;
            }
            _ => {
                println!("QEMU not found. Install with: brew install qemu");
                println!("For Renode: download from https://renode.io/");
            }
        }
    }

    if cfg!(target_os = "windows") {
        println!("Windows: Use WSL2 or manual QEMU setup");
    }

    let listener = TcpListener::bind("127.0.0.1:8080")?;
    println!("Emulator debug server listening on http://127.0.0.1:8080");
    println!("Connect with: telnet 127.0.0.1:8080");

    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let mut buf = [0u8; 1024];
                if let Ok(n) = s.read(&mut buf) {
                    println!("[Emulator] Debug: {}", String::from_utf8_lossy(&buf[..n]));
                }
                let _ = s.write(b"OK\r\n");
            }
            Err(e) => println!("Connection error: {}", e),
        }
    }

    Ok(())
}

fn start_qemu_emu(binary_path: &str) -> anyhow::Result<()> {
    println!("Launching QEMU ESP32 emulation...");

    let mut cmd = Command::new("qemu-system-xtensa");
    cmd.args(&[
        "-machine",
        "esp32",
        "-kernel",
        binary_path,
        "-m",
        "4M",
        "-nographic",
        "-net",
        "nic",
        "-net",
        "tap,ifname=tap0",
    ]);

    match cmd.spawn() {
        Ok(_) => println!("QEMU started successfully"),
        Err(e) => println!(
            "Failed to start QEMU: {}. Install from https://github.com/espressif/qemu/releases",
            e
        ),
    }

    Ok(())
}

pub mod renode {
    pub fn check_renode() -> bool {
        std::process::Command::new("renode")
            .arg("--version")
            .output()
            .is_ok()
    }

    pub fn launch_renode_script(script_path: &str) -> anyhow::Result<()> {
        println!("Launching Renode with script: {}", script_path);
        let output = std::process::Command::new("renode")
            .args(&["--disable-xwt", "-r", script_path])
            .output()?;

        if output.status.success() {
            println!("Renode emulation started");
        } else {
            println!("Renode output: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(())
    }
}
