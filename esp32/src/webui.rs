use core::sync::atomic::AtomicBool;
use esp_idf_svc::http::server::{EspHttpServer, EspRequest, EspResponse};

pub fn start(server: EspHttpServer, state: SharedState) -> anyhow::Result<()> {
    let _running = AtomicBool::new(true);
    server.on_not_found(move |req: EspRequest| {
        let path = req.uri();
        if path == "/" || path == "/index.html" {
            serve_dashboard(req, state)
        } else if path == "/api/status" {
            serve_status_json(req, state)
        } else {
            serve_404(req)
        }
    })?;
    log::info!("Web dashboard running at http://192.168.4.1");
    Ok(())
}

fn serve_dashboard(req: EspRequest, _state: SharedState) -> anyhow::Result<()> {
    let html = include_str!("../../webui/index.html");
    let mut resp = EspResponse::new(200, "OK", req)?;
    resp.add_header("Content-Type", "text/html")?;
    resp.add_header("Connection", "keep-alive")?;
    resp.send(html.as_bytes())?;
    Ok(())
}

fn serve_status_json(req: EspRequest, state: SharedState) -> anyhow::Result<()> {
    let bpm = state.get_master_bpm();
    let beat = state.get_master_beat();
    let playing = state.get_master_is_playing();
    let device = state.get_master_device();
    let json = heapless::format!(
        heapless::String<256>,
        "{{\"bpm\":{:.2},\"beat\":{},\"playing\":{},\"master\":{}}}",
        bpm,
        beat,
        playing,
        device
    )?;
    let mut resp = EspResponse::new(200, "OK", req)?;
    resp.add_header("Content-Type", "application/json")?;
    resp.send(json.as_bytes())?;
    Ok(())
}

fn serve_404(req: EspRequest) -> anyhow::Result<()> {
    let mut resp = EspResponse::new(404, "Not Found", req)?;
    resp.send(b"404 Not Found")?;
    Ok(())
}
