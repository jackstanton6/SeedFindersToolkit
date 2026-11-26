//! LevelChunk.postProcessGeneration: the one fluid tick a chunk gets when it becomes FULL, for the
//! positions generation marked (FlowingFluid.tick / spread for water and lava, LiquidBlock's lava
//! and water conversions). Scheduled ticks are never run (a world just generated has not ticked).

use crate::blocks::State;
use crate::chunk::{Chunk, Materials};
use crate::world::World;
use std::collections::HashMap;

/// Direction order (down, up, north, south, west, east) and steps.
const STEP: [(i32, i32, i32); 6] = [(0, -1, 0), (0, 1, 0), (0, 0, -1), (0, 0, 1), (-1, 0, 0), (1, 0, 0)];
const DOWN: usize = 0;
const UP: usize = 1;
/// Direction.Plane.HORIZONTAL: north, east, south, west.
const HORIZONTAL: [usize; 4] = [2, 5, 3, 4];
/// NeighborUpdater.UPDATE_ORDER: west, east, down, up, north, south.
const UPDATE_ORDER: [usize; 6] = [4, 5, 0, 1, 2, 3];
/// LiquidBlock.POSSIBLE_FLOW_DIRECTIONS: down, south, north, east, west.
const FLOW_DIRECTIONS: [usize; 5] = [0, 3, 2, 5, 4];

fn opposite(d: usize) -> usize {
    d ^ 1
}

/// A FluidState: type (0 empty, 1 water, 2 lava), amount, source, falling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct F {
    ty: u8,
    amount: u8,
    source: bool,
    falling: bool,
}

const EMPTY: F = F { ty: 0, amount: 0, source: false, falling: false };

/// The 3x3 chunks around the one being finished, as the level the fluid code reads and writes.
pub struct Region<'a> {
    w: &'a World,
    m: &'a Materials,
    cx: i32,
    cz: i32,
    pub chunks: Vec<Chunk>,
    washed: Vec<bool>,
    soul_soil: State,
    blue_ice: State,
}

impl<'a> Region<'a> {
    /// `chunks`: the 3x3 chunks, rows of x for z = cz-1..=cz+1.
    pub fn new(w: &'a World, m: &'a Materials, cx: i32, cz: i32, chunks: Vec<Chunk>) -> Self {
        let b = &w.blocks;
        Region {
            w, m, cx, cz, chunks,
            washed: b.tag_table("minecraft:washed_away_by_fluids"),
            soul_soil: b.state("soul_soil").unwrap_or(0),
            blue_ice: b.state("blue_ice").unwrap_or(0),
        }
    }

    fn chunk(&self, x: i32, z: i32) -> Option<usize> {
        let (dx, dz) = ((x >> 4) - self.cx + 1, (z >> 4) - self.cz + 1);
        if (0..3).contains(&dx) && (0..3).contains(&dz) { Some((dz * 3 + dx) as usize) } else { None }
    }

    pub fn get(&self, x: i32, y: i32, z: i32) -> State {
        // ponytail: outside the 3x3 chunks reads as void air; fluids spread at most a few blocks
        self.chunk(x, z).map_or(self.w.blocks.void_air, |i| self.chunks[i].get(self.w, x, y, z))
    }

    fn put(&mut self, x: i32, y: i32, z: i32, s: State) {
        if let Some(i) = self.chunk(x, z) {
            let (m, w) = (self.m, self.w);
            self.chunks[i].set(m, w, x, y, z, s);
        }
    }

    fn fluid_of(&self, s: State) -> F {
        let (ty, amount, source, falling) = self.w.blocks.fluid(s);
        F { ty, amount, source, falling }
    }

    fn fluid(&self, p: (i32, i32, i32)) -> F {
        self.fluid_of(self.get(p.0, p.1, p.2))
    }

    fn is_liquid_block(&self, s: State) -> bool {
        let n = self.w.blocks.name(s);
        n == "minecraft:water" || n == "minecraft:lava"
    }

