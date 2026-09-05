use std::{env, fs, path::PathBuf};

const PACKAGE_FILES: [&str; 3] = ["WispDisk.sys", "WispDisk.inf", "WispDisk.cat"];

fn main() {
    println!("cargo:rerun-if-env-changed=WISPDISK_DRIVER_PACKAGE_DIR");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo always defines OUT_DIR"));
    let package_dir = env::var_os("WISPDISK_DRIVER_PACKAGE_DIR").map(PathBuf::from);

    for file_name in PACKAGE_FILES {
        let destination = out_dir.join(file_name);
        match &package_dir {
            Some(directory) => {
                let source = directory.join(file_name);
                if !source.is_file() {
                    panic!(
                        "WISPDISK_DRIVER_PACKAGE_DIR is missing required file: {}",
                        source.display()
                    );
                }
                let metadata = fs::metadata(&source).unwrap_or_else(|error| {
                    panic!("failed to inspect {}: {error}", source.display())
                });
                if metadata.len() == 0 {
                    panic!(
                        "embedded driver-package file is empty: {}",
                        source.display()
                    );
                }
                fs::copy(&source, &destination).unwrap_or_else(|error| {
                    panic!(
                        "failed to copy {} to {}: {error}",
                        source.display(),
                        destination.display()
                    )
                });
                println!("cargo:rerun-if-changed={}", source.display());
            }
            None => {
                fs::write(&destination, []).unwrap_or_else(|error| {
                    panic!("failed to create {}: {error}", destination.display())
                });
            }
        }
    }

    println!(
        "cargo:rustc-env=WISPDISK_PACKAGE_EMBEDDED={}",
        u8::from(package_dir.is_some())
    );
}
