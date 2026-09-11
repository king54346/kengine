//! Offline texture decode utility: output is width:u32 LE, height:u32 LE, then RGBA8.
use kasset::ResourceManager;
use ktexture::{Texture, TextureLoader};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: texture_decode INPUT OUTPUT.rgba".into());
    }
    let manager = ResourceManager::new();
    manager.add_loader(TextureLoader);
    let resource = manager.request_blocking::<Texture>(std::path::PathBuf::from(&args[0]))?;
    let texture = resource.data_ref().ok_or("texture is not ready")?;
    let mut output = Vec::with_capacity(8 + texture.data().len());
    output.extend(texture.width().to_le_bytes());
    output.extend(texture.height().to_le_bytes());
    output.extend(texture.data());
    std::fs::write(&args[1], output)?;
    Ok(())
}
