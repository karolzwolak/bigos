use crate::graphics::window::WindowBounds;
use alloc::vec::Vec;
use core::cmp::max;

// pixels on one row
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelSpan {
    pub start: i32,
    pub end: i32,
}

impl PixelSpan {
    pub const fn new(start: i32, end: i32) -> Self {
        Self { start, end }
    }

    pub const fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    pub const fn len(&self) -> usize {
        debug_assert!(!self.is_empty());

        (self.end - self.start) as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerSource {
    Background,
    Window(usize),
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeLayer {
    pub bounds: WindowBounds,
    pub source: LayerSource,
}

impl ComposeLayer {
    pub const fn new(bounds: WindowBounds, source: LayerSource) -> Self {
        Self { bounds, source }
    }

    const fn row_span(&self) -> PixelSpan {
        PixelSpan::new(self.bounds.x, self.bounds.right())
    }

    const fn covers_band(&self, band_top: i32) -> bool {
        !self.bounds.is_empty() && self.bounds.y <= band_top && band_top < self.bounds.bottom()
    }
}

// temporary lists used during visible span calculations
#[derive(Debug, Default)]
pub struct PixelSpanScratch {
    // top and bottom y of every layer
    pub band_edges: Vec<i32>,
    // spans of the covering layers for the current band
    pub occluders: Vec<PixelSpan>,
    // spans handed to draw after occluder subtraction
    pub visible: Vec<PixelSpan>,
}

impl PixelSpanScratch {
    pub const fn new() -> Self {
        Self {
            band_edges: Vec::new(),
            occluders: Vec::new(),
            visible: Vec::new(),
        }
    }
}

/// Appends the parts of `span` that no occluder covers to `visible`, left to right
pub fn subtract_spans(span: PixelSpan, occluders: &mut [PixelSpan], visible: &mut Vec<PixelSpan>) {
    occluders.sort_unstable_by_key(|occluder| occluder.start);

    let mut uncovered_start = span.start;
    for occluder in occluders.iter() {
        if occluder.is_empty() {
            continue;
        }
        if occluder.start >= span.end {
            break;
        }

        if occluder.start > uncovered_start {
            visible.push(PixelSpan::new(uncovered_start, occluder.start));
        }
        uncovered_start = max(uncovered_start, occluder.end);
        if uncovered_start >= span.end {
            return;
        }
    }

    if uncovered_start < span.end {
        visible.push(PixelSpan::new(uncovered_start, span.end));
    }
}

/// we draw spans after subtracting their occluders, so each screen pixel is drawn at
/// most once per frame
pub fn for_each_visible_span(
    layers: &[ComposeLayer],
    scratch: &mut PixelSpanScratch,
    mut draw: impl FnMut(&ComposeLayer, i32, i32, PixelSpan),
) {
    scratch.band_edges.clear();
    for layer in layers.iter().filter(|layer| !layer.bounds.is_empty()) {
        scratch.band_edges.push(layer.bounds.y);
        scratch.band_edges.push(layer.bounds.bottom());
    }
    scratch.band_edges.sort_unstable();
    scratch.band_edges.dedup();

    for edge_index in 1..scratch.band_edges.len() {
        let band_top = scratch.band_edges[edge_index - 1];
        let band_bottom = scratch.band_edges[edge_index];

        for (layer_index, layer) in layers.iter().enumerate() {
            if !layer.covers_band(band_top) {
                continue;
            }

            scratch.occluders.clear();
            for higher_layer in &layers[layer_index + 1..] {
                if higher_layer.covers_band(band_top) {
                    scratch.occluders.push(higher_layer.row_span());
                }
            }

            scratch.visible.clear();
            subtract_spans(
                layer.row_span(),
                &mut scratch.occluders,
                &mut scratch.visible,
            );
            for span in &scratch.visible {
                draw(layer, band_top, band_bottom, *span);
            }
        }
    }
}
