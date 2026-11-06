use core::simd::f32x4;
use micromath::F32Ext;

#[derive(Clone, Copy, Debug)]
#[repr(C, align(16))]
pub struct Matrix4x4 {
    pub cols: [f32x4; 4],
}

impl Matrix4x4 {
    pub fn identity() -> Self {
        Self {
            cols: [
                f32x4::from_array([1.0, 0.0, 0.0, 0.0]),
                f32x4::from_array([0.0, 1.0, 0.0, 0.0]),
                f32x4::from_array([0.0, 0.0, 1.0, 0.0]),
                f32x4::from_array([0.0, 0.0, 0.0, 1.0]),
            ],
        }
    }

    #[inline(always)]
    pub fn mul_vec(&self, v: f32x4) -> f32x4 {
        f32x4::splat(v[0]) * self.cols[0]
            + f32x4::splat(v[1]) * self.cols[1]
            + f32x4::splat(v[2]) * self.cols[2]
            + f32x4::splat(v[3]) * self.cols[3]
    }

    pub fn mul(&self, other: &Matrix4x4) -> Matrix4x4 {
        let mut result = Matrix4x4::identity();
        for j in 0..4 {
            let col = other.cols[j];
            result.cols[j] = f32x4::splat(col[0]) * self.cols[0]
                + f32x4::splat(col[1]) * self.cols[1]
                + f32x4::splat(col[2]) * self.cols[2]
                + f32x4::splat(col[3]) * self.cols[3];
        }
        result
    }

    pub fn look_at(eye: f32x4, target: f32x4, up: f32x4) -> Matrix4x4 {
        let f = (target - eye).normalize();
        let s = f.cross(up).normalize();
        let u = s.cross(f);

        Matrix4x4 {
            cols: [
                f32x4::from_array([s[0], u[0], -f[0], 0.0]),
                f32x4::from_array([s[1], u[1], -f[1], 0.0]),
                f32x4::from_array([s[2], u[2], -f[2], 0.0]),
                f32x4::from_array([-s.dot3(eye), -u.dot3(eye), f.dot3(eye), 1.0]),
            ],
        }
    }

    pub fn translation(x: f32, y: f32, z: f32) -> Self {
        Self {
            cols: [
                f32x4::from_array([1.0, 0.0, 0.0, 0.0]),
                f32x4::from_array([0.0, 1.0, 0.0, 0.0]),
                f32x4::from_array([0.0, 0.0, 1.0, 0.0]),
                f32x4::from_array([x, y, z, 1.0]),
            ],
        }
    }

    pub fn scale_matrix(x: f32, y: f32, z: f32) -> Self {
        Self {
            cols: [
                f32x4::from_array([x, 0.0, 0.0, 0.0]),
                f32x4::from_array([0.0, y, 0.0, 0.0]),
                f32x4::from_array([0.0, 0.0, z, 0.0]),
                f32x4::from_array([0.0, 0.0, 0.0, 1.0]),
            ],
        }
    }

    pub fn rotation_x(angle: f32) -> Self {
        let cos = angle.cos();
        let sin = angle.sin();
        Self {
            cols: [
                f32x4::from_array([1.0, 0.0, 0.0, 0.0]),
                f32x4::from_array([0.0, cos, sin, 0.0]),
                f32x4::from_array([0.0, -sin, cos, 0.0]),
                f32x4::from_array([0.0, 0.0, 0.0, 1.0]),
            ],
        }
    }

    pub fn rotation_y(angle: f32) -> Self {
        let cos = angle.cos();
        let sin = angle.sin();
        Self {
            cols: [
                f32x4::from_array([cos, 0.0, -sin, 0.0]),
                f32x4::from_array([0.0, 1.0, 0.0, 0.0]),
                f32x4::from_array([sin, 0.0, cos, 0.0]),
                f32x4::from_array([0.0, 0.0, 0.0, 1.0]),
            ],
        }
    }

    pub fn rotation_z(angle: f32) -> Self {
        let cos = angle.cos();
        let sin = angle.sin();
        Self {
            cols: [
                f32x4::from_array([cos, sin, 0.0, 0.0]),
                f32x4::from_array([-sin, cos, 0.0, 0.0]),
                f32x4::from_array([0.0, 0.0, 1.0, 0.0]),
                f32x4::from_array([0.0, 0.0, 0.0, 1.0]),
            ],
        }
    }

    pub fn from_trs(translation: f32x4, rotation_euler: f32x4, scale: f32x4) -> Self {
        let rot_x = Self::rotation_x(rotation_euler[0]);
        let rot_y = Self::rotation_y(rotation_euler[1]);
        let rot_z = Self::rotation_z(rotation_euler[2]);
        let rotation = rot_z.mul(&rot_y).mul(&rot_x);
        let scale_mat = Self::scale_matrix(scale[0], scale[1], scale[2]);
        let translation_mat = Self::translation(translation[0], translation[1], translation[2]);
        translation_mat.mul(&rotation).mul(&scale_mat)
    }

    pub fn translate(&mut self, x: f32, y: f32, z: f32) {
        *self = Self::translation(x, y, z).mul(self);
    }

    pub fn scale(&mut self, x: f32, y: f32, z: f32) {
        *self = Self::scale_matrix(x, y, z).mul(self);
    }

    pub fn rotate_x(&mut self, angle: f32) {
        *self = Self::rotation_x(angle).mul(self);
    }

    pub fn rotate_y(&mut self, angle: f32) {
        *self = Self::rotation_y(angle).mul(self);
    }

    pub fn rotate_z(&mut self, angle: f32) {
        *self = Self::rotation_z(angle).mul(self);
    }
}

pub trait F32x4Ext {
    fn normalize(self) -> f32x4;
    fn cross(self, other: f32x4) -> f32x4;
    fn dot3(self, other: f32x4) -> f32;
}

impl F32x4Ext for f32x4 {
    fn normalize(self) -> f32x4 {
        let len = (self[0] * self[0] + self[1] * self[1] + self[2] * self[2]).sqrt();
        if len > f32::EPSILON {
            self / f32x4::splat(len)
        } else {
            self
        }
    }

    fn cross(self, other: f32x4) -> f32x4 {
        f32x4::from_array([
            self[1] * other[2] - self[2] * other[1],
            self[2] * other[0] - self[0] * other[2],
            self[0] * other[1] - self[1] * other[0],
            0.0,
        ])
    }

    fn dot3(self, other: f32x4) -> f32 {
        self[0] * other[0] + self[1] * other[1] + self[2] * other[2]
    }
}

pub fn create_perspective_matrix(fov: f32, aspect: f32, near: f32, far: f32) -> Matrix4x4 {
    let f = 1.0 / (fov * 0.5).tan();
    Matrix4x4 {
        cols: [
            f32x4::from_array([f / aspect, 0.0, 0.0, 0.0]),
            f32x4::from_array([0.0, f, 0.0, 0.0]),
            f32x4::from_array([0.0, 0.0, (far + near) / (near - far), -1.0]),
            f32x4::from_array([0.0, 0.0, (2.0 * far * near) / (near - far), 0.0]),
        ],
    }
}
