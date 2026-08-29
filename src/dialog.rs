//! 对话状态机 — 智能音箱会话生命周期
//!
//! 纯逻辑：只维护状态与合法转移，输出固件应执行的副作用（[`Action`]），
//! 不触碰任何硬件/网络。时间、重试、I2S 等都在固件侧。
//!
//! 两种聆听模式：
//! - [`ListenMode::Auto`]：唤醒词/声音触发，说话即对讲，TTS 播完继续聆听
//! - [`ListenMode::Manual`]：按住 BOOT 对讲（push-to-talk），松开即收音

/// 对话阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogState {
    /// 待机：无会话，正在监听唤醒
    Idle,
    /// 会话建立中（WS 连接 + hello 握手）
    Connecting,
    /// 聆听中（上行 opus 已开）
    Listening,
    /// 已停止收音，等待服务器回复（manual 模式松开后）
    WaitingReply,
    /// 服务器语音播报中（TTS 下行）
    Speaking,
}

/// 聆听模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenMode {
    Auto,
    Manual,
}

/// 对话事件（固件从按键/网络/唤醒引擎投递）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogEvent {
    /// 唤醒触发（auto）
    WakeTriggered,
    /// 按住 BOOT（manual）
    ManualListenStart,
    /// 松开 BOOT（manual）
    ManualListenStop,
    /// WS 会话就绪（hello 握手完成）
    SessionReady,
    /// 服务器开始语音回复（tts start）
    TtsStart,
    /// 服务器语音回复结束（tts stop）
    TtsStop,
    /// 会话关闭/断开/失败
    SessionClosed,
}

/// 状态机要求固件执行的副作用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 建立 WS 会话（连接 + hello）
    ConnectSession,
    /// 发送 listen start（模式由当前 mode 决定，固件组包）
    SendListenStart,
    /// 发送 listen stop
    SendListenStop,
    /// 发送 tts abort（打断播报）
    SendTtsAbort,
    /// 播放唤醒提示音
    PlayWakeChime,
}

impl Action {
    /// 便于固件日志的动作名
    pub fn name(self) -> &'static str {
        match self {
            Action::ConnectSession => "连接会话",
            Action::SendListenStart => "listen start",
            Action::SendListenStop => "listen stop",
            Action::SendTtsAbort => "tts abort",
            Action::PlayWakeChime => "唤醒提示音",
        }
    }
}

/// 对话状态机
#[derive(Debug)]
pub struct DialogSm {
    state: DialogState,
    mode: ListenMode,
}

impl DialogSm {
    pub fn new() -> Self {
        Self {
            state: DialogState::Idle,
            mode: ListenMode::Auto,
        }
    }

    pub fn state(&self) -> DialogState {
        self.state
    }

    pub fn mode(&self) -> ListenMode {
        self.mode
    }

    /// 处理事件，返回 (新状态, 需执行的副作用列表)
    pub fn transition(&mut self, ev: DialogEvent) -> (DialogState, &[Action]) {
        use Action::*;
        use DialogEvent as E;
        use DialogState as S;

        self.mode = match ev {
            E::WakeTriggered => ListenMode::Auto,
            E::ManualListenStart | E::ManualListenStop => ListenMode::Manual,
            _ => self.mode,
        };

        let actions: &[Action] = match (self.state, ev) {
            // ---- 待机 → 建立会话 ----
            (S::Idle, E::WakeTriggered) => &[PlayWakeChime, ConnectSession],
            (S::Idle, E::ManualListenStart) => &[ConnectSession],
            // ---- 建立中 → 就绪聆听 ----
            (S::Connecting, E::SessionReady) => &[SendListenStart],
            (S::Connecting, E::SessionClosed) => &[],
            // 用户在建连期间松开了 BOOT（manual）
            (S::Connecting, E::ManualListenStop) => &[],
            // ---- 聆听 ----
            (S::Listening, E::TtsStart) => &[],
            (S::Listening, E::ManualListenStop) => &[SendListenStop],
            // ---- 等待回复 ----
            (S::WaitingReply, E::TtsStart) => &[],
            (S::WaitingReply, E::ManualListenStart) => &[SendListenStart],
            // 没等到回复且用户没有再按：结束（会话由固件决定是否保活）
            (S::WaitingReply, E::SessionClosed) => &[],
            // ---- 播报 ----
            // auto 模式播完继续聆听（连续对话）；manual 模式播完回待机
            (S::Speaking, E::TtsStop) if self.mode == ListenMode::Auto => &[SendListenStart],
            (S::Speaking, E::TtsStop) => &[],
            // manual 模式播报中按住 BOOT：打断并转入聆听
            (S::Speaking, E::ManualListenStart) => &[SendTtsAbort, SendListenStart],
            // ---- 会话断开 → 待机（Connecting 已单独匹配）----
            (S::Listening | S::Speaking, E::SessionClosed) => &[],
            // ---- 其余事件忽略 ----
            _ => &[],
        };

        self.state = match (self.state, ev) {
            (S::Idle, E::WakeTriggered | E::ManualListenStart) => S::Connecting,
            (S::Connecting, E::SessionReady) => S::Listening,
            (S::Connecting, E::SessionClosed | E::ManualListenStop) => S::Idle,
            (S::Listening, E::TtsStart) => S::Speaking,
            (S::Listening, E::ManualListenStop) => S::WaitingReply,
            (S::WaitingReply, E::TtsStart) => S::Speaking,
            (S::WaitingReply, E::ManualListenStart) => S::Listening,
            (S::Speaking, E::TtsStop) if self.mode == ListenMode::Auto => S::Listening,
            (S::Speaking, E::TtsStop) => S::Idle,
            (S::Speaking, E::ManualListenStart) => S::Listening,
            (_, E::SessionClosed) => S::Idle,
            _ => self.state,
        };

        (self.state, actions)
    }
}

