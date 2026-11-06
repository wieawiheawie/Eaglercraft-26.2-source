/// Axis-aligned bounding box used by chunk culling and collision queries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plane {
    /// Points toward the visible half-space.
    pub normal: [f32; 3],
    pub distance: f32,
}

impl Aabb {
    pub fn new(min: [f32; 3], max: [f32; 3]) -> Option<Self> {
        if min.iter().chain(max.iter()).any(|value| !value.is_finite())
            || (0..3).any(|axis| min[axis] > max[axis])
        {
            return None;
        }
        Some(Self { min, max })
    }

    /// Inclusive AABB overlap check. wasm32 uses four packed SIMD comparisons
    /// to test all three axes plus a neutral fourth lane in parallel.
    pub fn intersects(self, other: Self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            return self.intersects_simd(other);
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            (0..3).all(|axis| self.min[axis] <= other.max[axis] && self.max[axis] >= other.min[axis])
        }
    }

    /// Rejects an AABB when it lies entirely outside any frustum plane.
    ///
    /// The positive support vertex is tested for each plane. wasm32 computes
    /// the XYZ dot product with one f32x4 SIMD multiply before reducing it.
    pub fn visible_in_frustum(self, planes: &[Plane]) -> bool {
        planes.iter().all(|plane| {
            let positive = [
                if plane.normal[0] >= 0.0 { self.max[0] } else { self.min[0] },
                if plane.normal[1] >= 0.0 { self.max[1] } else { self.min[1] },
                if plane.normal[2] >= 0.0 { self.max[2] } else { self.min[2] },
            ];
            plane_distance(positive, *plane) >= 0.0
        })
    }
}

#[cfg(target_arch = "wasm32")]
impl Aabb {
    fn intersects_simd(self, other: Self) -> bool {
        use std::arch::wasm32::{
            f32x4_ge, f32x4_le, v128_and, v128_any_true, v128_load, v128_not,
        };

        let self_min = [self.min[0], self.min[1], self.min[2], 0.0];
        let self_max = [self.max[0], self.max[1], self.max[2], 0.0];
        let other_min = [other.min[0], other.min[1], other.min[2], 0.0];
        let other_max = [other.max[0], other.max[1], other.max[2], 0.0];

        // SAFETY: each pointer refers to a live four-f32 local array; v128_load
        // explicitly permits unaligned reads.
        let self_min = unsafe { v128_load(self_min.as_ptr().cast()) };
        let self_max = unsafe { v128_load(self_max.as_ptr().cast()) };
        let other_min = unsafe { v128_load(other_min.as_ptr().cast()) };
        let other_max = unsafe { v128_load(other_max.as_ptr().cast()) };

        let overlaps_min = f32x4_le(self_min, other_max);
        let overlaps_max = f32x4_ge(self_max, other_min);
        let overlaps = v128_and(overlaps_min, overlaps_max);
        !v128_any_true(v128_not(overlaps))
    }
}

#[cfg(target_arch = "wasm32")]
fn plane_distance(point: [f32; 3], plane: Plane) -> f32 {
    use std::arch::wasm32::{f32x4_add, f32x4_extract_lane, f32x4_mul, v128_load};

    let point_and_distance = [point[0], point[1], point[2], plane.distance];
    let normal_and_one = [
        plane.normal[0],
        plane.normal[1],
        plane.normal[2],
        1.0,
    ];
    // SAFETY: these pointers refer to live four-f32 local arrays.
    let point_vector = unsafe { v128_load(point_and_distance.as_ptr().cast()) };
    let normal_vector = unsafe { v128_load(normal_and_one.as_ptr().cast()) };
    let products = f32x4_mul(point_vector, normal_vector);
    let pairwise = f32x4_add(products, products);
    f32x4_extract_lane::<0>(pairwise)
        + f32x4_extract_lane::<1>(pairwise)
        + f32x4_extract_lane::<2>(pairwise)
        + f32x4_extract_lane::<3>(pairwise)
}

#[cfg(not(target_arch = "wasm32"))]
fn plane_distance(point: [f32; 3], plane: Plane) -> f32 {
    point[0] * plane.normal[0]
        + point[1] * plane.normal[1]
        + point[2] * plane.normal[2]
        + plane.distance
}

#[cfg(test)]
mod tests {
    use super::{Aabb, Plane};

    fn box_at(min: [f32; 3], max: [f32; 3]) -> Aabb {
        Aabb::new(min, max).unwrap()
    }

    #[test]
    fn rejects_inverted_or_non_finite_bounds() {
        assert!(Aabb::new([1.0, 0.0, 0.0], [0.0, 1.0, 1.0]).is_none());
        assert!(Aabb::new([f32::NAN, 0.0, 0.0], [1.0; 3]).is_none());
    }

    #[test]
    fn touching_aabb_faces_count_as_intersection() {
        let left = box_at([0.0; 3], [1.0; 3]);
        let right = box_at([1.0, 0.2, 0.2], [2.0, 0.8, 0.8]);
        assert!(left.intersects(right));
    }

    #[test]
    fn separated_aabbs_do_not_intersect() {
        let left = box_at([0.0; 3], [1.0; 3]);
        let right = box_at([1.01, 0.0, 0.0], [2.0; 3]);
        assert!(!left.intersects(right));
    }

    #[test]
    fn frustum_keeps_aabb_inside_and_rejects_outside() {
        let planes = [
            Plane { normal: [1.0, 0.0, 0.0], distance: 10.0 },
            Plane { normal: [-1.0, 0.0, 0.0], distance: 10.0 },
            Plane { normal: [0.0, 1.0, 0.0], distance: 10.0 },
            Plane { normal: [0.0, -1.0, 0.0], distance: 10.0 },
            Plane { normal: [0.0, 0.0, 1.0], distance: 10.0 },
            Plane { normal: [0.0, 0.0, -1.0], distance: 10.0 },
        ];
        assert!(box_at([-1.0; 3], [1.0; 3]).visible_in_frustum(&planes));
        assert!(!box_at([11.0, 0.0, 0.0], [12.0, 1.0, 1.0]).visible_in_frustum(&planes));
    }
}
