// SPDX-License-Identifier: GPL-3.0-only

use anyhow::Context;
use calloop::{EventLoop, LoopHandle};

mod session;

pub struct State {
    loop_handle: LoopHandle<'static, Self>,
}

fn main() -> anyhow::Result<()> {
    tracing::subscriber::set_global_default(tracing_subscriber::FmtSubscriber::new())
        .expect("setting tracing default failed");

    let mut evl = EventLoop::<'static, State>::try_new().context("Failed to create event loop")?;
    let evlh = evl.handle();
    let mut state = State {
        loop_handle: evl.handle(),
    };
    session::setup_socket(evlh).context("Failed to connect to cosmic-session")?;
    evl.run(None, &mut state, |_| {})
        .context("Event loop terminated")
}
