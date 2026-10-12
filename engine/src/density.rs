//! Density functions (levelgen/densityfunction/*), ported literally:
//!   JSON -> `Df` tree -> rewrite (inline refs, dedupe caches, slice uniform axes) -> compiled `S`.
//! Both evaluation paths are kept (`sample` = sampleValue, `sample_volume` = sampleVolume) because
//! they are not always bit-identical, and the game picks one or the other depending on the caller.

use crate::noise::{blended_fbm, blended_fbm_range, NoiseStack, NormalNoise};
use crate::rng::Positional;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// ------------------------------------------------------------------------------------------------
// DensityVolume
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Volume {
    pub sx: i32, pub sy: i32, pub sz: i32,
    pub x0: i32, pub y0: i32, pub z0: i32,
    pub stx: i32, pub sty: i32, pub stz: i32,
}

impl Volume {
    pub fn new(sx: i32, sy: i32, sz: i32, x0: i32, y0: i32, z0: i32) -> Self {
        Self::stepped(sx, sy, sz, x0, y0, z0, 1, 1, 1)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn stepped(sx: i32, sy: i32, sz: i32, x0: i32, y0: i32, z0: i32, stx: i32, sty: i32, stz: i32) -> Self {
        Self { sx, sy, sz, x0, y0, z0, stx, sty, stz }
    }
    pub fn size(&self) -> usize { (self.sx * self.sy * self.sz) as usize }
    pub fn bx(&self, i: i32) -> i32 { self.x0 + i * self.stx }
    pub fn by(&self, i: i32) -> i32 { self.y0 + i * self.sty }
    pub fn bz(&self, i: i32) -> i32 { self.z0 + i * self.stz }
    pub fn max_x(&self) -> i32 { self.x0 + self.sx * self.stx - 1 }
    pub fn max_y(&self) -> i32 { self.y0 + self.sy * self.sty - 1 }
    pub fn max_z(&self) -> i32 { self.z0 + self.sz * self.stz - 1 }
    #[inline]
    pub fn idx(&self, ix: i32, iy: i32, iz: i32) -> usize { (iy + (ix + iz * self.sx) * self.sy) as usize }

    pub fn index_of_block(&self, x: i32, y: i32, z: i32) -> Option<usize> {
        let (rx, ry, rz) = (x - self.x0, y - self.y0, z - self.z0);
        if rx < 0 || ry < 0 || rz < 0 {
            return None;
        }
        if self.stx == 1 && self.sty == 1 && self.stz == 1 {
            return (rx < self.sx && ry < self.sy && rz < self.sz).then(|| self.idx(rx, ry, rz));
        }
        let inside = rx < self.sx * self.stx && ry < self.sy * self.sty && rz < self.sz * self.stz
            && rx.rem_euclid(self.stx) == 0 && ry.rem_euclid(self.sty) == 0 && rz.rem_euclid(self.stz) == 0;
        inside.then(|| self.idx(rx.div_euclid(self.stx), ry.div_euclid(self.sty), rz.div_euclid(self.stz)))
    }
}

// ------------------------------------------------------------------------------------------------
// Interval (net.minecraft.util.Interval) - drives compile-time choices (min/max shortcuts)
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct Iv { pub lo: f32, pub hi: f32 }
const NAI: Iv = Iv { lo: f32::NAN, hi: f32::NAN };

impl Iv {
    fn of(lo: f32, hi: f32) -> Iv { Iv { lo, hi } }
    fn exact(v: f32) -> Iv { Iv { lo: v, hi: v } }
    fn nai(&self) -> bool { self.lo.is_nan() }
    fn contains(&self, v: f32) -> bool { v >= self.lo && v <= self.hi }
}

/// Java Math.min / Math.max semantics for floats (NaN-propagating, -0 < +0).
#[inline]
pub fn jmin(a: f32, b: f32) -> f32 {
    if a.is_nan() { a } else if b.is_nan() { b } else if a == 0.0 && b == 0.0 { if a.is_sign_negative() { a } else { b } } else if a < b { a } else { b }
}
#[inline]
pub fn jmax(a: f32, b: f32) -> f32 {
    if a.is_nan() { a } else if b.is_nan() { b } else if a == 0.0 && b == 0.0 { if a.is_sign_negative() { b } else { a } } else if a > b { a } else { b }
}
#[inline]
fn clampf(v: f32, lo: f32, hi: f32) -> f32 { if v < lo { lo } else { jmin(v, hi) } }
#[inline]
fn lerpf(a: f32, p0: f32, p1: f32) -> f32 { p0 + a * (p1 - p0) }

fn mul_bound(a: f32, b: f32) -> f32 { if a != 0.0 && b != 0.0 { a * b } else { 0.0 } }
fn iv_add(a: Iv, b: Iv) -> Iv { let (lo, hi) = (a.lo + b.lo, a.hi + b.hi); if lo.is_nan() || hi.is_nan() { NAI } else { Iv::of(lo, hi) } }
fn iv_sub(a: Iv, b: Iv) -> Iv { let (lo, hi) = (a.lo - b.hi, a.hi - b.lo); if lo.is_nan() || hi.is_nan() { NAI } else { Iv::of(lo, hi) } }
fn iv_mul(a: Iv, b: Iv) -> Iv {
    if a.nai() || b.nai() { return NAI; }
    let (p, q, r, s) = (mul_bound(a.lo, b.lo), mul_bound(a.lo, b.hi), mul_bound(a.hi, b.lo), mul_bound(a.hi, b.hi));
    Iv::of(jmin(jmin(p, q), jmin(r, s)), jmax(jmax(p, q), jmax(r, s)))
}
fn iv_reciprocal(a: Iv) -> Iv {
    if a.nai() || (a.lo == 0.0 && a.hi == 0.0) { return NAI; }
    if !a.contains(0.0) { Iv::of(1.0 / a.hi, 1.0 / a.lo) }
    else if a.hi == 0.0 { Iv::of(f32::NEG_INFINITY, 1.0 / a.lo) }
    else if a.lo == 0.0 { Iv::of(1.0 / a.hi, f32::INFINITY) }
    else { Iv::of(f32::NEG_INFINITY, f32::INFINITY) }
}
fn iv_min(a: Iv, b: Iv) -> Iv { if a.nai() || b.nai() { NAI } else { Iv::of(jmin(a.lo, b.lo), jmin(a.hi, b.hi)) } }
fn iv_max(a: Iv, b: Iv) -> Iv { if a.nai() || b.nai() { NAI } else { Iv::of(jmax(a.lo, b.lo), jmax(a.hi, b.hi)) } }
fn iv_enc(list: &[Iv]) -> Iv {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for i in list.iter().filter(|i| !i.nai()) { lo = jmin(i.lo, lo); hi = jmax(i.hi, hi); }
    if hi < lo { NAI } else { Iv::of(lo, hi) }
}
fn iv_enc2(a: f32, b: f32) -> Iv {
    match (a.is_nan(), b.is_nan()) { (true, true) => NAI, (true, _) => Iv::exact(b), (_, true) => Iv::exact(a), _ => Iv::of(jmin(a, b), jmax(a, b)) }
}
fn iv_clamp(a: Iv, lo: f32, hi: f32) -> Iv {
    if a.nai() { NAI } else if a.lo >= hi { Iv::exact(hi) } else if a.hi <= lo { Iv::exact(lo) } else { Iv::of(jmax(a.lo, lo), jmin(a.hi, hi)) }
}
fn iv_abs(a: Iv) -> Iv {
    if a.nai() { return NAI; }
    let m = jmax(a.lo.abs(), a.hi.abs());
    if a.contains(0.0) { Iv::of(0.0, m) } else { Iv::of(jmin(a.lo.abs(), a.hi.abs()), m) }
}
fn iv_square(a: Iv) -> Iv {
    if a.nai() { return NAI; }
    let m = jmax(a.lo * a.lo, a.hi * a.hi);
    if a.contains(0.0) { Iv::of(0.0, m) } else { Iv::of(jmin(a.lo * a.lo, a.hi * a.hi), m) }
}
fn iv_mono(a: Iv, f: impl Fn(f32) -> f32) -> Iv {
    if a.nai() { return NAI; }
    let (p, q) = (f(a.lo), f(a.hi));
    Iv::of(jmin(p, q), jmax(p, q))
}
fn lerp_bound(alpha: f32, first: f32, second: f32) -> f32 { first + mul_bound(alpha, second - first) }
fn lerp_inf_bound(alpha: f32, first: f32, second: f32) -> f32 {
    let (a, b) = (mul_bound(1.0 - alpha, first), mul_bound(alpha, second));
    if !a.is_infinite() || !b.is_infinite() { a + b }
    else if alpha <= 0.0 { if second > first { f32::NEG_INFINITY } else { f32::INFINITY } }
    else if alpha >= 1.0 { if second > first { f32::INFINITY } else { f32::NEG_INFINITY } }
    else { f32::NAN }
}
fn iv_lerp_pt(alpha: Iv, first: f32, second: f32) -> Iv {
    if alpha.nai() || first.is_nan() || second.is_nan() { return NAI; }
    if first.is_finite() && second.is_finite() {
        return iv_enc2(lerp_bound(alpha.lo, first, second), lerp_bound(alpha.hi, first, second));
    }
    if first == second { return Iv::exact(first); }
    let (a, b) = (lerp_inf_bound(alpha.lo, first, second), lerp_inf_bound(alpha.hi, first, second));
    if a.is_nan() || b.is_nan() { NAI } else { iv_enc2(a, b) }
}
fn iv_lerp(alpha: Iv, f: Iv, s: Iv) -> Iv {
    if alpha.nai() || f.nai() || s.nai() { return NAI; }
    iv_enc(&[iv_lerp_pt(alpha, f.lo, s.lo), iv_lerp_pt(alpha, f.hi, s.lo), iv_lerp_pt(alpha, f.lo, s.hi), iv_lerp_pt(alpha, f.hi, s.hi)])
}
fn iv_sign(a: Iv) -> Iv {
    if a.nai() { return NAI; }
    if a.lo == a.hi { return Iv::exact(signum_java(a.lo)); }
    if a.contains(0.0) {
        if a.lo == 0.0 { Iv::of(0.0, 1.0) } else if a.hi == 0.0 { Iv::of(-1.0, 0.0) } else { Iv::of(-1.0, 1.0) }
    } else { Iv::exact(if a.lo > 0.0 { 1.0 } else { -1.0 }) }
}
fn signum_java(v: f32) -> f32 { if v == 0.0 || v.is_nan() { v } else { v.signum() } }

// ------------------------------------------------------------------------------------------------
// Parsed tree
// ------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Un { Abs, Square, Cube, Sqrt, HalfNeg, QuarterNeg, Reciprocal, Negate, Squeeze, Log, Sign }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bi { Add, Sub, Mul, Div, Min, Max }
const AX_X: u8 = 1;
const AX_Y: u8 = 2;
const AX_Z: u8 = 4;

#[derive(Clone, Debug)]
pub enum Spline<C> {
    Const(f32),
    Multi { coord: C, loc: Vec<f32>, val: Vec<Spline<C>>, der: Vec<f32> },
}

#[derive(Clone, Debug)]
pub enum Df {
    Const(f32),
    Ref(String),
    Noise { noise: String, xz: f64, y: f64, sx: Box<Df>, sy: Box<Df>, sz: Box<Df> },
    ShiftA(String),
    ShiftB(String),
    Shift(String),
    Gradient { axis: u8, tiling: u8, from: i32, to: i32, fv: f32, tv: f32 },
    Unary(Un, Box<Df>),
    Bin(Bi, Box<Df>, Box<Df>),
    Clamp(Box<Df>, f32, f32),
    Lerp(Box<Df>, Box<Df>, Box<Df>),
    RangeChoice(Box<Df>, f32, f32, Box<Df>, Box<Df>),
    IntervalSelect(Box<Df>, Vec<f32>, Vec<Df>),
    Spline(Box<Spline<(String, Df)>>),
    Cache(Box<Df>, String),
    BlendDensity(Box<Df>),
    BlendAlpha,
    BlendOffset,
    Beardifier,
    Interpolated(Box<Df>, i32, i32),
    Slice(u8, i32, Box<Df>),
    FindTopSurface(Box<Df>, Box<Df>, i32, i32),
    OldBlended { xz_scale: f64, y_scale: f64, xz_factor: f64, y_factor: f64, smear: f64 },
    Prepared(usize),
}

/// Floats in worldgen JSON are read with Float.parseFloat on the literal, so parse from the text.
pub fn f32_of(v: &Value) -> f32 {
    v.to_string().parse().unwrap_or_else(|_| v.as_f64().unwrap() as f32)
}
pub fn ns(s: &str) -> String {
    if s.contains(':') { s.to_string() } else { format!("minecraft:{s}") }
}
fn axis_of(v: &Value) -> u8 {
    match v.as_str().unwrap() { "x" => AX_X, "y" => AX_Y, _ => AX_Z }
}

pub fn parse(v: &Value) -> Result<Df, String> {
    let b = |k: &str| -> Result<Box<Df>, String> { Ok(Box::new(parse(&v[k])?)) };
    let opt = |k: &str| -> Result<Box<Df>, String> { v.get(k).map(parse).unwrap_or(Ok(Df::Const(0.0))).map(Box::new) };
    Ok(match v {
        Value::Number(_) => Df::Const(f32_of(v)),
        Value::String(s) => Df::Ref(ns(s)),
        Value::Object(o) => {
            let ty = o.get("type").and_then(Value::as_str).ok_or("density function without type")?;
            let ty = ty.strip_prefix("minecraft:").unwrap_or(ty);
            let noise = || ns(v["noise"].as_str().unwrap());
            match ty {
                "constant" => Df::Const(f32_of(&v["value"])),
                "noise" => Df::Noise {
                    noise: noise(), xz: v["xz_scale"].as_f64().unwrap(), y: v["y_scale"].as_f64().unwrap(),
                    sx: opt("shift_x")?, sy: opt("shift_y")?, sz: opt("shift_z")?,
                },
                "shift_a" => Df::ShiftA(noise()),
                "shift_b" => Df::ShiftB(noise()),
                "shift" => Df::Shift(noise()),
                "gradient" => Df::Gradient {
                    axis: axis_of(&v["axis"]),
                    tiling: match v.get("tiling").and_then(Value::as_str) { Some("repeat") => 1, Some("mirrored_repeat") => 2, _ => 0 },
                    from: v["from_coordinate"].as_i64().unwrap() as i32, to: v["to_coordinate"].as_i64().unwrap() as i32,
                    fv: f32_of(&v["from_value"]), tv: f32_of(&v["to_value"]),
                },
                "abs" | "square" | "cube" | "sqrt" | "half_negative" | "quarter_negative" | "reciprocal" | "negate" | "squeeze" | "log" | "sign" => {
                    let op = match ty {
                        "abs" => Un::Abs, "square" => Un::Square, "cube" => Un::Cube, "sqrt" => Un::Sqrt,
                        "half_negative" => Un::HalfNeg, "quarter_negative" => Un::QuarterNeg, "reciprocal" => Un::Reciprocal,
                        "negate" => Un::Negate, "squeeze" => Un::Squeeze, "log" => Un::Log, _ => Un::Sign,
                    };
                    Df::Unary(op, b("input")?)
                }
                "add" | "sub" | "mul" | "div" | "min" | "max" => {
                    let op = match ty { "add" => Bi::Add, "sub" => Bi::Sub, "mul" => Bi::Mul, "div" => Bi::Div, "min" => Bi::Min, _ => Bi::Max };
                    Df::Bin(op, b("left")?, b("right")?)
                }
                "clamp" => Df::Clamp(b("input")?, f32_of(&v["min"]), f32_of(&v["max"])),
                "lerp" => Df::Lerp(b("alpha")?, b("first")?, b("second")?),
                "range_choice" => Df::RangeChoice(b("input")?, f32_of(&v["min_inclusive"]), f32_of(&v["max_exclusive"]), b("when_in_range")?, b("when_out_of_range")?),
                "interval_select" => Df::IntervalSelect(
                    b("input")?,
                    v["thresholds"].as_array().unwrap().iter().map(f32_of).collect(),
                    v["functions"].as_array().unwrap().iter().map(parse).collect::<Result<_, _>>()?,
                ),
                "spline" => Df::Spline(Box::new(parse_spline(&v["spline"])?)),
                "cache" => Df::Cache(b("input")?, v["input"].to_string()),
                "blend_density" => Df::BlendDensity(b("input")?),
                "blend_alpha" => Df::BlendAlpha,
                "blend_offset" => Df::BlendOffset,
                "beardifier" => Df::Beardifier,
                "interpolated" => Df::Interpolated(b("input")?, v["cell_size_xz"].as_i64().unwrap() as i32, v["cell_size_y"].as_i64().unwrap() as i32),
                "slice" => Df::Slice(axis_of(&v["axis"]), v["coordinate"].as_i64().unwrap() as i32, b("input")?),
                "find_top_surface" => Df::FindTopSurface(b("density")?, b("upper_bound")?, v["lower_bound"].as_i64().unwrap() as i32, v["cell_height"].as_i64().unwrap() as i32),
                "old_blended_noise" => Df::OldBlended {
                    xz_scale: v["xz_scale"].as_f64().unwrap(), y_scale: v["y_scale"].as_f64().unwrap(),
                    xz_factor: v["xz_factor"].as_f64().unwrap(), y_factor: v["y_factor"].as_f64().unwrap(),
                    smear: v["smear_scale_multiplier"].as_f64().unwrap(),
                },
                // ponytail: End/datapack-only types (end_outer_islands, distance_to_point, pow, floor/round/...) are not ported yet.
                other => return Err(format!("unsupported density function type: {other}")),
            }
        }
        _ => return Err(format!("bad density function: {v}")),
    })
}

fn parse_spline(v: &Value) -> Result<Spline<(String, Df)>, String> {
    if v.is_number() {
        return Ok(Spline::Const(f32_of(v)));
    }
    let pts = v["points"].as_array().ok_or("spline without points")?;
    Ok(Spline::Multi {
        coord: (v["coordinate"].to_string(), parse(&v["coordinate"])?),
        loc: pts.iter().map(|p| f32_of(&p["location"])).collect(),
        val: pts.iter().map(|p| parse_spline(&p["value"])).collect::<Result<_, _>>()?,
        der: pts.iter().map(|p| f32_of(&p["derivative"])).collect(),
    })
}

fn map_spline(s: &Spline<(String, Df)>, f: &mut impl FnMut(&Df) -> Df) -> Spline<(String, Df)> {
    match s {
        Spline::Const(c) => Spline::Const(*c),
        Spline::Multi { coord, loc, val, der } => Spline::Multi {
            coord: (coord.0.clone(), f(&coord.1)),
            loc: loc.clone(),
            val: val.iter().map(|v| map_spline(v, f)).collect(),
            der: der.clone(),
        },
    }
}

impl Df {
    /// Names of density functions referenced anywhere in this tree.
    pub fn refs(&self, out: &mut Vec<String>) {
        if let Df::Ref(n) = self {
            out.push(n.clone());
        }
        self.map_children(&mut |c| {
            c.refs(out);
            c.clone()
        });
    }

