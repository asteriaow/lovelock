#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

pub mod action;
pub mod app;
pub mod bridge_listener;
pub mod deadlock_path;
pub mod logging;
pub mod persistence;
pub mod provider;
pub mod version_check;
use app::CompanionApp;
use eframe::egui;

impl eframe::App for CompanionApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.flush_pending();
    }
}

fn app_icon() -> egui::IconData {
    let image = image::load_from_memory_with_format(
        include_bytes!("../assets/logo.png"),
        image::ImageFormat::Png,
    )
    .expect("embedded application icon must be a valid PNG image")
    .into_rgba8();

    egui::IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    }
}

fn main() -> eframe::Result {
    let log_store = logging::init_logging();
    log::info!(
        target: "companion",
        "process_start version={} os={} arch={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Lovelock Companion")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([980.0, 640.0])
            .with_icon(app_icon()),
        ..Default::default()
    };

    let result = eframe::run_native(
        "Lovelock Companion",
        options,
        Box::new(|creation_context| {
            Ok(Box::new(CompanionApp::load_with_context(
                creation_context.egui_ctx.clone(),
                log_store.clone(),
            )))
        }),
    );
    match &result {
        Ok(()) => log::info!(target: "companion", "process_exit status=success"),
        Err(error) => log::error!(target: "companion", "eframe_launch_failed error={:?}", error),
    }
    result
}
