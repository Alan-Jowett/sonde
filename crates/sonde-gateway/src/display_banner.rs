// SPDX-License-Identifier: MIT
// Copyright (c) 2026 sonde contributors

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::{OriginDimensions, Size};
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_8X13_BOLD};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{Pixel, Point};
use embedded_graphics::text::Text;
use embedded_graphics::Drawable;
use sonde_protocol::modem::DISPLAY_FRAME_BODY_SIZE;

use crate::modem::UsbEspNowTransport;
use crate::transport::TransportError;

pub const FRAMEBUFFER_WIDTH: u32 = 128;
pub const FRAMEBUFFER_HEIGHT: u32 = 64;
const ROW_BYTES: usize = (FRAMEBUFFER_WIDTH as usize) / 8;
const LINE_SPACING: i32 = 4;
const STATUS_LINE_SPACING: i32 = 2;
pub const STATUS_TEXT_COLUMNS: usize =
    (FRAMEBUFFER_WIDTH as usize) / (FONT_6X10.character_size.width as usize);

struct Framebuffer {
    bytes: [u8; DISPLAY_FRAME_BODY_SIZE],
}

impl Framebuffer {
    fn new() -> Self {
        Self {
            bytes: [0u8; DISPLAY_FRAME_BODY_SIZE],
        }
    }

    fn into_bytes(self) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
        self.bytes
    }

    fn set_pixel(&mut self, x: u32, y: u32) {
        if x >= FRAMEBUFFER_WIDTH || y >= FRAMEBUFFER_HEIGHT {
            return;
        }
        let index = (y as usize * ROW_BYTES) + (x as usize / 8);
        let bit = 7 - (x as usize % 8);
        self.bytes[index] |= 1 << bit;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollableFramebuffer {
    height: u32,
    bytes: Vec<u8>,
}

impl ScrollableFramebuffer {
    fn new(height: u32) -> Self {
        let height = height.max(FRAMEBUFFER_HEIGHT);
        Self {
            height,
            bytes: vec![0u8; height as usize * ROW_BYTES],
        }
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn max_offset(&self) -> u32 {
        self.height.saturating_sub(FRAMEBUFFER_HEIGHT)
    }

    pub fn is_scrollable(&self) -> bool {
        self.max_offset() > 0
    }

    pub fn visible_window(&self, offset_y: u32) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
        let mut window = [0u8; DISPLAY_FRAME_BODY_SIZE];
        let offset_y = offset_y.min(self.max_offset());

        for row in 0..FRAMEBUFFER_HEIGHT {
            let src_row = offset_y + row;
            if src_row >= self.height {
                break;
            }

            let src_start = src_row as usize * ROW_BYTES;
            let dst_start = row as usize * ROW_BYTES;
            window[dst_start..dst_start + ROW_BYTES]
                .copy_from_slice(&self.bytes[src_start..src_start + ROW_BYTES]);
        }

        window
    }

    fn set_pixel(&mut self, x: u32, y: u32) {
        if x >= FRAMEBUFFER_WIDTH || y >= self.height {
            return;
        }
        let index = (y as usize * ROW_BYTES) + (x as usize / 8);
        let bit = 7 - (x as usize % 8);
        self.bytes[index] |= 1 << bit;
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(FRAMEBUFFER_WIDTH, FRAMEBUFFER_HEIGHT)
    }
}

impl DrawTarget for Framebuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if color != BinaryColor::On {
                continue;
            }
            let Ok(x) = u32::try_from(point.x) else {
                continue;
            };
            let Ok(y) = u32::try_from(point.y) else {
                continue;
            };
            self.set_pixel(x, y);
        }
        Ok(())
    }
}

impl OriginDimensions for ScrollableFramebuffer {
    fn size(&self) -> Size {
        Size::new(FRAMEBUFFER_WIDTH, self.height)
    }
}

impl DrawTarget for ScrollableFramebuffer {
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if color != BinaryColor::On {
                continue;
            }
            let Ok(x) = u32::try_from(point.x) else {
                continue;
            };
            let Ok(y) = u32::try_from(point.y) else {
                continue;
            };
            self.set_pixel(x, y);
        }
        Ok(())
    }
}

fn centered_text_x(text: &str) -> i32 {
    let character_width = FONT_8X13_BOLD.character_size.width as i32;
    let text_width = (text.chars().count() as i32) * character_width;
    ((FRAMEBUFFER_WIDTH as i32 - text_width).max(0)) / 2
}

