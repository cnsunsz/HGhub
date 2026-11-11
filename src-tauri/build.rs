fn main() {
    // 先让 tauri-build 写好 iOS 相关环境，再编译 URLSession 封装。
    tauri_build::build();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        println!("cargo:rerun-if-changed=src/ios_http.m");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=CFNetwork");
        cc::Build::new()
            .file("src/ios_http.m")
            .flag("-fobjc-arc")
            .compile("hghub_ios_http");
    }
}
