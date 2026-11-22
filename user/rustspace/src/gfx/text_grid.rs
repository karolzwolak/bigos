use crate::gfx::surface::UserSurface;
use core::cmp::min;
use embedded_graphics::{
    Pixel,
    mono_font::{MonoTextStyle, ascii::FONT_8X13},
    pixelcolor::Rgb888,
    prelude::*,
    text::{Baseline, Text},
};

pub const GLYPH_WIDTH_PX: u32 = 8;
pub const GLYPH_HEIGHT_PX: usize = 13;
pub const CELL_WIDTH_PX: u32 = GLYPH_WIDTH_PX;
pub const CELL_HEIGHT_PX: u32 = 15;
pub const MAX_GRID_COLUMNS: usize = 128;
pub const MAX_GRID_ROWS: usize = 64;
pub const MAX_GRID_CELLS: usize = MAX_GRID_COLUMNS * MAX_GRID_ROWS;
pub const REPLACEMENT_CHAR: u8 = b'?';

const CELL_WIDTH_PIXELS: usize = CELL_WIDTH_PX as usize;
const FIRST_GLYPH: u8 = b' ';
const LAST_GLYPH: u8 = b'~';
const GLYPH_COUNT: usize = (LAST_GLYPH - FIRST_GLYPH + 1) as usize;
const HIGHEST_GLYPH_BIT: u32 = 7;
const WINDOW_BUFFER_COUNT: usize = 2;
const ALL_WINDOW_BUFFERS: u8 = (1 << WINDOW_BUFFER_COUNT) - 1;

static_assertions::const_assert!(FONT_8X13.character_size.width == GLYPH_WIDTH_PX);
static_assertions::const_assert!(FONT_8X13.character_size.height as usize == GLYPH_HEIGHT_PX);

const COLOR_BLACK: u32 = 0x00_00_00;
const COLOR_TEXT: u32 = 0xCC_CC_CC;
const COLOR_DIM: u32 = 0x70_70_70;
const COLOR_CYAN: u32 = 0x55_DD_FF;
const COLOR_SELECTION: u32 = 0x24_4C_8C;
const COLOR_BAR: u32 = 0xFF_D7_5F;
const COLOR_BAR_ERROR: u32 = 0xA0_00_00;
const COLOR_WHITE: u32 = 0xFF_FF_FF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CellStyle {
    Text,
    Dim,
    Directory,
    Selected,
    Header,
    Status,
    Error,
    Cursor,
}

impl CellStyle {
    pub const COUNT: usize = 8;
}

#[derive(Debug, Clone, Copy)]
pub struct CellColors {
    pub foreground_xrgb: u32,
    pub background_xrgb: u32,
}

impl CellColors {
    const fn new(foreground_xrgb: u32, background_xrgb: u32) -> Self {
        Self {
            foreground_xrgb,
            background_xrgb,
        }
    }
}

pub const STYLE_COLORS: [CellColors; CellStyle::COUNT] = [
    CellColors::new(COLOR_TEXT, COLOR_BLACK),
    CellColors::new(COLOR_DIM, COLOR_BLACK),
    CellColors::new(COLOR_CYAN, COLOR_BLACK),
    CellColors::new(COLOR_WHITE, COLOR_SELECTION),
    CellColors::new(COLOR_BLACK, COLOR_BAR),
    CellColors::new(COLOR_BLACK, COLOR_BAR),
    CellColors::new(COLOR_BAR_ERROR, COLOR_BAR),
    CellColors::new(COLOR_BLACK, COLOR_TEXT),
];

pub struct BitmapFont {
    pub rows: [[u8; GLYPH_HEIGHT_PX]; GLYPH_COUNT],
}

impl BitmapFont {
    pub fn font_8x13() -> Self {
        let mut font = Self {
            rows: [[0; GLYPH_HEIGHT_PX]; GLYPH_COUNT],
        };
        let style = MonoTextStyle::new(&FONT_8X13, Rgb888::WHITE);

        for (glyph_index, glyph_rows) in font.rows.iter_mut().enumerate() {
            let character = [FIRST_GLYPH + glyph_index as u8];
            // SAFETY: printable ASCII
            let text = unsafe { core::str::from_utf8_unchecked(&character) };
            let mut bits_target = GlyphBitsTarget { rows: glyph_rows };
            let _ = Text::with_baseline(text, Point::zero(), style, Baseline::Top)
                .draw(&mut bits_target);
        }

        font
    }

    #[inline(always)]
    pub fn glyph(&self, character: u8) -> &[u8; GLYPH_HEIGHT_PX] {
        debug_assert!(is_printable(character));

        &self.rows[(character - FIRST_GLYPH) as usize]
    }
}

