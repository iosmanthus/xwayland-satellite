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
use super::event::{CurrentSurface, OutputDimensions, OutputScaleFactor, SurfaceScaleFactor};
use super::model::{OutputId, RawEvent, Role};
use super::{
    Event, GlobalName, InnerServerState, PopupData, ServerState, SurfaceRole, WindowData,
    WindowOutputOffset, X11Selection, XdgSurfaceData,
};
use crate::XConnection;
use crate::xstate::WindowDims;
use hecs::{Entity, World};
use log::{debug, warn};
use smithay_client_toolkit::registry::SimpleGlobal;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use wayland_client::protocol::{
    wl_compositor::WlCompositor, wl_output::WlOutput, wl_shm, wl_surface::WlSurface,
};
use wayland_client::{Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_positioner::{Anchor as PopupAnchor, ConstraintAdjustment, Gravity},
    xdg_surface::XdgSurface,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use wayland_server::Resource;
use wayland_server::protocol as server;
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
        if !self.mapped && self.layer.version() >= 4 {
            // Keys go to a notification window its client wants typed into (Feishu's
            // danmaku input) through its overlay, when it is clicked. Not on the commit that
            // maps the overlay: the compositor gives an overlay that maps taking keys on
            // demand keyboard focus.
            self.layer
                .set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            self.surface.commit();
        }
        self.mapped = true;
    }

    pub(super) fn destroy(self) {
        self.layer.destroy();
        self.surface.destroy();
    }

    pub(super) fn surface(&self) -> &WlSurface {
        &self.surface
    }
}

impl<S: X11Selection + 'static> InnerServerState<S> {
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

    /// The output under the centre of a window at `dims`, if it has a mapped overlay.
    fn overlay_output_under(&self, dims: WindowDims) -> Option<Entity> {
        let centre_x = dims.x as i32 + dims.width as i32 / 2;
        let centre_y = dims.y as i32 + dims.height as i32 / 2;
        let mut query = self.world.query::<(&OutputDimensions, &Overlay)>();
        query.iter().find_map(|(entity, (output, overlay))| {
            let (x, y) = self.output_x_origin(output);
            let (width, height) = output.x_size();
            let inside = (x..x + width).contains(&centre_x) && (y..y + height).contains(&centre_y);
            (overlay.mapped && inside).then_some(entity)
        })
    }

    /// The output an overlay popup at `dims` belongs on, if not the one it is on: the
    /// compositor keeps a popup on its parent's output.
    pub(super) fn overlay_output_moved_to(
        &self,
        entity: Entity,
        dims: WindowDims,
    ) -> Option<Entity> {
        let parent = self.world.get::<&OverlayParent>(entity).ok()?.0;
        self.overlay_output_under(dims)
            .filter(|&output| output != parent)
    }

    /// Makes an overlay popup again on `output`'s overlay.
    pub(super) fn move_overlay_popup(&mut self, entity: Entity, output: Entity) {
        debug!("moving overlay popup {entity:?} to output {output:?}");
        if let Ok(mut role) = self.world.remove_one::<SurfaceRole>(entity) {
            role.destroy();
        }
        let surface = WlSurface::clone(&self.world.get::<&WlSurface>(entity).unwrap());
        // A new role object is for an unmapped surface.
        surface.attach(None, 0, 0);
        surface.commit();
        let xdg = self.xdg_wm_base.get_xdg_surface(&surface, &self.qh, entity);
        let popup = self.create_overlay_popup(entity, xdg, output);
        let last = self
            .world
            .get::<&LastBuffer>(entity)
            .ok()
            .and_then(|last| last.0.clone());
        if last.is_some() {
            self.world
                .insert_one(
                    entity,
                    super::SurfaceAttach {
                        buffer: last,
                        x: 0,
                        y: 0,
                    },
                )
                .unwrap();
        }
        surface.commit();
        self.world
            .insert_one(entity, SurfaceRole::Popup(Some(popup)))
            .unwrap();

        let window = *self.world.get::<&x::Window>(entity).unwrap();
        let output = OutputId(self.world.get::<&GlobalName>(output).unwrap().0);
        self.feed(RawEvent::OverlayRehome { window, output });
    }

    /// Where `output` starts in X's coordinate space.
    pub(super) fn output_x_origin(&self, output: &OutputDimensions) -> (i32, i32) {
        self.global_output_offset
            .x_origin(output.x, output.y, self.current_scale)
    }

    /// The output whose overlay `surface` is.
    pub(super) fn overlay_output_id(&self, surface: &WlSurface) -> Option<OutputId> {
        self.world
            .query::<(&Overlay, &GlobalName)>()
            .iter()
            .find(|(_, (overlay, _))| overlay.surface == *surface)
            .map(|(_, (_, name))| OutputId(name.0))
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
                let mut became_mapped = false;
                if let Ok(mut overlay) = state.world.get::<&mut Overlay>(target) {
                    let was_mapped = overlay.mapped;
                    overlay.configure(serial, &mut pool);
                    became_mapped = !was_mapped && overlay.mapped;
                }
                drop(pool);
                if became_mapped {
                    let output = OutputId(state.world.get::<&GlobalName>(target).unwrap().0);
                    state.feed(RawEvent::OverlayMapped { output });
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                debug!("output overlay closed");
                let output = OutputId(state.world.get::<&GlobalName>(target).unwrap().0);
                state.feed(RawEvent::OverlayClosed { output });
                state.remove_overlay(target);
            }
            _ => {}
        }
    }
}

