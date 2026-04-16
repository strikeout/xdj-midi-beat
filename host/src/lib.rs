pub mod config;
pub mod midi;
pub mod prolink;
pub mod state;
pub mod tui;

pub fn interface_priority(name: &str, ip: &std::net::Ipv4Addr) -> u8 {
    if ip.is_loopback() || ip.is_link_local() {
        return 0;
    }
    let n = name.to_lowercase();
    if n.contains("en") || n.contains("eth") || n.contains("wired") {
        return 3;
    }
    if n.contains("wl") || n.contains("wi") || n.contains("air") {
        return 2;
    }
    1
}