struct GlyphBitsTarget<'a> {
    rows: &'a mut [u8; GLYPH_HEIGHT_PX],
}

impl DrawTarget for GlyphBitsTarget<'_> {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let inside = (0..GLYPH_WIDTH_PX as i32).contains(&point.x)
                && (0..GLYPH_HEIGHT_PX as i32).contains(&point.y);
            if inside && color != Rgb888::BLACK {
                self.rows[point.y as usize] |= 0x80 >> point.x;
            }
        }

        Ok(())
    }
}

impl OriginDimensions for GlyphBitsTarget<'_> {
    fn size(&self) -> Size {
        Size::new(GLYPH_WIDTH_PX, GLYPH_HEIGHT_PX as u32)
    }
}

#[inline(always)]
pub const fn is_printable(byte: u8) -> bool {
    byte >= FIRST_GLYPH && byte <= LAST_GLYPH
}

pub struct RowWriter {
    pub chars: [u8; MAX_GRID_COLUMNS],
    pub styles: [CellStyle; MAX_GRID_COLUMNS],
    pub columns: usize,
    pub column: usize,
    pub style: CellStyle,
}

impl RowWriter {
    pub const fn new(columns: usize, fill_style: CellStyle) -> Self {
        debug_assert!(columns <= MAX_GRID_COLUMNS);

        Self {
            chars: [b' '; MAX_GRID_COLUMNS],
            styles: [fill_style; MAX_GRID_COLUMNS],
            columns,
            column: 0,
            style: fill_style,
        }
    }

    pub fn set_style(&mut self, style: CellStyle) {
        self.style = style;
    }

    #[inline(always)]
    pub fn put_byte(&mut self, byte: u8) {
        if self.column < self.columns {
            self.chars[self.column] = if is_printable(byte) {
                byte
            } else {
                REPLACEMENT_CHAR
            };
            self.styles[self.column] = self.style;
        }
        self.column += 1;
    }

    pub fn put_bytes(&mut self, bytes: &[u8]) {
        let start = min(self.column, self.columns);
        let end = min(self.column + bytes.len(), self.columns);

        for (cell, &byte) in self.chars[start..end].iter_mut().zip(bytes) {
            *cell = if is_printable(byte) {
                byte
            } else {
                REPLACEMENT_CHAR
            };
        }
        self.styles[start..end].fill(self.style);
        self.column += bytes.len();
    }

    pub fn pad_to_column(&mut self, column: usize) {
        let start = min(self.column, self.columns);
        let end = min(column, self.columns);
        if start < end {
            self.chars[start..end].fill(b' ');
            self.styles[start..end].fill(self.style);
        }
        self.column = self.column.max(column);
    }

    pub fn set_cell_style(&mut self, column: usize, style: CellStyle) {
        if column < self.columns {
            self.styles[column] = style;
        }
    }
}

impl core::fmt::Write for RowWriter {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.put_bytes(text.as_bytes());

        Ok(())
    }
}

/// Views rebuild their rows whenever something changes, unchanged rows are not drawn again
/// Each row remembers which of the two window buffers still shows an older version of it
pub struct TextGrid {
    pub columns: usize,
    pub rows: usize,
    pub chars: [u8; MAX_GRID_CELLS],
    pub styles: [CellStyle; MAX_GRID_CELLS],
    pub stale_row_buffers: [u8; MAX_GRID_ROWS],
    pub uncleared_margin_buffers: u8,
    pub font: BitmapFont,
}

impl TextGrid {
    pub fn new(width_px: u32, height_px: u32) -> Self {
        let columns = min((width_px / CELL_WIDTH_PX) as usize, MAX_GRID_COLUMNS);
        let rows = min((height_px / CELL_HEIGHT_PX) as usize, MAX_GRID_ROWS);
        debug_assert!(columns > 0 && rows > 0);

        Self {
            columns,
            rows,
            chars: [b' '; MAX_GRID_CELLS],
            styles: [CellStyle::Text; MAX_GRID_CELLS],
            stale_row_buffers: [ALL_WINDOW_BUFFERS; MAX_GRID_ROWS],
            uncleared_margin_buffers: ALL_WINDOW_BUFFERS,
            font: BitmapFont::font_8x13(),
        }
    }

    pub const fn row_writer(&self, fill_style: CellStyle) -> RowWriter {
        RowWriter::new(self.columns, fill_style)
    }

