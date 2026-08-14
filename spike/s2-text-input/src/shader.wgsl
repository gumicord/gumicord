// S2: 矩形パイプライン (S1 から流用) + テキストパイプライン

struct Globals {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

fn to_ndc(p: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(p / globals.screen * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

// ---------------------------------------------------------------- 角丸矩形

struct RectInst {
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) radius: f32,
};

struct RectOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) half_size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) radius: f32,
};

@vertex
fn vs_rect(@builtin(vertex_index) vi: u32, inst: RectInst) -> RectOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    var out: RectOut;
    out.pos = to_ndc(inst.rect.xy + corner * inst.rect.zw);
    out.half_size = inst.rect.zw * 0.5;
    out.local = (corner - vec2<f32>(0.5, 0.5)) * inst.rect.zw;
    out.color = inst.color;
    out.radius = min(inst.radius, min(inst.rect.z, inst.rect.w) * 0.5);
    return out;
}

fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_rect(in: RectOut) -> @location(0) vec4<f32> {
    let d = sd_round_box(in.local, in.half_size, in.radius);
    let aa = max(fwidth(d), 0.0001);
    let alpha = 1.0 - smoothstep(-aa, aa, d);
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}

// ---------------------------------------------------------------- テキスト

@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;

struct GlyphInst {
    @location(0) rect: vec4<f32>,   // x, y, w, h (物理px)
    @location(1) uv: vec4<f32>,     // u0, v0, u1, v1
    @location(2) color: vec4<f32>,
    @location(3) flags: f32,        // 1.0 = カラー絵文字
};

struct GlyphOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) flags: f32,
};

@vertex
fn vs_text(@builtin(vertex_index) vi: u32, inst: GlyphInst) -> GlyphOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    var out: GlyphOut;
    out.pos = to_ndc(inst.rect.xy + corner * inst.rect.zw);
    out.uv = mix(inst.uv.xy, inst.uv.zw, corner);
    out.color = inst.color;
    out.flags = inst.flags;
    return out;
}

@fragment
fn fs_text(in: GlyphOut) -> @location(0) vec4<f32> {
    let tex = textureSample(atlas_tex, atlas_smp, in.uv);
    if (in.flags > 0.5) {
        // カラー絵文字: テクスチャの色をそのまま使う
        return vec4<f32>(tex.rgb, tex.a * in.color.a);
    }
    // マスクグリフ: アルファのみ使い、色は指定色
    return vec4<f32>(in.color.rgb, in.color.a * tex.a);
}
