//! `.bsp` — the collision and space-partition trees.
//!
//! Despite the extension these hold no geometry. A `.bsp` is a flat array of
//! 24-byte nodes and nothing else — no header, no signature, no counts, so
//! the node count is simply `filesize / 24`. The renderable geometry of a
//! level lives in the big `.mod` files; this is the partition the engine
//! collides against, matching the debug paths `omPolyhedron.c` and
//! `omCollision.c`.
//!
//! ```text
//! struct node {          // 24 bytes
//!     float normal[3];   // unit plane normal
//!     float dist;        // plane distance from the origin
//!     u32   front;       // child index, 0xFFFFFFFF for a leaf
//!     u32   back;        // child index, 0xFFFFFFFF for a leaf
//! };
//! ```
//!
//! Node 0 is the root, and a point is **inside** when the descent reaches a
//! leaf through the *front* child. `l6r8_Stack5.bsp` shows this with nothing
//! to interpret: seven planes forming one oriented 4.5-cube, every `back` a
//! leaf, `front` running 0 -> 1 -> ... -> 6. It is a crate from a stack, and
//! it is a crate.
//!
//! **The query point must be negated.** The same file settles it: its box is
//! centred on (0.73, 219.73, -2.25) while `l6r8_Stack5.mod` is centred on
//! the exact opposite. The tree is authored in a mirrored frame. Testing
//! points either side of a face separates them 799 of 800 on `l3_maze`;
//! without the negation it is 0 of 800, every point landing on the same side.
//!
//! `../../tools/bsp.py` is the reference: all 692 files a multiple of 24
//! bytes, all 64387 normals unit to within 1e-3, all 692 trees structurally
//! sound.

pub const NODE_SIZE: usize = 24;
pub const LEAF: u32 = 0xFFFF_FFFF;

#[derive(Debug, PartialEq)]
pub enum Error {
    NotWholeNodes(usize),
    Empty,
    Normal { node: usize, length: f32 },
    ChildOutOfRange { node: usize, child: u32 },
    SharedNode(usize),
    Roots(Vec<usize>),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotWholeNodes(n) => write!(f, "{n} bytes is not a multiple of {NODE_SIZE}"),
            Error::Empty => write!(f, "no nodes at all"),
            Error::Normal { node, length } => write!(f, "node {node}: normal length {length:.5}"),
            Error::ChildOutOfRange { node, child } => {
                write!(f, "node {node}: child {child} out of range")
            }
            Error::SharedNode(i) => write!(f, "node {i} is referenced more than once"),
            Error::Roots(r) => write!(f, "expected node 0 alone unreferenced, got {r:?}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub normal: [f32; 3],
    pub dist: f32,
    pub front: u32,
    pub back: u32,
}

pub struct Bsp {
    pub nodes: Vec<Node>,
}

impl Bsp {
    pub fn parse(data: &[u8]) -> Result<Bsp, Error> {
        if data.is_empty() {
            return Err(Error::Empty);
        }
        if data.len() % NODE_SIZE != 0 {
            return Err(Error::NotWholeNodes(data.len()));
        }
        let f = |at: usize| f32::from_bits(u32::from_le_bytes(data[at..at + 4].try_into().unwrap()));
        let u = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let nodes = (0..data.len() / NODE_SIZE)
            .map(|i| {
                let o = i * NODE_SIZE;
                Node {
                    normal: [f(o), f(o + 4), f(o + 8)],
                    dist: f(o + 12),
                    front: u(o + 16),
                    back: u(o + 20),
                }
            })
            .collect();
        Ok(Bsp { nodes })
    }

