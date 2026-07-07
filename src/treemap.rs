use std::sync::Arc;
use egui::{pos2, Rect};
use crate::scanner::FileEntry;

/// Height of the title strip drawn at the top of a nested directory box.
pub const HEADER_H: f32 = 14.0;
/// Padding between a directory's frame and its nested children.
const NEST_PAD: f32 = 2.0;
/// Safety cap on the number of cells produced in a single layout pass.
const MAX_NODES: usize = 80_000;

/// One painted cell in the treemap.
#[derive(Clone)]
pub struct LayoutNode {
    pub rect: Rect,
    pub entry: Arc<FileEntry>,
    pub depth: u32,
    /// True when this is a directory whose children are drawn *inside* it
    /// (a nested container). False for leaves and collapsed directories.
    pub is_container: bool,
}

/// Build a **nested** squarified treemap for `node`.
///
/// Unlike a flat treemap, directories are recursively subdivided so their
/// contents appear as boxes-within-boxes, up to `max_depth` levels deep or
/// until a cell becomes too small to usefully subdivide (`min_size`).
///
/// Cells are emitted parent-before-child, so painting them in order naturally
/// draws children on top of their container.
pub fn build_layout(
    node: &Arc<FileEntry>,
    container: Rect,
    min_size: f32,
    max_depth: u32,
) -> Vec<LayoutNode> {
    let mut out = Vec::new();
    nest(node, container, min_size, 0, max_depth.max(1), &mut out);
    out
}

fn nest(
    node: &Arc<FileEntry>,
    rect: Rect,
    min_size: f32,
    depth: u32,
    max_depth: u32,
    out: &mut Vec<LayoutNode>,
) {
    if out.len() >= MAX_NODES {
        return;
    }
    if rect.width() < min_size || rect.height() < min_size {
        return;
    }

    let children: Vec<&Arc<FileEntry>> =
        node.children.iter().filter(|c| c.size > 0).collect();
    if children.is_empty() {
        return;
    }

    let rects = squarify_level(&children, rect);

    for (child, crect) in children.iter().zip(rects) {
        if crect.width() < min_size || crect.height() < min_size {
            continue;
        }

        // Can we draw this directory's contents nested inside it?
        let can_nest = child.is_dir
            && !child.is_unscanned
            && depth + 1 < max_depth
            && !child.children.is_empty()
            && crect.width() > min_size * 3.0
            && crect.height() > HEADER_H + min_size * 2.0;

        out.push(LayoutNode {
            rect: crect,
            entry: Arc::clone(child),
            depth,
            is_container: can_nest,
        });

        if can_nest {
            let inner = Rect::from_min_max(
                pos2(crect.left() + NEST_PAD, crect.top() + HEADER_H),
                pos2(crect.right() - NEST_PAD, crect.bottom() - NEST_PAD),
            );
            nest(child, inner, min_size, depth + 1, max_depth, out);
        }
    }
}

/// Compute one rectangle per item (aligned to `items` order) using the
/// squarified treemap algorithm. Items are expected to be sorted descending
/// by size for the best aspect ratios, but correctness does not depend on it.
fn squarify_level(items: &[&Arc<FileEntry>], rect: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::NOTHING; items.len()];
    let total: f64 = items.iter().map(|e| e.size as f64).sum();
    if total <= 0.0 || rect.width() < 1.0 || rect.height() < 1.0 {
        return out;
    }

    // Scale sizes so their combined "area" equals the container area.
    let area = rect.width() as f64 * rect.height() as f64;
    let scale = area / total;
    let sizes: Vec<f64> = items.iter().map(|e| (e.size as f64) * scale).collect();

    squarify_rects(&sizes, rect, &mut out);
    out
}

/// Core squarify: pack `sizes` (already scaled to area units) into `rect`,
/// writing the resulting rectangle for each size into `out`.
fn squarify_rects(sizes: &[f64], mut rect: Rect, out: &mut [Rect]) {
    let n = sizes.len();
    let mut start = 0;

    while start < n {
        let w = rect.width() as f64;
        let h = rect.height() as f64;
        let shorter = w.min(h);
        if shorter < 1.0 {
            break;
        }

        // Grow the current row while the worst aspect ratio keeps improving.
        let mut end = start + 1;
        let mut row_sum = sizes[start];
        let mut worst = worst_ratio(&sizes[start..end], shorter);
        while end < n {
            let new_sum = row_sum + sizes[end];
            let new_worst = worst_ratio(&sizes[start..=end], shorter);
            if new_worst <= worst {
                worst = new_worst;
                row_sum = new_sum;
                end += 1;
            } else {
                break;
            }
        }

        rect = place_row(&sizes[start..end], row_sum, rect, &mut out[start..end]);
        start = end;
    }
}

/// Worst (largest) aspect ratio produced by laying `row` along a side of
/// length `w`. Smaller is better (closer to square).
fn worst_ratio(row: &[f64], w: f64) -> f64 {
    let s: f64 = row.iter().sum();
    if s <= 0.0 || w <= 0.0 {
        return f64::MAX;
    }
    let rmax = row.iter().cloned().fold(0.0_f64, f64::max);
    let rmin = row.iter().cloned().fold(f64::MAX, f64::min);
    if rmin <= 0.0 {
        return f64::MAX;
    }
    let a = (w * w * rmax) / (s * s);
    let b = (s * s) / (w * w * rmin);
    a.max(b)
}

/// Lay a single row of `sizes` along the shorter side of `rect`, filling a
/// strip whose thickness makes its area equal `row_sum`. Returns the leftover
/// rectangle after the strip.
fn place_row(sizes: &[f64], row_sum: f64, rect: Rect, out: &mut [Rect]) -> Rect {
    let w = rect.width() as f64;
    let h = rect.height() as f64;
    let count = sizes.len().max(1);

    if w <= h {
        // Horizontal strip across the top.
        let strip_h = (row_sum / w).min(h);
        let mut x = rect.left() as f64;
        for (i, &sz) in sizes.iter().enumerate() {
            let iw = if row_sum > 0.0 { sz / row_sum * w } else { w / count as f64 };
            out[i] = Rect::from_min_max(
                pos2(x as f32, rect.top()),
                pos2((x + iw) as f32, (rect.top() as f64 + strip_h) as f32),
            );
            x += iw;
        }
        Rect::from_min_max(
            pos2(rect.left(), rect.top() + strip_h as f32),
            rect.max,
        )
    } else {
        // Vertical strip down the left.
        let strip_w = (row_sum / h).min(w);
        let mut y = rect.top() as f64;
        for (i, &sz) in sizes.iter().enumerate() {
            let ih = if row_sum > 0.0 { sz / row_sum * h } else { h / count as f64 };
            out[i] = Rect::from_min_max(
                pos2(rect.left(), y as f32),
                pos2((rect.left() as f64 + strip_w) as f32, (y + ih) as f32),
            );
            y += ih;
        }
        Rect::from_min_max(
            pos2(rect.left() + strip_w as f32, rect.top()),
            rect.max,
        )
    }
}
