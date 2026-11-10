//! The scene's primitives carry no padding bytes.
//!
//! `gpui_wgpu`'s partial redraws (`fast::damage`) hash each primitive by its
//! bytes, as the renderer uploads them: a padding byte would be an
//! uninitialized read. These tests prove, for every type whose bytes are
//! hashed, that its size is the sum of its fields' sizes, so a field added
//! with a gap fails here instead of quietly hashing padding.

#[cfg(test)]
mod tests {
    use crate::{
        AtlasTextureId, AtlasTile, Background, Bounds, ContentMask, Corners, DevicePixels, Edges,
        Hsla, LinearColorStop, MonochromeSprite, Point, PolychromeSprite, Quad, ScaledPixels,
        Shadow, Size, SubpixelSprite, TransformationMatrix, Underline,
    };

    /// `size_of::<$ty>()` equals the sum of the sizes of the fields listed,
    /// which must be all of its fields (a private one given by its type
    /// after `+`).
    macro_rules! no_padding {
        ($ty:ty { $($field:ident),+ $(,)? } $(+ $private:ty)*) => {{
            fn size_of_field<T, F>(_: fn(&T) -> &F) -> usize {
                std::mem::size_of::<F>()
            }
            let fields = 0 $(+ size_of_field(|value: &$ty| &value.$field))+
                $(+ std::mem::size_of::<$private>())*;
            assert_eq!(
                std::mem::size_of::<$ty>(),
                fields,
                concat!(stringify!($ty), " has padding between or after its fields")
            );
        }};
    }

    #[test]
    fn geometry_has_no_padding() {
        no_padding!(Point<ScaledPixels> { x, y });
        no_padding!(Point<f32> { x, y });
        no_padding!(Size<ScaledPixels> { width, height });
        no_padding!(Bounds<ScaledPixels> { origin, size });
        no_padding!(Bounds<DevicePixels> { origin, size });
        no_padding!(Corners<ScaledPixels> { top_left, top_right, bottom_right, bottom_left });
        no_padding!(Edges<ScaledPixels> { top, right, bottom, left });
        no_padding!(ContentMask<ScaledPixels> { bounds });
        no_padding!(TransformationMatrix {
            rotation_scale,
            translation
        });
    }

    #[test]
    fn colors_and_tiles_have_no_padding() {
        no_padding!(Hsla { h, s, l, a });
        no_padding!(LinearColorStop { color, percentage });
        no_padding!(
            Background {
                tag,
                color_space,
                solid,
                gradient_angle_or_pattern_height,
                colors
            } + u32
        ); // its private `pad: u32`
        no_padding!(AtlasTextureId { index, kind });
        no_padding!(AtlasTile {
            texture_id,
            tile_id,
            padding,
            bounds
        });
    }

    #[test]
    fn the_hashed_primitives_have_no_padding() {
        no_padding!(Shadow {
            order,
            blur_radius,
            bounds,
            corner_radii,
            content_mask,
            color,
            element_bounds,
            element_corner_radii,
            inset,
            pad
        });
        no_padding!(Quad {
            order,
            border_style,
            bounds,
            content_mask,
            background,
            border_color,
            corner_radii,
            border_widths
        });
        no_padding!(Underline {
            order,
            pad,
            bounds,
            content_mask,
            color,
            thickness,
            wavy
        });
        no_padding!(MonochromeSprite {
            order,
            pad,
            tile,
            bounds,
            content_mask,
            color,
            transformation
        });
        no_padding!(SubpixelSprite {
            order,
            pad,
            tile,
            bounds,
            content_mask,
            color,
            transformation
        });
        no_padding!(PolychromeSprite {
            order,
            pad,
            tile,
            grayscale,
            opacity,
            bounds,
            content_mask,
            corner_radii
        });
    }
}