impl<S: X11Selection + 'static> InnerServerState<S> {
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
        drop(query);
        debug!(
            "creating overlay popup {:?} {dims:?} {entity:?} (scale: {scale})",
            *self.world.get::<&x::Window>(entity).unwrap(),
        );
        self.world
            .insert(
                entity,
                (
                    Placement {
                        x: dims.x.into(),
                        y: dims.y.into(),
                    },
                    OverlayParent(output),
                ),
            )
            .unwrap();

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

/// The output whose overlay an overlay popup is of.
pub(super) struct OverlayParent(Entity);

/// The buffer Xwayland last attached to an overlay popup, for the popup made again on
/// another output: Xwayland attaches none for a window only moved.
pub(super) struct LastBuffer(pub(super) Option<wayland_client::protocol::wl_buffer::WlBuffer>);

/// Where an overlay popup is, in X's coordinates, as far as the compositor has placed it.
///
/// X moves the window as soon as its client asks, and the compositor moves the popup a
/// round trip later. Meanwhile, pointer positions the compositor gives in the popup are
/// from where it was; they are given to X from where the window is now (see
/// [`pointer_offset`]). Otherwise a client that moves the window by where the pointer is in
/// it, as it drags the window, counts each move again, and the window runs away from the
/// pointer.
pub(super) struct Placement {
    pub(super) x: i32,
    pub(super) y: i32,
}

/// User data of a callback done once the compositor has placed a popup at `x`, `y`.
pub(super) struct Placed {
    pub(super) entity: Entity,
    pub(super) x: i32,
    pub(super) y: i32,
}

impl Placed {
    pub(super) fn apply(&self, world: &World) {
        if let Ok(mut placement) = world.get::<&mut Placement>(self.entity) {
            placement.x = self.x;
            placement.y = self.y;
        }
    }
}

/// What to add to a pointer position in `entity`'s surface, in X's pixels, for it to be one
/// in its X window.
pub(super) fn pointer_offset(world: &World, entity: Entity) -> (f64, f64) {
    let Ok(mut query) = world.query_one::<(&Placement, &WindowData)>(entity) else {
        return (0., 0.);
    };
    let Some((placement, window)) = query.get() else {
        return (0., 0.);
    };
    (
        (placement.x - i32::from(window.attrs.dims.x)).into(),
        (placement.y - i32::from(window.attrs.dims.y)).into(),
    )
}

