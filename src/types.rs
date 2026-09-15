#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct BlockPos {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct IVec3 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}
