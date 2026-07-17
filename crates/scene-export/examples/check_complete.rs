//! Diagnostic: per-replay completeness signal + raw-packet parse health.
//! Reproduces `recording_complete` (BattleResults presence) and adds packet /
//! error counts so a version-layout break shows up even when BattleResults still
//! happens to match.
//!   cargo run -q -p scene-export --example check_complete -- <dir_or_file> [more...]
use std::path::{Path, PathBuf};
use wows_replays::packet2::{PacketTypeId, RawPacketIterator};
use wows_replays::ReplayFile;
use wowsunpack::data::Version;

fn check(path: &Path) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            println!("ERR read {}: {e}", path.display());
            return;
        }
    };
    let replay = match ReplayFile::from_bytes(&bytes) {
        Ok(r) => r,
        Err(e) => {
            println!("ERR parse {}: {e:?}", path.display());
            return;
        }
    };
    let version = Version::from_client_exe(&replay.meta.clientVersionFromExe);
    let (mut total, mut errs, mut battle_results) = (0u32, 0u32, 0u32);
    for item in RawPacketIterator::with_version(&replay.packet_data, version) {
        match item {
            Ok(pkt) => {
                total += 1;
                if matches!(pkt.packet_type, PacketTypeId::BattleResults) {
                    battle_results += 1;
                }
            }
            Err(_) => errs += 1,
        }
    }
    let ver = replay
        .meta
        .clientVersionFromExe
        .split(',')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    println!(
        "{:<7} complete={:<5} BR={} pkts={:>6} errs={:>5}  {}",
        ver,
        battle_results > 0,
        battle_results,
        total,
        errs,
        name
    );
}

fn main() {
    for arg in std::env::args().skip(1) {
        let p = PathBuf::from(&arg);
        if p.is_dir() {
            let mut files: Vec<_> = std::fs::read_dir(&p)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "wowsreplay").unwrap_or(false))
                .collect();
            files.sort();
            for f in files {
                check(&f);
            }
        } else {
            check(&p);
        }
    }
}
