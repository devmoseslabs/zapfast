//! Crops and turns a staged picture over the whole window, before it is sent.

use egui::{Align, Color32, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};

use super::widgets;
use crate::app::{App, Pending};
use crate::model::{
    Action, CropEdge, PictureCrop, PictureEdit, PictureOverlay, PictureOverlaySource, PicturePoint,
    PictureSource, PictureStroke, PictureTool,
};
use crate::theme::{self, Icon};

/// Room left around the cropper, in points.
const MARGIN: f32 = 28.0;
/// Height of the row of controls above and below the picture.
const BAR: f32 = 44.0;
/// Side of a corner or edge handle, in points.
const HANDLE: f32 = 13.0;
/// How far around a handle a drag still counts as gripping it.
const GRIP: f32 = 22.0;
/// The crop's border, in points.
const BORDER: f32 = 2.0;
/// The round button that keeps the crop, like the composer's send button.
const KEEP: f32 = 36.0;

/// A change asked for by the controls under the picture. It is recorded while
/// the window is drawn and applied afterwards, so the edit is not consumed
/// from inside the closure that borrows it.
#[derive(Clone, Copy)]
enum Change {
    Reset,
    Turn { clockwise: bool },
}

/// The cropper's layer.
pub(crate) fn area_id() -> egui::Id {
    egui::Id::new("picture-editor")
}

/// Where the cropper notes its "Keep this crop" button, which focus enters
/// on when the keys or a screen reader opened it.
pub(crate) fn keep_note_id() -> egui::Id {
    egui::Id::new("picture-editor-keep")
}