/// Where the pointer is, in X's coordinates, while it is in an overlay popup: where it
/// entered, moved by its relative motion since. A component of the pointer's entity.
///
/// Pointer positions the compositor gives in a popup are from where it has the popup, which
/// is behind where X has the window while its client moves it, and it gives new ones as the
/// popup catches up, as if the pointer had moved. Only relative motion is the pointer moving,
/// so the pointer's position in the window is given from it instead, as X would have it.
pub(super) struct OverlayPointer {
    x: f64,
    y: f64,
}

/// Marks a pointer that satellite gets relative motion for.
pub(super) struct HasRelativeMotion;

/// The pointer a relative pointer is of: a component of the relative pointer's entity.
pub(super) struct RelativeOf(pub(super) Entity);

/// The pointer's position in X as it enters `surface` at `x`, `y` (in the window, in X's
/// pixels), if it goes by relative motion there: it is an overlay popup, and satellite gets
/// relative motion for the pointer.
pub(super) fn overlay_pointer(
    world: &World,
    pointer: Entity,
    surface: Entity,
    (x, y): (f64, f64),
) -> Option<OverlayPointer> {
    if !world
        .satisfies::<&HasRelativeMotion>(pointer)
        .unwrap_or(false)
    {
        return None;
    }
    let mut query = world.query_one::<(&Placement, &WindowData)>(surface).ok()?;
    let (_, window) = query.get()?;
    Some(OverlayPointer {
        x: f64::from(window.attrs.dims.x) + x,
        y: f64::from(window.attrs.dims.y) + y,
    })
}

/// Whether the pointer goes by relative motion, in an overlay popup.
pub(super) fn follows_relative_motion(world: &World, pointer: Entity) -> bool {
    world.satisfies::<&OverlayPointer>(pointer).unwrap_or(false)
}

impl<S: X11Selection + 'static> InnerServerState<S> {
    /// Moves the pointer of `relative` by `dx`, `dy` (logical pixels), if it is in an overlay
    /// popup, and gives X where it now is in the window.
    pub(super) fn overlay_relative_motion(
        &mut self,
        relative: Entity,
        time: u32,
        dx: f64,
        dy: f64,
    ) {
        let Ok(pointer) = self.world.get::<&RelativeOf>(relative).map(|r| r.0) else {
            return;
        };
        let bounds = self.x_screen_bounds();
        let Ok(mut query) = self.world.query_one::<(
            &mut OverlayPointer,
            &CurrentSurface,
            &SurfaceScaleFactor,
            &server::wl_pointer::WlPointer,
        )>(pointer) else {
            return;
        };
        let Some((position, &CurrentSurface::Xwayland(surface), scale, server)) = query.get()
        else {
            return;
        };
        position.x += dx * scale.0;
        position.y += dy * scale.0;
        // The compositor keeps the pointer on its outputs, and goes on giving relative motion.
        if let Some((x0, y0, x1, y1)) = bounds {
            position.x = position.x.clamp(x0, x1);
            position.y = position.y.clamp(y0, y1);
        }
        let Ok(window) = self.world.get::<&WindowData>(surface) else {
            return;
        };
        let dims = window.attrs.dims;
        server.motion(
            time,
            position.x - f64::from(dims.x),
            position.y - f64::from(dims.y),
        );
    }

    /// The bounds of all outputs in X: left, top, right, bottom (the last pixels in).
    fn x_screen_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for (_, output) in self.world.query::<&OutputDimensions>().iter() {
            let (x, y) = self.output_x_origin(output);
            let (width, height) = output.x_size();
            let (x1, y1) = (x + width - 1, y + height - 1);
            bounds = Some(match bounds {
                None => (x, y, x1, y1),
                Some((bx, by, bx1, by1)) => (bx.min(x), by.min(y), bx1.max(x1), by1.max(y1)),
            });
        }
        bounds.map(|(x, y, x1, y1)| (x.into(), y.into(), x1.into(), y1.into()))
    }
}

