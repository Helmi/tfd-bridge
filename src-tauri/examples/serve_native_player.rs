//! Isolated WebView2 validation host; no tray, updater, donation or main app.
//! Usage: cargo run -p tfd-bridge --example serve_native_player -- <replays> <dist>
//! Uses a separate app identifier/profile; stop with Ctrl-C.
#[path = "../src/render_worker.rs"]
mod render_worker;
use bridge_core::server::{start_full, PlayerConfig};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::Manager;

fn main() {
    eprintln!("native validation: entering main");
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(args.len(),2,"expected <replays-dir> <player-dist>");
    let replays=PathBuf::from(&args[0]).canonicalize().unwrap();
    let dist=PathBuf::from(&args[1]).canonicalize().unwrap();
    eprintln!("native validation: creating context");
    let mut context=tauri::generate_context!();
    context.config_mut().identifier="rocks.tfd.bridge.render-validation".into();
    context.config_mut().app.windows.clear();
    eprintln!("native validation: building runtime");
    tauri::Builder::default().setup(move |app| {
        tauri::WebviewWindowBuilder::new(app,"validation-host",tauri::WebviewUrl::External("about:blank".parse().unwrap())).title("Render validation host").visible(false).build()?;
        eprintln!("native validation: starting bridge");
        let bridge=start_full(replays.clone(),None,None,None,Some(PlayerConfig { share_service: None,
            player_dist:dist.clone(),render_dispatcher:Some(render_worker::dispatcher(app.handle().clone())),
        }))?;
        println!("http://127.0.0.1:{}/player/",bridge.port());
        app.manage(Mutex::new(bridge));
        Ok(())
    }).run(context).expect("native render validation host failed");
}