    /// Raise unless the array really is a well-formed tree of unit planes:
    /// every child in range, every node referenced once, exactly one node
    /// unreferenced and that one the root. It is what identified the format,
    /// and it is cheap enough to keep doing.
    pub fn validate(&self) -> Result<(), Error> {
        let n = self.nodes.len();
        let mut refs = vec![0u32; n];
        for (i, node) in self.nodes.iter().enumerate() {
            let length = node.normal.iter().map(|c| c * c).sum::<f32>().sqrt();
            if (length - 1.0).abs() > 1e-3 {
                return Err(Error::Normal { node: i, length });
            }
            for child in [node.front, node.back] {
                if child == LEAF {
                    continue;
                }
                if child as usize >= n {
                    return Err(Error::ChildOutOfRange { node: i, child });
                }
                refs[child as usize] += 1;
            }
        }
        if let Some(i) = refs.iter().position(|&r| r > 1) {
            return Err(Error::SharedNode(i));
        }
        let roots: Vec<usize> = refs
            .iter()
            .enumerate()
            .filter(|(_, &r)| r == 0)
            .map(|(i, _)| i)
            .collect();
        if roots != [0] {
            return Err(Error::Roots(roots));
        }
        Ok(())
    }

    /// Is the point inside solid geometry? The point is in **model**
    /// coordinates and is negated on the way in — see the module note.
    pub fn contains(&self, point: [f64; 3]) -> bool {
        let (x, y, z) = (-point[0], -point[1], -point[2]);
        let mut i = 0usize;
        loop {
            let node = self.nodes[i];
            let side = node.normal[0] as f64 * x
                + node.normal[1] as f64 * y
                + node.normal[2] as f64 * z
                - node.dist as f64;
            let child = if side >= 0.0 { node.front } else { node.back };
            if child == LEAF {
                return side >= 0.0;
            }
            i = child as usize;
        }
    }

    /// Does the segment `a`→`b` pass through solid geometry?
    ///
    /// The same descent as [`Bsp::contains`], split at the plane crossings
    /// instead of following one side: where the ends fall on opposite sides
    /// of a node's plane the segment is cut there and both halves are tested.
    /// That makes it **exact for the tree** rather than a sampling of it — a
    /// wall thinner than any step size still stops it, which is the whole
    /// reason a line of sight cannot be done by marching points.
    ///
    /// The leaf convention is `contains`'s: a leaf reached on the front side
    /// is solid, on the back side it is empty. Coordinates are negated the
    /// same way and for the same reason.
    ///
    /// The original's own occlusion test (0x471dc0, reached from
    /// `mdkAILineOfSight` at 0x402950 after the field-of-view check) is a
    /// query against its world structure rather than this traversal. The
    /// trees are the same trees, so the question is the same; identical
    /// answers in every corner are **not** claimed.
    pub fn crosses(&self, a: [f64; 3], b: [f64; 3]) -> bool {
        let plane = |n: &Node, p: [f64; 3]| {
            n.normal[0] as f64 * -p[0]
                + n.normal[1] as f64 * -p[1]
                + n.normal[2] as f64 * -p[2]
                - n.dist as f64
        };
        let mut stack = vec![(0usize, a, b)];
        while let Some((i, p, q)) = stack.pop() {
            let node = self.nodes[i];
            let (dp, dq) = (plane(&node, p), plane(&node, q));
            if dp >= 0.0 && dq >= 0.0 {
                if node.front == LEAF {
                    return true;
                }
                stack.push((node.front as usize, p, q));
            } else if dp < 0.0 && dq < 0.0 {
                if node.back != LEAF {
                    stack.push((node.back as usize, p, q));
                }
            } else {
                let t = dp / (dp - dq);
                let m = [0, 1, 2].map(|c| p[c] + (q[c] - p[c]) * t);
                // whichever half lies on the front side is the one that can
                // be solid, and it is the first half when p is in front
                let (fp, fq) = if dp >= 0.0 { (p, m) } else { (m, q) };
                let (bp, bq) = if dp >= 0.0 { (m, q) } else { (p, m) };
                if node.front == LEAF {
                    return true;
                }
                stack.push((node.front as usize, fp, fq));
                if node.back != LEAF {
                    stack.push((node.back as usize, bp, bq));
                }
            }
        }
        false
    }

