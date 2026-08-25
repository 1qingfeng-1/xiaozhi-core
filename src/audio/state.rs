//! 音频子系统状态机
//!
//! 建模音频链路（mic → codec → I2S → speaker）的运行状态与事件转移。
//! 纯逻辑，无硬件依赖，可在 host 上单元测试所有合法/非法转移。

/// 音频子系统状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AudioState {
    /// 上电默认：未初始化
    #[default]
    Idle,
    /// Codec 初始化中
    Initializing,
    /// 已就绪，可收/发
    Ready,
    /// 正在采集/播放
    Active,
    /// 暂停
    Paused,
    /// 出错（等待复位）
    Error,
}

/// 音频子系统事件
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioEvent {
    /// 请求初始化
    Init,
    /// 初始化完成
    InitDone,
    /// 初始化失败
    InitFail,
    /// 开始采集/播放
    Start,
    /// 暂停
    Pause,
    /// 恢复
    Resume,
    /// 停止
    Stop,
    /// 发生错误
    Fault,
    /// 复位（从错误恢复）
    Reset,
}

/// 状态机：给定当前状态和事件，返回下一状态。
///
/// 非法转移返回 `None`（调用方可记录告警并保持原状态）。
///
/// # 转移表
/// | 当前 \ 事件 | Init | Start | Pause | Resume | Stop | Fault | Reset |
/// |---|---|---|---|---|---|---|---|
/// | Idle | Some(Initializing) | - | - | - | - | Some(Error) | - |
/// | Initializing | - | - | - | - | - | Some(Error) | Some(Idle) |
/// | Ready | - | Some(Active) | - | - | Some(Idle) | Some(Error) | Some(Idle) |
/// | Active | - | - | Some(Paused) | - | Some(Ready) | Some(Error) | Some(Idle) |
/// | Paused | - | - | - | Some(Active) | Some(Ready) | Some(Error) | Some(Idle) |
/// | Error | - | - | - | - | - | - | Some(Idle) |
///
/// `InitDone` 从 Initializing → Ready。
#[must_use]
pub fn transition(state: AudioState, event: AudioEvent) -> Option<AudioState> {
    match (state, event) {
        (AudioState::Idle, AudioEvent::Init) => Some(AudioState::Initializing),
        (AudioState::Initializing, AudioEvent::InitDone) => Some(AudioState::Ready),
        (AudioState::Initializing, AudioEvent::InitFail) => Some(AudioState::Error),

        (AudioState::Ready, AudioEvent::Start) => Some(AudioState::Active),
        (AudioState::Ready, AudioEvent::Stop) => Some(AudioState::Idle),

        (AudioState::Active, AudioEvent::Pause) => Some(AudioState::Paused),
        (AudioState::Active, AudioEvent::Stop) => Some(AudioState::Ready),

        (AudioState::Paused, AudioEvent::Resume) => Some(AudioState::Active),
        (AudioState::Paused, AudioEvent::Stop) => Some(AudioState::Ready),

        // 任何运行态遇到 Fault → Error
        (AudioState::Initializing, AudioEvent::Fault) => Some(AudioState::Error),
        (AudioState::Ready, AudioEvent::Fault) => Some(AudioState::Error),
        (AudioState::Active, AudioEvent::Fault) => Some(AudioState::Error),
        (AudioState::Paused, AudioEvent::Fault) => Some(AudioState::Error),
        (AudioState::Idle, AudioEvent::Fault) => Some(AudioState::Error),

        // 错误只能复位回 Idle
        (AudioState::Error, AudioEvent::Reset) => Some(AudioState::Idle),

        _ => None,
    }
}

/// 判断状态是否允许音频数据流（Active 或 Paused 表示链路已建立）。
#[must_use]
pub fn is_link_up(state: AudioState) -> bool {
    matches!(
        state,
        AudioState::Ready | AudioState::Active | AudioState::Paused
    )
}

/// 判断状态是否允许接受新的 `Start` 命令。
#[must_use]
pub fn can_start(state: AudioState) -> bool {
    matches!(state, AudioState::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_init_to_initializing() {
        assert_eq!(
            transition(AudioState::Idle, AudioEvent::Init),
            Some(AudioState::Initializing)
        );
    }

    #[test]
    fn init_done_to_ready() {
        assert_eq!(
            transition(AudioState::Initializing, AudioEvent::InitDone),
            Some(AudioState::Ready)
        );
    }

    #[test]
    fn init_fail_to_error() {
        assert_eq!(
            transition(AudioState::Initializing, AudioEvent::InitFail),
            Some(AudioState::Error)
        );
    }

    #[test]
    fn ready_start_to_active() {
        assert_eq!(
            transition(AudioState::Ready, AudioEvent::Start),
            Some(AudioState::Active)
        );
    }

    #[test]
    fn active_pause_resume() {
        assert_eq!(
            transition(AudioState::Active, AudioEvent::Pause),
            Some(AudioState::Paused)
        );
        assert_eq!(
            transition(AudioState::Paused, AudioEvent::Resume),
            Some(AudioState::Active)
        );
    }

    #[test]
    fn error_only_resets() {
        // Error 状态除 Reset 外都非法
        assert_eq!(
            transition(AudioState::Error, AudioEvent::Reset),
            Some(AudioState::Idle)
        );
        assert_eq!(transition(AudioState::Error, AudioEvent::Init), None);
        assert_eq!(transition(AudioState::Error, AudioEvent::Start), None);
    }

    #[test]
    fn fault_from_any_running_state() {
        for s in [
            AudioState::Idle,
            AudioState::Initializing,
            AudioState::Ready,
            AudioState::Active,
            AudioState::Paused,
        ] {
            assert_eq!(transition(s, AudioEvent::Fault), Some(AudioState::Error));
        }
    }

    #[test]
    fn illegal_transitions_return_none() {
        // Idle 不能直接 Start
        assert_eq!(transition(AudioState::Idle, AudioEvent::Start), None);
        // Active 不能直接 Resume
        assert_eq!(transition(AudioState::Active, AudioEvent::Resume), None);
        // Ready 不能 Pause
        assert_eq!(transition(AudioState::Ready, AudioEvent::Pause), None);
    }

    #[test]
    fn link_up_states() {
        assert!(is_link_up(AudioState::Ready));
        assert!(is_link_up(AudioState::Active));
        assert!(is_link_up(AudioState::Paused));
        assert!(!is_link_up(AudioState::Idle));
        assert!(!is_link_up(AudioState::Initializing));
        assert!(!is_link_up(AudioState::Error));
    }

    #[test]
    fn can_start_only_ready() {
        assert!(can_start(AudioState::Ready));
        assert!(!can_start(AudioState::Idle));
        assert!(!can_start(AudioState::Active));
        assert!(!can_start(AudioState::Error));
    }
}
