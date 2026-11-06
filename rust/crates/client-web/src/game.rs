use std::collections::BTreeMap;

use eagler_game_core::{BlockId, BlockRegistry, FixedTickClock, Player, PlayerInput, TickReport};
use eagler_network::{DecodedChunk, WorldPacket};
use eagler_world::{BlockPos, ChunkPos, World};

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
    network_state_map: BTreeMap<u32, BlockId>,
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

        let mut network_state_map = BTreeMap::new();
        network_state_map.insert(0, BlockId::AIR);
        network_state_map.insert(1, stone);

        let mut player = Player::new([0.5, 1.0, 0.5]);
        player.set_look(0.0, 0.35);
        player.on_ground = true;
        Ok(Self {
            world,
            registry,
            player,
            clock: FixedTickClock::new(),
            last_frame_ms: None,
            last_report: TickReport::default(),
            network_state_map,
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

    /// Applies an authoritative chunk packet to the sparse world. Unknown
    /// network block-state IDs receive stable local placeholder definitions;
    /// resource-registry parity later replaces their generic material metadata.
    pub fn ingest_world_packet(&mut self, packet: WorldPacket) -> Result<bool, String> {
        match packet {
            WorldPacket::ForgetChunk { x, z } => {
                Ok(self.world.remove_chunk(ChunkPos::new(x, z)))
            }
            WorldPacket::Chunk(chunk) => {
                self.ingest_chunk(chunk)?;
                Ok(true)
            }
            WorldPacket::PlayerPosition {
                position,
                delta_movement,
                yaw,
                pitch,
                relative_flags,
                ..
            } => {
                let current = self.player.position;
                let target = [
                    if relative_flags & 1 != 0 { current[0] as f64 + position[0] } else { position[0] },
                    if relative_flags & 2 != 0 { current[1] as f64 + position[1] } else { position[1] },
                    if relative_flags & 4 != 0 { current[2] as f64 + position[2] } else { position[2] },
                ];
                if target.iter().any(|value| !value.is_finite() || value.abs() > 30_000_000.0) {
                    return Err("Server sent an invalid player position".to_owned());
                }
                self.player.position = target.map(|value| value as f32);
                self.player.velocity = [
                    if relative_flags & 32 != 0 { self.player.velocity[0] + delta_movement[0] as f32 } else { delta_movement[0] as f32 },
                    if relative_flags & 64 != 0 { self.player.velocity[1] + delta_movement[1] as f32 } else { delta_movement[1] as f32 },
                    if relative_flags & 128 != 0 { self.player.velocity[2] + delta_movement[2] as f32 } else { delta_movement[2] as f32 },
                ];
                let current_yaw = self.player.yaw.to_degrees();
                let current_pitch = self.player.pitch.to_degrees();
                let yaw_degrees = if relative_flags & 8 != 0 { current_yaw + yaw } else { yaw };
                let pitch_degrees = if relative_flags & 16 != 0 { current_pitch + pitch } else { pitch };
                self.player.yaw = yaw_degrees.to_radians().rem_euclid(std::f32::consts::TAU);
                self.player.pitch = pitch_degrees.to_radians().clamp(-1.553_343, 1.553_343);
                self.player.on_ground = false;
                Ok(false)
            }
        }
    }

    fn ingest_chunk(&mut self, chunk: DecodedChunk) -> Result<(), String> {
        let chunk_pos = ChunkPos::new(chunk.x, chunk.z);
        for section in chunk.sections {
            let mut local_ids = Vec::with_capacity(section.block_states.len());
            for network_id in section.block_states {
                let local_id = if let Some(id) = self.network_state_map.get(&network_id) {
                    *id
                } else {
                    let key = format!("minecraft:network_state/{network_id}");
                    let id = self.registry
                        .register(&key, true, true)
                        .map_err(|error| format!("Could not register network block state {network_id}: {error:?}"))?;
                    self.network_state_map.insert(network_id, id);
                    id
                };
                local_ids.push(local_id);
            }
            self.world
                .replace_section(chunk_pos, section.section_y, local_ids)
                .map_err(|error| format!("Could not ingest chunk {},{} section {}: {error:?}", chunk.x, chunk.z, section.section_y))?;
        }
        Ok(())
    }

    pub fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.player.set_look(yaw_delta, pitch_delta);
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn registry(&self) -> &BlockRegistry {
        &self.registry
    }

    pub fn block_id(&self, key: &str) -> Option<BlockId> {
        self.registry.id(key)
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

    pub fn player_network_snapshot(&self) -> (u64, [f32; 3], f32, f32, bool) {
        (
            self.clock.tick_index(),
            self.player.position,
            self.player.yaw.to_degrees(),
            self.player.pitch.to_degrees(),
            self.player.on_ground,
        )
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
        for frame in 0..=60 {
            game.frame(frame as f64 * 1000.0 / 60.0, Controls::default());
        }
        assert_eq!(game.tick_index(), 20);
    }

    #[test]
    fn movement_input_is_applied_on_fixed_ticks() {
        let mut game = GameSimulation::new().unwrap();
        for frame in 0..=60 {
            game.frame(
                frame as f64 * 1000.0 / 60.0,
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
