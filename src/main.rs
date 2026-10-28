#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use eframe::egui;
use rfd::FileDialog;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};

const DEFAULT_CRF: &str = "24";
const VIDEO_CODEC: &str = "libx264";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const EMBEDDED_FFMPEG: &[u8] = include_bytes!(env!("EMBEDDED_FFMPEG_PATH"));
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "qt", "mkv", "avi", "divx", "webm", "flv", "f4v", "wmv", "asf", "mpg",
    "mpeg", "mpe", "mpv", "m2v", "ts", "mts", "m2ts", "vob", "3gp", "3g2", "ogv", "ogg", "mxf",
    "rm", "rmvb",
];

fn main() -> eframe::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args
        .first()
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!("easy_video_compressor {APP_VERSION}");
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "--compress") {
        run_cli_compress(&args[1..]);
        return Ok(());
    }

    run_gui()
}

fn run_gui() -> eframe::Result<()> {
    let app_title = format!("Compressione Video v{APP_VERSION}");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app_title.clone())
            .with_inner_size([800.0, 430.0])
            .with_min_inner_size([640.0, 300.0]),
        ..Default::default()
    };

    eframe::run_native(
        &app_title,
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::light());
            Ok(Box::<VideoCompressApp>::default())
        }),
    )
}

fn run_cli_compress(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: easy_video_compressor --compress <input> [output] [crf]");
        std::process::exit(2);
    }

    let input_path = PathBuf::from(&args[0]);
    let mut output_path = default_output_path(&input_path);
    let mut crf = DEFAULT_CRF.to_owned();

    if let Some(arg) = args.get(1) {
        if valid_crf(arg) {
            crf = arg.clone();
        } else {
            output_path = PathBuf::from(arg);
        }
    }
    if let Some(arg) = args.get(2) {
        crf = arg.clone();
    }

    if !valid_crf(&crf) {
        eprintln!("Errore: CRF non valido. Usa un intero tra 0 e 51.");
        std::process::exit(2);
    }

    println!("Compressione di {}", input_path.display());
    println!("Uscita: {}", output_path.display());
    println!("CRF: {crf}");

    if let Err(err) = compress_video(&input_path, &output_path, &crf) {
        eprintln!("Errore: {err}");
        std::process::exit(1);
    }

    println!("Compressione completata: {}", output_path.display());
}

#[derive(Clone, Copy, PartialEq)]
enum InputMode {
    File,
    Folder,
}

enum CompressionEvent {
    Progress(String),
    Finished(Result<PathBuf, String>),
}

struct VideoCompressApp {
    mode: Option<InputMode>,
    input_path: String,
    output_path: String,
    crf: String,
    status: String,
    running: bool,
    crf_unlocked: bool,
    output_was_auto: bool,
    receiver: Option<Receiver<CompressionEvent>>,
}

impl Default for VideoCompressApp {
    fn default() -> Self {
        Self {
            mode: None,
            input_path: String::new(),
            output_path: String::new(),
            crf: DEFAULT_CRF.to_owned(),
            status: "Scegli un video da comprimere.".to_owned(),
            running: false,
            crf_unlocked: false,
            output_was_auto: true,
            receiver: None,
        }
    }
}