    /// FlowingFluid.createLegacyBlock (via LiquidBlock.LEVEL)
    fn legacy_block(&self, f: F) -> State {
        let b = &self.w.blocks;
        match f.ty {
            0 => b.air,
            ty => {
                let base = if ty == 1 { b.water } else { b.lava };
                let level = if f.source { 0 } else { 8 - f.amount.min(8) + if f.falling { 8 } else { 0 } };
                b.with(base, "level", &level.to_string()).unwrap_or(base)
            }
        }
    }

    /// Level.setBlockAndUpdate: the block, its onPlace, then neighbour updates.
    fn set_and_update(&mut self, p: (i32, i32, i32), s: State) {
        self.put(p.0, p.1, p.2, s);
        // LiquidBlock.onPlace
        if self.is_liquid_block(s) {
            self.should_spread_liquid(p, s);
        }
        // neighbour updates: a liquid neighbour checks for lava meeting water
        // ponytail: shape updates of other blocks are not run (terrain blocks have none); the game
        // queues nested updates (CollectingNeighborUpdater), here they run depth first
        for &d in &UPDATE_ORDER {
            let q = (p.0 + STEP[d].0, p.1 + STEP[d].1, p.2 + STEP[d].2);
            let n = self.get(q.0, q.1, q.2);
            if self.is_liquid_block(n) {
                self.should_spread_liquid(q, n);
            }
        }
    }

    /// LiquidBlock.shouldSpreadLiquid: lava next to water turns to obsidian (source) or
    /// cobblestone; over soul soil next to blue ice, basalt.
    fn should_spread_liquid(&mut self, p: (i32, i32, i32), s: State) {
        if self.fluid_of(s).ty != 2 {
            return;
        }
        let below = self.get(p.0, p.1 - 1, p.2);
        let over_soul_soil = self.w.blocks.name(below) == self.w.blocks.name(self.soul_soil);
        for &d in &FLOW_DIRECTIONS {
            let o = STEP[opposite(d)];
            let q = (p.0 + o.0, p.1 + o.1, p.2 + o.2);
            if self.fluid(q).ty == 1 {
                let to = if self.fluid(p).source { "obsidian" } else { "cobblestone" };
                let st = self.w.blocks.state(to).unwrap();
                self.set_and_update(p, st);
                return;
            }
            if over_soul_soil && self.w.blocks.name(self.get(q.0, q.1, q.2)) == self.w.blocks.name(self.blue_ice) {
                let st = self.w.blocks.state("basalt").unwrap();
                self.set_and_update(p, st);
                return;
            }
        }
    }

    // --- FlowingFluid ------------------------------------------------------------------------

    fn drop_off(ty: u8) -> u8 {
        if ty == 1 { 1 } else { 2 }
    }

    fn slope_find_distance(ty: u8) -> i32 {
        if ty == 1 { 4 } else { 2 }
    }

    /// canConvertToSource: the gamerules waterSourceConversion (on) and lavaSourceConversion (off).
    fn can_convert_to_source(ty: u8) -> bool {
        ty == 1
    }

    /// canPassThroughWall
    fn pass_wall(&self, d: usize, source: State, target: State) -> bool {
        let b = &self.w.blocks;
        if b.collision_full(target) || b.collision_full(source) {
            return false;
        }
        if b.collision_empty(source) && b.collision_empty(target) {
            return true;
        }
        // ponytail: Shapes.mergedFaceOccludes approximated by either face being full
        !(b.collision_face_full(source, d) || b.collision_face_full(target, opposite(d)))
    }

    fn is_container(&self, s: State) -> bool {
        // LiquidBlockContainer: waterloggable blocks
        // ponytail: kelp, seagrass and other non-waterlogged containers are not told apart yet
        self.w.blocks.get(s, "waterlogged").is_some()
    }

    fn can_hold_any(&self, s: State) -> bool {
        self.is_container(s) || self.w.blocks.in_table(&self.washed, s)
    }

    /// canHoldSpecificFluid; a waterloggable block takes only source water (fluid == Fluids.WATER)
    fn can_hold_specific(&self, s: State, ty: u8, source: bool) -> bool {
        if self.is_container(s) { self.w.blocks.get(s, "waterlogged") == Some("false") && ty == 1 && source } else { true }
    }

