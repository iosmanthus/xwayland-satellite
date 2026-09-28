//! Overlays that let X11 clients place their notification windows.
//!
//! Wayland leaves where a toplevel goes to the compositor, but X11 applications place
//! their notification windows themselves: a video call puts its control bars at the
//! bottom of the screen, and moves them as the user drags a handle it draws. A popup
//! can be placed, relative to its parent, so each output gets an overlay: a layer
//! surface above everything else, one transparent pixel at the output's top-left
//! corner that takes no input. A notification window becomes a popup of the overlay
//! on its output, offset by its X position within that output.

use super::clientside::MyWorld;
use super::event::{OutputDimensions, OutputScaleFactor, SurfaceScaleFactor};
use super::{
    Event, InnerServerState, PopupData, ServerState, SurfaceRole, WindowData, WindowOutputOffset,
    X11Selection, XdgSurfaceData,
};
use crate::XConnection;
use crate::xstate::{WindowDims, WindowRole};
use hecs::Entity;
use log::{debug, warn};
use smithay_client_toolkit::registry::SimpleGlobal;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_client::QueueHandle;
use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_output::WlOutput, wl_shm, wl_surface::WlSurface,
};
use wayland_protocols::xdg::shell::client::{
    xdg_positioner::{Anchor as PopupAnchor, ConstraintAdjustment, Gravity},
    xdg_surface::XdgSurface,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use xcb::x;

/// User data of an overlay's surface, whose events are of no interest.
pub(super) struct OverlayMarker;

/// The overlay of an output, a component of the output's entity.
pub(super) struct Overlay {
    surface: WlSurface,
    pub(super) layer: ZwlrLayerSurfaceV1,
    buffer: Option<Buffer>,
    /// Whether the overlay is mapped, so popups can be made of it.
    pub(super) mapped: bool,
}

impl Overlay {
    pub(super) fn new(
        layer_shell: &ZwlrLayerShellV1,
        compositor: &WlCompositor,
        output: &WlOutput,
        entity: Entity,
        qh: &QueueHandle<MyWorld>,
    ) -> Self {
        let surface = compositor.create_surface(qh, OverlayMarker);
        let region = compositor.create_region(qh, ());
        surface.set_input_region(Some(&region));
        region.destroy();

        let layer = layer_shell.get_layer_surface(
            &surface,
            Some(output),
            Layer::Overlay,
            "xwayland-satellite".to_string(),
            qh,
            entity,
        );
        layer.set_size(1, 1);
        layer.set_anchor(Anchor::Top | Anchor::Left);
        // Placed from the output's corner, not from the edge of other surfaces'
        // exclusive zones: offsets within the output are X positions within it.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        surface.commit();

        Self {
            surface,
            layer,
            buffer: None,
            mapped: false,
        }
    }

    fn configure(&mut self, serial: u32, pool: &mut SlotPool) {
        self.layer.ack_configure(serial);
        if self.buffer.is_none() {
            let (buffer, canvas) = match pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888) {
                Ok(b) => b,
                Err(err) => {
                    warn!("Couldn't create a buffer for an output overlay: {err:?}");
                    return;
                }
            };
            canvas.fill(0);
            if let Err(err) = buffer.attach_to(&self.surface) {
                warn!("Couldn't attach an output overlay's buffer: {err:?}");
                return;
            }
            self.buffer = Some(buffer);
        }
        self.surface.commit();
        self.mapped = true;
    }

    pub(super) fn destroy(self) {
        self.layer.destroy();
        self.surface.destroy();
    }
}

impl<S: X11Selection> InnerServerState<S> {
    /// Gives an output its overlay, if the compositor has wlr-layer-shell.
    pub(super) fn add_overlay(&mut self, output: Entity) {
        let Some(layer_shell) = &self.layer_shell else {
            return;
        };
        let client = self.world.get::<&WlOutput>(output).unwrap();
        let overlay = Overlay::new(layer_shell, &self.compositor, &client, output, &self.qh);
        drop(client);
        self.world.insert_one(output, overlay).unwrap();
    }

    pub(super) fn remove_overlay(&mut self, output: Entity) {
        if let Ok(overlay) = self.world.remove_one::<Overlay>(output) {
            overlay.destroy();
        }
    }