impl Default for DialogSm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;
    use std::vec::Vec;
    use super::*;

    fn actions_of(sm: &mut DialogSm, ev: DialogEvent) -> std::vec::Vec<Action> {
        sm.transition(ev).1.to_vec()
    }

    #[test]
    fn auto_flow_full_cycle() {
        let mut sm = DialogSm::new();
        assert_eq!(sm.state(), DialogState::Idle);

        let a = actions_of(&mut sm, DialogEvent::WakeTriggered);
        assert_eq!(a, vec![Action::PlayWakeChime, Action::ConnectSession]);
        assert_eq!(sm.state(), DialogState::Connecting);
        assert_eq!(sm.mode(), ListenMode::Auto);

        let a = actions_of(&mut sm, DialogEvent::SessionReady);
        assert_eq!(a, vec![Action::SendListenStart]);
        assert_eq!(sm.state(), DialogState::Listening);

        // 说话 → 服务器回复
        assert_eq!(actions_of(&mut sm, DialogEvent::TtsStart), Vec::new());
        assert_eq!(sm.state(), DialogState::Speaking);

        // auto 模式播完 → 继续聆听（连续对话）
        let a = actions_of(&mut sm, DialogEvent::TtsStop);
        assert_eq!(a, vec![Action::SendListenStart]);
        assert_eq!(sm.state(), DialogState::Listening);

        // 断开 → 待机
        actions_of(&mut sm, DialogEvent::SessionClosed);
        assert_eq!(sm.state(), DialogState::Idle);
    }

    #[test]
    fn manual_flow_push_to_talk() {
        let mut sm = DialogSm::new();
        actions_of(&mut sm, DialogEvent::ManualListenStart);
        assert_eq!(sm.state(), DialogState::Connecting);
        assert_eq!(sm.mode(), ListenMode::Manual);

        let a = actions_of(&mut sm, DialogEvent::SessionReady);
        assert_eq!(a, vec![Action::SendListenStart]);

        // 松开 → 等回复
        let a = actions_of(&mut sm, DialogEvent::ManualListenStop);
        assert_eq!(a, vec![Action::SendListenStop]);
        assert_eq!(sm.state(), DialogState::WaitingReply);

        // 回复到达 → 播报
        actions_of(&mut sm, DialogEvent::TtsStart);
        assert_eq!(sm.state(), DialogState::Speaking);

        // manual 播完 → 待机（不再自动聆听）
        let a = actions_of(&mut sm, DialogEvent::TtsStop);
        assert_eq!(a, Vec::new());
        assert_eq!(sm.state(), DialogState::Idle);
    }

    #[test]
    fn manual_barge_in_during_speaking() {
        let mut sm = DialogSm::new();
        for ev in [
            DialogEvent::ManualListenStart,
            DialogEvent::SessionReady,
            DialogEvent::TtsStart,
        ] {
            actions_of(&mut sm, ev);
        }
        assert_eq!(sm.state(), DialogState::Speaking);

        // 播报中按住 BOOT：打断播报并开始聆听
        let a = actions_of(&mut sm, DialogEvent::ManualListenStart);
        assert_eq!(a, vec![Action::SendTtsAbort, Action::SendListenStart]);
        assert_eq!(sm.state(), DialogState::Listening);
    }

    #[test]
    fn release_before_session_ready_goes_idle() {
        let mut sm = DialogSm::new();
        actions_of(&mut sm, DialogEvent::ManualListenStart);
        actions_of(&mut sm, DialogEvent::ManualListenStop);
        assert_eq!(sm.state(), DialogState::Idle);
        // 迟到的 SessionReady 不应把 Idle 拉回 Listening
        actions_of(&mut sm, DialogEvent::SessionReady);
        assert_eq!(sm.state(), DialogState::Idle);
    }

    #[test]
    fn session_closed_from_any_active_state() {
        for start in [
            DialogEvent::WakeTriggered,
            DialogEvent::ManualListenStart,
        ] {
            let mut sm = DialogSm::new();
            actions_of(&mut sm, start);
            actions_of(&mut sm, DialogEvent::SessionReady);
            actions_of(&mut sm, DialogEvent::TtsStart);
            actions_of(&mut sm, DialogEvent::SessionClosed);
            assert_eq!(sm.state(), DialogState::Idle);
        }
    }

    #[test]
    fn ignored_events_do_not_change_state() {
        let mut sm = DialogSm::new();
        // Idle 下的网络事件与松键无效
        for ev in [
            DialogEvent::TtsStart,
            DialogEvent::TtsStop,
            DialogEvent::ManualListenStop,
            DialogEvent::SessionClosed,
        ] {
            actions_of(&mut sm, ev);
            assert_eq!(sm.state(), DialogState::Idle);
        }
        // manual 模式下 auto 的唤醒不改变模式
        actions_of(&mut sm, DialogEvent::ManualListenStart);
        actions_of(&mut sm, DialogEvent::SessionReady);
        assert_eq!(sm.mode(), ListenMode::Manual);
        assert_eq!(sm.state(), DialogState::Listening);
    }
}
