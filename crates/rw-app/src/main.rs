//! RustWorks desktop app: parametric part modeling with a FeatureManager
//! tree, PropertyManager, GPU viewport, and live sync with the MCP server.

#![allow(deprecated)] // egui 0.37 deprecates Id::new; migrate to IdSalt later

mod app;
mod asm_ui;
mod sketcher;
mod viewport;
mod worker;

fn main() -> eframe::Result {
    let open = std::env::args().nth(1).map(std::path::PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustWorks")
            .with_inner_size([1400.0, 880.0])
            .with_min_inner_size([900.0, 560.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native("RustWorks", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, open)))))
}
