// スパイク S1: 角丸矩形を SDF で描くインスタンス化パイプライン。
//
// UI の描画プリミティブは「角丸矩形」「画像」「テキスト」にほぼ集約される。
// 汎用ベクタエンジン (vello / skia) を使わず特化バッチャで足りるか、
// これで当たりを取る (spec/08-spike-plan.md 1-6)。

struct Globals {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct Instance {
    // 物理ピクセル座標。左上原点。
    @location(0) rect: vec4<f32>,   // x, y, w, h
    @location(1) color: vec4<f32>,  // 事前乗算しない straight alpha
    @location(2) radius: f32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,      // 矩形中心を原点とした座標
    @location(1) half_size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) radius: f32,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    // triangle-strip の 4 頂点をインデックスから生成する。頂点バッファを持たない。
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = inst.rect.xy + corner * inst.rect.zw;

    var out: VsOut;
    // ピクセル座標 → NDC (Y 反転)
    out.pos = vec4<f32>(p / globals.screen * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.half_size = inst.rect.zw * 0.5;
    out.local = (corner - vec2<f32>(0.5, 0.5)) * inst.rect.zw;
    out.color = inst.color;
    // 半径が短辺の半分を超えないよう丸める
    out.radius = min(inst.radius, min(inst.rect.z, inst.rect.w) * 0.5);
    return out;
}

// 角丸矩形の符号付き距離場
fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let d = sd_round_box(in.local, in.half_size, in.radius);
    // 画面空間の微分でアンチエイリアス幅を決める。DPI に自動追従する。
    let aa = max(fwidth(d), 0.0001);
    let alpha = 1.0 - smoothstep(-aa, aa, d);
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}
