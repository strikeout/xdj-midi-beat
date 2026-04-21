#![warn(clippy::all)]

mod app;
mod config;
mod link;
mod midi;
mod prolink;
mod runtime;
mod state;
mod tui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let ctx = app::init()?;
    let use_tui = !ctx.cli.no_tui;
    runtime::run(ctx, use_tui).await
}