pub fn render_display_message(lines: &[&str]) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
    if lines.is_empty() {
        return [0u8; DISPLAY_FRAME_BODY_SIZE];
    }

    let line_height = FONT_8X13_BOLD.character_size.height as i32;
    let block_height =
        line_height * lines.len() as i32 + LINE_SPACING * (lines.len().saturating_sub(1) as i32);
    let top = ((FRAMEBUFFER_HEIGHT as i32 - block_height).max(0)) / 2;
    let style = MonoTextStyle::new(&FONT_8X13_BOLD, BinaryColor::On);
    let mut framebuffer = Framebuffer::new();

    for (index, line) in lines.iter().enumerate() {
        let baseline =
            top + index as i32 * (line_height + LINE_SPACING) + FONT_8X13_BOLD.baseline as i32;
        let _ = Text::new(line, Point::new(centered_text_x(line), baseline), style)
            .draw(&mut framebuffer);
    }

    framebuffer.into_bytes()
}

pub fn render_gateway_version_banner(version: &str) -> [u8; DISPLAY_FRAME_BODY_SIZE] {
    let line2 = format!("v{version}");
    render_display_message(&["Sonde Gateway", &line2])
}

pub fn render_status_text_page(lines: &[String]) -> ScrollableFramebuffer {
    if lines.is_empty() {
        return ScrollableFramebuffer::new(FRAMEBUFFER_HEIGHT);
    }

    let line_height = FONT_6X10.character_size.height as i32;
    let content_height = line_height * lines.len() as i32
        + STATUS_LINE_SPACING * (lines.len().saturating_sub(1) as i32);
    let height = u32::try_from(content_height.max(FRAMEBUFFER_HEIGHT as i32))
        .expect("status page height must fit in u32");
    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let mut framebuffer = ScrollableFramebuffer::new(height);

    for (index, line) in lines.iter().enumerate() {
        let baseline =
            index as i32 * (line_height + STATUS_LINE_SPACING) + FONT_6X10.baseline as i32;
        let _ = Text::new(line, Point::new(0, baseline), style).draw(&mut framebuffer);
    }

    framebuffer
}

pub async fn send_display_message(
    transport: &UsbEspNowTransport,
    lines: &[&str],
) -> Result<(), TransportError> {
    transport
        .send_display_frame(render_display_message(lines))
        .await
}

pub async fn send_gateway_version_banner(
    transport: &UsbEspNowTransport,
) -> Result<(), TransportError> {
    transport
        .send_display_frame(render_gateway_version_banner(env!("CARGO_PKG_VERSION")))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_banner_renders_visible_pixels() {
        let framebuffer = render_gateway_version_banner("0.5.0");
        assert!(
            framebuffer.iter().any(|byte| *byte != 0),
            "rendered banner must set at least one pixel"
        );
    }

    #[test]
    fn gateway_banner_leaves_outer_margins() {
        let framebuffer = render_gateway_version_banner("0.5.0");
        let first_nonzero = framebuffer
            .iter()
            .position(|byte| *byte != 0)
            .expect("banner should render pixels");
        let last_nonzero = framebuffer
            .iter()
            .rposition(|byte| *byte != 0)
            .expect("banner should render pixels");
        assert!(
            first_nonzero > 0,
            "left/top margin should contain at least one empty byte"
        );
        assert!(
            last_nonzero + 1 < framebuffer.len(),
            "right/bottom margin should not consume the full framebuffer"
        );
    }

    #[test]
    fn gateway_banner_renders_across_two_vertical_regions() {
        let framebuffer = render_gateway_version_banner("0.5.0");
        let half = framebuffer.len() / 2;
        assert!(
            framebuffer[..half].iter().any(|byte| *byte != 0),
            "top half should contain the first line"
        );
        assert!(
            framebuffer[half..].iter().any(|byte| *byte != 0),
            "bottom half should contain the second line"
        );
    }

    #[test]
    fn pairing_message_renders_visible_pixels() {
        let framebuffer = render_display_message(&["Pairing"]);
        assert!(
            framebuffer.iter().any(|byte| *byte != 0),
            "rendered pairing message must set at least one pixel"
        );
    }

    #[test]
    fn passkey_message_renders_across_two_vertical_regions() {
        let framebuffer = render_display_message(&["Pin", "123456"]);
        let half = framebuffer.len() / 2;
        assert!(
            framebuffer[..half].iter().any(|byte| *byte != 0),
            "top half should contain the first line"
        );
        assert!(
            framebuffer[half..].iter().any(|byte| *byte != 0),
            "bottom half should contain the second line"
        );
    }

    #[test]
    fn status_text_page_grows_taller_than_display() {
        let lines: Vec<String> = (0..8).map(|i| format!("line {i:02}")).collect();
        let framebuffer = render_status_text_page(&lines);
        assert!(
            framebuffer.height() > FRAMEBUFFER_HEIGHT,
            "many status lines should require a taller off-screen framebuffer"
        );
    }

    #[test]
    fn status_text_page_visible_window_changes_with_offset() {
        let lines: Vec<String> = (0..8).map(|i| format!("line {i:02}")).collect();
        let framebuffer = render_status_text_page(&lines);
        assert!(framebuffer.is_scrollable(), "test page must be scrollable");
        assert_ne!(
            framebuffer.visible_window(0),
            framebuffer.visible_window(2),
            "different offsets should expose different visible windows"
        );
    }
}
