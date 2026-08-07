//! Just enough matrix arithmetic to look at the world, written out rather
//! than depended on.
//!
//! Four functions and one type. A dependency would be more code to read, not
//! less, and this is the kind of arithmetic whose failures are loud: get it
//! wrong and nothing appears at all.
//!
//! **Z is up.** That is the game's convention, not a choice made here:
//! accumulating `.mod` node translations down the parent chain puts Kurt in a
//! T-pose with his head at z = +0.215 and his foot at −0.525.

/// Column-major 4x4, the order OpenGL wants — `m[col * 4 + row]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]);

    /// `self * rhs`, applying `rhs` first.
    pub fn times(&self, rhs: &Mat4) -> Mat4 {
        let mut out = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                out[col * 4 + row] = (0..4)
                    .map(|k| self.0[k * 4 + row] * rhs.0[col * 4 + k])
                    .sum();
            }
        }
        Mat4(out)
    }

    pub fn translation(t: [f32; 3]) -> Mat4 {
        let mut m = Mat4::IDENTITY;
        m.0[12] = t[0];
        m.0[13] = t[1];
        m.0[14] = t[2];
        m
    }

    /// A rotation from a quaternion in **(w, x, y, z)** order, which is how
    /// the models and the scene graphs both store one.
    pub fn rotation(q: [f32; 4]) -> Mat4 {
        let [w, x, y, z] = q;
        Mat4([
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + z * w),
            2.0 * (x * z - y * w),
            0.0,
            2.0 * (x * y - z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + x * w),
            0.0,
            2.0 * (x * z + y * w),
            2.0 * (y * z - x * w),
            1.0 - 2.0 * (x * x + y * y),
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }

    /// Right-handed perspective, `fov` in radians, mapping depth to
    /// [-1, 1] — the GL convention rather than the Vulkan one.
    pub fn perspective(fov: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fov / 2.0).tan();
        let mut m = Mat4([0.0; 16]);
        m.0[0] = f / aspect;
        m.0[5] = f;
        m.0[10] = (far + near) / (near - far);
        m.0[11] = -1.0;
        m.0[14] = 2.0 * far * near / (near - far);
        m
    }

    /// Look from `eye` at `at`, with `up` deciding the roll.
    pub fn look_at(eye: [f32; 3], at: [f32; 3], up: [f32; 3]) -> Mat4 {
        let f = normalise(sub(at, eye));
        let s = normalise(cross(f, up));
        let u = cross(s, f);
        Mat4([
            s[0],
            u[0],
            -f[0],
            0.0,
            s[1],
            u[1],
            -f[1],
            0.0,
            s[2],
            u[2],
            -f[2],
            0.0,
            -dot(s, eye),
            -dot(u, eye),
            dot(f, eye),
            1.0,
        ])
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalise(v: [f32; 3]) -> [f32; 3] {
    let len = dot(v, v).sqrt();
    if len == 0.0 {
        v
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

/// Apply a matrix to a point, dividing through by w.
pub fn project(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 4];
    for row in 0..4 {
        out[row] = (0..3).map(|k| m.0[k * 4 + row] * p[k]).sum::<f32>() + m.0[12 + row];
    }
    if out[3] == 0.0 {
        [out[0], out[1], out[2]]
    } else {
        [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|c| (a[c] - b[c]).abs() < 1e-5)
    }

    #[test]
    fn multiplying_by_the_identity_changes_nothing() {
        let m = Mat4::translation([1.0, 2.0, 3.0]);
        assert_eq!(m.times(&Mat4::IDENTITY), m);
        assert_eq!(Mat4::IDENTITY.times(&m), m);
    }

    /// Order matters and is the usual one: `a.times(&b)` applies `b` first.
    #[test]
    fn composition_applies_the_right_hand_side_first() {
        let move_x = Mat4::translation([1.0, 0.0, 0.0]);
        let quarter_turn_about_z = Mat4::rotation([
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
        ]);
        // rotate, then move: the point ends up moved along x after turning
        let m = move_x.times(&quarter_turn_about_z);
        assert!(near(project(&m, [1.0, 0.0, 0.0]), [1.0, 1.0, 0.0]));
        // move, then rotate: the move turns with it
        let m = quarter_turn_about_z.times(&move_x);
        assert!(near(project(&m, [0.0, 0.0, 0.0]), [0.0, 1.0, 0.0]));
    }

    /// A camera at +x looking back at the origin, with **z up**: the origin
    /// lands in the middle of the screen, and a point higher in the world
    /// lands higher on the screen.
    #[test]
    fn the_camera_looks_where_it_is_told_with_z_up() {
        let view = Mat4::look_at([10.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let proj = Mat4::perspective(1.0, 1.0, 0.1, 100.0);
        let mvp = proj.times(&view);

        let centre = project(&mvp, [0.0, 0.0, 0.0]);
        assert!(centre[0].abs() < 1e-5 && centre[1].abs() < 1e-5, "{centre:?}");

        let above = project(&mvp, [0.0, 0.0, 1.0]);
        assert!(above[1] > 0.1, "up in the world must be up on the screen: {above:?}");

        // and something behind the camera must not land in front of it
        let behind = project(&mvp, [20.0, 0.0, 0.0]);
        assert!(behind[2] > 1.0 || behind[2] < -1.0, "{behind:?}");
    }

    /// Nearer things must come out with a smaller depth than farther ones, or
    /// the depth test is inverted and the world renders inside out.
    #[test]
    fn depth_grows_with_distance() {
        let proj = Mat4::perspective(1.0, 1.0, 1.0, 100.0);
        let view = Mat4::look_at([0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let mvp = proj.times(&view);
        let near = project(&mvp, [2.0, 0.0, 0.0])[2];
        let far = project(&mvp, [50.0, 0.0, 0.0])[2];
        assert!(near < far, "{near} should be nearer than {far}");
    }
}
