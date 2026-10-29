//! Embeds assets/filee-lite.rc (the icon) into filee-lite.exe with the Windows SDK's rc.exe.

fn main() {
    println!("cargo:rerun-if-changed=assets/filee-lite.rc");
    println!("cargo:rerun-if-changed=assets/filee.ico");
    embed_resource::compile("assets/filee-lite.rc", embed_resource::NONE).manifest_optional().unwrap();
}
