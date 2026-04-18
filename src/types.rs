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
