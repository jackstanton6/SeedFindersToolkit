//! Structure-seed cracking: known structure start chunks -> the 48-bit structure seeds that place
//! them all. The 65536 sister seeds of each result are then checked with a normal list search.
//!
//! Low-bit lifting: a random-spread offset is `nextInt(limit)` = `next(31) % limit`. When 2^k
//! divides `limit`, the offset mod 2^k equals bits 17..17+k of the LCG state, which depend only on
//! the low 17+k bits of the world seed. So the low bits are enumerated and filtered first, and only
//! the survivors get the high bits brute-forced.

use crate::structure::{Placement, Structures};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// A structure start the user saw: set index and start chunk.
#[derive(Clone, Copy, Debug)]
pub struct Obs {
    pub set: usize,
    pub chunk: (i32, i32),
}

struct Cell {
    set: usize,
    grid: (i32, i32),
    chunk: (i32, i32),
    /// low bits of the offsets this observation fixes (0: odd or power-of-two limit, triangular)
    k: u32,
}

/// Cells to check, the lifted low-bit width and its survivors. `work` = seeds the brute force tries.
pub struct Plan {
    cells: Vec<Cell>,
    bits: u32,
    pub lows: Vec<i64>,
    pub work: u64,
}

pub fn plan(st: &Structures, obs: &[Obs]) -> Result<Plan, String> {
    let mut cells = vec![];
    for o in obs {
        let s = &st.sets[o.set];
        let Placement::Spread { spacing, separation, triangular } = s.placement else {
            return Err(format!("{}: only random-spread structures can be cracked (not strongholds)", s.name));
        };
        let limit = spacing - separation;
        let grid = (o.chunk.0.div_euclid(spacing), o.chunk.1.div_euclid(spacing));
        let off = (o.chunk.0 - grid.0 * spacing, o.chunk.1 - grid.1 * spacing);
        if off.0 >= limit || off.1 >= limit {
            return Err(format!("{} cannot start in chunk {:?} (offset {off:?} >= {limit})", s.name, o.chunk));
        }
        let k = if triangular || limit & (limit - 1) == 0 { 0 } else { limit.trailing_zeros() };
        cells.push(Cell { set: o.set, grid, chunk: o.chunk, k });
    }
    if cells.is_empty() {
        return Err("no observations".into());
    }
    // most selective first: the brute force rejects on the first mismatch
    cells.sort_by_key(|c| match st.sets[c.set].placement {
        Placement::Spread { spacing, separation, .. } => -(spacing - separation),
        _ => 0,
    });
    let bits = 17 + cells.iter().map(|c| c.k).max().unwrap();
    // ponytail: ignores nextInt's rejection loop (p ~ limit/2^31 per call); such seeds are missed
    let lows: Vec<i64> = (0..1i64 << bits)
        .into_par_iter()
        .filter(|&lo| {
            cells.iter().filter(|c| c.k > 0).all(|c| {
                let got = st.grid_chunk(c.set, lo, c.grid.0, c.grid.1).unwrap();
                let m = (1 << c.k) - 1;
                (got.0 ^ c.chunk.0) & m == 0 && (got.1 ^ c.chunk.1) & m == 0
            })
        })
        .collect();
    let work = (lows.len() as u64) << (48 - bits);
    Ok(Plan { cells, bits, lows, work })
}

/// Brute-forces the high bits for every surviving low-bit pattern. `done` counts seeds tried.
pub fn run(st: &Structures, p: &Plan, stop: &AtomicBool, done: &AtomicU64) -> Vec<i64> {
    let high = 1i64 << (48 - p.bits);
    let mut out: Vec<i64> = p
        .lows
        .iter()
        .flat_map(|&lo| {
            (0..high)
                .into_par_iter()
                .map(|hi| (hi << p.bits) | lo)
                .filter(|&seed| {
                    if seed >> p.bits & 0xFFF == 0 {
                        done.fetch_add(0x1000, Relaxed);
                    }
                    !stop.load(Relaxed) && p.cells.iter().all(|c| st.grid_chunk(c.set, seed, c.grid.0, c.grid.1) == Some(c.chunk))
                })
                .collect::<Vec<_>>()
        })
        // frequency reducers and exclusion zones (outposts near villages) last: they cost more
        .filter(|&seed| p.cells.iter().all(|c| st.is_structure_chunk(c.set, seed, c.chunk.0, c.chunk.1)))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;
    use std::path::Path;

    /// Needs data/26.3: recovers a structure seed from its own huts, outposts, temples, shipwrecks.
    #[test]
    #[ignore]
    fn cracks_own_structures() {
        let w = World::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/26.3")).unwrap();
        let st = &w.structures;
        let seed = 0x1234_5678_9ABCi64;
        let mut obs = vec![];
        for name in ["swamp_huts", "pillager_outposts", "desert_pyramids", "shipwrecks", "villages"] {
            let set = st.sets.iter().position(|s| s.name.ends_with(name)).unwrap();
            for (gx, gz) in [(0, 0), (1, -1)] {
                let c = st.grid_chunk(set, seed, gx, gz).unwrap();
                if st.is_structure_chunk(set, seed, c.0, c.1) {
                    obs.push(Obs { set, chunk: c });
                }
            }
        }
        let p = plan(st, &obs).unwrap();
        eprintln!("{} observations, {} low candidates, work {}", obs.len(), p.lows.len(), p.work);
        let found = run(st, &p, &AtomicBool::new(false), &AtomicU64::new(0));
        assert!(found.contains(&seed), "{found:?}");
    }
}
