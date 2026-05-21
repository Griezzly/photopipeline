pub mod detector;
pub mod embedder;
pub mod iqa;

/// Axis-aligned bounding box, normalised to [0.0, 1.0] in both axes.
#[derive(Debug, Clone, Copy)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectClass {
    Person,
    Animal,
    Vehicle,
    Object,
    Other,
}

#[derive(Debug, Clone)]
pub struct DetectedSubject {
    pub bbox: BBox,
    pub class: SubjectClass,
    pub confidence: f32,
}