/// Draws the picture editor over the whole window. The staged attachment is
/// not changed until the complete edit has been rendered successfully.
pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(mut edit) = app.picture_edit.clone() else {
        return;
    };
    let palette = app.palette;
    let locale = app.locale;
    let source = edit.source.clone();
    let pasted = pasted_texture(app, ctx, &edit);
    let screen = ctx.content_rect();
    let mut actions = Vec::new();
    ctx.memory_mut(|memory| {
        memory.set_modal_layer(egui::LayerId::new(egui::Order::Foreground, area_id()));
    });

    egui::Area::new(area_id())
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::hover());
            ui.painter().rect_filled(rect, 0.0, palette.window);

            let room = rect.shrink(MARGIN);
            let top = Rect::from_min_size(room.min, vec2(room.width(), BAR));
            let picker_height = if edit.picker_open { 92.0 } else { 0.0 };
            let bottom_height = BAR + picker_height;
            let bottom = Rect::from_min_size(
                pos2(room.min.x, room.max.y - bottom_height),
                vec2(room.width(), bottom_height),
            );
            let (wide, tall) = edit.displayed_size();
            let inner = Rect::from_min_max(
                pos2(room.min.x, top.max.y),
                pos2(room.max.x, bottom.min.y),
            );
            let picture = fitted(vec2(wide as f32, tall as f32), inner);
            let scale = picture.width() / wide.max(1) as f32;
            let crop_rect = to_screen(edit.crop, picture, scale);

            if let Some(id) = texture(ui, &source, pasted.as_ref(), picture.size()) {
                paint_turned(ui, id, picture, edit.turns);
            }

            // Existing strokes are painted into the live preview, in source
            // coordinates, so their placement remains stable while cropping.
            let picture_painter = ui.painter().with_clip_rect(crop_rect);
            for stroke in &edit.strokes {
                let points: Vec<Pos2> = stroke
                    .points
                    .iter()
                    .map(|point| {
                        pos2(
                            picture.left() + point.x * picture.width(),
                            picture.top() + point.y * picture.height(),
                        )
                    })
                    .collect();
                let color = Color32::from_rgba_unmultiplied(
                    stroke.color[0],
                    stroke.color[1],
                    stroke.color[2],
                    stroke.color[3],
                );
                if points.len() == 1 {
                    picture_painter.circle_filled(
                        points[0],
                        (stroke.width * scale / 2.0).max(0.8),
                        color,
                    );
                } else if points.len() > 1 {
                    picture_painter.add(egui::Shape::line(
                        points,
                        Stroke::new((stroke.width * scale).max(1.0), color),
                    ));
                }
            }

            // Overlay hit targets are only active in Select mode. The same
            // normalized coordinates are used by the raster compositor.
            for index in 0..edit.overlays.len() {
                let overlay = edit.overlays[index].clone();
                let center = pos2(
                    picture.left() + overlay.x * picture.width(),
                    picture.top() + overlay.y * picture.height(),
                );
                let side = (overlay.size * scale).clamp(16.0, picture.width().max(16.0));
                let overlay_rect = Rect::from_center_size(center, Vec2::splat(side));
                match &overlay.source {
                    PictureOverlaySource::Emoji(cluster) => {
                        crate::emoji::paint_cluster(ui, cluster, overlay_rect);
                    }
                    PictureOverlaySource::Sticker(path) => {
                        widgets::file_image(ui, path)
                            .fit_to_exact_size(overlay_rect.size())
                            .paint_at(ui, overlay_rect);
                    }
                }
                if edit.selected_overlay == Some(index) {
                    ui.painter().rect_stroke(
                        overlay_rect.expand(2.0),
                        3.0,
                        Stroke::new(1.5, palette.accent),
                        StrokeKind::Inside,
                    );
                }
                if edit.tool == PictureTool::Select {
                    let response = ui
                        .interact(
                            overlay_rect.expand(3.0),
                            ui.id().with(("picture-overlay", index)),
                            Sense::click_and_drag(),
                        )
                        .on_hover_cursor(egui::CursorIcon::Grab);
                    if response.drag_started() {
                        edit.checkpoint();
                        edit.selected_overlay = Some(index);
                    }
                    if response.dragged() {
                        edit.selected_overlay = Some(index);
                        let delta = response.drag_delta();
                        edit.overlays[index].x =
                            (edit.overlays[index].x + delta.x / picture.width().max(1.0))
                                .clamp(0.0, 1.0);
                        edit.overlays[index].y =
                            (edit.overlays[index].y + delta.y / picture.height().max(1.0))
                                .clamp(0.0, 1.0);
                        ctx.request_repaint();
                    } else if response.clicked() {
                        edit.selected_overlay = Some(index);
                    }
                }
            }

            // Crop and brush have mutually exclusive pointer ownership.
            if edit.tool == PictureTool::Crop {
                let moving = ui.interact(
                    crop_rect,
                    ui.id().with("crop-inside"),
                    Sense::DRAG,
                );
                if moving.drag_started() {
                    edit.checkpoint();
                }
                if moving.dragged() {
                    let delta = moving.drag_delta() / scale.max(0.001);
                    edit.crop = edit.crop.drag(
                        CropEdge::Inside,
                        delta.x.round() as i64,
                        delta.y.round() as i64,
                        wide,
                        tall,
                    );
                }
                for (edge, at) in handles(to_screen(edit.crop, picture, scale)) {
                    let grip = Rect::from_center_size(at, Vec2::splat(GRIP));
                    let response = ui
                        .interact(grip, ui.id().with(("crop-edge", edge as u8)), Sense::DRAG)
                        .on_hover_cursor(cursor(edge));
                    if response.drag_started() {
                        edit.checkpoint();
                    }
                    if response.dragged() {
                        let delta = response.drag_delta() / scale.max(0.001);
                        edit.crop = edit.crop.drag(
                            edge,
                            delta.x.round() as i64,
                            delta.y.round() as i64,
                            wide,
                            tall,
                        );
                    }
                }
            } else if edit.tool == PictureTool::Brush {
                let response = ui
                    .interact(
                        crop_rect,
                        ui.id().with("picture-brush-canvas"),
                        Sense::drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::Crosshair);
                if response.drag_started() {
                    edit.checkpoint();
                    edit.strokes.push(PictureStroke {
                        points: Vec::new(),
                        color: edit.brush_color,
                        width: edit.brush_width,
                    });
                }
                if response.dragged() {
                    if let Some(at) = response.interact_pointer_pos()
                        && crop_rect.contains(at)
                        && let Some(stroke) = edit.strokes.last_mut()
                    {
                        let point = PicturePoint {
                            x: ((at.x - picture.left()) / picture.width().max(1.0))
                                .clamp(0.0, 1.0),
                            y: ((at.y - picture.top()) / picture.height().max(1.0))
                                .clamp(0.0, 1.0),
                        };
                        let add = stroke.points.last().is_none_or(|previous| {
                            let dx = (previous.x - point.x) * picture.width();
                            let dy = (previous.y - point.y) * picture.height();
                            dx * dx + dy * dy >= 4.0
                        });
                        if add {
                            stroke.points.push(point);
                        }
                    }
                    ctx.request_repaint();
                }
            }

            shade_outside(
                ui,
                palette,
                picture,
                to_screen(edit.crop, picture, scale),
                edit.tool == PictureTool::Crop,
            );

            // Tool selection and the existing cancel/send actions share the
            // top bar. Text labels remain readable at narrow window widths.
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(top)
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    for (tool, label) in [
                        (PictureTool::Select, "Select"),
                        (PictureTool::Crop, "Crop"),
                        (PictureTool::Brush, "Draw"),
                        (PictureTool::Emoji, "Emoji"),
                        (PictureTool::Sticker, "Stickers"),
                    ] {
                        if theme::pill_button(ui, &palette, label, edit.tool == tool).clicked() {
                            edit.tool = tool;
                            edit.picker_open =
                                matches!(tool, PictureTool::Emoji | PictureTool::Sticker);
                            edit.picker_query.clear();
                            if tool != PictureTool::Select {
                                edit.selected_overlay = None;
                            }
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let close = crate::i18n::gettext(locale, "Cancel (Esc)");
                        if theme::icon_button(
                            ui,
                            Icon::X,
                            18.0,
                            palette.secondary,
                            palette.text,
                            &close,
                        )
                        .clicked()
                        {
                            actions.push(Action::CancelPictureEdit);
                        }
                        let keep = crate::i18n::gettext(locale, "Send edited image");
                        let kept = theme::circle_button(
                            ui,
                            Icon::Check,
                            KEEP,
                            palette.accent,
                            palette.accent_hover,
                            palette.on_accent,
                            &keep,
                        );
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(keep_note_id(), kept.id));
                        if kept.clicked() {
                            actions.push(Action::ApplyPictureEdit);
                        }
                    });
                },
            );

            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(bottom.min, vec2(bottom.width(), BAR)))
                    .layout(Layout::left_to_right(Align::Center)),
                |ui| {
                    if theme::pill_button(ui, &palette, "Undo", false).clicked() {
                        edit.undo();
                    }
                    if theme::pill_button(ui, &palette, "Redo", false).clicked() {
                        edit.redo();
                    }
                    if theme::icon_button(
                        ui,
                        Icon::RotateLeft,
                        18.0,
                        palette.secondary,
                        palette.text,
                        "Turn left",
                    )
                    .clicked()
                    {
                        edit = edit.turned(false);
                    }
                    if theme::icon_button(
                        ui,
                        Icon::RotateRight,
                        18.0,
                        palette.secondary,
                        palette.text,
                        "Turn right",
                    )
                    .clicked()
                    {
                        edit = edit.turned(true);
                    }
                    if theme::pill_button(ui, &palette, "Reset", false).clicked() {
                        edit = edit.reset();
                    }

                    if edit.tool == PictureTool::Brush {
                        ui.separator();
                        for color in [
                            [255, 255, 255, 255],
                            [255, 82, 82, 255],
                            [255, 214, 10, 255],
                            [45, 190, 110, 255],
                            [70, 145, 255, 255],
                            [25, 25, 25, 255],
                        ] {
                            let (swatch, response) =
                                ui.allocate_exact_size(Vec2::splat(19.0), Sense::click());
                            ui.painter().circle_filled(
                                swatch.center(),
                                7.0,
                                Color32::from_rgba_unmultiplied(
                                    color[0], color[1], color[2], color[3],
                                ),
                            );
                            if edit.brush_color == color {
                                ui.painter().circle_stroke(
                                    swatch.center(),
                                    9.0,
                                    Stroke::new(1.5, palette.accent),
                                );
                            }
                            if response.clicked() {
                                edit.brush_color = color;
                            }
                        }
                        ui.add(
                            egui::Slider::new(&mut edit.brush_width, 2.0..=64.0)
                                .text("Brush")
                                .show_value(false),
                        );
                    }

                    if let Some(index) = edit.selected_overlay
                        && index < edit.overlays.len()
                    {
                        if theme::pill_button(ui, &palette, "−", false).clicked() {
                            edit.checkpoint();
                            edit.overlays[index].size =
                                (edit.overlays[index].size * 0.85).max(12.0);
                        }
                        if theme::pill_button(ui, &palette, "+", false).clicked() {
                            edit.checkpoint();
                            edit.overlays[index].size =
                                (edit.overlays[index].size * 1.18).min(wide as f32 * 0.85);
                        }
                        if theme::pill_button(ui, &palette, "Delete", false).clicked() {
                            edit.checkpoint();
                            edit.overlays.remove(index);
                            edit.selected_overlay = None;
                        }
                    }
                },
            );

            if edit.picker_open {
                let picker_rect = Rect::from_min_size(
                    pos2(bottom.min.x, bottom.min.y + BAR),
                    vec2(bottom.width(), picker_height),
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(picker_rect),
                    |ui| match edit.tool {
                        PictureTool::Emoji => {
                            ui.horizontal(|ui| {
                                ui.label("Search emoji");
                                ui.add(
                                    egui::TextEdit::singleline(&mut edit.picker_query)
                                        .desired_width(180.0)
                                        .hint_text("Search by name"),
                                );
                            });
                            let query = edit.picker_query.trim().to_lowercase();
                            let matches: Vec<&'static str> = emojis::iter()
                                .filter(|emoji| {
                                    query.is_empty()
                                        || emoji.name().to_lowercase().contains(&query)
                                        || emoji.shortcodes().any(|code| {
                                            code.to_lowercase().contains(&query)
                                        })
                                })
                                .take(72)
                                .map(|emoji| emoji.as_str())
                                .collect();
                            egui::ScrollArea::horizontal()
                                .id_salt("picture-emoji-picker")
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        for emoji in matches {
                                            if ui.button(emoji).clicked() {
                                                add_overlay(
                                                    &mut edit,
                                                    PictureOverlaySource::Emoji(emoji.to_owned()),
                                                );
                                            }
                                        }
                                    });
                                });
                        }
                        PictureTool::Sticker => {
                            let stickers = sticker_paths(app);
                            if stickers.is_empty() {
                                ui.label("No stickers available. Add stickers in the chat picker first.");
                            } else {
                                egui::ScrollArea::horizontal()
                                    .id_salt("picture-sticker-picker")
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            for path in stickers {
                                                let (tile, response) = ui.allocate_exact_size(
                                                    Vec2::splat(48.0),
                                                    Sense::click(),
                                                );
                                                widgets::file_image(ui, &path)
                                                    .fit_to_exact_size(tile.size())
                                                    .paint_at(ui, tile);
                                                if response.clicked() {
                                                    add_overlay(
                                                        &mut edit,
                                                        PictureOverlaySource::Sticker(path),
                                                    );
                                                }
                                            }
                                        });
                                    });
                            }
                        }
                        _ => {}
                    },
                );
            }
        });

    app.picture_edit = Some(edit);
    app.actions.extend(actions);
}

