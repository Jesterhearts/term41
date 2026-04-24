use std::env;
use std::fs;
use std::fs::File;
use std::io;
use std::path::Path;
use std::path::PathBuf;

const APP_ICON_RESOURCE_ID: u32 = 1;

fn main() -> io::Result<()> {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "CARGO_MANIFEST_DIR is not set")
        })?);
    let icon_png = manifest_dir.join("resources/icon.png");

    println!("cargo:rerun-if-changed={}", icon_png.display());

    let target = env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return Ok(());
    }

    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OUT_DIR is not set"))?,
    );
    let icon_ico = out_dir.join("term41.ico");
    let resource_rc = out_dir.join("term41.rc");

    write_windows_icon(&icon_png, &icon_ico)?;
    write_windows_resource(&resource_rc, &icon_ico)?;
    compile_windows_resource(&resource_rc)
}

fn write_windows_icon(
    source_png: &Path,
    target_ico: &Path,
) -> io::Result<()> {
    let image = ico::IconImage::read_png(File::open(source_png)?)?;
    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    icon_dir.add_entry(ico::IconDirEntry::encode(&image)?);
    icon_dir.write(File::create(target_ico)?)
}

fn write_windows_resource(
    target_rc: &Path,
    icon_ico: &Path,
) -> io::Result<()> {
    let icon_path = rc_string_path(icon_ico);
    fs::write(
        target_rc,
        format!("{APP_ICON_RESOURCE_ID} ICON \"{icon_path}\"\n"),
    )
}

fn rc_string_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

fn compile_windows_resource(resource_rc: &Path) -> io::Result<()> {
    embed_resource::compile_for(resource_rc, ["term41"], embed_resource::NONE)
        .manifest_required()
        .map_err(|err| io::Error::other(format!("failed to compile Windows resources: {err}")))
}