impl eframe::App for VideoCompressApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.accept_dropped_file(ctx);
        self.poll_compression(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Compressione Video");
            ui.label(format!("Versione {APP_VERSION}"));
            ui.add_space(8.0);

            ui.add_enabled_ui(!self.running, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Cosa vuoi comprimere?");
                    let previous = self.mode;
                    ui.selectable_value(&mut self.mode, Some(InputMode::File), "File");
                    ui.selectable_value(&mut self.mode, Some(InputMode::Folder), "Cartella");
                    if self.mode != previous {
                        self.input_path.clear();
                        self.output_path.clear();
                        self.output_was_auto = true;
                        self.status = "Seleziona la sorgente da comprimere.".to_owned();
                    }
                });
            });
            let Some(mode) = self.mode else {
                ui.add_space(16.0);
                ui.label("Scegli File per un singolo video oppure Cartella per tutti i video di una cartella.");
                return;
            };
            let folder_mode = mode == InputMode::Folder;
            ui.add_space(8.0);
            ui.label(if folder_mode { "Cartella sorgente" } else { "File sorgente" });
            ui.horizontal(|ui| {
                let button_width = 110.0;
                let spacing = ui.spacing().item_spacing.x;
                let text_width = (ui.available_width() - button_width - spacing).max(260.0);
                let response = ui.add_enabled(
                    !self.running,
                    egui::TextEdit::singleline(&mut self.input_path)
                        .hint_text(if folder_mode { "/percorso/cartella" } else { "/percorso/video.mp4" })
                        .desired_width(text_width),
                );
                if response.changed() && self.output_was_auto {
                    self.refresh_auto_output();
                }
                if ui
                    .add_enabled(
                        !self.running,
                        egui::Button::new("Scegli...").min_size(egui::vec2(button_width, 24.0)),
                    )
                    .clicked()
                {
                    self.choose_input_file();
                }
            });

            ui.add_space(6.0);
            ui.label(if folder_mode { "Cartella di uscita (automatica)" } else { "File di uscita" });
            ui.horizontal(|ui| {
                let button_width = 145.0;
                let spacing = ui.spacing().item_spacing.x;
                let text_width = (ui.available_width() - button_width - spacing).max(225.0);
                let response = ui.add_enabled(
                    !self.running && !folder_mode,
                    egui::TextEdit::singleline(&mut self.output_path)
                        .hint_text("Automatico se vuoto")
                        .desired_width(text_width),
                );
                if response.changed() {
                    self.output_was_auto = false;
                }
                if ui
                    .add_enabled(
                        !self.running && !folder_mode,
                        egui::Button::new("Salva come...").min_size(egui::vec2(button_width, 24.0)),
                    )
                    .clicked()
                {
                    self.choose_output_file();
                }
            });

            if folder_mode {
                ui.label("Solo i video nella cartella selezionata; le sottocartelle sono escluse.");
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_enabled(
                    !self.running,
                    egui::Checkbox::new(&mut self.crf_unlocked, "Personalizza la compressione"),
                );
                ui.add_enabled(
                    !self.running && self.crf_unlocked,
                    egui::TextEdit::singleline(&mut self.crf).desired_width(60.0),
                );
                ui.label("predefinito 24; 0 = qualita massima, 51 = piu compresso");
            });

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.running, egui::Button::new("Comprimi"))
                    .clicked()
                {
                    self.start_compression();
                }
                if self.running {
                    ui.spinner();
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(&self.status);
        });
    }
}

impl VideoCompressApp {
    fn accept_dropped_file(&mut self, ctx: &egui::Context) {
        if self.running {
            return;
        }

        let dropped = ctx.input(|input| input.raw.dropped_files.clone());
        if let Some(path) = dropped.into_iter().find_map(|file| file.path) {
            self.mode = Some(if path.is_dir() {
                InputMode::Folder
            } else {
                InputMode::File
            });
            self.set_input_path(path);
        }
    }

    fn poll_compression(&mut self, ctx: &egui::Context) {
        if !self.running {
            return;
        }

        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(CompressionEvent::Progress(status)) => {
                    self.status = status;
                    ctx.request_repaint();
                }
                Ok(CompressionEvent::Finished(Ok(output))) => {
                    self.running = false;
                    self.receiver = None;
                    self.status = format!("Compressione completata: {}", output.display());
                }
                Ok(CompressionEvent::Finished(Err(err))) => {
                    self.running = false;
                    self.receiver = None;
                    self.status = format!("Errore: {err}");
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = false;
                    self.receiver = None;
                    self.status = "Errore: compressione interrotta.".to_owned();
                }
            }
        }
    }

    fn choose_input_file(&mut self) {
        if self.mode == Some(InputMode::Folder) {
            if let Some(path) = FileDialog::new()
                .set_title("Seleziona una cartella di video")
                .pick_folder()
            {
                self.set_input_path(path);
            }
            return;
        }
        let extensions = video_filter_extensions();
        let Some(path) = FileDialog::new()
            .set_title("Seleziona un video")
            .add_filter("Video", &extensions)
            .add_filter("Tutti i file", &["*"])
            .pick_file()
        else {
            return;
        };

        self.set_input_path(path);
    }

    fn choose_output_file(&mut self) {
        let mut dialog = FileDialog::new();
        if let Some(default_name) = default_output_path(Path::new(&self.input_path))
            .file_name()
            .and_then(|name| name.to_str())
        {
            dialog = dialog.set_file_name(default_name);
        }

        let Some(path) = dialog.save_file() else {
            return;
        };

        self.output_path = path.display().to_string();
        self.output_was_auto = false;
    }

    fn set_input_path(&mut self, path: PathBuf) {
        self.input_path = path.display().to_string();
        self.output_was_auto = true;
        self.refresh_auto_output();
        self.status = "Pronto per comprimere.".to_owned();
    }

    fn refresh_auto_output(&mut self) {
        let input = self.input_path.trim();
        if input.is_empty() {
            self.output_path.clear();
            return;
        }
        let output = if self.mode == Some(InputMode::Folder) {
            folder_output_path(Path::new(input))
        } else {
            default_output_path(Path::new(input))
        };
        self.output_path = output.display().to_string();
    }

    fn start_compression(&mut self) {
        let input = self.input_path.trim();
        if input.is_empty() {
            self.status = "Errore: sorgente mancante.".to_owned();
            return;
        }

        let input_path = PathBuf::from(input);
        let folder_mode = self.mode == Some(InputMode::Folder);
        if (folder_mode && !input_path.is_dir()) || (!folder_mode && !input_path.is_file()) {
            self.status = format!("Errore: sorgente non trovata: {}", input_path.display());
            return;
        }

        if folder_mode || self.output_path.trim().is_empty() {
            self.output_was_auto = true;
            self.refresh_auto_output();
        }

        let output_path = PathBuf::from(self.output_path.trim());
        if output_path.as_os_str().is_empty() {
            self.status = "Errore: file di uscita mancante.".to_owned();
            return;
        }

        let crf = if self.crf_unlocked {
            self.crf.trim().to_owned()
        } else {
            DEFAULT_CRF.to_owned()
        };
        if !valid_crf(&crf) {
            self.status = "Errore: CRF non valido. Usa un intero tra 0 e 51.".to_owned();
            return;
        }

        if same_path(&input_path, &output_path) {
            self.status = "Errore: l'uscita deve essere diversa dalla sorgente.".to_owned();
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.running = true;
        self.status = "Compressione in corso...".to_owned();

        std::thread::spawn(move || {
            let result = if folder_mode {
                compress_folder(&input_path, &output_path, &crf, |status| {
                    let _ = tx.send(CompressionEvent::Progress(status));
                })
            } else {
                compress_video(&input_path, &output_path, &crf)
            };
            let _ = tx.send(CompressionEvent::Finished(result.map(|_| output_path)));
        });
    }
}

