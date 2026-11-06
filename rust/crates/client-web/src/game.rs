use eagler_game_core::{BlockId, BlockRegistry, FixedTickClock, Player, PlayerInput, TickReport};
use eagler_world::{BlockPos, World};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Controls {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
}

impl Controls {
    pub fn player_input(self) -> PlayerInput {
        let forward = if self.forward { 1.0 } else { 0.0 };
        let backward = if self.backward { 1.0 } else { 0.0 };
        let left = if self.left { 1.0 } else { 0.0 };
        let right = if self.right { 1.0 } else { 0.0 };
        PlayerInput {
            forward: forward - backward,
            strafe: left - right,
            jump: self.jump,
        }
    }
}

/// Stateful 20 TPS world simulation owned by the Rust browser client.
pub struct GameSimulation {
    world: World,
    registry: BlockRegistry,
    player: Player,
    clock: FixedTickClock,
    last_frame_ms: Option<f64>,
    last_report: TickReport,
}

impl GameSimulation {
    pub fn new() -> Result<Self, String> {
        let mut registry = BlockRegistry::new();
        let stone = registry
            .register("minecraft:stone", true, true)
            .map_err(|error| format!("Could not register base blocks: {error:?}"))?;
        let mut world = World::new(-64, 384)
            .map_err(|error| format!("Could not create the initial dimension: {error:?}"))?;

        // Small local test area for physics and the first renderer pass.
        // The eventual terrain generator replaces this starter patch.
        for x in -16..=16 {
            for z in -16..=16 {
                world
                    .set_block(BlockPos::new(x, 0, z), stone)
                    .map_err(|error| format!("Could not generate starter floor: {error:?}"))?;
            }
        }

        let mut player = Player::new([0.5, 1.0, 0.5]);
        player.on_ground = true;
        Ok(Self {
            world,
            registry,
            player,
            clock: FixedTickClock::new(),
            last_frame_ms: None,
            last_report: TickReport::default(),
        })
    }

    /// Called once per browser animation frame; simulation itself advances only
    /// in 50 ms steps. Input is sampled once and applied to every catch-up tick.
    pub fn frame(&mut self, timestamp_ms: f64, controls: Controls) -> TickReport {
        if !timestamp_ms.is_finite() {
            return self.last_report;
        }
        let elapsed = self
            .last_frame_ms
            .replace(timestamp_ms)
            .map(|previous| ((timestamp_ms - previous).max(0.0)) / 1000.0)
            .unwrap_or(0.0);
        let report = self.clock.advance(elapsed, |_| {});
        let input = controls.player_input();
        for _ in 0..report.ticks_run {
            self.player.tick(
                &self.world,
                &self.registry,
                input,
                FixedTickClock::tick_seconds() as f32,
            );
        }
        self.last_report = report;
        report
    }

    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.player.set_look(yaw_delta, pitch_delta);
    }

    pub fn tick_index(&self) -> u64 {
        self.clock.tick_index()
    }

    pub fn player_position(&self) -> [f32; 3] {
        self.player.position
    }

    pub fn player_eye_position(&self) -> [f32; 3] {
        self.player.eye_position()
    }

    pub fn player_forward(&self) -> [f32; 3] {
        self.player.forward()
    }

    pub fn world_block_count_hint(&self) -> usize {
        self.world.chunk_count()
    }

    pub fn has_starter_floor(&self) -> bool {
        self.world.get_block(BlockPos::new(0, 0, 0)) == Ok(BlockId(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{Controls, GameSimulation};

    #[test]
    fn simulation_runs_exactly_twenty_ticks_over_one_second_at_sixty_fps() {
        let mut game = GameSimulation::new().unwrap();
        for frame in 0..60 {
            game.frame((frame as f64 + 1.0) * 1000.0 / 60.0, Controls::default());
        }
        assert_eq!(game.tick_index(), 20);
    }

    #[test]
    fn movement_input_is_applied_on_fixed_ticks() {
        let mut game = GameSimulation::new().unwrap();
        for frame in 0..60 {
            game.frame(
                (frame as f64 + 1.0) * 1000.0 / 60.0,
                Controls { forward: true, ..Controls::default() },
            );
        }
        assert!(game.player_position()[2] > 2.0);
    }

    #[test]
    fn initial_world_contains_a_starter_floor() {
        let game = GameSimulation::new().unwrap();
        assert!(game.has_starter_floor());
        assert!(game.world_block_count_hint() > 0);
    }
}
