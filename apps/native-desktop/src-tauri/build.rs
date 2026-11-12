fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let object = out.join("contacts.o");
        let arch = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
            "arm64"
        } else {
            "x86_64"
        };
        assert!(
            std::process::Command::new("xcrun")
                .args([
                    "clang",
                    "-fobjc-arc",
                    "-fmodules",
                    "-arch",
                    arch,
                    "-mmacosx-version-min=26.0",
                    "-c",
                    "native/contacts.m",
                    "-o"
                ])
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("ar")
                .arg("crs")
                .arg(out.join("libmonica_contacts.a"))
                .arg(object)
                .status()
                .unwrap()
                .success()
        );
        let messages = out.join("messages.o");
        assert!(
            std::process::Command::new("xcrun")
                .args([
                    "clang",
                    "-fobjc-arc",
                    "-fmodules",
                    "-arch",
                    arch,
                    "-mmacosx-version-min=26.0",
                    "-c",
                    "native/messages.m",
                    "-o"
                ])
                .arg(&messages)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("ar")
                .arg("crs")
                .arg(out.join("libmonica_contacts.a"))
                .arg(messages)
                .status()
                .unwrap()
                .success()
        );
        println!("cargo:rustc-link-search=native={}", out.display());
        println!("cargo:rustc-link-lib=static=monica_contacts");
        println!("cargo:rustc-link-lib=framework=Contacts");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=sqlite3");
        println!("cargo:rerun-if-changed=native/contacts.m");
        println!("cargo:rerun-if-changed=native/messages.m");
    }
    #[cfg(feature = "desktop")]
    tauri_build::build();
}