/// Adds an overlay centred in the currently selected crop.
fn add_overlay(edit: &mut PictureEdit, source: PictureOverlaySource) {
    let (width, height) = edit.displayed_size();
    let crop = edit.crop.clamped(width, height);
    edit.checkpoint();
    edit.overlays.push(PictureOverlay {
        source,
        x: (crop.x as f32 + crop.width as f32 / 2.0) / width.max(1) as f32,
        y: (crop.y as f32 + crop.height as f32 / 2.0) / height.max(1) as f32,
        size: (width.min(height) as f32 * 0.18).clamp(24.0, (width as f32 * 0.6).max(24.0)),
    });
    edit.selected_overlay = Some(edit.overlays.len() - 1);
    edit.tool = PictureTool::Select;
    edit.picker_open = false;
}

/// Recent, saved, received and installed-pack stickers, deduplicated by path.
fn sticker_paths(app: &App) -> Vec<std::path::PathBuf> {
    let mut seen = std::collections::HashSet::new();
    app.stickers
        .iter()
        .chain(app.stickers_saved.iter())
        .chain(app.stickers_received.iter())
        .chain(app.sticker_packs.iter().flat_map(|pack| pack.stickers.iter()))
        .filter(|path| path.is_file() && seen.insert((*path).clone()))
        .cloned()
        .take(120)
        .collect()
}