/// A keyboard whose compositor focus is on `output`'s overlay, with the keys held now: those
/// the compositor said were down when it entered, kept current by its key events, so a
/// re-route enters the new surface with what is really held (Xwayland presses every key an
/// enter lists and releases them all on leave). A component of the keyboard's entity.
pub(super) struct OnOverlay {
    pub(super) output: OutputId,
    pub(super) keys: Vec<u32>,
}

impl OnOverlay {
    /// `keys` as the compositor sends them: native-endian u32 key codes.
    pub(super) fn new(output: OutputId, keys: &[u8]) -> Self {
        let keys = keys
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Self { output, keys }
    }

    /// The held keys as a `wl_keyboard.enter` carries them.
    pub(super) fn keys_array(&self) -> Vec<u8> {
        self.keys.iter().flat_map(|k| k.to_ne_bytes()).collect()
    }

    pub(super) fn key(&mut self, key: u32, pressed: bool) {
        self.keys.retain(|&k| k != key);
        if pressed {
            self.keys.push(key);
        }
    }
}

/// The Xwayland surface a keyboard on an overlay gives keys to (design §2.0).
pub(super) struct Routed(pub(super) Entity);

/// The latest serial the compositor sent on a keyboard (enter or key), for the enters and
/// leaves satellite sends Xwayland itself.
pub(super) struct LastKeyboardSerial(pub(super) u32);

/// The modifiers the compositor last sent on a keyboard, sent again after a re-route.
pub(super) struct LastModifiers {
    pub(super) depressed: u32,
    pub(super) latched: u32,
    pub(super) locked: u32,
    pub(super) group: u32,
}

impl<S: X11Selection> InnerServerState<S> {
    /// Gives the keys of every keyboard on an overlay to `route`'s window (or to nothing):
    /// the surface with keyboard focus is Xwayland's, and X gives keys to the window with
    /// X focus.
    pub(super) fn apply_route(&mut self, route: Option<x::Window>) {
        let target = route.and_then(|w| self.windows.get(&w).copied());
        let keyboards: Vec<Entity> = self
            .world
            .query::<&OnOverlay>()
            .iter()
            .map(|(keyboard, _)| keyboard)
            .collect();
        for keyboard in keyboards {
            let serial = self
                .world
                .get::<&LastKeyboardSerial>(keyboard)
                .map_or(0, |s| s.0);
            if let Ok(Routed(old)) = self.world.remove_one::<Routed>(keyboard)
                && let (Ok(server), Ok(surface)) = (
                    self.world.get::<&server::wl_keyboard::WlKeyboard>(keyboard),
                    self.world.get::<&server::wl_surface::WlSurface>(old),
                )
                && surface.is_alive()
            {
                server.leave(serial, &surface);
            }
            let Some(entity) = target else {
                continue;
            };
            let entered = {
                let keys = self
                    .world
                    .get::<&OnOverlay>(keyboard)
                    .map(|o| o.keys_array())
                    .unwrap_or_default();
                let (Ok(server), Ok(surface)) = (
                    self.world.get::<&server::wl_keyboard::WlKeyboard>(keyboard),
                    self.world.get::<&server::wl_surface::WlSurface>(entity),
                ) else {
                    continue;
                };
                if !surface.is_alive() {
                    continue;
                }
                server.enter(serial, &surface, keys);
                if let Ok(m) = self.world.get::<&LastModifiers>(keyboard) {
                    server.modifiers(serial, m.depressed, m.latched, m.locked, m.group);
                }
                true
            };
            if entered {
                self.world.insert_one(keyboard, Routed(entity)).unwrap();
            }
        }
    }
}

/// Whether `entity` is an overlay window: a popup of an output's overlay its client places.
pub(super) fn is_overlay_window(entity: hecs::EntityRef) -> bool {
    entity
        .get::<&super::Classified>()
        .is_some_and(|c| matches!(c.0.role, Role::OverlayWindow { .. }))
}
