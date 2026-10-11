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

fn main() -> eframe::Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args
        .first()
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!("video_compress_gui {APP_VERSION}");
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
            .with_inner_size([760.0, 360.0])
            .with_min_inner_size([640.0, 300.0]),
        ..Default::default()
    };

    eframe::run_native(
        &app_title,
        options,
        Box::new(|_cc| Ok(Box::<VideoCompressApp>::default())),
    )
}

fn run_cli_compress(args: &[String]) {
    if args.is_empty() {
        eprintln!("Usage: video_compress_gui --compress <input> [output] [crf]");
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

struct VideoCompressApp {
    input_path: String,
    output_path: String,
    crf: String,
    status: String,
    running: bool,
    crf_unlocked: bool,
    output_was_auto: bool,
    receiver: Option<Receiver<Result<PathBuf, String>>>,
}

impl Default for VideoCompressApp {
    fn default() -> Self {
        Self {
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

            ui.label("File sorgente");
            ui.horizontal(|ui| {
                let button_width = 110.0;
                let spacing = ui.spacing().item_spacing.x;
                let text_width = (ui.available_width() - button_width - spacing).max(260.0);
                let response = ui.add_enabled(
                    !self.running,
                    egui::TextEdit::singleline(&mut self.input_path)
                        .hint_text("/percorso/video.mp4")
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
            ui.label("File di uscita");
            ui.horizontal(|ui| {
                let button_width = 145.0;
                let spacing = ui.spacing().item_spacing.x;
                let text_width = (ui.available_width() - button_width - spacing).max(225.0);
                let response = ui.add_enabled(
                    !self.running,
                    egui::TextEdit::singleline(&mut self.output_path)
                        .hint_text("Automatico se vuoto")
                        .desired_width(text_width),
                );
                if response.changed() {
                    self.output_was_auto = false;
                }
                if ui
                    .add_enabled(
                        !self.running,
                        egui::Button::new("Salva come...").min_size(egui::vec2(button_width, 24.0)),
                    )
                    .clicked()
                {
                    self.choose_output_file();
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_enabled(
                    !self.running,
                    egui::Checkbox::new(&mut self.crf_unlocked, "Sblocca CRF"),
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
            self.set_input_path(path);
        }
    }

    fn poll_compression(&mut self, ctx: &egui::Context) {
        if !self.running {
            return;
        }

        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(Ok(output)) => {
                    self.running = false;
                    self.receiver = None;
                    self.status = format!("Compressione completata: {}", output.display());
                }
                Ok(Err(err)) => {
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
        let Some(path) = FileDialog::new()
            .add_filter(
                "Videos",
                &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mts", "m2ts"],
            )
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
        self.output_path = default_output_path(Path::new(input)).display().to_string();
    }

    fn start_compression(&mut self) {
        let input = self.input_path.trim();
        if input.is_empty() {
            self.status = "Errore: file sorgente mancante.".to_owned();
            return;
        }

        let input_path = PathBuf::from(input);
        if !input_path.is_file() {
            self.status = format!("Errore: file non trovato: {}", input_path.display());
            return;
        }

        if self.output_path.trim().is_empty() {
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
            let result = compress_video(&input_path, &output_path, &crf).map(|_| output_path);
            let _ = tx.send(result);
        });
    }
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
    let dir = std::env::temp_dir().join("video_compress_gui");
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