fn video_filter_extensions() -> Vec<String> {
    VIDEO_EXTENSIONS
        .iter()
        .flat_map(|extension| [(*extension).to_owned(), extension.to_ascii_uppercase()])
        .collect()
}

fn default_output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("video");
    let extension = input
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("mp4");
    let file_name = format!("{stem}_compressed.{extension}");

    input
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(file_name)
}

fn folder_output_path(input: &Path) -> PathBuf {
    let mut name = input
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("video"))
        .to_os_string();
    name.push("_compressed");
    input.with_file_name(name)
}

fn folder_video_output_path(input: &Path, output_dir: &Path) -> PathBuf {
    let mut name = input.file_stem().unwrap_or_default().to_os_string();
    name.push("_COMPRESSED");
    if let Some(extension) = input.extension() {
        name.push(".");
        name.push(extension);
    }
    output_dir.join(name)
}

fn folder_videos(input: &Path) -> Result<Vec<PathBuf>, String> {
    let entries =
        fs::read_dir(input).map_err(|err| format!("lettura della cartella impossibile: {err}"))?;
    let mut videos = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| format!("lettura del file impossibile: {err}"))?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    VIDEO_EXTENSIONS
                        .iter()
                        .any(|known| ext.eq_ignore_ascii_case(known))
                })
        {
            videos.push(path);
        }
    }
    videos.sort();
    Ok(videos)
}

fn compress_folder(
    input: &Path,
    output: &Path,
    crf: &str,
    mut progress: impl FnMut(String),
) -> Result<(), String> {
    let videos = folder_videos(input)?;
    if videos.is_empty() {
        return Err("nessun video trovato nella cartella selezionata.".to_owned());
    }
    // Never overwrite an earlier batch or files from a partially completed batch.
    fs::create_dir(output).map_err(|err| {
        format!(
            "creazione della cartella {} impossibile (se esiste già, rinominala o spostala): {err}",
            output.display()
        )
    })?;
    let mut errors = Vec::new();
    for (index, video) in videos.iter().enumerate() {
        progress(format!(
            "Compressione {}/{}: {}",
            index + 1,
            videos.len(),
            video.file_name().unwrap_or_default().to_string_lossy()
        ));
        let destination = folder_video_output_path(video, output);
        if let Err(err) = compress_video(video, &destination, crf) {
            // Remove the incomplete output, retaining successful videos.
            let _ = fs::remove_file(&destination);
            errors.push(format!("{}: {err}", video.display()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} video su {} non compressi. I video completati sono in {}.\n{}",
            errors.len(),
            videos.len(),
            output.display(),
            errors.join("\n")
        ))
    }
}

fn valid_crf(crf: &str) -> bool {
    crf.parse::<u8>().is_ok_and(|value| value <= 51)
}

fn same_path(input: &Path, output: &Path) -> bool {
    if input == output {
        return true;
    }

    let Ok(canonical_input) = input.canonicalize() else {
        return false;
    };

    if let Ok(canonical_output) = output.canonicalize() {
        return canonical_input == canonical_output;
    }

    false
}