    fn is_source_of(&self, f: F, ty: u8) -> bool {
        f.ty == ty && f.source
    }

    fn can_maybe_pass(&self, ty: u8, src: State, d: usize, test: State, tf: F) -> bool {
        !self.is_source_of(tf, ty) && self.can_hold_any(test) && self.pass_wall(d, src, test)
    }

    /// canPassThrough with this.getFlowing()
    fn can_pass(&self, ty: u8, src: State, d: usize, test: State, tf: F) -> bool {
        self.can_maybe_pass(ty, src, d, test, tf) && self.can_hold_specific(test, ty, false)
    }

    /// FluidState.getHeight: 1 under the same fluid, else amount / 9
    fn height(&self, p: (i32, i32, i32), f: F) -> f32 {
        if f.ty != 0 && self.fluid((p.0, p.1 + 1, p.2)).ty == f.ty { 1.0 } else { f.amount as f32 / 9.0 }
    }

    /// Fluid.canBeReplacedWith for the fluid already at `p`
    fn can_be_replaced_with(&self, p: (i32, i32, i32), f: F, other: u8, d: usize) -> bool {
        match f.ty {
            0 => true,
            1 => d == DOWN && other != 1,
            _ => self.height(p, f) >= 0.44444445 && other == 1,
        }
    }

    fn rel(p: (i32, i32, i32), d: usize) -> (i32, i32, i32) {
        (p.0 + STEP[d].0, p.1 + STEP[d].1, p.2 + STEP[d].2)
    }

    /// getNewLiquid
    fn new_liquid(&self, ty: u8, p: (i32, i32, i32), state: State) -> F {
        let (mut highest, mut sources) = (0u8, 0);
        for &d in &HORIZONTAL {
            let q = Self::rel(p, d);
            let bs = self.get(q.0, q.1, q.2);
            let f = self.fluid_of(bs);
            if f.ty == ty && self.pass_wall(d, state, bs) {
                if f.source {
                    sources += 1;
                }
                highest = highest.max(f.amount);
            }
        }
        if sources >= 2 && Self::can_convert_to_source(ty) {
            let below = self.get(p.0, p.1 - 1, p.2);
            if self.w.blocks.is_solid(below) || self.is_source_of(self.fluid_of(below), ty) {
                return F { ty, amount: 8, source: true, falling: false };
            }
        }
        let above = Self::rel(p, UP);
        let a = self.get(above.0, above.1, above.2);
        let af = self.fluid_of(a);
        if af.ty != 0 && af.ty == ty && self.pass_wall(UP, state, a) {
            return F { ty, amount: 8, source: false, falling: true };
        }
        let amount = highest as i32 - Self::drop_off(ty) as i32;
        if amount <= 0 { EMPTY } else { F { ty, amount: amount as u8, source: false, falling: false } }
    }

    fn is_water_hole(&self, ty: u8, top: State, bottom_pos: (i32, i32, i32), bottom: State) -> bool {
        let _ = bottom_pos;
        if !self.pass_wall(DOWN, top, bottom) {
            return false;
        }
        if self.fluid_of(bottom).ty == ty { true } else { self.can_hold_any(bottom) && self.can_hold_specific(bottom, ty, false) }
    }

    /// getSlopeDistance; the SpreadContext caches by x, z (one level)
    #[allow(clippy::too_many_arguments)]
    fn slope_distance(&self, ty: u8, p: (i32, i32, i32), pass: i32, from: usize, state: State, holes: &mut HashMap<(i32, i32), bool>) -> i32 {
        let mut lowest = 1000;
        for &d in &HORIZONTAL {
            if d == from {
                continue;
            }
            let q = Self::rel(p, d);
            let ts = self.get(q.0, q.1, q.2);
            let tf = self.fluid_of(ts);
            if self.can_pass(ty, state, d, ts, tf) {
                if self.hole(ty, q, holes) {
                    return pass;
                }
                if pass < Self::slope_find_distance(ty) {
                    let v = self.slope_distance(ty, q, pass + 1, opposite(d), ts, holes);
                    lowest = lowest.min(v);
                }
            }
        }
        lowest
    }