    /// rewriteChildren: rebuild with every direct child passed through `f`.
    fn map_children(&self, f: &mut impl FnMut(&Df) -> Df) -> Df {
        match self {
            Df::Noise { noise, xz, y, sx, sy, sz } => {
                let (sx, sy, sz) = (f(sx), f(sy), f(sz));
                Df::Noise { noise: noise.clone(), xz: *xz, y: *y, sx: Box::new(sx), sy: Box::new(sy), sz: Box::new(sz) }
            }
            Df::Unary(op, i) => Df::Unary(*op, Box::new(f(i))),
            Df::Bin(op, l, r) => { let l = f(l); Df::Bin(*op, Box::new(l), Box::new(f(r))) }
            Df::Clamp(i, lo, hi) => Df::Clamp(Box::new(f(i)), *lo, *hi),
            Df::Lerp(a, b, c) => { let (a, b) = (f(a), f(b)); Df::Lerp(Box::new(a), Box::new(b), Box::new(f(c))) }
            Df::RangeChoice(i, lo, hi, a, b) => {
                let (i, a) = (f(i), f(a));
                Df::RangeChoice(Box::new(i), *lo, *hi, Box::new(a), Box::new(f(b)))
            }
            Df::IntervalSelect(i, t, fs) => { let i = f(i); Df::IntervalSelect(Box::new(i), t.clone(), fs.iter().map(&mut *f).collect()) }
            Df::Spline(s) => Df::Spline(Box::new(map_spline(s, f))),
            Df::Cache(i, k) => Df::Cache(Box::new(f(i)), k.clone()),
            Df::BlendDensity(i) => Df::BlendDensity(Box::new(f(i))),
            Df::Interpolated(i, a, b) => Df::Interpolated(Box::new(f(i)), *a, *b),
            Df::Slice(a, c, i) => Df::Slice(*a, *c, Box::new(f(i))),
            Df::FindTopSurface(d, u, lo, c) => { let d = f(d); Df::FindTopSurface(Box::new(d), Box::new(f(u)), *lo, *c) }
            other => other.clone(),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Compiled samplers
// ------------------------------------------------------------------------------------------------

pub enum CSpline {
    Const(f32),
    Multi { coord: usize, loc: Vec<f32>, val: Vec<CSpline>, der: Vec<f32> },
}

pub enum S {
    Const(f32),
    Noise(Arc<NoiseStack>, f64, f64),
    ShiftedXz(Box<S>, Box<S>, Arc<NoiseStack>, f64, f64),
    ShiftedXyz(Box<S>, Box<S>, Box<S>, Arc<NoiseStack>, f64, f64),
    ShiftB(Arc<NoiseStack>),
    ConstAdd(Box<S>, f32),
    ConstSub(f32, Box<S>),
    ConstMul(Box<S>, f32),
    ConstDiv(f32, Box<S>),
    ConstMin(Box<S>, f32),
    ConstMax(Box<S>, f32),
    Add(Box<S>, Box<S>),
    Sub(Box<S>, Box<S>),
    Mul(Box<S>, Box<S>),
    Div(Box<S>, Box<S>),
    Min(Box<S>, Box<S>, f32),
    Max(Box<S>, Box<S>, f32),
    Grad { axis: u8, tiling: u8, from: i32, lo: i32, hi: i32, range: i32, fv: f32, factor: f32 },
    Unary(Un, Box<S>),
    Clamp(Box<S>, f32, f32),
    Lerp(Box<S>, Box<S>, Box<S>),
    LerpCF(Box<S>, f32, Box<S>),
    LerpCS(Box<S>, Box<S>, f32),
    RangeConst(Box<S>, f32, f32, f32, f32),
    Range(Box<S>, f32, f32, Box<S>, Box<S>),
    Sel1(Box<S>, f32, Box<S>, Box<S>),
    Sel(Box<S>, Vec<f32>, Vec<S>),
    Spline(Box<CSpline>, Vec<S>),
    Cache(usize, Arc<S>),
    Interp(Box<S>, i32, i32, f32, f32),
    SliceX(Box<S>, i32),
    SliceY(Box<S>, i32),
    SliceZ(Box<S>, i32),
    SliceXz(Box<S>, i32, i32),
    FindTop(Box<S>, Box<S>, i32, i32),
    Shared(Arc<S>),
}

/// SamplerContext: per-thread/per-task cache cells. `Ctx::uncached()` = SamplerContext.EMPTY_UNCACHED.
pub struct Ctx {
    cells: Option<Vec<Cell>>,
}

struct Cell {
    vol: Option<Volume>,
    buf: Vec<f32>,
    key: i64,
    val: f32,
}

fn pack_pos(x: i32, y: i32, z: i32) -> i64 {
    ((x as i64 & 0x3FF_FFFF) << 38) | (y as i64 & 0xFFF) | ((z as i64 & 0x3FF_FFFF) << 12)
}

impl Ctx {
    pub fn uncached() -> Self { Ctx { cells: None } }
    pub fn cached() -> Self { Ctx { cells: Some(Vec::new()) } }

    fn cell(&mut self, id: usize) -> Option<&mut Cell> {
        let cells = self.cells.as_mut()?;
        while cells.len() <= id {
            cells.push(Cell { vol: None, buf: Vec::new(), key: 0, val: f32::NAN });
        }
        Some(&mut cells[id])
    }

    fn value_cached(&mut self, id: usize, input: &S, x: i32, y: i32, z: i32) -> f32 {
        let key = pack_pos(x, y, z);
        match self.cell(id) {
            None => return input.sample(self, x, y, z),
            Some(c) => {
                if c.key == key && !c.val.is_nan() {
                    return c.val;
                }
                if let Some(i) = c.vol.and_then(|v| v.index_of_block(x, y, z)) {
                    return c.buf[i];
                }
            }
        }
        let v = input.sample(self, x, y, z);
        let c = self.cell(id).unwrap();
        c.key = key;
        c.val = v;
        v
    }

    fn volume_cached(&mut self, id: usize, input: &S, out: &mut [f32], vol: &Volume) {
        match self.cell(id) {
            None => return input.sample_volume(self, out, vol),
            Some(c) if c.vol == Some(*vol) => {
                out.copy_from_slice(&c.buf);
                return;
            }
            Some(_) => {}
        }
        let mut buf = vec![0.0; vol.size()];
        input.sample_volume(self, &mut buf, vol);
        out.copy_from_slice(&buf);
        let c = self.cell(id).unwrap();
        c.vol = Some(*vol);
        c.buf = buf;
    }
}

fn spline_eval(sp: &CSpline, get: &mut impl FnMut(usize) -> f32) -> f32 {
    match sp {
        CSpline::Const(v) => *v,
        CSpline::Multi { coord, loc, val, der } => {
            let input = get(*coord);
            let start = loc.partition_point(|&l| !(input < l)) as i32 - 1;
            let last = loc.len() as i32 - 1;
            let extend = |i: usize, v: f32| if der[i] == 0.0 { v } else { v + der[i] * (input - loc[i]) };
            if start < 0 {
                let v = spline_eval(&val[0], get);
                return extend(0, v);
            }
            if start == last {
                let v = spline_eval(&val[last as usize], get);
                return extend(last as usize, v);
            }
            let s = start as usize;
            let (x1, x2) = (loc[s], loc[s + 1]);
            let t = (input - x1) / (x2 - x1);
            let (d1, d2) = (der[s], der[s + 1]);
            let y1 = spline_eval(&val[s], get);
            let y2 = spline_eval(&val[s + 1], get);
            let a = d1 * (x2 - x1) - (y2 - y1);
            let b = -d2 * (x2 - x1) + (y2 - y1);
            lerpf(t, y1, y2) + t * (1.0 - t) * lerpf(t, a, b)
        }
    }
}

fn unary(op: Un, v: f32) -> f32 {
    match op {
        Un::Abs => v.abs(),
        Un::Square => v * v,
        Un::Cube => v * v * v,
        Un::Sqrt => (v as f64).sqrt() as f32,
        Un::HalfNeg => if v > 0.0 { v } else { v * 0.5 },
        Un::QuarterNeg => if v > 0.0 { v } else { v * 0.25 },
        Un::Reciprocal => 1.0 / v,
        Un::Negate => -v,
        Un::Squeeze => { let c = clampf(v, -1.0, 1.0); c / 2.0 - c * c * c / 24.0 }
        Un::Log => (v as f64).ln() as f32,
        Un::Sign => signum_java(v),
    }
}

#[allow(clippy::too_many_arguments)]
fn grad_compute(tiling: u8, c: i32, from: i32, lo: i32, hi: i32, range: i32, fv: f32, factor: f32) -> f32 {
    match tiling {
        0 => fv + (c.clamp(lo, hi) - from) as f32 * factor,
        1 => fv + (c - from).rem_euclid(range) as f32 * factor,
        _ => {
            let rel = c - from;
            let tile = rel.div_euclid(range);
            let local = rel - tile * range;
            if tile & 1 == 0 { fv + local as f32 * factor } else { fv + (range - local) as f32 * factor }
        }
    }
}

fn pick(axis: u8, x: i32, y: i32, z: i32) -> i32 {
    match axis { AX_X => x, AX_Y => y, _ => z }
}

/// Fresh buffer holding `s` sampled over `vol` (ScopedDensityBuffer equivalent).
fn vol_of(s: &S, ctx: &mut Ctx, vol: &Volume) -> Vec<f32> {
    let mut b = vec![0.0f32; vol.size()];
    s.sample_volume(ctx, &mut b, vol);
    b
}

impl S {
    /// DensitySampler.sampleValue
    pub fn sample(&self, ctx: &mut Ctx, x: i32, y: i32, z: i32) -> f32 {
        match self {
            S::Const(v) => *v,
            S::Noise(n, xz, ys) => n.get(x as f64 * xz, y as f64 * ys, z as f64 * xz),
            S::ShiftedXz(sx, sz, n, xz, ys) => {
                let nx = x as f64 * xz + sx.sample(ctx, x, y, z) as f64;
                let ny = y as f64 * ys;
                let nz = z as f64 * xz + sz.sample(ctx, x, y, z) as f64;
                n.get(nx, ny, nz)
            }
            S::ShiftedXyz(sx, sy, sz, n, xz, ys) => {
                let nx = x as f64 * xz + sx.sample(ctx, x, y, z) as f64;
                let ny = y as f64 * ys + sy.sample(ctx, x, y, z) as f64;
                let nz = z as f64 * xz + sz.sample(ctx, x, y, z) as f64;
                n.get(nx, ny, nz)
            }
            S::ShiftB(n) => n.get(z as f64 * 0.25, x as f64 * 0.25, 0.0) * 4.0,
            S::ConstAdd(l, c) => l.sample(ctx, x, y, z) + c,
            S::ConstSub(c, r) => c - r.sample(ctx, x, y, z),
            S::ConstMul(l, c) => l.sample(ctx, x, y, z) * c,
            S::ConstDiv(c, r) => c / r.sample(ctx, x, y, z),
            S::ConstMin(l, c) => jmin(l.sample(ctx, x, y, z), *c),
            S::ConstMax(l, c) => jmax(l.sample(ctx, x, y, z), *c),
            S::Add(l, r) => l.sample(ctx, x, y, z) + r.sample(ctx, x, y, z),
            S::Sub(l, r) => l.sample(ctx, x, y, z) - r.sample(ctx, x, y, z),
            S::Mul(l, r) => { let a = l.sample(ctx, x, y, z); if a == 0.0 { 0.0 } else { a * r.sample(ctx, x, y, z) } }
            S::Div(l, r) => { let a = l.sample(ctx, x, y, z); if a == 0.0 { 0.0 } else { a / r.sample(ctx, x, y, z) } }
            S::Min(l, r, rmin) => { let a = l.sample(ctx, x, y, z); if a <= *rmin { a } else { jmin(a, r.sample(ctx, x, y, z)) } }
            S::Max(l, r, rmax) => { let a = l.sample(ctx, x, y, z); if a >= *rmax { a } else { jmax(a, r.sample(ctx, x, y, z)) } }
            S::Grad { axis, tiling, from, lo, hi, range, fv, factor } => {
                grad_compute(*tiling, pick(*axis, x, y, z), *from, *lo, *hi, *range, *fv, *factor)
            }
            S::Unary(op, i) => unary(*op, i.sample(ctx, x, y, z)),
            S::Clamp(i, lo, hi) => clampf(i.sample(ctx, x, y, z), *lo, *hi),
            S::Lerp(a, f, s) => {
                let al = a.sample(ctx, x, y, z);
                if al == 0.0 { f.sample(ctx, x, y, z) } else if al == 1.0 { s.sample(ctx, x, y, z) } else {
                    let fv = f.sample(ctx, x, y, z);
                    lerpf(al, fv, s.sample(ctx, x, y, z))
                }
            }
            S::LerpCF(a, f, s) => {
                let al = a.sample(ctx, x, y, z);
                if al == 0.0 { *f } else if al == 1.0 { s.sample(ctx, x, y, z) } else { lerpf(al, *f, s.sample(ctx, x, y, z)) }
            }
            S::LerpCS(a, f, s) => {
                let al = a.sample(ctx, x, y, z);
                if al == 0.0 { f.sample(ctx, x, y, z) } else if al == 1.0 { *s } else { lerpf(al, f.sample(ctx, x, y, z), *s) }
            }
            S::RangeConst(i, lo, hi, a, b) => { let v = i.sample(ctx, x, y, z); if v >= *lo && v < *hi { *a } else { *b } }
            S::Range(i, lo, hi, a, b) => {
                let v = i.sample(ctx, x, y, z);
                if v >= *lo && v < *hi { a.sample(ctx, x, y, z) } else { b.sample(ctx, x, y, z) }
            }
            S::Sel1(i, t, lo, hi) => { if i.sample(ctx, x, y, z) < *t { lo.sample(ctx, x, y, z) } else { hi.sample(ctx, x, y, z) } }
            S::Sel(i, ts, fs) => {
                let v = i.sample(ctx, x, y, z);
                let k = ts.iter().position(|t| v < *t).unwrap_or(fs.len() - 1);
                fs[k].sample(ctx, x, y, z)
            }
            S::Spline(sp, coords) => {
                // PointSplineInput: each coordinate evaluated at most once per sample
                let mut cache = [f32::NAN; 32];
                spline_eval(sp, &mut |i| {
                    if cache[i].is_nan() { cache[i] = coords[i].sample(ctx, x, y, z); }
                    cache[i]
                })
            }
            S::Cache(id, i) => ctx.value_cached(*id, i, x, y, z),
            S::Interp(i, cxz, cy, _, _) => {
                let (xi, yi, zi) = (x.rem_euclid(*cxz), y.rem_euclid(*cy), z.rem_euclid(*cxz));
                if xi == 0 && yi == 0 && zi == 0 {
                    return i.sample(ctx, x, y, z);
                }
                let vol = Volume::stepped(2, 2, 2, x - xi, y - yi, z - zi, *cxz, *cy, *cxz);
                let mut b = [0.0f32; 8];
                i.sample_volume(ctx, &mut b, &vol);
                let (ax, ay, az) = (xi as f32 / *cxz as f32, yi as f32 / *cy as f32, zi as f32 / *cxz as f32);
                let g = |ix, iy, iz| b[vol.idx(ix, iy, iz)];
                lerpf(az,
                    lerpf(ay, lerpf(ax, g(0, 0, 0), g(1, 0, 0)), lerpf(ax, g(0, 1, 0), g(1, 1, 0))),
                    lerpf(ay, lerpf(ax, g(0, 0, 1), g(1, 0, 1)), lerpf(ax, g(0, 1, 1), g(1, 1, 1))))
            }
            S::SliceX(i, c) => i.sample(ctx, *c, y, z),
            S::SliceY(i, c) => i.sample(ctx, x, *c, z),
            S::SliceZ(i, c) => i.sample(ctx, x, y, *c),
            S::SliceXz(i, cx, cz) => i.sample(ctx, *cx, y, *cz),
            S::FindTop(d, u, lower, cell) => {
                let ub = u.sample(ctx, x, y, z);
                find_surface(d, ctx, x, z, ub, *lower, *cell) as f32
            }
            S::Shared(i) => i.sample(ctx, x, y, z),
        }
    }

    /// DensitySampler.sampleVolume
    pub fn sample_volume(&self, ctx: &mut Ctx, out: &mut [f32], vol: &Volume) {
        let n = out.len();
        match self {
            S::Const(v) => out.fill(*v),
            S::Noise(ns, xz, ys) => { out.fill(0.0); ns.add_to_volume(out, vol, *xz, *ys, 1.0) }
            S::ShiftedXz(sx, sz, ns, xz, ys) => {
                sx.sample_volume(ctx, out, vol);
                let bz = vol_of(sz, ctx, vol);
                let mut i = 0;
                for iz in 0..vol.sz {
                    let z = vol.bz(iz) as f64 * xz;
                    for ix in 0..vol.sx {
                        let x = vol.bx(ix) as f64 * xz;
                        for iy in 0..vol.sy {
                            out[i] = ns.get(x + out[i] as f64, vol.by(iy) as f64 * ys, z + bz[i] as f64);
                            i += 1;
                        }
                    }
                }
            }
            S::ShiftedXyz(sx, sy, sz, ns, xz, ys) => {
                sx.sample_volume(ctx, out, vol);
                let by = vol_of(sy, ctx, vol);
                let bz = vol_of(sz, ctx, vol);
                let mut i = 0;
                for iz in 0..vol.sz {
                    let z = vol.bz(iz) as f64 * xz;
                    for ix in 0..vol.sx {
                        let x = vol.bx(ix) as f64 * xz;
                        for iy in 0..vol.sy {
                            out[i] = ns.get(x + out[i] as f64, vol.by(iy) as f64 * ys + by[i] as f64, z + bz[i] as f64);
                            i += 1;
                        }
                    }
                }
            }
            S::ShiftB(ns) => {
                // transposed 2D volume (z as x, x as y), then broadcast along y
                let t = Volume::stepped(vol.sz, vol.sx, 1, vol.z0, vol.x0, 0, vol.stz, vol.stx, 1);
                let mut tb = vec![0.0f32; t.size()];
                ns.add_to_volume(&mut tb, &t, 0.25, 0.25, 4.0);
                for iz in 0..vol.sz {
                    for ix in 0..vol.sx {
                        let v = tb[t.idx(iz, ix, 0)];
                        let s = vol.idx(ix, 0, iz);
                        out[s..s + vol.sy as usize].fill(v);
                    }
                }
            }
            S::ConstAdd(l, c) => { l.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| *v += c) }
            S::ConstSub(c, r) => { r.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| *v = c - *v) }
            S::ConstMul(l, c) => { l.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| *v *= c) }
            S::ConstDiv(c, r) => { r.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| *v = c / *v) }
            S::ConstMin(l, c) => { l.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| if *c < *v { *v = *c }) }
            S::ConstMax(l, c) => { l.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| if *c > *v { *v = *c }) }
            S::Add(l, r) | S::Sub(l, r) | S::Mul(l, r) | S::Div(l, r) | S::Min(l, r, _) | S::Max(l, r, _) => {
                l.sample_volume(ctx, out, vol);
                let rb = vol_of(r, ctx, vol);
                for (v, w) in out.iter_mut().zip(rb) {
                    match self {
                        S::Add(..) => *v += w,
                        S::Sub(..) => *v += -w,
                        S::Mul(..) => *v *= w,
                        S::Div(..) => *v /= w,
                        S::Min(..) => if w < *v { *v = w },
                        _ => if w > *v { *v = w },
                    }
                }
            }
            S::Grad { axis, tiling, from, lo, hi, range, fv, factor } => {
                let f = |c| grad_compute(*tiling, c, *from, *lo, *hi, *range, *fv, *factor);
                for iz in 0..vol.sz {
                    for ix in 0..vol.sx {
                        for iy in 0..vol.sy {
                            out[vol.idx(ix, iy, iz)] = f(pick(*axis, vol.bx(ix), vol.by(iy), vol.bz(iz)));
                        }
                    }
                }
            }
            S::Unary(op, i) => { i.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| *v = unary(*op, *v)) }
            S::Clamp(i, lo, hi) => { i.sample_volume(ctx, out, vol); out.iter_mut().for_each(|v| *v = clampf(*v, *lo, *hi)) }
            S::Lerp(a, f, s) => {
                a.sample_volume(ctx, out, vol);
                let fb = vol_of(f, ctx, vol);
                let sb = vol_of(s, ctx, vol);
                for i in 0..n {
                    let al = out[i];
                    out[i] = if al == 0.0 { fb[i] } else if al == 1.0 { sb[i] } else { lerpf(al, fb[i], sb[i]) };
                }
            }
            S::LerpCF(a, f, s) => {
                a.sample_volume(ctx, out, vol);
                let sb = vol_of(s, ctx, vol);
                for i in 0..n {
                    let al = out[i];
                    out[i] = if al == 0.0 { *f } else if al == 1.0 { sb[i] } else { lerpf(al, *f, sb[i]) };
                }
            }
            S::LerpCS(a, f, s) => {
                a.sample_volume(ctx, out, vol);
                let fb = vol_of(f, ctx, vol);
                for i in 0..n {
                    let al = out[i];
                    out[i] = if al == 0.0 { fb[i] } else if al == 1.0 { *s } else { lerpf(al, fb[i], *s) };
                }
            }
            S::RangeConst(i, lo, hi, a, b) => {
                i.sample_volume(ctx, out, vol);
                out.iter_mut().for_each(|v| *v = if *v >= *lo && *v < *hi { *a } else { *b });
            }
            S::Range(i, lo, hi, a, b) => {
                a.sample_volume(ctx, out, vol);
                let ib = vol_of(i, ctx, vol);
                let bb = vol_of(b, ctx, vol);
                for k in 0..n {
                    if !(ib[k] >= *lo) || !(ib[k] < *hi) { out[k] = bb[k]; }
                }
            }
            S::Sel1(i, t, lo, hi) => {
                i.sample_volume(ctx, out, vol);
                let lb = vol_of(lo, ctx, vol);
                let hb = vol_of(hi, ctx, vol);
                for k in 0..n { out[k] = if out[k] < *t { lb[k] } else { hb[k] }; }
            }
            S::Sel(i, ts, fs) => {
                i.sample_volume(ctx, out, vol);
                let bufs: Vec<Vec<f32>> = fs.iter().map(|f| vol_of(f, ctx, vol)).collect();
                for k in 0..n {
                    let j = ts.iter().position(|t| out[k] < *t).unwrap_or(fs.len() - 1);
                    out[k] = bufs[j][k];
                }
            }
            S::Spline(sp, coords) => {
                // BufferSplineInput: a coordinate's volume is sampled lazily on first use
                let mut bufs: Vec<Option<Vec<f32>>> = (0..coords.len()).map(|_| None).collect();
                for k in 0..n {
                    out[k] = spline_eval(sp, &mut |c| {
                        if bufs[c].is_none() { bufs[c] = Some(vol_of(&coords[c], ctx, vol)); }
                        bufs[c].as_ref().unwrap()[k]
                    });
                }
            }
            S::Cache(id, i) => ctx.volume_cached(*id, i, out, vol),
            S::Interp(i, cxz, cy, ixz, iy) => interp_volume(i, *cxz, *cy, *ixz, *iy, ctx, out, vol),
            S::SliceX(i, c) => {
                if vol.sx == 1 && vol.x0 == *c { return i.sample_volume(ctx, out, vol); }
                let iv = Volume::stepped(1, vol.sy, vol.sz, *c, vol.y0, vol.z0, vol.stx, vol.sty, vol.stz);
                let b = vol_of(i, ctx, &iv);
                let mut k = 0;
                for iz in 0..vol.sz { for _ in 0..vol.sx { for iy in 0..vol.sy { out[k] = b[iv.idx(0, iy, iz)]; k += 1; } } }
            }
            S::SliceXz(i, cx, cz) => {
                if vol.sx == 1 && vol.sz == 1 && vol.x0 == *cx && vol.z0 == *cz { return i.sample_volume(ctx, out, vol); }
                let iv = Volume::stepped(1, vol.sy, 1, *cx, vol.y0, *cz, vol.stx, vol.sty, vol.stz);
                let b = vol_of(i, ctx, &iv);
                for iy in 0..vol.sy {
                    let v = b[iv.idx(0, iy, 0)];
                    let mut k = vol.idx(0, iy, 0);
                    for _ in 0..vol.sz * vol.sx { out[k] = v; k += vol.sy as usize; }
                }
            }
            S::SliceY(i, c) => {
                if vol.sy == 1 && vol.y0 == *c { return i.sample_volume(ctx, out, vol); }
                let iv = Volume::stepped(vol.sx, 1, vol.sz, vol.x0, *c, vol.z0, vol.stx, vol.sty, vol.stz);
                let b = vol_of(i, ctx, &iv);
                for iz in 0..vol.sz {
                    for ix in 0..vol.sx {
                        let s = vol.idx(ix, 0, iz);
                        out[s..s + vol.sy as usize].fill(b[iv.idx(ix, 0, iz)]);
                    }
                }
            }
            S::SliceZ(i, c) => {
                if vol.sz == 1 && vol.z0 == *c { return i.sample_volume(ctx, out, vol); }
                let iv = Volume::stepped(vol.sx, vol.sy, 1, vol.x0, vol.y0, *c, vol.stx, vol.sty, vol.stz);
                let b = vol_of(i, ctx, &iv);
                let mut k = 0;
                for _ in 0..vol.sz { for ix in 0..vol.sx { for iy in 0..vol.sy { out[k] = b[iv.idx(ix, iy, 0)]; k += 1; } } }
            }
            S::FindTop(d, u, lower, cell) => {
                assert_eq!(vol.sy, 1, "find_top_surface sampled with sizeY != 1");
                u.sample_volume(ctx, out, vol);
                let mut k = 0;
                for iz in 0..vol.sz {
                    for ix in 0..vol.sx {
                        out[k] = find_surface(d, ctx, vol.bx(ix), vol.bz(iz), out[k], *lower, *cell) as f32;
                        k += 1;
                    }
                }
            }
            S::Shared(i) => i.sample_volume(ctx, out, vol),
        }
    }
}

