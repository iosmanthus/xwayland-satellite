//! The root window's cursor, which every X window that sets none of its own shows.
//! xcb-util-cursor takes its size from XCURSOR_SIZE before `Xcursor.size`, so it
//! loads the logical size, and once cursors are shown at logical size (see
//! `make_cursor_surface` in server/dispatch.rs) that image comes out at a fraction
//! of every other cursor. Load it from the theme at the size X clients render at.

use super::XState;
use log::{debug, warn};
use xcb::render;
use xcb::x;
use xcursor::parser::Image;

/// What the root cursor is made from, read once at startup so a scale change only
/// sends a few requests.
pub(super) struct RootCursorSource {
    images: Vec<Image>,
    argb32: render::Pictformat,
}

impl RootCursorSource {
    /// The theme's `left_ptr` images (XCURSOR_THEME, else "default") and the
    /// server's ARGB32 picture format; `None` when either is missing.
    pub(super) fn load(connection: &xcb::Connection) -> Option<Self> {
        let theme = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_owned());
        let Some(path) = xcursor::CursorTheme::load(&theme).load_icon("left_ptr") else {
            warn!("cursor theme {theme:?} has no left_ptr; the root cursor stays unscaled");
            return None;
        };
        let images = match std::fs::read(&path) {
            Ok(content) => xcursor::parser::parse_xcursor(&content).unwrap_or_default(),
            Err(err) => {
                warn!("cannot read {}: {err}", path.display());
                return None;
            }
        };
        if images.is_empty() {
            warn!("no cursor image in {}", path.display());
            return None;
        }
        let reply = connection
            .wait_for_reply(connection.send_request(&render::QueryPictFormats {}))
            .ok()?;
        let Some(argb32) = reply.formats().iter().find(|f| is_argb32(f)) else {
            warn!("no ARGB32 picture format; the root cursor stays unscaled");
            return None;
        };
        Some(Self {
            images,
            argb32: argb32.id(),
        })
    }
}

/// The image of `images` whose nominal size is closest to `size` (the first one,
/// so an animated cursor's first frame).
fn closest_image(images: &[Image], size: u32) -> Option<&Image> {
    images.iter().min_by_key(|image| image.size.abs_diff(size))
}

fn is_argb32(format: &render::Pictforminfo) -> bool {
    let d = format.direct();
    format.depth() == 32
        && format.r#type() == render::PictType::Direct
        && (d.alpha_shift, d.alpha_mask) == (24, 0xff)
        && (d.red_shift, d.red_mask) == (16, 0xff)
        && (d.green_shift, d.green_mask) == (8, 0xff)
        && (d.blue_shift, d.blue_mask) == (0, 0xff)
}

impl XState {
    /// Sets the root window's cursor to the `left_ptr` image closest to `size`
    /// pixels, unless that image is already set. Without a source the cursor
    /// xcb-util-cursor set at startup stays.
    pub(super) fn set_root_cursor(&mut self, size: u32) {
        let Some(source) = &self.root_cursor_source else {
            return;
        };
        let Some(image) = closest_image(&source.images, size) else {
            return;
        };
        if self.root_cursor.is_some_and(|(_, set)| set == image.size) {
            return;
        }

        let connection = &self.connection;
        let (width, height) = (image.width as u16, image.height as u16);
        let pixmap: x::Pixmap = connection.generate_id();
        let gc: x::Gcontext = connection.generate_id();
        let picture: render::Picture = connection.generate_id();
        let cursor: x::Cursor = connection.generate_id();
        // Checked, then checked together: one round trip, and an error is logged
        // here instead of reaching the event loop.
        let cookies = [
            connection.send_request_checked(&x::CreatePixmap {
                depth: 32,
                pid: pixmap,
                drawable: x::Drawable::Window(self.root),
                width,
                height,
            }),
            connection.send_request_checked(&x::CreateGc {
                cid: gc,
                drawable: x::Drawable::Pixmap(pixmap),
                value_list: &[],
            }),
            // Xcursor files store little-endian ARGB words, the byte order of a
            // depth-32 ZPixmap here.
            connection.send_request_checked(&x::PutImage {
                format: x::ImageFormat::ZPixmap,
                drawable: x::Drawable::Pixmap(pixmap),
                gc,
                width,
                height,
                dst_x: 0,
                dst_y: 0,
                left_pad: 0,
                depth: 32,
                data: &image.pixels_rgba,
            }),
            connection.send_request_checked(&render::CreatePicture {
                pid: picture,
                drawable: x::Drawable::Pixmap(pixmap),
                format: source.argb32,
                value_list: &[],
            }),
            connection.send_request_checked(&render::CreateCursor {
                cid: cursor,
                source: picture,
                x: image.xhot as u16,
                y: image.yhot as u16,
            }),
            connection.send_request_checked(&render::FreePicture { picture }),
            connection.send_request_checked(&x::FreeGc { gc }),
            connection.send_request_checked(&x::FreePixmap { pixmap }),
            connection.send_request_checked(&x::ChangeWindowAttributes {
                window: self.root,
                value_list: &[x::Cw::Cursor(cursor)],
            }),
        ];
        let errors: Vec<_> = cookies
            .into_iter()
            .filter_map(|cookie| connection.check_request(cookie).err())
            .collect();
        if !errors.is_empty() {
            warn!("cannot set the root cursor: {errors:?}");
            return;
        }
        if let Some((old, _)) = self.root_cursor.replace((cursor, image.size)) {
            if let Err(err) = connection.send_and_check_request(&x::FreeCursor { cursor: old }) {
                warn!("cannot free the old root cursor: {err:?}");
            }
        }
        debug!(
            "root cursor: left_ptr {}x{} for size {size}",
            image.width, image.height
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(size: u32, delay: u32) -> Image {
        Image {
            size,
            width: size,
            height: size,
            xhot: 1,
            yhot: 1,
            delay,
            pixels_rgba: vec![0; (4 * size * size) as usize],
            pixels_argb: vec![0; (4 * size * size) as usize],
        }
    }

    #[test]
    fn closest_image_prefers_the_nearest_size_and_the_first_frame() {
        let images = [image(24, 0), image(48, 1), image(48, 2), image(96, 0)];
        assert_eq!(
            closest_image(&images, 48).map(|i| (i.size, i.delay)),
            Some((48, 1))
        );
        assert_eq!(closest_image(&images, 60).map(|i| i.size), Some(48));
        assert_eq!(closest_image(&images, 24).map(|i| i.size), Some(24));
        assert_eq!(closest_image(&images, 200).map(|i| i.size), Some(96));
        assert!(closest_image(&[], 24).is_none());
    }
}
