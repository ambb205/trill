// The passes over the drawn interface. Each is one triangle that covers
// its target and a fragment function below.
//
//   combine    the interface and its light become one picture in linear
//              light, with how far out of focus each pixel is beside it
//   focus      the blur of what is out of focus, at half size
//   bloom_*    the light, spread: down a chain of ever smaller pictures
//              and back up it
//   composite  the three together, added into the frame being exposed
//   finish     the exposed frame developed: shoulder, vignette, grain

struct Globals {
    // Size of the scene in pixels, and one over it.
    scene: vec4<f32>,
    // Size of the output in pixels, and one over it.
    out: vec4<f32>,
    // x: how strong the light layer is. y: how much bloom is added.
    // z: the flare. w: exposure.
    light: vec4<f32>,
    // x: the widest blur, as a radius in scene pixels. y: samples of it.
    // z: the punch, as how much the picture is enlarged. w: the tear, in
    // scene pixels.
    focus: vec4<f32>,
    // x: vignette. y: grain. z: a number that differs every frame.
    // w: how far the bloom spreads, 0..1.
    finish: vec4<f32>,
    // The color of the flare.
    tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var linear_sampler: sampler;
@group(1) @binding(0) var first: texture_2d<f32>;
@group(1) @binding(1) var second: texture_2d<f32>;
@group(1) @binding(2) var third: texture_2d<f32>;

struct Covered {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn cover(@builtin(vertex_index) index: u32) -> Covered {
    var out: Covered;
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    out.uv = vec2<f32>(x, y);
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    return out;
}

fn linear_from_gamma(color: vec3<f32>) -> vec3<f32> {
    let low = color / vec3<f32>(12.92);
    let high = pow((color + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(high, low, color < vec3<f32>(0.04045));
}

fn gamma_from_linear(color: vec3<f32>) -> vec3<f32> {
    let low = color * vec3<f32>(12.92);
    let high = vec3<f32>(1.055) * pow(max(color, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, color < vec3<f32>(0.0031308));
}

// A number in 0..1 that differs from pixel to pixel in a way the eye does
// not read as a pattern (Jimenez, 2014).
fn scatter(pixel: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * pixel.x + 0.00583715 * pixel.y));
}

fn hash(pixel: vec2<f32>) -> f32 {
    var p = fract(vec3<f32>(pixel.xyx) * 0.1031);
    p += dot(p, p.yzx + 33.33);
    return fract((p.x + p.y) * p.z);
}

// first: the interface. second: its light. third: the focus.
@fragment
fn combine(in: Covered) -> @location(0) vec4<f32> {
    // The punch enlarges the picture about its middle.
    let uv = (in.uv - vec2<f32>(0.5)) / (1.0 + g.focus.z) + vec2<f32>(0.5);
    // The tear pulls red and blue apart sideways.
    let tear = vec2<f32>(g.focus.w * g.scene.z, 0.0);
    let red = textureSampleLevel(first, linear_sampler, uv + tear, 0.0).r;
    let green = textureSampleLevel(first, linear_sampler, uv, 0.0).g;
    let blue = textureSampleLevel(first, linear_sampler, uv - tear, 0.0).b;
    let light = textureSampleLevel(second, linear_sampler, uv, 0.0).rgb;
    let soft = textureSampleLevel(third, linear_sampler, uv, 0.0).r;
    let color = linear_from_gamma(vec3<f32>(red, green, blue)) + linear_from_gamma(light) * g.light.x;
    return vec4<f32>(color, soft);
}

// first: the combined picture, with the focus in its alpha.
@fragment
fn focus(in: Covered) -> @location(0) vec4<f32> {
    let middle = textureSampleLevel(first, linear_sampler, in.uv, 0.0);
    let radius = middle.a * g.focus.x;
    if radius < 0.75 {
        return middle;
    }
    let samples = i32(g.focus.y);
    let turn = scatter(in.position.xy + vec2<f32>(g.finish.z * 7.0, g.finish.z * 3.0)) * 6.2831853;
    var sum = middle.rgb;
    var weight = 1.0;
    for (var index = 0; index < samples; index++) {
        // A spiral that fills the disc evenly.
        let reach = sqrt((f32(index) + 0.5) / f32(samples)) * radius;
        let angle = turn + f32(index) * 2.3999632;
        let offset = vec2<f32>(cos(angle), sin(angle)) * reach;
        let found = textureSampleLevel(first, linear_sampler, in.uv + offset * g.scene.zw, 0.0);
        // What is in focus stays where it is: a sample is spread no
        // further than its own blur reaches.
        let reaches = clamp(found.a * g.focus.x - reach + 1.5, 0.0, 1.0);
        sum += found.rgb * reaches;
        weight += reaches;
    }
    return vec4<f32>(sum / weight, middle.a);
}

// first: the light layer, as it was painted.
@fragment
fn bloom_first(in: Covered) -> @location(0) vec4<f32> {
    let texel = vec2<f32>(1.0) / vec2<f32>(textureDimensions(first));
    var sum = linear_from_gamma(textureSampleLevel(first, linear_sampler, in.uv, 0.0).rgb) * 4.0;
    sum += linear_from_gamma(textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(-1.0, -1.0), 0.0).rgb);
    sum += linear_from_gamma(textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(1.0, -1.0), 0.0).rgb);
    sum += linear_from_gamma(textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(-1.0, 1.0), 0.0).rgb);
    sum += linear_from_gamma(textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(1.0, 1.0), 0.0).rgb);
    return vec4<f32>(sum / 8.0 * g.light.x, 1.0);
}