    pub fn commit_row(&mut self, row: usize, writer: &RowWriter) {
        debug_assert!(row < self.rows);
        debug_assert!(writer.columns == self.columns);

        let first_cell = row * self.columns;
        let cells = first_cell..first_cell + self.columns;
        let new_chars = &writer.chars[..self.columns];
        let new_styles = &writer.styles[..self.columns];
        if self.chars[cells.clone()] == *new_chars && self.styles[cells.clone()] == *new_styles {
            return;
        }

        self.chars[cells.clone()].copy_from_slice(new_chars);
        self.styles[cells].copy_from_slice(new_styles);
        self.stale_row_buffers[row] = ALL_WINDOW_BUFFERS;
    }

    pub fn row_chars(&self, row: usize) -> &[u8] {
        debug_assert!(row < self.rows);

        let first_cell = row * self.columns;
        &self.chars[first_cell..first_cell + self.columns]
    }

    /// The row without trailing/padded spaces
    pub fn row_text(&self, row: usize) -> &str {
        let chars = self.row_chars(row).trim_ascii_end();

        // SAFETY: cells only ever hold printable ASCII
        unsafe { core::str::from_utf8_unchecked(chars) }
    }

    pub fn cell_style(&self, row: usize, column: usize) -> CellStyle {
        debug_assert!(row < self.rows && column < self.columns);

        self.styles[row * self.columns + column]
    }

    pub fn has_stale_rows(&self) -> bool {
        self.uncleared_margin_buffers != 0
            || self.stale_row_buffers[..self.rows]
                .iter()
                .any(|&buffers| buffers != 0)
    }

    pub fn render(&mut self, surface: &mut UserSurface) -> bool {
        debug_assert!(surface.back_buffer_index < WINDOW_BUFFER_COUNT);
        debug_assert!(self.columns as u32 * CELL_WIDTH_PX <= surface.width);
        debug_assert!(self.rows as u32 * CELL_HEIGHT_PX <= surface.height);

        let buffer_bit = 1u8 << surface.back_buffer_index;
        let mut drew_anything = false;

        if self.uncleared_margin_buffers & buffer_bit != 0 {
            surface.clear(crate::gfx::color::xrgb_to_rgba(COLOR_BLACK));
            self.uncleared_margin_buffers &= !buffer_bit;
            for stale_buffers in &mut self.stale_row_buffers[..self.rows] {
                *stale_buffers |= buffer_bit;
            }
            drew_anything = true;
        }

        for row in 0..self.rows {
            if self.stale_row_buffers[row] & buffer_bit == 0 {
                continue;
            }

            self.stale_row_buffers[row] &= !buffer_bit;
            self.draw_row(surface, row);
            drew_anything = true;
        }

        drew_anything
    }

    fn draw_row(&self, surface: &mut UserSurface, row: usize) {
        let first_cell = row * self.columns;
        let cells = first_cell..first_cell + self.columns;
        let chars = &self.chars[cells.clone()];
        let styles = &self.styles[cells];
        let top_y = row as u32 * CELL_HEIGHT_PX;
        let row_width_px = self.columns as u32 * CELL_WIDTH_PX;

        for glyph_row in 0..GLYPH_HEIGHT_PX {
            let span = surface.row_span_mut(0, top_y + glyph_row as u32, row_width_px);
            let (cell_spans, _) = span.as_chunks_mut::<CELL_WIDTH_PIXELS>();

            for ((cell_pixels, &character), &style) in cell_spans.iter_mut().zip(chars).zip(styles)
            {
                let colors = STYLE_COLORS[style as usize];
                let bits = self.font.glyph(character)[glyph_row];
                expand_glyph_row(bits, colors, cell_pixels);
            }
        }

        for gap_row in GLYPH_HEIGHT_PX as u32..CELL_HEIGHT_PX {
            let span = surface.row_span_mut(0, top_y + gap_row, row_width_px);
            let (cell_spans, _) = span.as_chunks_mut::<CELL_WIDTH_PIXELS>();

            for (cell_pixels, &style) in cell_spans.iter_mut().zip(styles) {
                cell_pixels.fill(STYLE_COLORS[style as usize].background_xrgb);
            }
        }
    }
}

#[inline(always)]
fn expand_glyph_row(bits: u8, colors: CellColors, cell_pixels: &mut [u32; CELL_WIDTH_PIXELS]) {
    let difference = colors.foreground_xrgb ^ colors.background_xrgb;
    for (bit_index, pixel) in cell_pixels.iter_mut().enumerate() {
        let is_set = ((bits as u32) << bit_index >> HIGHEST_GLYPH_BIT) & 1;
        *pixel = colors.background_xrgb ^ (difference & is_set.wrapping_neg());
    }
}