/// The largest rectangle of `shape`'s proportions centred in `room`.
fn fitted(shape: Vec2, room: Rect) -> Rect {
    let scale = (room.width() / shape.x.max(1.0)).min(room.height() / shape.y.max(1.0));
    Rect::from_center_size(room.center(), shape * scale)
}

/// The texture to draw a pasted picture with, built from its buffer when the
/// strip no longer has it. Cropping a pasted picture leaves a file on disk and
/// takes the strip's copy of the pixels with it, so on the second visit the
/// pixels have to come from the buffer the crop was taken from.
fn pasted_texture(
    app: &mut App,
    ctx: &egui::Context,
    edit: &crate::model::PictureEdit,
) -> Option<egui::TextureHandle> {
    if let Some(at) = app.staged_at(edit.target)
        && let Some(Pending::Picture {
            texture: Some(handle),
            ..
        }) = app.pending.get(at).map(|staged| &staged.item)
    {
        return Some(handle.clone());
    }
    let PictureSource::Pasted(rgba) = &edit.source else {
        return None;
    };
    let key = std::sync::Arc::as_ptr(rgba) as usize;
    if let Some((cached, handle)) = &app.picture_texture
        && *cached == key
    {
        return Some(handle.clone());
    }
    let image = pasted_image(rgba, edit.width, edit.height)?;
    let handle = ctx.load_texture(
        format!("edited-picture-{key}"),
        image,
        egui::TextureOptions::LINEAR,
    );
    app.picture_texture = Some((key, handle.clone()));
    Some(handle)
}