// first: the level above, twice the size.
@fragment
fn bloom_down(in: Covered) -> @location(0) vec4<f32> {
    let texel = vec2<f32>(1.0) / vec2<f32>(textureDimensions(first));
    var sum = textureSampleLevel(first, linear_sampler, in.uv, 0.0).rgb * 4.0;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(-1.0, -1.0), 0.0).rgb;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(1.0, -1.0), 0.0).rgb;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(-1.0, 1.0), 0.0).rgb;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(1.0, 1.0), 0.0).rgb;
    return vec4<f32>(sum / 8.0, 1.0);
}

// first: the level below, half the size. Added to the level it is drawn on.
@fragment
fn bloom_up(in: Covered) -> @location(0) vec4<f32> {
    let texel = vec2<f32>(0.5) / vec2<f32>(textureDimensions(first));
    var sum = textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(-2.0, 0.0), 0.0).rgb;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(2.0, 0.0), 0.0).rgb;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(0.0, -2.0), 0.0).rgb;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(0.0, 2.0), 0.0).rgb;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(-1.0, 1.0), 0.0).rgb * 2.0;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(1.0, 1.0), 0.0).rgb * 2.0;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(-1.0, -1.0), 0.0).rgb * 2.0;
    sum += textureSampleLevel(first, linear_sampler, in.uv + texel * vec2<f32>(1.0, -1.0), 0.0).rgb * 2.0;
    return vec4<f32>(sum / 12.0 * g.finish.w, 1.0);
}

// first: the combined picture. second: its blur. third: the bloom.
// Multiplied by the blend constant and added to what is there: one of the
// moments a frame is exposed for.
@fragment
fn composite(in: Covered) -> @location(0) vec4<f32> {
    let sharp = textureSampleLevel(first, linear_sampler, in.uv, 0.0);
    let blurred = textureSampleLevel(second, linear_sampler, in.uv, 0.0);
    let bloom = textureSampleLevel(third, linear_sampler, in.uv, 0.0).rgb;
    // The blur of each pixel's own focus, and of its neighbors': a soft
    // thing next to a sharp one stays soft up to its edge.
    let radius = max(sharp.a, blurred.a) * g.focus.x;
    var color = mix(sharp.rgb, blurred.rgb, smoothstep(0.75, 2.5, radius));
    color += bloom * g.light.y;
    // The flare: light let in across the top of the picture, falling off
    // down it.
    let down = in.uv.y;
    color += g.tint.rgb * g.light.z * exp(-down * 5.0) * (0.6 + 0.4 * sin(in.uv.x * 3.14159));
    return vec4<f32>(color * g.light.w, 1.0);
}

// first: the exposed frame, in linear light.
@fragment
fn finish(in: Covered) -> @location(0) vec4<f32> {
    var color = textureSampleLevel(first, linear_sampler, in.uv, 0.0).rgb;
    // Darker towards the corners, by the distance from the middle of a
    // frame taken as square.
    let from_middle = length((in.uv - vec2<f32>(0.5)) * vec2<f32>(1.0, g.out.y / g.out.x) * 2.0 * vec2<f32>(0.82, 1.3));
    color *= 1.0 - g.finish.x * smoothstep(0.55, 1.45, from_middle);
    // The interface's own tones pass as they are; what the light pushed
    // over them rolls off towards white instead of clipping.
    let knee = vec3<f32>(0.72);
    let over = max(color - knee, vec3<f32>(0.0));
    color = min(color, knee) + (vec3<f32>(1.0) - knee) * (vec3<f32>(1.0) - exp(-over / (vec3<f32>(1.0) - knee)));
    var shown = gamma_from_linear(color);
    // Grain: two throws of the dice make it soft-edged, and it is finer
    // in the lights than in the darks.
    let pixel = in.position.xy + vec2<f32>(g.finish.z * 37.0, g.finish.z * 17.0);
    let grain = hash(pixel) + hash(pixel + vec2<f32>(11.7, 5.3)) - 1.0;
    let luma = dot(shown, vec3<f32>(0.2126, 0.7152, 0.0722));
    shown += vec3<f32>(grain * g.finish.y * (1.0 - 0.6 * luma));
    return vec4<f32>(shown, 1.0);
}
