// Draws what the kit painted: triangles with a color at each corner and
// the font's texture, blended as the window blends them (in gamma space,
// colors already multiplied by their alpha), so the stage's interface is
// the window's, pixel for pixel, before anything is done to it.

struct Screen {
    // Size of the picture in points, and nothing in the other two.
    size: vec4<f32>,
};
@group(0) @binding(0) var<uniform> screen: Screen;
@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct Corner {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vertex(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) color: u32) -> Corner {
    var out: Corner;
    out.position = vec4<f32>(
        2.0 * position.x / screen.size.x - 1.0,
        1.0 - 2.0 * position.y / screen.size.y,
        0.0,
        1.0,
    );
    out.uv = uv;
    out.color = vec4<f32>(
        f32(color & 255u),
        f32((color >> 8u) & 255u),
        f32((color >> 16u) & 255u),
        f32((color >> 24u) & 255u),
    ) / 255.0;
    return out;
}

@fragment
fn fragment(in: Corner) -> @location(0) vec4<f32> {
    return in.color * textureSample(atlas, atlas_sampler, in.uv);
}
