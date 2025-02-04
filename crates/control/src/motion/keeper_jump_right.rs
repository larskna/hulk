use color_eyre::Result;
use context_attribute::context;
use framework::MainOutput;
use hardware::PathsInterface;
use motionfile::{MotionFile, MotionInterpolator};
use serde::{Deserialize, Serialize};
use types::{
    condition_input::ConditionInput,
    cycle_time::CycleTime,
    joints::Joints,
    motion_selection::{MotionSafeExits, MotionSelection, MotionType},
    motor_commands::MotorCommands,
};

#[derive(Deserialize, Serialize)]
pub struct KeeperJumpRight {
    interpolator: MotionInterpolator<MotorCommands<Joints<f32>>>,
}

#[context]
pub struct CreationContext {
    hardware_interface: HardwareInterface,
}

#[context]
pub struct CycleContext {
    condition_input: Input<ConditionInput, "condition_input">,
    cycle_time: Input<CycleTime, "cycle_time">,
    motion_selection: Input<MotionSelection, "motion_selection">,

    motion_safe_exits: CyclerState<MotionSafeExits, "motion_safe_exits">,
}

#[context]
#[derive(Default)]
pub struct MainOutputs {
    pub keeper_jump_right_motor_commands: MainOutput<MotorCommands<Joints<f32>>>,
}

impl KeeperJumpRight {
    pub fn new(context: CreationContext<impl PathsInterface>) -> Result<Self> {
        let paths = context.hardware_interface.get_paths();
        Ok(Self {
            interpolator: MotionFile::from_path(paths.motions.join("keeper_jump_right.json"))?
                .try_into()?,
        })
    }

    pub fn cycle(&mut self, context: CycleContext) -> Result<MainOutputs> {
        let last_cycle_duration = context.cycle_time.last_cycle_duration;
        let condition_input = context.condition_input;

        if context.motion_selection.current_motion == MotionType::KeeperJumpRight {
            self.interpolator
                .advance_by(last_cycle_duration, condition_input);
        } else {
            self.interpolator.reset();
        }
        context.motion_safe_exits[MotionType::KeeperJumpRight] = self.interpolator.is_finished();

        Ok(MainOutputs {
            keeper_jump_right_motor_commands: self.interpolator.value().into(),
        })
    }
}
