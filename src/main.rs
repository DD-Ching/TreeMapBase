mod app;
mod format;
mod model;
mod scanner;
mod treemap;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([900.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "tree-map-base",
        options,
        Box::new(|creation_context| Ok(Box::new(app::TreeMapApp::new(creation_context)))),
    )
}