    fn hole(&self, ty: u8, q: (i32, i32, i32), holes: &mut HashMap<(i32, i32), bool>) -> bool {
        if let Some(&h) = holes.get(&(q.0, q.2)) {
            return h;
        }
        let top = self.get(q.0, q.1, q.2);
        let b = (q.0, q.1 - 1, q.2);
        let h = self.is_water_hole(ty, top, b, self.get(b.0, b.1, b.2));
        holes.insert((q.0, q.2), h);
        h
    }

    /// getSpread: the cheapest directions (EnumMap: in Direction order)
    fn get_spread(&self, ty: u8, p: (i32, i32, i32), state: State) -> Vec<(usize, F)> {
        let mut lowest = 1000;
        let mut result: Vec<(usize, F)> = vec![];
        let mut holes = HashMap::new();
        for &d in &HORIZONTAL {
            let q = Self::rel(p, d);
            let ts = self.get(q.0, q.1, q.2);
            let tf = self.fluid_of(ts);
            if self.can_maybe_pass(ty, state, d, ts, tf) {
                let nf = self.new_liquid(ty, q, ts);
                if self.can_hold_specific(ts, nf.ty, nf.source) {
                    let dist = if self.hole(ty, q, &mut holes) { 0 } else { self.slope_distance(ty, q, 1, opposite(d), ts, &mut holes) };
                    if dist < lowest {
                        result.clear();
                    }
                    if dist <= lowest {
                        if self.can_be_replaced_with(q, tf, nf.ty, d) {
                            result.retain(|e| e.0 != d);
                            result.push((d, nf));
                        }
                        lowest = dist;
                    }
                }
            }
        }
        result.sort_by_key(|e| e.0);
        result
    }

    /// spreadTo (LavaFluid's: lava flowing down into water makes stone)
    fn spread_to(&mut self, ty: u8, q: (i32, i32, i32), state: State, d: usize, target: F) {
        if ty == 2 && d == DOWN && self.fluid(q).ty == 1 {
            if self.is_liquid_block(state) {
                let stone = self.w.blocks.stone;
                self.set_and_update(q, stone);
            }
            return;
        }
        if self.is_container(state) {
            // SimpleWaterloggedBlock.placeLiquid
            if target.ty == 1 && target.source && self.w.blocks.get(state, "waterlogged") == Some("false") {
                if let Some(s) = self.w.blocks.with(state, "waterlogged", "true") {
                    self.set_and_update(q, s);
                }
            }
            return;
        }
        let s = self.legacy_block(target);
        self.set_and_update(q, s);
    }

    fn spread_to_sides(&mut self, ty: u8, p: (i32, i32, i32), f: F, state: State) {
        let mut neighbour = f.amount as i32 - Self::drop_off(ty) as i32;
        if f.falling {
            neighbour = 7;
        }
        if neighbour > 0 {
            for (d, nf) in self.get_spread(ty, p, state) {
                let q = Self::rel(p, d);
                let s = self.get(q.0, q.1, q.2);
                self.spread_to(ty, q, s, d, nf);
            }
        }
    }

    fn source_neighbours(&self, ty: u8, p: (i32, i32, i32)) -> usize {
        HORIZONTAL.iter().filter(|&&d| self.is_source_of(self.fluid(Self::rel(p, d)), ty)).count()
    }

    /// FlowingFluid.spread
    fn spread(&mut self, p: (i32, i32, i32), state: State, f: F) {
        if f.ty == 0 {
            return;
        }
        let ty = f.ty;
        let below = (p.0, p.1 - 1, p.2);
        let bs = self.get(below.0, below.1, below.2);
        let bf = self.fluid_of(bs);
        if self.can_maybe_pass(ty, state, DOWN, bs, bf) {
            let nb = self.new_liquid(ty, below, bs);
            if self.can_be_replaced_with(below, bf, nb.ty, DOWN) && self.can_hold_specific(bs, nb.ty, nb.source) {
                self.spread_to(ty, below, bs, DOWN, nb);
                if self.source_neighbours(ty, p) >= 3 {
                    self.spread_to_sides(ty, p, f, state);
                }
                return;
            }
        }
        if f.source || !self.is_water_hole(ty, state, below, bs) {
            self.spread_to_sides(ty, p, f, state);
        }
    }