    /// The output a notification window at `dims` (in X's coordinates) is on, if it
    /// has a mapped overlay: the one under the window's centre, or failing that the
    /// output of the window the user was last in.
    pub(super) fn overlay_output_for(&self, dims: WindowDims) -> Option<Entity> {
        let centre_x = dims.x as i32 + dims.width as i32 / 2;
        let centre_y = dims.y as i32 + dims.height as i32 / 2;
        let mut query = self.world.query::<(&OutputDimensions, &Overlay)>();
        let under = query.iter().find_map(|(entity, (output, overlay))| {
            let (x, y) = self.output_x_origin(output);
            let (width, height) = output.x_size();
            let inside = (x..x + width).contains(&centre_x) && (y..y + height).contains(&centre_y);
            (overlay.mapped && inside).then_some(entity)
        });
        drop(query);

        under.or_else(|| {
            let window = self.last_focused_toplevel?;
            let on_output = self
                .world
                .get::<&super::OnOutput>(self.windows[&window])
                .ok()?;
            let overlay = self.world.get::<&Overlay>(on_output.0).ok()?;
            overlay.mapped.then_some(on_output.0)
        })
    }

    /// Where `output` starts in X's coordinate space.
    pub(super) fn output_x_origin(&self, output: &OutputDimensions) -> (i32, i32) {
        self.global_output_offset
            .x_origin(output.x, output.y, self.current_scale)
    }
}

impl Event for zwlr_layer_surface_v1::Event {
    fn handle<C: XConnection>(self, target: Entity, state: &mut ServerState<C>) {
        match self {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                // Decorations draw from the same pool.
                let existing = state
                    .world
                    .query::<&SlotPool>()
                    .iter()
                    .next()
                    .map(|(e, _)| e);
                let pool_entity = match existing {
                    Some(entity) => entity,
                    None => match SlotPool::new(1, &SimpleGlobal::from_bound(state.shm.clone())) {
                        Ok(pool) => state.world.spawn((pool,)),
                        Err(err) => {
                            warn!("Couldn't create a pool for output overlays: {err:?}");
                            return;
                        }
                    },
                };
                let mut pool = state.world.get::<&mut SlotPool>(pool_entity).unwrap();
                if let Ok(mut overlay) = state.world.get::<&mut Overlay>(target) {
                    overlay.configure(serial, &mut pool);
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                debug!("output overlay closed");
                state.remove_overlay(target);
            }
            _ => {}
        }
    }
}

impl<S: X11Selection> InnerServerState<S> {
    /// Makes a notification window a popup of `output`'s overlay, placed where its client
    /// put it in X.
    pub(super) fn create_overlay_popup(
        &mut self,
        entity: Entity,
        xdg: XdgSurface,
        output: Entity,
    ) -> PopupData {
        let (origin_x, origin_y, scale) = {
            let output = self.world.entity(output).unwrap();
            let (x, y) = self.output_x_origin(&output.get::<&OutputDimensions>().unwrap());
            (x, y, output.get::<&OutputScaleFactor>().unwrap().get())
        };
        let mut query = self
            .world
            .query_one::<(&mut WindowData, &mut SurfaceScaleFactor)>(entity)
            .unwrap();
        let (window, surface_scale) = query.get().unwrap();
        surface_scale.0 = scale;
        // Where the overlay is, so the popup's position within it is the window's within
        // the output.
        window.output_offset = WindowOutputOffset {
            x: origin_x,
            y: origin_y,
        };
        let dims = window.attrs.dims;
        debug!(
            "creating overlay popup {:?} {dims:?} {entity:?} (scale: {scale})",
            *self.world.get::<&x::Window>(entity).unwrap(),
        );

        let positioner = self.xdg_wm_base.create_positioner(&self.qh, ());
        positioner.set_size(
            1.max((dims.width as f64 / scale).ceil() as i32),
            1.max((dims.height as f64 / scale).ceil() as i32),
        );
        positioner.set_offset(
            ((dims.x as i32 - origin_x) as f64 / scale) as i32,
            ((dims.y as i32 - origin_y) as f64 / scale) as i32,
        );
        positioner.set_anchor_rect(0, 0, 1, 1);
        positioner.set_anchor(PopupAnchor::TopLeft);
        positioner.set_gravity(Gravity::BottomRight);
        positioner
            .set_constraint_adjustment(ConstraintAdjustment::SlideX | ConstraintAdjustment::SlideY);
        let popup = xdg.get_popup(None, &positioner, &self.qh, entity);
        self.world
            .get::<&Overlay>(output)
            .unwrap()
            .layer
            .get_popup(&popup);

        PopupData {
            popup,
            positioner,
            xdg: XdgSurfaceData {
                surface: xdg,
                configured: false,
                pending: None,
            },
        }
    }
}

/// Whether `entity` is a notification window made a popup of an overlay, which its
/// client places.
pub(super) fn is_overlay_popup(window: &WindowData, entity: hecs::EntityRef) -> bool {
    window.attrs.role == WindowRole::Notification
        && matches!(
            entity.get::<&SurfaceRole>().as_deref(),
            Some(SurfaceRole::Popup(_))
        )
}