fn find_surface(density: &S, ctx: &mut Ctx, x: i32, z: i32, upper: f32, lower: i32, cell: i32) -> i32 {
    let top = (upper / cell as f32).floor() as i32 * cell;
    if top <= lower {
        return lower;
    }
    let mut y = top;
    while y >= lower {
        if density.sample(ctx, x, y, z) > 0.0 {
            return y;
        }
        y -= cell;
    }
    lower
}

#[allow(clippy::too_many_arguments)]
fn interp_volume(input: &S, cxz: i32, cy: i32, ixz: f32, iy: f32, ctx: &mut Ctx, out: &mut [f32], vol: &Volume) {
    let aligned = (vol.stx == cxz || vol.sx == 1) && (vol.sty == cy || vol.sy == 1) && (vol.stz == cxz || vol.sz == 1)
        && vol.x0.rem_euclid(cxz) == 0 && vol.y0.rem_euclid(cy) == 0 && vol.z0.rem_euclid(cxz) == 0;
    if aligned {
        return input.sample_volume(ctx, out, vol);
    }
    if vol.stx == 1 && vol.sty == 1 && vol.stz == 1 {
        return interp_block_step(input, cxz, cy, ixz, iy, ctx, out, vol);
    }
    // sampleWithNonBlockStep: interpolate at block resolution, then pick every step-th block
    let bv = Volume::new(vol.sx * vol.stx, vol.sy * vol.sty, vol.sz * vol.stz, vol.x0, vol.y0, vol.z0);
    let mut b = vec![0.0; bv.size()];
    interp_block_step(input, cxz, cy, ixz, iy, ctx, &mut b, &bv);
    for iz in 0..vol.sz {
        for ix in 0..vol.sx {
            for yy in 0..vol.sy {
                out[vol.idx(ix, yy, iz)] = b[bv.idx(ix * vol.stx, yy * vol.sty, iz * vol.stz)];
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn interp_block_step(input: &S, cxz: i32, cy: i32, ixz: f32, iy: f32, ctx: &mut Ctx, out: &mut [f32], vol: &Volume) {
    let (mcx, mcy, mcz) = (vol.x0.div_euclid(cxz), vol.y0.div_euclid(cy), vol.z0.div_euclid(cxz));
    let (ccx, ccy, ccz) = (vol.max_x().div_euclid(cxz) - mcx + 1, vol.max_y().div_euclid(cy) - mcy + 1, vol.max_z().div_euclid(cxz) - mcz + 1);
    let cv = Volume::stepped(
        if vol.max_x().rem_euclid(cxz) == 0 { ccx } else { ccx + 1 },
        if vol.max_y().rem_euclid(cy) == 0 { ccy } else { ccy + 1 },
        if vol.max_z().rem_euclid(cxz) == 0 { ccz } else { ccz + 1 },
        mcx * cxz, mcy * cy, mcz * cxz, cxz, cy, cxz,
    );
    let cb = vol_of(input, ctx, &cv);
    for cz in 0..ccz {
        let nz = (cz + 1).min(cv.sz - 1);
        for cx in 0..ccx {
            let nx = (cx + 1).min(cv.sx - 1);
            let mut v000 = cb[cv.idx(cx, 0, cz)];
            let mut v100 = cb[cv.idx(nx, 0, cz)];
            let mut v001 = cb[cv.idx(cx, 0, nz)];
            let mut v101 = cb[cv.idx(nx, 0, nz)];
            for cyy in 0..ccy {
                let ny = (cyy + 1).min(cv.sy - 1);
                let v010 = cb[cv.idx(cx, ny, cz)];
                let v110 = cb[cv.idx(nx, ny, cz)];
                let v011 = cb[cv.idx(cx, ny, nz)];
                let v111 = cb[cv.idx(nx, ny, nz)];
                // fillCell
                let (ox, oy, oz) = (cv.bx(cx) - vol.x0, cv.by(cyy) - vol.y0, cv.bz(cz) - vol.z0);
                let (x0, y0, z0) = (0.max(-ox), 0.max(-oy), 0.max(-oz));
                let (x1, y1, z1) = (cxz.min(vol.sx - ox) - 1, cy.min(vol.sy - oy) - 1, cxz.min(vol.sz - oz) - 1);
                for z in z0..=z1 {
                    let az = z as f32 * ixz;
                    let (v00, v01, v10, v11) = (lerpf(az, v000, v001), lerpf(az, v010, v011), lerpf(az, v100, v101), lerpf(az, v110, v111));
                    for x in x0..=x1 {
                        let ax = x as f32 * ixz;
                        let (a, b) = (lerpf(ax, v00, v10), lerpf(ax, v01, v11));
                        let step = (b - a) * iy;
                        let mut val = a + step * y0 as f32;
                        let mut k = vol.idx(ox + x, oy + y0, oz + z);
                        for _ in y0..=y1 {
                            out[k] = val;
                            k += 1;
                            val += step;
                        }
                    }
                }
                v000 = v010;
                v100 = v110;
                v001 = v011;
                v101 = v111;
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Compiler (DensityFunctionCompiler + RandomState's CompileContext)
// ------------------------------------------------------------------------------------------------

/// Rewritten cache inputs by cache id, with their range and axes. Seed-independent, so a World
/// rewrites once (`Compiler::rewrite`) and every seed compiles from the same table.
pub type Caches = Arc<Vec<(Iv, u8, Df)>>;

pub struct Compiler<'a> {
    pub defs: &'a HashMap<String, Df>,
    pub noise_params: &'a HashMap<String, NormalNoise>,
    pub random: Positional,
    noises: HashMap<String, Arc<NoiseStack>>,
    prepared: HashMap<String, usize>,
    pub caches: Caches,
    compiled: Vec<Option<Arc<S>>>,
}

impl<'a> Compiler<'a> {
    pub fn new(defs: &'a HashMap<String, Df>, noise_params: &'a HashMap<String, NormalNoise>, random: Positional) -> Self {
        Self::with_caches(defs, noise_params, random, Arc::new(vec![]))
    }

    /// A compiler for already rewritten functions (see `Caches`).
    pub fn with_caches(defs: &'a HashMap<String, Df>, noise_params: &'a HashMap<String, NormalNoise>, random: Positional, caches: Caches) -> Self {
        Self { defs, noise_params, random, noises: HashMap::new(), prepared: HashMap::new(), caches, compiled: vec![] }
    }

    /// RandomState.getOrCreateNoise: the noise's random is fromHashOf(its id).
    pub fn noise(&mut self, name: &str) -> Arc<NoiseStack> {
        if let Some(n) = self.noises.get(name) {
            return n.clone();
        }
        let params = self.noise_params.get(name).unwrap_or_else(|| panic!("unknown noise {name}"));
        let n = Arc::new(params.create(&mut self.random.from_hash(name)));
        self.noises.insert(name.to_string(), n.clone());
        n
    }

    /// DensityFunctionCompiler.getSampler
    pub fn compile_root(&mut self, df: &Df) -> Arc<S> {
        let f = self.rewrite(df);
        self.compile_rewritten(&f)
    }

    /// The seed-independent half of `compile_root`: inline refs, dedupe caches, slice axes.
    pub fn rewrite(&mut self, df: &Df) -> Df {
        let f = self.inline(df);
        self.slice(&f, 7)
    }

    /// The per-seed half of `compile_root`, for a function from `rewrite`.
    pub fn compile_rewritten(&mut self, f: &Df) -> Arc<S> {
        Arc::new(self.compile(f))
    }

    pub fn compile_ref(&mut self, name: &str) -> Arc<S> {
        let df = self.defs.get(name).unwrap_or_else(|| panic!("unknown density function {name}")).clone();
        self.compile_root(&df)
    }

    fn inline(&mut self, df: &Df) -> Df {
        match df {
            Df::Ref(name) => {
                let d = self.defs.get(name).unwrap_or_else(|| panic!("unknown density function {name}")).clone();
                self.inline(&d)
            }
            Df::Cache(input, key) => Df::Prepared(self.prepare(input, key)),
            _ => df.map_children(&mut |c| self.inline(c)),
        }
    }

    fn prepare(&mut self, input: &Df, key: &str) -> usize {
        if let Some(&id) = self.prepared.get(key) {
            return id;
        }
        let id = self.caches.len();
        Arc::make_mut(&mut self.caches).push((NAI, 0, Df::Const(0.0)));
        let f = self.inline(input);
        let f = self.slice(&f, 7);
        let (range, axes) = (self.range(&f), self.axes(&f));
        Arc::make_mut(&mut self.caches)[id] = (range, axes, f);
        self.prepared.insert(key.to_string(), id);
        id
    }

    /// DfRewriteRule.SliceUniformAxes
    fn slice(&self, df: &Df, parent: u8) -> Df {
        if matches!(df, Df::Const(_) | Df::Gradient { .. }) {
            return df.clone();
        }
        let axes = self.axes(df);
        if axes == parent {
            return df.map_children(&mut |c| self.slice(c, parent));
        }
        let mut f = df.map_children(&mut |c| self.slice(c, axes));
        let mut existing = 0;
        let mut cur = &f;
        while let Df::Slice(a, _, i) = cur {
            existing |= a;
            cur = i;
        }
        let removed = (parent & !axes) & !existing;
        for ax in [AX_X, AX_Z, AX_Y] {
            if removed & ax != 0 {
                f = Df::Slice(ax, 0, Box::new(f));
            }
        }
        f
    }

    fn noise_range(&self, name: &str) -> Iv {
        let r = self.noise_params[name].range;
        Iv::of(-r, r)
    }

    pub fn range(&self, df: &Df) -> Iv {
        match df {
            Df::Const(v) => Iv::exact(*v),
            Df::Ref(n) => self.range(&self.defs[n]),
            Df::Noise { noise, .. } => self.noise_range(noise),
            Df::ShiftA(n) | Df::ShiftB(n) | Df::Shift(n) => iv_mul(self.noise_range(n), Iv::exact(4.0)),
            Df::Gradient { fv, tv, .. } => iv_enc2(*fv, *tv),
            Df::Unary(op, i) => {
                let r = self.range(i);
                match op {
                    Un::Abs => iv_abs(r),
                    Un::Square => iv_square(r),
                    Un::Cube => iv_mono(r, |v| v * v * v),
                    Un::Sqrt => iv_mono(r, |v| (v as f64).sqrt() as f32),
                    Un::HalfNeg => iv_mono(r, |v| if v > 0.0 { v } else { v * 0.5 }),
                    Un::QuarterNeg => iv_mono(r, |v| if v > 0.0 { v } else { v * 0.25 }),
                    Un::Reciprocal => iv_reciprocal(r),
                    Un::Negate => iv_sub(Iv::exact(0.0), r),
                    Un::Squeeze => iv_mono(r, |v| unary(Un::Squeeze, v)),
                    Un::Log => if r.hi < 0.0 { NAI } else { iv_mono(iv_max(r, Iv::exact(0.0)), |v| (v as f64).ln() as f32) },
                    Un::Sign => iv_sign(r),
                }
            }
            Df::Bin(op, l, r) => {
                let (a, b) = (self.range(l), self.range(r));
                match op {
                    Bi::Add => iv_add(a, b),
                    Bi::Sub => iv_sub(a, b),
                    Bi::Mul => iv_mul(a, b),
                    Bi::Div => iv_mul(a, iv_reciprocal(b)),
                    Bi::Min => iv_min(a, b),
                    Bi::Max => iv_max(a, b),
                }
            }
            Df::Clamp(i, lo, hi) => iv_clamp(self.range(i), *lo, *hi),
            Df::Lerp(a, f, s) => iv_lerp(self.range(a), self.range(f), self.range(s)),
            Df::RangeChoice(_, _, _, a, b) => iv_enc(&[self.range(a), self.range(b)]),
            Df::IntervalSelect(_, _, fs) => iv_enc(&fs.iter().map(|f| self.range(f)).collect::<Vec<_>>()),
            Df::Spline(s) => self.spline_range(s),
            Df::Cache(i, _) | Df::BlendDensity(i) | Df::Interpolated(i, ..) | Df::Slice(_, _, i) => self.range(i),
            Df::BlendAlpha => Iv::of(0.0, 1.0),
            Df::BlendOffset | Df::Beardifier => Iv::of(f32::NEG_INFINITY, f32::INFINITY),
            Df::FindTopSurface(_, u, lower, _) => Iv::of(*lower as f32, jmax(*lower as f32, self.range(u).hi)),
            Df::OldBlended { y_scale, smear, .. } => {
                let r = blended_fbm_range(-15, 684.412 * y_scale * smear, 0.99998474f32 as f64);
                Iv::of(-r, r)
            }
            Df::Prepared(id) => self.caches[*id].0,
        }
    }

    fn spline_range(&self, s: &Spline<(String, Df)>) -> Iv {
        let (coord, loc, val, der) = match s {
            Spline::Const(c) => return Iv::exact(*c),
            Spline::Multi { coord, loc, val, der } => (coord, loc, val, der),
        };
        let last = loc.len() - 1;
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        let input = self.range(&coord.1);
        if input.nai() {
            return input;
        }
        let ext = |x: f32, v: f32, i: usize| if der[i] == 0.0 { v } else { v + der[i] * (x - loc[i]) };
        if input.lo < loc[0] {
            let r = self.spline_range(&val[0]);
            let (e1, e2) = (ext(input.lo, r.lo, 0), ext(input.lo, r.hi, 0));
            lo = jmin(lo, jmin(e1, e2));
            hi = jmax(hi, jmax(e1, e2));
        }
        if input.hi > loc[last] {
            let r = self.spline_range(&val[last]);
            let (e1, e2) = (ext(input.hi, r.lo, last), ext(input.hi, r.hi, last));
            lo = jmin(lo, jmin(e1, e2));
            hi = jmax(hi, jmax(e1, e2));
        }
        let ranges: Vec<Iv> = val.iter().map(|v| self.spline_range(v)).collect();
        for r in &ranges {
            lo = jmin(lo, r.lo);
            hi = jmax(hi, r.hi);
        }
        for i in 0..last {
            let xd = loc[i + 1] - loc[i];
            let (r1, r2) = (ranges[i], ranges[i + 1]);
            let (d1, d2) = (der[i], der[i + 1]);
            if d1 != 0.0 || d2 != 0.0 {
                let (p1, p2) = (d1 * xd, d2 * xd);
                let min_l1 = jmin(r1.lo, r2.lo);
                let max_l1 = jmax(r1.hi, r2.hi);
                let min_a = p1 - r2.hi + r1.lo;
                let max_a = p1 - r2.lo + r1.hi;
                let min_b = -p2 + r2.lo - r1.hi;
                let max_b = -p2 + r2.hi - r1.lo;
                lo = jmin(lo, min_l1 + 0.25 * jmin(min_a, min_b));
                hi = jmax(hi, max_l1 + 0.25 * jmax(max_a, max_b));
            }
        }
        Iv::of(lo, hi)
    }

    pub fn axes(&self, df: &Df) -> u8 {
        match df {
            Df::Const(_) => 0,
            Df::Ref(n) => self.axes(&self.defs[n]),
            Df::Noise { xz, y, sx, sy, sz, .. } => {
                let mut a = 7;
                if *y == 0.0 { a &= !AX_Y; }
                if *xz == 0.0 { a &= !(AX_X | AX_Z); }
                a | self.axes(sx) | self.axes(sy) | self.axes(sz)
            }
            Df::ShiftA(_) | Df::ShiftB(_) | Df::BlendAlpha | Df::BlendOffset => 5,
            Df::Shift(_) | Df::Beardifier | Df::OldBlended { .. } => 7,
            Df::Gradient { axis, .. } => *axis,
            Df::Unary(_, i) | Df::Clamp(i, ..) | Df::Cache(i, _) | Df::BlendDensity(i) | Df::Interpolated(i, ..) => self.axes(i),
            Df::Bin(_, l, r) => self.axes(l) | self.axes(r),
            Df::Lerp(a, b, c) | Df::RangeChoice(a, _, _, b, c) => self.axes(a) | self.axes(b) | self.axes(c),
            Df::IntervalSelect(i, _, fs) => fs.iter().fold(self.axes(i), |a, f| a | self.axes(f)),
            Df::Spline(s) => self.spline_axes(s),
            Df::Slice(ax, _, i) => self.axes(i) & !ax,
            Df::FindTopSurface(d, u, ..) => (self.axes(d) | self.axes(u)) & !AX_Y,
            Df::Prepared(id) => self.caches[*id].1,
        }
    }

    fn spline_axes(&self, s: &Spline<(String, Df)>) -> u8 {
        match s {
            Spline::Const(_) => 0,
            Spline::Multi { coord, val, .. } => val.iter().fold(self.axes(&coord.1), |a, v| a | self.spline_axes(v)),
        }
    }

    fn compile(&mut self, df: &Df) -> S {
        let bx = |s: S| Box::new(s);
        match df {
            Df::Const(v) => S::Const(*v),
            Df::Ref(_) | Df::Cache(..) => unreachable!("rewrite pass must remove refs and caches"),
            Df::Noise { noise, xz, y, sx, sy, sz } => {
                let n = self.noise(noise);
                let zero = |d: &Df| matches!(d, Df::Const(v) if *v == 0.0 && v.is_sign_positive());
                if zero(sx) && zero(sy) && zero(sz) {
                    return S::Noise(n, *xz, *y);
                }
                let (cx, cz) = (self.compile(sx), self.compile(sz));
                if zero(sy) {
                    S::ShiftedXz(bx(cx), bx(cz), n, *xz, *y)
                } else {
                    S::ShiftedXyz(bx(cx), bx(self.compile(sy)), bx(cz), n, *xz, *y)
                }
            }
            Df::ShiftA(n) => S::ConstMul(bx(S::Noise(self.noise(n), 0.25, 0.0)), 4.0),
            Df::Shift(n) => S::ConstMul(bx(S::Noise(self.noise(n), 0.25, 0.25)), 4.0),
            Df::ShiftB(n) => S::ShiftB(self.noise(n)),
            Df::Gradient { axis, tiling, from, to, fv, tv } => {
                let range = to - from;
                S::Grad { axis: *axis, tiling: *tiling, from: *from, lo: *from.min(to), hi: *from.max(to), range, fv: *fv, factor: (tv - fv) / range as f32 }
            }
            Df::Unary(op, i) => S::Unary(*op, bx(self.compile(i))),
            Df::Bin(op, l, r) => {
                let (cl, cr) = (self.compile(l), self.compile(r));
                let lc = if let Df::Const(v) = **l { Some(v) } else { None };
                let rc = if let Df::Const(v) = **r { Some(v) } else { None };
                match op {
                    Bi::Add => match (lc, rc) { (Some(a), _) => S::ConstAdd(bx(cr), a), (_, Some(b)) => S::ConstAdd(bx(cl), b), _ => S::Add(bx(cl), bx(cr)) },
                    Bi::Sub => match (lc, rc) { (Some(a), _) => S::ConstSub(a, bx(cr)), (_, Some(b)) => S::ConstAdd(bx(cl), -b), _ => S::Sub(bx(cl), bx(cr)) },
                    Bi::Mul => match (lc, rc) { (Some(a), _) => S::ConstMul(bx(cr), a), (_, Some(b)) => S::ConstMul(bx(cl), b), _ => S::Mul(bx(cl), bx(cr)) },
                    Bi::Div => match (lc, rc) { (Some(a), _) => S::ConstDiv(a, bx(cr)), (_, Some(b)) => S::ConstMul(bx(cl), 1.0 / b), _ => S::Div(bx(cl), bx(cr)) },
                    Bi::Min | Bi::Max => {
                        let (ra, rb) = (self.range(l), self.range(r));
                        let is_min = *op == Bi::Min;
                        if (is_min && ra.hi < rb.lo) || (!is_min && ra.lo > rb.hi) { return cl; }
                        if (is_min && rb.hi < ra.lo) || (!is_min && rb.lo > ra.hi) { return cr; }
                        match (lc, rc, is_min) {
                            (Some(a), _, true) => S::ConstMin(bx(cr), a),
                            (_, Some(b), true) => S::ConstMin(bx(cl), b),
                            (_, _, true) => S::Min(bx(cl), bx(cr), rb.lo),
                            (Some(a), _, false) => S::ConstMax(bx(cr), a),
                            (_, Some(b), false) => S::ConstMax(bx(cl), b),
                            _ => S::Max(bx(cl), bx(cr), rb.hi),
                        }
                    }
                }
            }
            Df::Clamp(i, lo, hi) => S::Clamp(bx(self.compile(i)), *lo, *hi),
            Df::Lerp(a, f, s) => {
                let (ca, cf, cs) = (self.compile(a), self.compile(f), self.compile(s));
                match (&**f, &**s) {
                    (Df::Const(v), _) => S::LerpCF(bx(ca), *v, bx(cs)),
                    (_, Df::Const(v)) => S::LerpCS(bx(ca), bx(cf), *v),
                    _ => S::Lerp(bx(ca), bx(cf), bx(cs)),
                }
            }
            Df::RangeChoice(i, lo, hi, a, b) => {
                let ci = self.compile(i);
                match (&**a, &**b) {
                    (Df::Const(x), Df::Const(y)) => S::RangeConst(bx(ci), *lo, *hi, *x, *y),
                    _ => S::Range(bx(ci), *lo, *hi, bx(self.compile(a)), bx(self.compile(b))),
                }
            }
            Df::IntervalSelect(i, ts, fs) => {
                let ci = self.compile(i);
                if ts.len() == 1 {
                    S::Sel1(bx(ci), ts[0], bx(self.compile(&fs[0])), bx(self.compile(fs.last().unwrap())))
                } else {
                    S::Sel(bx(ci), ts.clone(), fs.iter().map(|f| self.compile(f)).collect())
                }
            }
            Df::Spline(s) => {
                let mut keys: Vec<String> = vec![];
                let mut coords: Vec<S> = vec![];
                let cs = self.compile_spline(s, &mut keys, &mut coords);
                S::Spline(Box::new(cs), coords)
            }
            Df::BlendDensity(i) => self.compile(i), // empty blender: pass-through
            Df::BlendAlpha => S::Const(1.0),
            Df::BlendOffset | Df::Beardifier => S::Const(0.0),
            Df::Interpolated(i, cxz, cy) => S::Interp(bx(self.compile(i)), *cxz, *cy, 1.0 / *cxz as f32, 1.0 / *cy as f32),
            Df::Slice(ax, c, i) => {
                if let Df::Slice(iax, ic, ii) = &**i {
                    if (*ax == AX_X && *iax == AX_Z) || (*ax == AX_Z && *iax == AX_X) {
                        let (x, z) = if *ax == AX_X { (*c, *ic) } else { (*ic, *c) };
                        return S::SliceXz(bx(self.compile(ii)), x, z);
                    }
                }
                let ci = bx(self.compile(i));
                match *ax { AX_X => S::SliceX(ci, *c), AX_Y => S::SliceY(ci, *c), _ => S::SliceZ(ci, *c) }
            }
            Df::FindTopSurface(d, u, lower, cell) => {
                S::SliceY(bx(S::FindTop(bx(self.compile(d)), bx(self.compile(u)), *lower, *cell)), 0)
            }
            Df::OldBlended { xz_scale, y_scale, xz_factor, y_factor, smear } => {
                let mut r = self.random.from_hash("minecraft:terrain");
                let (xzm, ym) = (684.412 * xz_scale, 684.412 * y_scale);
                let limit_smear = ym * smear;
                let lo = Arc::new(blended_fbm(&mut r, -15, limit_smear, 0.99998474f32 as f64));
                let hi = Arc::new(blended_fbm(&mut r, -15, limit_smear, 0.99998474f32 as f64));
                let main = Arc::new(blended_fbm(&mut r, -7, limit_smear / y_factor, 12.75));
                let choice = S::Clamp(bx(S::ConstAdd(bx(S::Noise(main, xzm / xz_factor, ym / y_factor)), 0.5)), 0.0, 1.0);
                S::Lerp(bx(choice), bx(S::Noise(lo, xzm, ym)), bx(S::Noise(hi, xzm, ym)))
            }
            Df::Prepared(id) => {
                // compiled on first use, then shared by every function that reads the cache
                self.compiled.resize(self.caches.len(), None);
                if self.compiled[*id].is_none() {
                    let caches = self.caches.clone();
                    let s = Arc::new(S::Cache(*id, Arc::new(self.compile(&caches[*id].2))));
                    self.compiled[*id] = Some(s);
                }
                S::Shared(self.compiled[*id].clone().unwrap())
            }
        }
    }

    fn compile_spline(&mut self, s: &Spline<(String, Df)>, keys: &mut Vec<String>, coords: &mut Vec<S>) -> CSpline {
        match s {
            Spline::Const(c) => CSpline::Const(*c),
            Spline::Multi { coord, loc, val, der } => {
                let idx = match keys.iter().position(|k| *k == coord.0) {
                    Some(i) => i,
                    None => {
                        keys.push(coord.0.clone());
                        let c = self.compile(&coord.1);
                        coords.push(c);
                        keys.len() - 1
                    }
                };
                assert!(idx < 32, "spline has more than 32 distinct coordinates");
                CSpline::Multi { coord: idx, loc: loc.clone(), val: val.iter().map(|v| self.compile_spline(v, keys, coords)).collect(), der: der.clone() }
            }
        }
    }
}