    /// Deepest path from the root, iteratively — these trees get deep enough
    /// that recursion is a real risk.
    pub fn depth(&self) -> usize {
        let mut best = 0;
        let mut stack = vec![(0usize, 1usize)];
        while let Some((i, d)) = stack.pop() {
            best = best.max(d);
            for child in [self.nodes[i].front, self.nodes[i].back] {
                if child != LEAF {
                    stack.push((child as usize, d + 1));
                }
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single plane at z = 0 (stored mirrored, as the format is): solid
    /// below, open above, and the negation is what makes that come out right.
    fn one_plane() -> Vec<u8> {
        let mut d = Vec::new();
        for v in [0.0f32, 0.0, 1.0, 0.0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&LEAF.to_le_bytes());
        d.extend_from_slice(&LEAF.to_le_bytes());
        d
    }

    #[test]
    fn a_leaf_through_front_is_inside() {
        let bsp = Bsp::parse(&one_plane()).unwrap();
        bsp.validate().unwrap();
        assert_eq!(bsp.nodes.len(), 1);
        assert_eq!(bsp.depth(), 1);
        // the point is negated, so it is +z that lands behind the plane
        assert!(bsp.contains([0.0, 0.0, -1.0]));
        assert!(!bsp.contains([0.0, 0.0, 1.0]));
    }

    /// The point about a segment query is that it does not sample: a plane
    /// has no thickness at all, and no number of probe points along the line
    /// would ever land on it, yet a line straight through must be stopped.
    #[test]
    fn a_segment_crosses_a_plane_that_no_sample_would_land_on() {
        let bsp = Bsp::parse(&one_plane()).unwrap();
        // -z is the solid side once the negation is applied, so this line
        // starts in the open and ends in solid
        assert!(bsp.crosses([0.0, 0.0, 1.0], [0.0, 0.0, -1.0]));
        assert!(bsp.crosses([0.0, 0.0, -1.0], [0.0, 0.0, 1.0]), "and the other way");
        // wholly on the open side, however far it runs
        assert!(!bsp.crosses([-500.0, 0.0, 1.0], [500.0, 0.0, 1.0]));
        // wholly inside is still solid
        assert!(bsp.crosses([-500.0, 0.0, -1.0], [500.0, 0.0, -1.0]));
        // and a segment that only touches the plane counts, because the leaf
        // convention is `side >= 0` and `contains` says the same of a point
        assert!(bsp.crosses([0.0, 0.0, 1.0], [0.0, 0.0, 0.0]));
        assert_eq!(bsp.contains([0.0, 0.0, 0.0]), true, "the boundary is solid for both");
    }

    #[test]
    fn a_short_file_is_not_a_tree() {
        assert!(matches!(Bsp::parse(&[0u8; 23]), Err(Error::NotWholeNodes(23))));
        assert!(matches!(Bsp::parse(&[]), Err(Error::Empty)));
    }

    #[test]
    fn validation_catches_a_normal_that_is_not_unit() {
        let mut d = one_plane();
        d[8..12].copy_from_slice(&2.0f32.to_le_bytes());
        assert!(matches!(
            Bsp::parse(&d).unwrap().validate(),
            Err(Error::Normal { node: 0, .. })
        ));
    }

    /// Two nodes both claiming the same child is not a tree, and the check
    /// that catches it is the one that identified the format.
    #[test]
    fn validation_catches_a_shared_child() {
        let mut d = one_plane();
        d.extend_from_slice(&one_plane());
        d.extend_from_slice(&one_plane());
        d[16..20].copy_from_slice(&2u32.to_le_bytes());
        d[NODE_SIZE + 16..NODE_SIZE + 20].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            Bsp::parse(&d).unwrap().validate(),
            Err(Error::SharedNode(2))
        ));
    }
}
