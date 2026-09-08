//! Export original map layers through the same version-matched VFS as scene export.
//! Usage: cargo run -p scene-export --example export_map_layers -- <game-dir> <client-version> <out-dir>
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read},
    path::Path,
};
use wows_minimap_renderer::assets::{load_map_image, load_map_info};
use wowsunpack::{data::Version, game_data, vfs::VfsPath};

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn export_layer(vfs: &VfsPath, relative: &str, output: &Path) -> Result<Option<Value>> {
    let entry = vfs.join(relative).map_err(|e| anyhow!("{e}"))?;
    if !entry.exists().unwrap_or(false) {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    entry
        .open_file()
        .map_err(|e| anyhow!("{e}"))?
        .read_to_end(&mut bytes)?;
    let image = image::load_from_memory(&bytes)?.to_rgba8();
    let mut alpha = [0_u64; 3];
    let mut total_rgb = [0_u64; 3];
    let mut visible = 0_u64;
    let first = image.get_pixel(0, 0);
    let uniform = image.pixels().all(|p| p == first);
    for pixel in image.pixels() {
        alpha[if pixel[3] == 0 {
            0
        } else if pixel[3] == 255 {
            2
        } else {
            1
        }] += 1;
        if pixel[3] == 255 {
            visible += 1;
            for channel in 0..3 {
                total_rgb[channel] += pixel[channel] as u64;
            }
        }
    }
    let destination = output.join(relative);
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::write(&destination, &bytes)?;
    Ok(Some(json!({
        "path": relative, "bytes": bytes.len(), "sha256": digest(&bytes),
        "pixelSha256": digest(image.as_raw()), "width": image.width(), "height": image.height(),
        "alphaTransparent": alpha[0], "alphaPartial": alpha[1], "alphaOpaque": alpha[2],
        "uniformRgba": if uniform { Some(first.0) } else { None },
        "meanOpaqueRgb": total_rgb.map(|value| if visible > 0 { value as f64 / visible as f64 } else { 0.0 }),
    })))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 3,
        "expected <game-dir> <client-version> <out-dir>"
    );
    let version = Version::from_client_exe(&args[1]);
    let resources = game_data::load_game_resources(Path::new(&args[0]), &version)
        .map_err(|e| anyhow!("loading game resources: {e}"))?;
    let vfs = resources.vfs;
    let output = Path::new(&args[2]);
    fs::create_dir_all(output)?;
    let mut map_dirs: Vec<_> = vfs
        .join("spaces")
        .map_err(|e| anyhow!("{e}"))?
        .read_dir()
        .map_err(|e| anyhow!("reading map directory: {e}"))?
        .collect();
    map_dirs.sort_by_key(|entry| entry.filename());
    let mut maps = Vec::new();
    let mut water_groups = BTreeMap::<String, Vec<String>>::new();
    let mut total_bytes = 0_u64;
    for directory in map_dirs {
        let name = directory.filename();
        ensure!(
            !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']),
            "unsafe map name"
        );
        let id = format!("spaces/{name}");
        let land = export_layer(&vfs, &format!("{id}/minimap.png"), output)
            .with_context(|| format!("land layer {id}"))?;
        let water = export_layer(&vfs, &format!("{id}/minimap_water.png"), output)
            .with_context(|| format!("water layer {id}"))?;
        if land.is_none() && water.is_none() {
            continue;
        }
        for layer in [&land, &water].into_iter().flatten() {
            total_bytes += layer["bytes"].as_u64().unwrap_or(0);
        }
        if let Some(layer) = &water {
            water_groups
                .entry(layer["pixelSha256"].as_str().unwrap().to_owned())
                .or_default()
                .push(id.clone());
        }
        let mut composite = Vec::new();
        let composite_sha256 = if let Some(image) = load_map_image(&id, &vfs) {
            image::DynamicImage::ImageRgb8(image)
                .write_to(&mut Cursor::new(&mut composite), image::ImageFormat::Png)?;
            fs::write(output.join(&id).join("reference.png"), &composite)?;
            Some(digest(&composite))
        } else {
            None
        };
        maps.push(json!({
            "id": id, "spaceSize": load_map_info(&id, &vfs).map(|info| info.space_size),
            "land": land, "water": water, "sceneCompositeSha256": composite_sha256,
        }));
    }
    let manifest = json!({
        "clientVersion": args[1], "source": "version-matched original game VFS",
        "mapCount": maps.len(), "totalLayerBytes": total_bytes,
        "waterPixelGroups": water_groups, "maps": maps,
    });
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    println!(
        "{} maps, {} bytes, {} unique water pixel groups -> {}",
        manifest["mapCount"],
        total_bytes,
        manifest["waterPixelGroups"].as_object().unwrap().len(),
        output.join("manifest.json").display()
    );
    Ok(())
}