fn compress_video(input: &Path, output: &Path, crf: &str) -> Result<(), String> {
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|err| format!("creazione della cartella di uscita impossibile: {err}"))?;
    }

    let ffmpeg = embedded_ffmpeg_path()?;
    let result = Command::new(ffmpeg)
        .arg("-hide_banner")
        .arg("-i")
        .arg(input)
        .arg("-map_metadata")
        .arg("0")
        .arg("-map_metadata:s")
        .arg("0:s")
        .arg("-map_chapters")
        .arg("0")
        .arg("-c:v")
        .arg(VIDEO_CODEC)
        .arg("-preset")
        .arg("medium")
        .arg("-crf")
        .arg(crf)
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg("128k")
        .arg("-movflags")
        .arg("+faststart")
        .arg("-y")
        .arg(output)
        .output()
        .map_err(|err| format!("esecuzione di ffmpeg impossibile: {err}"))?;

    if result.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&result.stderr);
    let stdout = String::from_utf8_lossy(&result.stdout);
    let message = if !stderr.trim().is_empty() {
        stderr.trim().to_owned()
    } else {
        stdout.trim().to_owned()
    };

    Err(if message.is_empty() {
        format!("ffmpeg non riuscito con codice {}", result.status)
    } else {
        message
    })
}

fn embedded_ffmpeg_path() -> Result<PathBuf, String> {
    let hash = fnv1a64(EMBEDDED_FFMPEG);
    let executable_name = if cfg!(windows) {
        format!("ffmpeg-{hash:016x}.exe")
    } else {
        format!("ffmpeg-{hash:016x}")
    };
    let dir = std::env::temp_dir().join("easy_video_compressor");
    let path = dir.join(executable_name);

    if path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.len() == EMBEDDED_FFMPEG.len() as u64)
            .unwrap_or(false)
    {
        return Ok(path);
    }

    fs::create_dir_all(&dir).map_err(|err| {
        format!(
            "creazione della cartella temporanea impossibile ({}): {err}",
            dir.display()
        )
    })?;
    fs::write(&path, EMBEDDED_FFMPEG).map_err(|err| {
        format!(
            "estrazione del ffmpeg integrato impossibile ({}): {err}",
            path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&path)
            .map_err(|err| format!("lettura dei permessi ffmpeg impossibile: {err}"))?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions)
            .map_err(|err| format!("permessi ffmpeg impossibili: {err}"))?;
    }

    Ok(path)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_naming_preserves_dots_and_extension() {
        assert_eq!(
            folder_output_path(Path::new("/tmp/Le vacanze")),
            PathBuf::from("/tmp/Le vacanze_compressed")
        );
        assert_eq!(
            folder_video_output_path(Path::new("film.parte.1.MOV"), Path::new("out")),
            PathBuf::from("out/film.parte.1_COMPRESSED.MOV")
        );
    }

    #[test]
    fn batch_compresses_videos_and_ignores_other_files_and_subfolders() {
        let root = std::env::temp_dir().join(format!(
            "compressor-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let input = root.join("vacanze");
        fs::create_dir_all(input.join("sottocartella")).unwrap();
        fs::write(input.join("note.txt"), "test").unwrap();
        fs::write(input.join("sottocartella/altro.mp4"), "excluded").unwrap();
        let ffmpeg = embedded_ffmpeg_path().unwrap();
        let generated = Command::new(ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=32x32:d=0.1",
                "-c:v",
                "libx264",
            ])
            .arg(input.join("video.parte.MP4"))
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        fs::copy(input.join("video.parte.MP4"), input.join("secondo.mp4")).unwrap();
        let output = folder_output_path(&input);
        let mut updates = Vec::new();
        compress_folder(&input, &output, "24", |status| updates.push(status)).unwrap();
        assert_eq!(updates.len(), 2);
        assert!(
            output
                .join("video.parte_COMPRESSED.MP4")
                .metadata()
                .unwrap()
                .len()
                > 0
        );
        assert!(output.join("secondo_COMPRESSED.mp4").is_file());
        assert_eq!(fs::read_dir(&output).unwrap().count(), 2);
        assert!(compress_folder(&input, &output, "24", |_| {}).is_err());
        fs::write(input.join("danneggiato.mp4"), "invalid video").unwrap();
        let partial_output = root.join("parziale_compressed");
        let error = compress_folder(&input, &partial_output, "24", |_| {}).unwrap_err();
        assert!(error.contains("1 video su 3 non compressi"));
        assert!(partial_output.join("secondo_COMPRESSED.mp4").is_file());
        assert!(!partial_output.join("danneggiato_COMPRESSED.mp4").exists());
        let empty = root.join("vuota");
        fs::create_dir(&empty).unwrap();
        assert!(compress_folder(&empty, &folder_output_path(&empty), "24", |_| {}).is_err());
        assert!(!folder_output_path(&empty).exists());
        fs::remove_dir_all(root).unwrap();
    }
}
