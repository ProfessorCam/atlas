mod app;
mod colors;
mod scanner;
mod treemap;

use app::AtlasApp;

fn main() -> eframe::Result<()> {
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Atlas")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([640.0, 480.0])
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Atlas",
        options,
        Box::new(|cc| Ok(Box::new(AtlasApp::new(cc)))),
    )
}

fn load_icon() -> egui::IconData {
    // Embedded 32x32 RGBA icon generated from our SVG design
    // (transparent background with colored squares motif)
    let size = 32usize;
    let mut rgba = vec![0u8; size * size * 4];

    // Draw a simple treemap-style icon
    // Top-left quadrant: blue (directory)
    fill_rect(&mut rgba, size, 2, 2, 14, 14, [52, 88, 130, 255]);
    // Top-right quadrant: green (images)
    fill_rect(&mut rgba, size, 17, 2, 13, 14, [45, 130, 70, 255]);
    // Bottom-left (tall): orange (archive)
    fill_rect(&mut rgba, size, 2, 17, 8, 13, [180, 90, 30, 255]);
    // Bottom-mid: purple (video)
    fill_rect(&mut rgba, size, 11, 17, 8, 13, [120, 50, 160, 255]);
    // Bottom-right: teal (document)
    fill_rect(&mut rgba, size, 20, 17, 10, 13, [30, 140, 140, 255]);

    // Borders (dark lines between rectangles)
    for i in 0..size {
        set_pixel(&mut rgba, size, 0, i, [10, 10, 15, 255]);
        set_pixel(&mut rgba, size, i, 0, [10, 10, 15, 255]);
        set_pixel(&mut rgba, size, 31, i, [10, 10, 15, 255]);
        set_pixel(&mut rgba, size, i, 31, [10, 10, 15, 255]);
    }

    egui::IconData {
        rgba,
        width: size as u32,
        height: size as u32,
    }
}

fn fill_rect(
    buf: &mut Vec<u8>,
    stride: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    color: [u8; 4],
) {
    for row in y..(y + h).min(stride) {
        for col in x..(x + w).min(stride) {
            set_pixel(buf, stride, col, row, color);
        }
    }
}

fn set_pixel(buf: &mut Vec<u8>, stride: usize, x: usize, y: usize, color: [u8; 4]) {
    let idx = (y * stride + x) * 4;
    if idx + 3 < buf.len() {
        buf[idx] = color[0];
        buf[idx + 1] = color[1];
        buf[idx + 2] = color[2];
        buf[idx + 3] = color[3];
    }
}
