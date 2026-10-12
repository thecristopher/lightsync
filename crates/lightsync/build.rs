//! Gives LightSync.exe its icon and version details, so Explorer, the Start menu, Settings > Apps
//! and Task Manager all show it properly. The icon comes from the same drawing the tray uses.

fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("lightsync.ico");
    std::fs::write(&ico, lightsync_core::icon::ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).unwrap();

    let version = env!("CARGO_PKG_VERSION");
    let numbers: Vec<&str> = version.split('.').chain(["0"]).take(4).collect();
    let commas = numbers.join(",");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "lightsync (open source)"
      VALUE "FileDescription", "LightSync"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "LightSync"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "LightSync.exe"
      VALUE "ProductName", "LightSync"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = ico.display().to_string().replace('\\', "/"),
    );
    let rc_path = out.join("lightsync.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../lightsync-core/src/icon.rs");
}
