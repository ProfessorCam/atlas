use std::sync::Arc;
use egui::Rect;
use crate::scanner::FileEntry;

/// One painted cell in the treemap.
#[derive(Clone)]
pub struct LayoutNode {
    pub rect: Rect,
    pub entry: Arc<FileEntry>,
    pub depth: u32,
}

/// Build a flat list of layout nodes for the children of `node`,
/// fitting them inside `container`.
///
/// Uses the squarified treemap algorithm for good aspect ratios.
pub fn layout(node: &Arc<FileEntry>, container: Rect, min_size: f32) -> Vec<LayoutNode> {
    let total = node.size as f64;
    if total == 0.0 || container.width() < 2.0 || container.height() < 2.0 {
        return vec![];
    }

    let children: Vec<&Arc<FileEntry>> = node
        .children
        .iter()
        .filter(|c| c.size > 0)
        .collect();

    if children.is_empty() {
        return vec![];
    }

    let mut result = Vec::new();
    squarify(
        &children,
        total,
        container,
        0,
        min_size,
        &mut result,
    );
    result
}

fn squarify(
    items: &[&Arc<FileEntry>],
    total: f64,
    rect: Rect,
    depth: u32,
    min_size: f32,
    out: &mut Vec<LayoutNode>,
) {
    if items.is_empty() || rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }

    // Normalise sizes to fill the container area
    let area = (rect.width() as f64) * (rect.height() as f64);
    let scale = area / total;

    let sizes: Vec<f64> = items.iter().map(|e| (e.size as f64) * scale).collect();

    squarify_inner(items, &sizes, 0, rect, depth, min_size, out);
}

fn squarify_inner(
    items: &[&Arc<FileEntry>],
    sizes: &[f64],
    start: usize,
    rect: Rect,
    depth: u32,
    min_size: f32,
    out: &mut Vec<LayoutNode>,
) {
    if start >= items.len() || rect.width() < 1.0 || rect.height() < 1.0 {
        return;
    }

    // Determine the shorter side of the current rectangle
    let w = rect.width() as f64;
    let h = rect.height() as f64;
    let shorter = w.min(h);

    // Find how many items to include in the current row
    // using the squarify criterion: keep adding while aspect ratio improves
    let mut row_end = start + 1;
    let mut row_sum = sizes[start];
    let mut worst = aspect_ratio(shorter, sizes[start], row_sum);

    for i in (start + 1)..items.len() {
        let new_sum = row_sum + sizes[i];
        let new_worst = aspect_ratio_row(shorter, &sizes[start..=i], new_sum);
        if new_worst <= worst {
            row_sum = new_sum;
            worst = new_worst;
            row_end = i + 1;
        } else {
            break;
        }
    }

    // Layout the row [start..row_end] along the shorter side
    let (next_rect, _used) = layout_row(
        &items[start..row_end],
        &sizes[start..row_end],
        row_sum,
        rect,
        depth,
        min_size,
        out,
    );

    // Recurse on the remaining rectangle
    squarify_inner(items, sizes, row_end, next_rect, depth, min_size, out);
}

/// Lay out a single row of items and return the remaining rectangle.
fn layout_row(
    row: &[&Arc<FileEntry>],
    sizes: &[f64],
    row_sum: f64,
    rect: Rect,
    depth: u32,
    min_size: f32,
    out: &mut Vec<LayoutNode>,
) -> (Rect, Rect) {
    let w = rect.width() as f64;
    let h = rect.height() as f64;

    let (strip_w, strip_h, horizontal) = if w >= h {
        // Lay out items vertically in a column of width = row_sum/h
        let cw = (row_sum / h).min(w);
        (cw, h, false)
    } else {
        // Lay out items horizontally in a row of height = row_sum/w
        let ch = (row_sum / w).min(h);
        (w, ch, true)
    };

    let mut pos = if horizontal {
        rect.left_top()
    } else {
        rect.left_top()
    };

    let mut used_strip = egui::Rect::NOTHING;

    for (i, item) in row.iter().enumerate() {
        let frac = if row_sum > 0.0 { sizes[i] / row_sum } else { 1.0 / row.len() as f64 };
        let (item_w, item_h) = if horizontal {
            (frac * strip_w, strip_h)
        } else {
            (strip_w, frac * strip_h)
        };

        let item_rect = egui::Rect::from_min_size(
            pos,
            egui::vec2(item_w as f32, item_h as f32),
        );

        if item_rect.width() >= min_size && item_rect.height() >= min_size {
            out.push(LayoutNode {
                rect: item_rect,
                entry: Arc::clone(item),
                depth,
            });
            used_strip = used_strip.union(item_rect);
        }

        if horizontal {
            pos.x += item_w as f32;
        } else {
            pos.y += item_h as f32;
        }
    }

    // Remaining rectangle after the strip
    let next_rect = if horizontal {
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.top() + strip_h as f32),
            rect.max,
        )
    } else {
        egui::Rect::from_min_max(
            egui::pos2(rect.left() + strip_w as f32, rect.top()),
            rect.max,
        )
    };

    (next_rect, used_strip)
}

fn aspect_ratio(shorter: f64, size: f64, row_sum: f64) -> f64 {
    if row_sum == 0.0 || shorter == 0.0 {
        return f64::MAX;
    }
    let h = size / row_sum * shorter;
    let w = row_sum / shorter;
    if h == 0.0 || w == 0.0 {
        return f64::MAX;
    }
    (w / h).max(h / w)
}

fn aspect_ratio_row(shorter: f64, sizes: &[f64], row_sum: f64) -> f64 {
    sizes
        .iter()
        .map(|&s| aspect_ratio(shorter, s, row_sum))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// Recursively build a full multilevel layout for display at zoom level.
/// Returns layout nodes for the immediate children of `node`, each annotated
/// with their computed rect. Subdirectory contents are NOT expanded at this
/// level (the UI zooms in on click).
pub fn build_layout(node: &Arc<FileEntry>, container: Rect, min_size: f32) -> Vec<LayoutNode> {
    layout(node, container, min_size)
}
