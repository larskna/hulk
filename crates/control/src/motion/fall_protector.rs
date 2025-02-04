use std::time::{Duration, SystemTime, UNIX_EPOCH};

use approx::relative_eq;
use color_eyre::Result;
use context_attribute::context;
use filtering::low_pass_filter::LowPassFilter;
use framework::MainOutput;
use hardware::PathsInterface;
use motionfile::{MotionFile, MotionInterpolator};
use nalgebra::Vector2;
use types::{
    parameters::{FallProtection, FallStateEstimation},
    BodyJoints, ConditionInput, CycleTime, FallDirection, FallState, HeadJoints, Joints,
    JointsCommand, MotionCommand, MotionSafeExits, MotionSelection, MotionType, SensorData,
};

pub struct FallProtector {
    start_time: SystemTime,
    interpolator: MotionInterpolator<Joints<f32>>,
    roll_pitch_filter: LowPassFilter<Vector2<f32>>,
    last_fall_state: FallState,
    fallen_time: Option<SystemTime>,
}

#[context]
pub struct CreationContext {
    hardware_interface: HardwareInterface,
    fall_state_estimation: Parameter<FallStateEstimation, "fall_state_estimation">,
}

#[context]
pub struct CycleContext {
    condition_input: Input<ConditionInput, "condition_input">,
    cycle_time: Input<CycleTime, "cycle_time">,
    fall_state: Input<FallState, "fall_state">,
    motion_command: Input<MotionCommand, "motion_command">,
    motion_selection: Input<MotionSelection, "motion_selection">,
    sensor_data: Input<SensorData, "sensor_data">,

    fall_protection: Parameter<FallProtection, "fall_protection">,

    motion_safe_exits: PersistentState<MotionSafeExits, "motion_safe_exits">,
}

#[context]
#[derive(Default)]
pub struct MainOutputs {
    pub fall_protection_command: MainOutput<JointsCommand<f32>>,
}

impl FallProtector {
    pub fn new(context: CreationContext<impl PathsInterface>) -> Result<Self> {
        let paths = context.hardware_interface.get_paths();
        Ok(Self {
            start_time: UNIX_EPOCH,
            interpolator: MotionFile::from_path(paths.motions.join("fall_back.json"))?
                .try_into()?,
            roll_pitch_filter: LowPassFilter::with_smoothing_factor(
                Vector2::zeros(),
                context.fall_state_estimation.roll_pitch_low_pass_factor,
            ),
            last_fall_state: FallState::Upright,
            fallen_time: None,
        })
    }

    pub fn cycle(&mut self, context: CycleContext) -> Result<MainOutputs> {
        let current_positions = context.sensor_data.positions;
        let mut head_stiffness = 1.0;

        self.roll_pitch_filter
            .update(context.sensor_data.inertial_measurement_unit.roll_pitch);

        context.motion_safe_exits[MotionType::FallProtection] = false;

        if context.motion_selection.current_motion != MotionType::FallProtection {
            self.start_time = context.cycle_time.start_time;

            return Ok(MainOutputs {
                fall_protection_command: JointsCommand {
                    positions: current_positions,
                    stiffnesses: Joints::fill(0.8),
                }
                .into(),
            });
        }

        if context
            .cycle_time
            .start_time
            .duration_since(self.start_time)
            .expect("time ran backwards")
            >= Duration::from_millis(500)
        {
            head_stiffness = 0.5;
        }

        if context
            .cycle_time
            .start_time
            .duration_since(self.start_time)
            .expect("time ran backwards")
            >= context.fall_protection.time_free_motion_exit
        {
            context.motion_safe_exits[MotionType::FallProtection] = true;
        }

        self.fallen_time = match (self.last_fall_state, context.fall_state) {
            (FallState::Falling { .. }, FallState::Fallen { .. }) => {
                Some(context.cycle_time.start_time)
            }
            (FallState::Fallen { .. }, FallState::Fallen { .. }) => self.fallen_time,
            _ => None,
        };

        match context.motion_command {
            MotionCommand::FallProtection {
                direction: FallDirection::Forward,
            } => {
                if relative_eq!(current_positions.head.pitch, -0.672, epsilon = 0.05)
                    && relative_eq!(current_positions.head.yaw.abs(), 0.0, epsilon = 0.05)
                {
                    head_stiffness = context.fall_protection.ground_impact_head_stiffness;
                }
            }
            MotionCommand::FallProtection { .. } => {
                if relative_eq!(current_positions.head.pitch, 0.5149, epsilon = 0.05)
                    && relative_eq!(current_positions.head.yaw.abs(), 0.0, epsilon = 0.05)
                {
                    head_stiffness = context.fall_protection.ground_impact_head_stiffness;
                }
            }
            _ => head_stiffness = context.fall_protection.ground_impact_head_stiffness,
        }

        let body_stiffnesses = if self.roll_pitch_filter.state().y.abs()
            > context.fall_protection.ground_impact_angular_threshold
        {
            BodyJoints::fill(context.fall_protection.ground_impact_body_stiffness)
        } else {
            BodyJoints::fill_mirrored(
                context.fall_protection.arm_stiffness,
                context.fall_protection.leg_stiffness,
            )
        };

        let stiffnesses =
            Joints::from_head_and_body(HeadJoints::fill(head_stiffness), body_stiffnesses);

        let fall_protection_command = match context.motion_command {
            MotionCommand::FallProtection {
                direction: FallDirection::Forward,
            } => {
                self.interpolator.reset();
                JointsCommand {
                    positions: Joints::from_head_and_body(
                        HeadJoints {
                            yaw: 0.0,
                            pitch: -0.672,
                        },
                        BodyJoints {
                            left_arm: context.fall_protection.left_arm_positions,
                            right_arm: context.fall_protection.right_arm_positions,
                            left_leg: current_positions.left_leg,
                            right_leg: current_positions.right_leg,
                        },
                    ),
                    stiffnesses,
                }
            }

            MotionCommand::FallProtection {
                direction: FallDirection::Backward,
            } => {
                self.interpolator.set_initial_positions(current_positions);
                self.interpolator.advance_by(
                    context.cycle_time.last_cycle_duration,
                    context.condition_input,
                );

                JointsCommand {
                    positions: self.interpolator.value(),
                    stiffnesses,
                }
            }
            _ => {
                self.interpolator.reset();
                JointsCommand {
                    positions: Joints::from_head_and_body(
                        HeadJoints {
                            yaw: 0.0,
                            pitch: 0.5149,
                        },
                        BodyJoints {
                            left_arm: context.fall_protection.left_arm_positions,
                            right_arm: context.fall_protection.right_arm_positions,
                            left_leg: current_positions.left_leg,
                            right_leg: current_positions.right_leg,
                        },
                    ),
                    stiffnesses,
                }
            }
        };

        self.last_fall_state = *context.fall_state;

        match self.fallen_time {
            Some(fallen_start)
                if context
                    .cycle_time
                    .start_time
                    .duration_since(fallen_start)
                    .expect("time ran backwards")
                    >= context.fall_protection.time_prolong_ground_impact =>
            {
                context.motion_safe_exits[MotionType::FallProtection] = true;
                self.fallen_time = None;
            }
            _ => (),
        }

        Ok(MainOutputs {
            fall_protection_command: fall_protection_command.into(),
        })
    }
}