/// A pasted buffer as an image to upload, capped the way the strip caps its own
/// copy so a large paste does not meet the GPU's texture limit here.
fn pasted_image(rgba: &[u8], width: u32, height: u32) -> Option<egui::ColorImage> {
    let full = image::RgbaImage::from_raw(width.max(1), height.max(1), rgba.to_vec())?;
    let small = if width > 1024 || height > 1024 {
        let scale = 1024.0 / width.max(height) as f32;
        let (wide, tall) = (
            ((width as f32 * scale) as u32).max(1),
            ((height as f32 * scale) as u32).max(1),
        );
        image::imageops::resize(&full, wide, tall, image::imageops::FilterType::Triangle)
    } else {
        full
    };
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [small.width() as usize, small.height() as usize],
        &small,
    ))
}

/// The picture's texture, once it is ready to draw.
fn texture(
    ui: &mut egui::Ui,
    source: &PictureSource,
    pasted: Option<&egui::TextureHandle>,
    size: Vec2,
) -> Option<egui::TextureId> {
    match source {
        PictureSource::File(path) => {
            let image = widgets::file_image(ui, path);
            match image.load_for_size(ui.ctx(), size) {
                Ok(egui::load::TexturePoll::Ready { texture }) => Some(texture.id),
                _ => None,
            }
        }
        PictureSource::Pasted(_) => pasted.map(egui::TextureHandle::id),
    }
}

/// Which corner of the texture each corner of the picture takes, turned by
/// whole quarter turns clockwise. Read as the picture's own corners, going
/// clockwise from the top left.
fn turned_corners(turns: u8) -> [Pos2; 4] {
    let quarter = usize::from(turns % 4);
    let corners = [
        Pos2::new(0.0, 0.0),
        Pos2::new(1.0, 0.0),
        Pos2::new(1.0, 1.0),
        Pos2::new(0.0, 1.0),
    ];
    [
        corners[(4 - quarter) % 4],
        corners[(5 - quarter) % 4],
        corners[(6 - quarter) % 4],
        corners[(7 - quarter) % 4],
    ]
}

