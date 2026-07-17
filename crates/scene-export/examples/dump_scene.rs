//! Throwaway: dump a real replay's scene JSON to stdout for dev previewing.
//! cargo run -q -p scene-export --example dump_scene -- <game_dir> <replay> > out.json
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let game_dir = args.next().expect("game_dir arg");
    let replay = args.next().expect("replay arg");
    let json = scene_export::export_scene_json(Path::new(&game_dir), Path::new(&replay))
        .expect("export_scene_json");
    print!("{json}");
}
