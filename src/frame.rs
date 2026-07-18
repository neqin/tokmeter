//! Client-side window frame: rounded corners, border, drop shadow, and
//! edge-resize inside the shadow band. Mirrors Zed's `client_side_decorations`.

use crate::{bg, border};
use gpui::{
    canvas, div, point, prelude::*, px, size, transparent_black, AnyElement, Bounds, BoxShadow,
    CursorStyle, Decorations, Global, HitboxBehavior, Hsla, MouseButton, Pixels, Point, ResizeEdge,
    Size, Stateful, Tiling, Window,
};

const BORDER_SIZE: Pixels = px(1.0);
const WINDOW_ROUNDING: f32 = 10.0;
const WINDOW_SHADOW: f32 = 10.0;

/// Which window corners an element may round when client-decorated: full-height
/// roots round all four, the titlebar only the top pair.
#[derive(Clone, Copy)]
pub enum FrameCorners {
    All,
    Top,
}

/// Round `element`'s window-facing corners to match the frame. No-op under
/// server decorations or on tiled sides.
pub fn rounded<E: Styled>(element: E, corners: FrameCorners, window: &Window) -> E {
    match window.window_decorations() {
        Decorations::Server => element,
        Decorations::Client { tiling } => round_corners(element, corners, tiling),
    }
}

fn round_corners<E: Styled>(element: E, corners: FrameCorners, tiling: Tiling) -> E {
    let radius = px(WINDOW_ROUNDING);
    let top = matches!(corners, FrameCorners::All | FrameCorners::Top);
    let bottom = matches!(corners, FrameCorners::All);
    let mut element = element;
    if top && !(tiling.top || tiling.left) {
        element = element.rounded_tl(radius);
    }
    if top && !(tiling.top || tiling.right) {
        element = element.rounded_tr(radius);
    }
    if bottom && !(tiling.bottom || tiling.left) {
        element = element.rounded_bl(radius);
    }
    if bottom && !(tiling.bottom || tiling.right) {
        element = element.rounded_br(radius);
    }
    element
}

struct GlobalResizeEdge(ResizeEdge);
impl Global for GlobalResizeEdge {}

pub fn window_frame(content: AnyElement, window: &mut Window) -> Stateful<gpui::Div> {
    let decorations = window.window_decorations();
    let tiling = match decorations {
        Decorations::Server => Tiling::default(),
        Decorations::Client { tiling } => tiling,
    };
    let shadow = px(WINDOW_SHADOW);
    match decorations {
        Decorations::Client { .. } => window.set_client_inset(shadow),
        Decorations::Server => window.set_client_inset(px(0.0)),
    }

    div()
        .id("window-frame")
        .bg(transparent_black())
        .map(|el| match decorations {
            Decorations::Server => el,
            Decorations::Client { .. } => round_corners(el, FrameCorners::All, tiling)
                .when(!tiling.top, |el| el.pt(shadow))
                .when(!tiling.bottom, |el| el.pb(shadow))
                .when(!tiling.left, |el| el.pl(shadow))
                .when(!tiling.right, |el| el.pr(shadow))
                .on_mouse_move(move |event, window, cx| {
                    let size = window.window_bounds().get_bounds().size;
                    let new_edge = resize_edge(event.position, shadow, size, tiling);
                    let edge = cx.try_global::<GlobalResizeEdge>();
                    if new_edge != edge.map(|edge| edge.0) {
                        window
                            .window_handle()
                            .update(cx, |root, _, cx| cx.notify(root.entity_id()))
                            .ok();
                    }
                })
                .on_mouse_down(MouseButton::Left, move |event, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    let Some(edge) = resize_edge(event.position, shadow, size, tiling) else {
                        return;
                    };
                    window.start_window_resize(edge);
                }),
        })
        .size_full()
        .child(
            div()
                .cursor(CursorStyle::Arrow)
                .map(|el| match decorations {
                    Decorations::Server => el,
                    Decorations::Client { .. } => round_corners(el, FrameCorners::All, tiling)
                        .overflow_hidden()
                        .border_color(border())
                        .when(!tiling.top, |el| el.border_t(BORDER_SIZE))
                        .when(!tiling.bottom, |el| el.border_b(BORDER_SIZE))
                        .when(!tiling.left, |el| el.border_l(BORDER_SIZE))
                        .when(!tiling.right, |el| el.border_r(BORDER_SIZE))
                        .when(!tiling.is_tiled(), |el| {
                            el.shadow(vec![BoxShadow {
                                color: Hsla {
                                    h: 0.,
                                    s: 0.,
                                    l: 0.,
                                    a: 0.4,
                                },
                                blur_radius: shadow / 2.,
                                spread_radius: px(0.),
                                offset: point(px(0.0), px(0.0)),
                            }])
                        }),
                })
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .size_full()
                .bg(bg())
                .child(content),
        )
        .map(|el| match decorations {
            Decorations::Server => el,
            Decorations::Client { .. } => el.child(
                canvas(
                    |_bounds, window, _| {
                        window.insert_hitbox(
                            Bounds::new(
                                point(px(0.0), px(0.0)),
                                window.window_bounds().get_bounds().size,
                            ),
                            HitboxBehavior::Normal,
                        )
                    },
                    move |_bounds, hitbox, window, cx| {
                        let mouse = window.mouse_position();
                        let size = window.window_bounds().get_bounds().size;
                        let Some(edge) = resize_edge(mouse, shadow, size, tiling) else {
                            return;
                        };
                        cx.set_global(GlobalResizeEdge(edge));
                        window.set_cursor_style(
                            match edge {
                                ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                                ResizeEdge::Left | ResizeEdge::Right => {
                                    CursorStyle::ResizeLeftRight
                                }
                                ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                                    CursorStyle::ResizeUpLeftDownRight
                                }
                                ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                                    CursorStyle::ResizeUpRightDownLeft
                                }
                            },
                            &hitbox,
                        );
                    },
                )
                .size_full()
                .absolute(),
            ),
        })
}

fn resize_edge(
    pos: Point<Pixels>,
    shadow_size: Pixels,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let bounds = Bounds::new(Point::default(), window_size).inset(shadow_size * 1.5);
    if bounds.contains(&pos) {
        return None;
    }

    let corner_size = size(shadow_size * 1.5, shadow_size * 1.5);
    let top_left_bounds = Bounds::new(Point::new(px(0.), px(0.)), corner_size);
    if !tiling.top && top_left_bounds.contains(&pos) {
        return Some(ResizeEdge::TopLeft);
    }

    let top_right_bounds = Bounds::new(
        Point::new(window_size.width - corner_size.width, px(0.)),
        corner_size,
    );
    if !tiling.top && top_right_bounds.contains(&pos) {
        return Some(ResizeEdge::TopRight);
    }

    let bottom_left_bounds = Bounds::new(
        Point::new(px(0.), window_size.height - corner_size.height),
        corner_size,
    );
    if !tiling.bottom && bottom_left_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomLeft);
    }

    let bottom_right_bounds = Bounds::new(
        Point::new(
            window_size.width - corner_size.width,
            window_size.height - corner_size.height,
        ),
        corner_size,
    );
    if !tiling.bottom && bottom_right_bounds.contains(&pos) {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && pos.y < shadow_size {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - shadow_size {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < shadow_size {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - shadow_size {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}
