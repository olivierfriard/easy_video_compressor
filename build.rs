use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_PATH");

    let ffmpeg_path = env::var_os("FFMPEG_PATH")
        .map(PathBuf::from)
        .or_else(|| find_in_path("ffmpeg"))
        .unwrap_or_else(|| {
            panic!(
                "ffmpeg introuvable. Installe ffmpeg ou compile avec FFMPEG_PATH=/chemin/ffmpeg cargo build --release"
            )
        });

    if !ffmpeg_path.is_file() {
        panic!("ffmpeg n'est pas un fichier: {}", ffmpeg_path.display());
    }

    println!("cargo:rerun-if-changed={}", ffmpeg_path.display());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR manquant"));
    let embedded_name = if cfg!(windows) {
        "embedded-ffmpeg.exe"
    } else {
        "embedded-ffmpeg"
    };
    let embedded_path = out_dir.join(embedded_name);

    fs::copy(&ffmpeg_path, &embedded_path).unwrap_or_else(|err| {
        panic!(
            "copie de ffmpeg impossible depuis {} vers {}: {err}",
            ffmpeg_path.display(),
            embedded_path.display()
        )
    });

    println!(
        "cargo:rustc-env=EMBEDDED_FFMPEG_PATH={}",
        embedded_path.display()
    );
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }

        #[cfg(windows)]
        {
            let candidate_exe = dir.join(format!("{program}.exe"));
            if candidate_exe.is_file() {
                return Some(candidate_exe);
            }
        }
    }

    known_ffmpeg_locations()
        .into_iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

fn known_ffmpeg_locations() -> Vec<OsString> {
    let mut locations = Vec::new();
    for location in [
        "/usr/bin/ffmpeg",
        "/usr/local/bin/ffmpeg",
        "/opt/homebrew/bin/ffmpeg",
        "/usr/local/opt/ffmpeg/bin/ffmpeg",
    ] {
        locations.push(OsString::from(location));
    }

    if cfg!(windows) {
        for location in [
            r"C:\ffmpeg\bin\ffmpeg.exe",
            r"C:\Program Files\ffmpeg\bin\ffmpeg.exe",
        ] {
            locations.push(OsString::from(location));
        }
    }

    locations
}
