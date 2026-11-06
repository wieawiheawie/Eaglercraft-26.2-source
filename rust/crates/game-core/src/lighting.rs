/// Propagates one 16-voxel light lane from a neighbor into a destination lane.
///
/// Each candidate loses `attenuation` levels with saturating subtraction,
/// then competes with the current destination value. wasm32 processes all
/// sixteen light values with one SIMD vector subtract and max.
pub fn propagate_light_16(
    destination: &mut [u8; 16],
    neighbor: &[u8; 16],
    attenuation: u8,
) {
    #[cfg(target_arch = "wasm32")]
    {
        use std::arch::wasm32::{
            u8x16_max, u8x16_splat, u8x16_sub_sat, v128_load, v128_store,
        };

        let current = unsafe { v128_load(destination.as_ptr().cast()) };
        let incoming = unsafe { v128_load(neighbor.as_ptr().cast()) };
        let loss = u8x16_splat(attenuation);
        let candidate = u8x16_sub_sat(incoming, loss);
        let result = u8x16_max(current, candidate);

        // SAFETY: destination is a valid writable sixteen-byte array.
        unsafe { v128_store(destination.as_mut_ptr().cast(), result) };
    }

    #[cfg(not(target_arch = "wasm32"))]
    for index in 0..16 {
        destination[index] = destination[index].max(neighbor[index].saturating_sub(attenuation));
    }
}

#[cfg(test)]
mod tests {
    use super::propagate_light_16;

    #[test]
    fn saturating_propagation_matches_expected_levels() {
        let mut destination = [0; 16];
        let neighbor = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        propagate_light_16(&mut destination, &neighbor, 1);
        assert_eq!(destination, [0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]);
    }

    #[test]
    fn preserves_stronger_existing_light_and_supports_full_attenuation() {
        let mut destination = [15; 16];
        let neighbor = [15; 16];
        propagate_light_16(&mut destination, &neighbor, 15);
        assert_eq!(destination, [15; 16]);

        let mut dark = [0; 16];
        propagate_light_16(&mut dark, &neighbor, 15);
        assert_eq!(dark, [0; 16]);
    }
}
