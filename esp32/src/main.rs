use esp_idf_svc::hal::gpio;
use esp_idf_svc::hal::uart;
use esp_idf_svc::netif;
use esp_idf_svc::wifi;
use esp_idf_svc::http;
use embedded_svc::wifi as ewifi;
use smoltcp;
use heapless;

pub mod midi;
pub mod prolink;
pub mod state;
pub mod webui;

use esp_idf_hal::peripherals::Peripherals;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::wifi::EspWifi;

fn main() -> anyhow::Result<()> {
    esp_idf_sys::link_panic();

    let peripherals = Peripherals::take().unwrap();
    let sys_event_loop = EspSystemEventLoop::take()?;

    let wifi_ssid = heapless::String::from("xdj-midi-setup");
    let wifi_password = heapless::String::from("xdjclock123");

    let mut wifi = EspWifi::new(peripherals.modem, sys_event_loop.clone(), None)?;

    let ap_config = ewifi::Configuration {
        mode: ewifi::Mode::AccessPoint,
        ssid: wifi_ssid.as_str().into(),
        password: wifi_password.as_str().into(),
        channel: 1,
        ..Default::default()
    };
    wifi.set_configuration(&ap_config)?;
    wifi.start()?;

    log::info!("xdj-clock ESP32 starting...");
    log::info!("WiFi AP: {} / {}", wifi_ssid.as_str(), wifi_password.as_str());
    log::info!("Dashboard: http://192.168.4.1");

    let midi_in_pin = peripherals.pins.gpio1;
    let midi_out_pin = peripherals.pins.gpio3;

    let midi_uart = uart::Uart::new(
        peripherals.uart0,
        midi_in_pin,
        midi_out_pin,
        Option::<gpio::Gpio0>::None,
        Option::<gpio::Gpio1>::None,
    )?;

    log::info!("MIDI UART initialized on GPIO1(GX)/GPIO3(TX)");

    let state = Arc::new(RwLock::new(DjState::new()));
    let (beat_tx, _beat_rx) = broadcast::channel(256);

    let prolink_stack = prolink::ProLinkStack::new(
        smoltcp::iface::EthernetInterface::new(),
        [0x02, 0xAB, 0xCD, 0xEF, 0x01, 0x02],
    )?;

    let http_server = http::Server::new(http::ServerConfig {
        port: 80,
        ..Default::default()
    })?;

    webui::start(http_server, state.clone(), midi_uart)?;

    loop {
        esp_idf_svc::timer::timer_task::run_tasks();
        embassy_time::Timer::after_millis(10).await;
    }
}