    /// FlowingFluid.tick
    fn tick(&mut self, p: (i32, i32, i32), mut state: State, mut f: F) {
        if !f.source {
            let nf = self.new_liquid(f.ty, p, self.get(p.0, p.1, p.2));
            if nf.ty == 0 {
                f = nf;
                state = self.w.blocks.air;
                self.set_and_update(p, state);
            } else if nf != f {
                f = nf;
                state = self.legacy_block(f);
                self.set_and_update(p, state);
            }
        }
        self.spread(p, state, f);
    }

    /// BubbleColumnBlock.canOccupy
    fn can_occupy(&self, s: State) -> bool {
        if self.w.blocks.name(s) == "minecraft:bubble_column" {
            return true;
        }
        let f = self.fluid_of(s);
        f.ty == 1 && self.is_liquid_block(s) && f.source && f.amount >= 8
    }

    /// BubbleColumnBlock.updateColumn
    fn bubble_column(&mut self, p: (i32, i32, i32), below: State) {
        let occupy = self.get(p.0, p.1, p.2);
        if !self.can_occupy(occupy) {
            return;
        }
        let b = &self.w.blocks;
        let col = b.state("bubble_column").unwrap();
        let column = if b.name(below) == "minecraft:bubble_column" {
            below
        } else if b.is(below, "minecraft:enables_bubble_column_push_up") {
            b.with(col, "drag", "false").unwrap()
        } else if b.is(below, "minecraft:enables_bubble_column_drag_down") {
            b.with(col, "drag", "true").unwrap()
        } else if b.name(occupy) == "minecraft:bubble_column" {
            b.water
        } else {
            occupy
        };
        self.put(p.0, p.1, p.2, column);
        let mut q = (p.0, p.1 + 1, p.2);
        while self.can_occupy(self.get(q.0, q.1, q.2)) {
            if self.chunk(q.0, q.2).is_none() {
                return;
            }
            self.put(q.0, q.1, q.2, column);
            q.1 += 1;
        }
    }

    /// LevelChunk.postProcessGeneration for the centre chunk, after the neighbours' marked
    /// positions next to it (a fluid tick reaches one block, so only those can spill into it).
    // ponytail: the game finishes chunks in no fixed order; neighbours first is the usual case
    pub fn post_process(&mut self) {
        for i in [0, 1, 2, 3, 5, 6, 7, 8, 4] {
            let (cx, cz, min_y) = (self.chunks[i].x, self.chunks[i].z, self.chunks[i].min_y);
            let lists = std::mem::take(&mut self.chunks[i].post);
            let (x0, z0) = (self.cx * 16, self.cz * 16);
            for (sec, list) in lists.iter().enumerate() {
                for &packed in list {
                    let p = (cx * 16 + (packed & 15) as i32, min_y + sec as i32 * 16 + (packed >> 4 & 15) as i32, cz * 16 + (packed >> 8 & 15) as i32);
                    if i != 4 && (p.0 < x0 - 1 || p.0 > x0 + 16 || p.2 < z0 - 1 || p.2 > z0 + 16) {
                        continue;
                    }
                    let state = self.get(p.0, p.1, p.2);
                    let f = self.fluid_of(state);
                    if f.ty != 0 {
                        self.tick(p, state, f);
                    }
                    // LiquidBlock.tick (called directly here): a full water source over magma or
                    // soul sand becomes a bubble column
                    if self.is_liquid_block(state) && f.ty == 1 && f.source && f.amount >= 8 {
                        let below = self.get(p.0, p.1 - 1, p.2);
                        self.bubble_column(p, below);
                    }
                    // LiquidBlock.tick only schedules bubble columns; other blocks would get
                    // updateFromNeighbourShapes
                    // ponytail: shape updates of marked non-fluid blocks are not run yet
                }
            }
        }
    }
}
