//! Throwaway: dump a real replay's scene JSON to stdout for dev previewing.
//! cargo run -q -p scene-export --example dump_scene -- <game_dir> <replay> [resources_dir] > out.json
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let game_dir = args.next().expect("game_dir arg");
    let replay = args.next().expect("replay arg");
    // Optional third arg: the resources dir (constants.json + constants/).
    let constants = args
        .next()
        .map(|dir| scene_export::constants::ConstantsSet::scan(Path::new(&dir)));
    let json = scene_export::export_scene_json(
        Path::new(&game_dir),
        Path::new(&replay),
        constants.as_ref(),
    )
    .expect("export_scene_json");
    print!("{json}");
}
