//! Transcript text panel rendered above the status pill.

use fontdue::Font;
use tiny_skia::{Color, Pixmap};

/// Panel width in pixels (wider than the pill so text is readable).
pub const PANEL_WIDTH: u32 = 380;
pub const PANEL_MAX_LINES: usize = 6;
pub const LINE_HEIGHT: f32 = 17.0;
pub const FONT_SIZE: f32 = 13.0;
pub const PADDING: f32 = 12.0;
pub const GAP_ABOVE_PILL: f32 = 8.0;
pub const PANEL_RADIUS: f32 = 10.0;

const FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
];

pub struct TranscriptPanel {
    font: Font,
}

impl TranscriptPanel {
    pub fn try_new() -> Option<Self> {
        for path in FONT_PATHS {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(font) = Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                    return Some(Self { font });
                }
            }
        }
        None
    }

    pub fn wrap_lines(&self, text: &str, max_width: f32) -> Vec<String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let mut lines = Vec::new();
        let mut current = String::new();

        for word in trimmed.split_whitespace() {
            let candidate = if current.is_empty() {
                word.to_string()
            } else {
                format!("{current} {word}")
            };
            let width = self.measure_line(&candidate);
            if width <= max_width || current.is_empty() {
                current = candidate;
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }

        if lines.len() > PANEL_MAX_LINES {
            lines = lines[lines.len() - PANEL_MAX_LINES..].to_vec();
        }
        lines
    }

    pub fn panel_height(&self, line_count: usize) -> u32 {
        if line_count == 0 {
            return 0;
        }
        let inner = line_count as f32 * LINE_HEIGHT;
        (inner + PADDING * 2.0).ceil() as u32
    }

    pub fn draw(
        &self,
        pixmap: &mut Pixmap,
        lines: &[String],
        panel_y: f32,
        panel_w: f32,
        panel_h: f32,
        bg: Color,
        fg: Color,
        alpha: f32,
    ) {
        if lines.is_empty() || panel_h <= 0.0 || alpha <= 0.0 {
            return;
        }

        draw_round_rect(pixmap, 0.0, panel_y, panel_w, panel_h, PANEL_RADIUS, bg, alpha);

        let mut y = panel_y + PADDING;
        for line in lines {
            self.draw_line(pixmap, line, PADDING, y, fg, alpha);
            y += LINE_HEIGHT;
        }
    }

    fn draw_line(&self, pixmap: &mut Pixmap, line: &str, x0: f32, y0: f32, fg: Color, alpha: f32) {
        let mut x = x0;
        for ch in line.chars() {
            let (metrics, bitmap) = self.font.rasterize(ch, FONT_SIZE);
            let base_x = x as i32;
            let base_y = (y0 + metrics.ymin as f32) as i32;
            for (i, coverage) in bitmap.iter().enumerate() {
                if *coverage == 0 {
                    continue;
                }
                let px = base_x + (i % metrics.width) as i32;
                let py = base_y + (i / metrics.width) as i32;
                if px < 0
                    || py < 0
                    || px >= pixmap.width() as i32
                    || py >= pixmap.height() as i32
                {
                    continue;
                }
                let a = ((*coverage as f32 / 255.0) * alpha * (fg.alpha() as f32 / 255.0) * 255.0)
                    as u8;
                if a == 0 {
                    continue;
                }
                let idx = (py as u32 * pixmap.width() + px as u32) as usize;
                pixmap.pixels_mut()[idx] = tiny_skia::PremultipliedColorU8::from_rgba(
                    ((fg.red() as f32 / 255.0) * a as f32) as u8,
                    ((fg.green() as f32 / 255.0) * a as f32) as u8,
                    ((fg.blue() as f32 / 255.0) * a as f32) as u8,
                    a,
                )
                .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
            }
            x += metrics.advance_width;
        }
    }

    fn measure_line(&self, text: &str) -> f32 {
        text.chars()
            .map(|ch| self.font.rasterize(ch, FONT_SIZE).0.advance_width)
            .sum()
    }
}

fn draw_round_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    color: Color,
    alpha: f32,
) {
    let r = radius.min(w / 2.0).min(h / 2.0);
    let mut path = tiny_skia::PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(x + w - r, y);
    path.quad_to(x + w, y, x + w, y + r);
    path.line_to(x + w, y + h - r);
    path.quad_to(x + w, y + h, x + w - r, y + h);
    path.line_to(x + r, y + h);
    path.quad_to(x, y + h, x, y + h - r);
    path.line_to(x, y + r);
    path.quad_to(x, y, x + r, y);
    path.close();

    let mut paint = tiny_skia::Paint {
        anti_alias: true,
        ..Default::default()
    };
    paint.set_color(Color::from_rgba8(
        (color.red() * 255.0) as u8,
        (color.green() * 255.0) as u8,
        (color.blue() * 255.0) as u8,
        ((color.alpha() as f32 / 255.0) * alpha * 255.0) as u8,
    ));
    if let Some(path) = path.finish() {
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
    }
}
