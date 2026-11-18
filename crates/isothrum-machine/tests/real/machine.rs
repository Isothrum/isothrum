//! The `machine` tier: this computer's own tools, as they are installed here.

mod cloud;
mod docker;
mod install;
mod needs;
mod net;
mod ovftool;
mod providers;
mod run;
mod selftest;
mod terminal;
mod tool_path;

use isothrum_machine::Context;

use crate::answer;

/// Whether UTM runs now, as macOS says (asking doesn't open it).
async fn utm_running(cx: &Context) -> bool {
    answer(cx, "osascript", &["-e", "application \"UTM\" is running"])
        .await
        .is_some_and(|said| said.trim() == "true")
}

/// Quits UTM, which the test opened: the user had it closed.
async fn quit_utm(cx: &Context) {
    answer(cx, "osascript", &["-e", "tell application \"UTM\" to quit"]).await;
}