/// Draws the picture into `rect`, turned by whole quarter turns clockwise.
/// The turn is taken out of the texture's own corners, so the pixels are
/// never copied and turning a picture costs nothing.
fn paint_turned(ui: &egui::Ui, texture: egui::TextureId, rect: Rect, turns: u8) {
    let places = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let mut mesh = egui::epaint::Mesh {
        texture_id: texture,
        ..Default::default()
    };
    for (place, uv) in places.into_iter().zip(turned_corners(turns)) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: place,
            uv,
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// Where a crop's edges and corners are on screen.
fn handles(crop: Rect) -> [(CropEdge, Pos2); 8] {
    [
        (CropEdge::TopLeft, crop.left_top()),
        (CropEdge::Top, crop.center_top()),
        (CropEdge::TopRight, crop.right_top()),
        (CropEdge::Right, crop.right_center()),
        (CropEdge::BottomRight, crop.right_bottom()),
        (CropEdge::Bottom, crop.center_bottom()),
        (CropEdge::BottomLeft, crop.left_bottom()),
        (CropEdge::Left, crop.left_center()),
    ]
}

/// The crop's place on screen, given where the picture is drawn.
fn to_screen(crop: PictureCrop, picture: Rect, scale: f32) -> Rect {
    Rect::from_min_size(
        picture.min + vec2(crop.x as f32 * scale, crop.y as f32 * scale),
        vec2(crop.width as f32 * scale, crop.height as f32 * scale),
    )
}

/// The cursor that fits the edge being dragged.
fn cursor(edge: CropEdge) -> egui::CursorIcon {
    match edge {
        CropEdge::Left | CropEdge::Right => egui::CursorIcon::ResizeHorizontal,
        CropEdge::Top | CropEdge::Bottom => egui::CursorIcon::ResizeVertical,
        CropEdge::TopLeft | CropEdge::BottomRight => egui::CursorIcon::ResizeNwSe,
        CropEdge::TopRight | CropEdge::BottomLeft => egui::CursorIcon::ResizeNeSw,
        CropEdge::Inside => egui::CursorIcon::Grab,
    }
}

/// Fades what the crop leaves out toward the window, and marks the crop's
/// border and handles in the theme's accent.
fn shade_outside(
    ui: &egui::Ui,
    palette: theme::Palette,
    picture: Rect,
    crop: Rect,
    show_handles: bool,
) {
    let shade = palette.window.gamma_multiply(0.7);
    for outside in [
        Rect::from_min_max(picture.min, pos2(picture.max.x, crop.min.y)),
        Rect::from_min_max(pos2(picture.min.x, crop.max.y), picture.max),
        Rect::from_min_max(
            pos2(picture.min.x, crop.min.y),
            pos2(crop.min.x, crop.max.y),
        ),
        Rect::from_min_max(
            pos2(crop.max.x, crop.min.y),
            pos2(picture.max.x, crop.max.y),
        ),
    ] {
        if outside.is_positive() {
            ui.painter().rect_filled(outside, 0.0, shade);
        }
    }
    ui.painter().rect_stroke(
        crop,
        0.0,
        Stroke::new(BORDER, palette.accent),
        StrokeKind::Inside,
    );
    if show_handles {
        for (_, at) in handles(crop) {
            let handle = Rect::from_center_size(at, Vec2::splat(HANDLE));
            ui.painter().rect_filled(handle, 2.0, palette.accent);
            ui.painter().rect_stroke(
                handle,
                2.0,
                Stroke::new(1.0, palette.on_accent),
                StrokeKind::Inside,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_fills_the_room_in_one_direction() {
        let room = Rect::from_min_size(egui::pos2(10.0, 10.0), vec2(1000.0, 500.0));
        let wide = fitted(vec2(1920.0, 1080.0), room);
        assert!((wide.height() - 500.0).abs() < 0.01);
        assert!((wide.width() - 500.0 * 16.0 / 9.0).abs() < 0.01);
        assert_eq!(wide.center(), room.center());
        let tall = fitted(vec2(100.0, 1000.0), room);
        assert!((tall.height() - 500.0).abs() < 0.01 && tall.width() < 60.0);
    }

    #[test]
    fn a_quarter_turn_clockwise_puts_the_top_left_at_the_top_right() {
        assert_eq!(
            turned_corners(1),
            [
                Pos2::new(0.0, 1.0),
                Pos2::new(0.0, 0.0),
                Pos2::new(1.0, 0.0),
                Pos2::new(1.0, 1.0),
            ]
        );
    }

    #[test]
    fn turning_takes_whole_quarters_and_comes_back_round() {
        assert_eq!(
            turned_corners(0),
            [
                Pos2::new(0.0, 0.0),
                Pos2::new(1.0, 0.0),
                Pos2::new(1.0, 1.0),
                Pos2::new(0.0, 1.0),
            ]
        );
        assert_eq!(turned_corners(4), turned_corners(0));
        assert_eq!(turned_corners(7), turned_corners(3));
        // Half a turn puts each corner where the one across from it was.
        assert_eq!(turned_corners(2)[0], Pos2::new(1.0, 1.0));
        assert_eq!(turned_corners(2)[1], Pos2::new(0.0, 1.0));
    }
}
