use crate::{Aabb, BlockRegistry};

/// Minimal world collision contract, implemented by the world storage crate.
pub trait CollisionWorld {
    fn is_solid_at(&self, x: i32, y: i32, z: i32, registry: &BlockRegistry) -> bool;
}

const PLAYER_RADIUS: f32 = 0.3;
const PLAYER_HEIGHT: f32 = 1.8;
const WALK_SPEED: f32 = 4.3;
const JUMP_SPEED: f32 = 8.0;
const GRAVITY: f32 = 24.0;
const MAX_STEP_SECONDS: f32 = 0.05;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerInput {
    /// Forward/backward input in [-1, 1].
    pub forward: f32,
    /// Left/right input in [-1, 1].
    pub strafe: f32,
    pub jump: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Player {
    /// Feet position in world coordinates.
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    /// Radians. Zero faces positive Z.
    pub yaw: f32,
    /// Radians, clamped away from the vertical singularity.
    pub pitch: f32,
    pub on_ground: bool,
}

impl Default for Player {
    fn default() -> Self {
        Self::new([0.5, 2.0, 0.5])
    }
}

impl Player {
    pub const fn new(position: [f32; 3]) -> Self {
        Self {
            position,
            velocity: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
        }
    }

    pub fn set_look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        if yaw_delta.is_finite() {
            self.yaw = (self.yaw + yaw_delta).rem_euclid(std::f32::consts::TAU);
        }
        if pitch_delta.is_finite() {
            self.pitch = (self.pitch + pitch_delta)
                .clamp(-1.553_343, 1.553_343);
        }
    }

    pub fn eye_position(self) -> [f32; 3] {
        [
            self.position[0],
            self.position[1] + 1.62,
            self.position[2],
        ]
    }

    pub fn forward(self) -> [f32; 3] {
        let cos_pitch = self.pitch.cos();
        [
            self.yaw.sin() * cos_pitch,
            -self.pitch.sin(),
            self.yaw.cos() * cos_pitch,
        ]
    }

    /// Advances movement with a fixed upper timestep and axis-separated
    /// voxel collision resolution. No renderer or browser APIs are involved.
    pub fn tick(
        &mut self,
        world: &impl CollisionWorld,
        registry: &BlockRegistry,
        input: PlayerInput,
        delta_seconds: f32,
    ) {
        if !delta_seconds.is_finite() || delta_seconds <= 0.0 {
            return;
        }
        let dt = delta_seconds.min(MAX_STEP_SECONDS);

        let forward = [self.yaw.sin(), self.yaw.cos()];
        let right = [self.yaw.cos(), -self.yaw.sin()];
        let mut move_x = forward[0] * input.forward + right[0] * input.strafe;
        let mut move_z = forward[1] * input.forward + right[1] * input.strafe;
        let magnitude = (move_x * move_x + move_z * move_z).sqrt();
        if magnitude > 1.0 {
            move_x /= magnitude;
            move_z /= magnitude;
        }
        self.velocity[0] = move_x * WALK_SPEED;
        self.velocity[2] = move_z * WALK_SPEED;

        if input.jump && self.on_ground {
            self.velocity[1] = JUMP_SPEED;
            self.on_ground = false;
        }
        self.velocity[1] -= GRAVITY * dt;

        self.move_axis(world, registry, 1, self.velocity[1] * dt);
        self.move_axis(world, registry, 0, self.velocity[0] * dt);
        self.move_axis(world, registry, 2, self.velocity[2] * dt);
    }

    fn move_axis(
        &mut self,
        world: &impl CollisionWorld,
        registry: &BlockRegistry,
        axis: usize,
        movement: f32,
    ) {
        if movement == 0.0 {
            return;
        }
        self.position[axis] += movement;
        if self.collides(world, registry) {
            self.position[axis] -= movement;
            self.velocity[axis] = 0.0;
            if axis == 1 && movement < 0.0 {
                self.on_ground = true;
            }
        }
    }

    fn bounds(self) -> Aabb {
        Aabb {
            min: [
                self.position[0] - PLAYER_RADIUS,
                self.position[1],
                self.position[2] - PLAYER_RADIUS,
            ],
            max: [
                self.position[0] + PLAYER_RADIUS,
                self.position[1] + PLAYER_HEIGHT,
                self.position[2] + PLAYER_RADIUS,
            ],
        }
    }

    fn collides(&self, world: &impl CollisionWorld, registry: &BlockRegistry) -> bool {
        let bounds = self.bounds();
        let min = bounds.min.map(|value| value.floor() as i32);
        let max = bounds.max.map(|value| (value - 0.0001).floor() as i32);

        for x in min[0]..=max[0] {
            for y in min[1]..=max[1] {
                for z in min[2]..=max[2] {
                    if world.is_solid_at(x, y, z, registry) {
                        let block_bounds = Aabb {
                            min: [x as f32, y as f32, z as f32],
                            max: [x as f32 + 1.0, y as f32 + 1.0, z as f32 + 1.0],
                        };
                        if bounds.intersects(block_bounds) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use crate::{BlockRegistry, CollisionWorld, Player, PlayerInput};

    struct FlatFloor;

    impl CollisionWorld for FlatFloor {
        fn is_solid_at(&self, x: i32, y: i32, z: i32, _registry: &BlockRegistry) -> bool {
            y == 0 && (-4..=4).contains(&x) && (-4..=4).contains(&z)
        }
    }

    fn test_world() -> (FlatFloor, BlockRegistry) {
        let mut registry = BlockRegistry::new();
        registry.register("minecraft:stone", true, true).unwrap();
        (FlatFloor, registry)
    }

    #[test]
    fn player_falls_onto_floor_and_stays_grounded() {
        let (world, registry) = test_world();
        let mut player = Player::new([0.5, 3.0, 0.5]);

        for _ in 0..120 {
            player.tick(&world, &registry, PlayerInput::default(), 1.0 / 60.0);
        }

        assert!(player.on_ground);
        assert!((player.position[1] - 1.0).abs() < 0.001);
        assert_eq!(player.velocity[1], 0.0);
    }

    #[test]
    fn player_moves_in_look_direction() {
        let (world, registry) = test_world();
        let mut player = Player::new([0.5, 1.0, 0.5]);
        player.on_ground = true;

        for _ in 0..12 {
            player.tick(
                &world,
                &registry,
                PlayerInput { forward: 1.0, ..PlayerInput::default() },
                1.0 / 60.0,
            );
        }

        assert!(player.position[2] > 1.0);
        assert!((player.position[0] - 0.5).abs() < 0.001);
    }

    #[test]
    fn jump_is_applied_only_while_grounded() {
        let (world, registry) = test_world();
        let mut player = Player::new([0.5, 1.0, 0.5]);
        player.on_ground = true;
        player.tick(
            &world,
            &registry,
            PlayerInput { jump: true, ..PlayerInput::default() },
            1.0 / 60.0,
        );
        assert!(!player.on_ground);
        assert!(player.velocity[1] > 7.0);
    }

    #[test]
    fn look_clamps_pitch_and_ignores_non_finite_deltas() {
        let mut player = Player::default();
        player.set_look(f32::NAN, 100.0);
        assert!((player.pitch - 1.553_343).abs() < 0.0001);
        assert_eq!(player.yaw, 0.0);
    }
}
