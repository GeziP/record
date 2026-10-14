//! 控制器：待命 → 启动中 → 录制中 → 收尾中 → 待命（设计文档 6.1）。
//!
//! 纯状态机，不碰线程和设备：外壳把热键、菜单、自动保护、系统事件翻译成 [`Input`] 送进来，
//! 按返回的 [`Action`] 去执行。所有输入由同一个线程串行送入。

use crate::session::EndReason;

/// 500 ms 内重复的同一命令被忽略。
pub const DEBOUNCE_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Starting,
    Recording,
    Finalizing,
}

/// 用户命令，热键与托盘菜单共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    /// 保存并退出程序。
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    Command(Command),
    /// 启动流程完成，已开始写入。
    StartSucceeded,
    StartFailed(String),
    /// 收尾完成。
    FinalizeDone,
    /// 自动保护、设备故障、睡眠等需要结束本场的原因。
    AutoStop(EndReason),
    /// 关机或注销：保存本场并退出程序。
    EndSession(EndReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 创建会话目录，启动采集与编码。
    BeginStart,
    /// 停止采集、冲刷队列、关闭编码器、写 `session.json`。
    BeginFinalize(EndReason),
    NotifyStartFailed(String),
    NotifySaved(EndReason),
    QuitApp,
}

#[derive(Debug)]
pub struct Controller {
    state: State,
    last_command: Option<(Command, u64)>,
    pending_stop: Option<EndReason>,
    pending_start: bool,
    quit_after: bool,
    finalizing_reason: EndReason,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

impl Controller {
    pub fn new() -> Self {
        Self { state: State::Idle, last_command: None, pending_stop: None, pending_start: false, quit_after: false, finalizing_reason: EndReason::User }
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// `now_ms` 是单调递增的毫秒时钟，只用于去抖。
    pub fn handle(&mut self, input: Input, now_ms: u64) -> Vec<Action> {
        match input {
            Input::Command(cmd) => {
                if let Some((last, at)) = self.last_command
                    && last == cmd
                    && now_ms.saturating_sub(at) < DEBOUNCE_MS
                {
                    return vec![];
                }
                self.last_command = Some((cmd, now_ms));
                self.command(cmd)
            }
            Input::StartSucceeded => self.start_succeeded(),
            Input::StartFailed(msg) => self.start_failed(msg),
            Input::FinalizeDone => self.finalize_done(),
            Input::AutoStop(reason) => self.auto_stop(reason, false),
            Input::EndSession(reason) => self.auto_stop(reason, true),
        }
    }

    fn command(&mut self, cmd: Command) -> Vec<Action> {
        match (self.state, cmd) {
            (State::Idle, Command::Start) => {
                self.state = State::Starting;
                vec![Action::BeginStart]
            }
            (State::Idle, Command::Stop) => vec![],
            (State::Idle, Command::Quit) => vec![Action::QuitApp],

            (State::Starting, Command::Start) => vec![],
            (State::Starting, Command::Stop) => {
                self.pending_stop.get_or_insert(EndReason::User);
                vec![]
            }
            (State::Starting, Command::Quit) => {
                self.pending_stop.get_or_insert(EndReason::User);
                self.quit_after = true;
                vec![]
            }

            (State::Recording, Command::Start) => vec![],
            (State::Recording, Command::Stop) => self.begin_finalize(EndReason::User),
            (State::Recording, Command::Quit) => {
                self.quit_after = true;
                self.begin_finalize(EndReason::User)
            }

            (State::Finalizing, Command::Start) => {
                self.pending_start = true;
                vec![]
            }
            (State::Finalizing, Command::Stop) => vec![],
            (State::Finalizing, Command::Quit) => {
                self.quit_after = true;
                self.pending_start = false;
                vec![]
            }
        }
    }

    fn begin_finalize(&mut self, reason: EndReason) -> Vec<Action> {
        self.state = State::Finalizing;
        self.finalizing_reason = reason;
        vec![Action::BeginFinalize(reason)]
    }

    fn start_succeeded(&mut self) -> Vec<Action> {
        if self.state != State::Starting {
            return vec![];
        }
        self.state = State::Recording;
        match self.pending_stop.take() {
            Some(reason) => self.begin_finalize(reason),
            None => vec![],
        }
    }

    fn start_failed(&mut self, msg: String) -> Vec<Action> {
        if self.state != State::Starting {
            return vec![];
        }
        self.state = State::Idle;
        self.pending_stop = None;
        let mut actions = vec![Action::NotifyStartFailed(msg)];
        if std::mem::take(&mut self.quit_after) {
            actions.push(Action::QuitApp);
        }
        actions
    }

    fn finalize_done(&mut self) -> Vec<Action> {
        if self.state != State::Finalizing {
            return vec![];
        }
        self.state = State::Idle;
        let mut actions = vec![Action::NotifySaved(self.finalizing_reason)];
        if std::mem::take(&mut self.quit_after) {
            self.pending_start = false;
            actions.push(Action::QuitApp);
        } else if std::mem::take(&mut self.pending_start) {
            self.state = State::Starting;
            actions.push(Action::BeginStart);
        }
        actions
    }

    fn auto_stop(&mut self, reason: EndReason, then_quit: bool) -> Vec<Action> {
        if then_quit {
            self.quit_after = true;
            self.pending_start = false;
        }
        match self.state {
            State::Idle => {
                if then_quit {
                    self.quit_after = false;
                    vec![Action::QuitApp]
                } else {
                    vec![]
                }
            }
            State::Starting => {
                self.pending_stop = Some(reason);
                vec![]
            }
            State::Recording => self.begin_finalize(reason),
            State::Finalizing => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;
    use Command::*;

    fn cmd(c: Command) -> Input {
        Input::Command(c)
    }

    fn recording() -> Controller {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        c.handle(Input::StartSucceeded, 100);
        assert_eq!(c.state(), State::Recording);
        c
    }

    #[test]
    fn start_stop_cycle() {
        let mut c = Controller::new();
        assert_eq!(c.handle(cmd(Start), 0), vec![BeginStart]);
        assert_eq!(c.state(), State::Starting);
        assert_eq!(c.handle(Input::StartSucceeded, 10), vec![]);
        assert_eq!(c.handle(cmd(Stop), 10_000), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.state(), State::Finalizing);
        assert_eq!(c.handle(Input::FinalizeDone, 11_000), vec![NotifySaved(EndReason::User)]);
        assert_eq!(c.state(), State::Idle);
    }

    #[test]
    fn commands_are_idempotent() {
        let mut c = Controller::new();
        assert_eq!(c.handle(cmd(Stop), 0), vec![], "待命时停止无效果");
        let mut c = recording();
        assert_eq!(c.handle(cmd(Start), 5_000), vec![], "录制中开始无效果");
        assert_eq!(c.state(), State::Recording);
    }

    #[test]
    fn repeated_commands_within_500ms_are_ignored() {
        let mut c = Controller::new();
        assert_eq!(c.handle(cmd(Start), 1_000), vec![BeginStart]);
        c.handle(Input::StartFailed("x".into()), 1_100); // 回到待命
        // 同一个命令 400 ms 后又来：去抖忽略，即使状态允许
        assert_eq!(c.handle(cmd(Start), 1_400), vec![]);
        assert_eq!(c.state(), State::Idle);
        // 满 500 ms 后接受
        assert_eq!(c.handle(cmd(Start), 1_500), vec![BeginStart]);
    }

    #[test]
    fn a_different_command_is_not_debounced() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        c.handle(Input::StartSucceeded, 50);
        assert_eq!(c.handle(cmd(Stop), 100), vec![BeginFinalize(EndReason::User)]);
    }

    #[test]
    fn start_failure_returns_to_idle_with_a_message() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(Input::StartFailed("磁盘不可写".into()), 10), vec![NotifyStartFailed("磁盘不可写".into())]);
        assert_eq!(c.state(), State::Idle);
        assert_eq!(c.handle(cmd(Start), 2_000), vec![BeginStart], "失败后可以重新开始");
    }

    #[test]
    fn quit_when_idle_exits_immediately() {
        assert_eq!(Controller::new().handle(cmd(Quit), 0), vec![QuitApp]);
    }

    #[test]
    fn quit_while_recording_saves_first() {
        let mut c = recording();
        assert_eq!(c.handle(cmd(Quit), 5_000), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.handle(Input::FinalizeDone, 6_000), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn stop_during_starting_is_deferred_until_recording_begins() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(cmd(Stop), 100), vec![]);
        assert_eq!(c.state(), State::Starting);
        assert_eq!(c.handle(Input::StartSucceeded, 200), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.state(), State::Finalizing);
    }

    #[test]
    fn quit_during_starting_waits_for_the_outcome() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(cmd(Quit), 100), vec![]);
        assert_eq!(c.handle(Input::StartFailed("设备打不开".into()), 200), vec![NotifyStartFailed("设备打不开".into()), QuitApp]);

        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        c.handle(cmd(Quit), 100);
        assert_eq!(c.handle(Input::StartSucceeded, 200), vec![BeginFinalize(EndReason::User)]);
        assert_eq!(c.handle(Input::FinalizeDone, 300), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn start_during_finalizing_begins_the_next_session_afterwards() {
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        assert_eq!(c.handle(cmd(Start), 5_600), vec![]);
        assert_eq!(c.handle(Input::FinalizeDone, 6_000), vec![NotifySaved(EndReason::User), BeginStart]);
        assert_eq!(c.state(), State::Starting);
    }

    #[test]
    fn quit_cancels_a_queued_start() {
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        c.handle(cmd(Start), 5_600);
        c.handle(cmd(Quit), 6_200);
        assert_eq!(c.handle(Input::FinalizeDone, 7_000), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn auto_protections_finalize_with_their_reason() {
        for reason in [EndReason::MaxDuration, EndReason::DiskLow, EndReason::Sleep, EndReason::WriteError, EndReason::CaptureFailure] {
            let mut c = recording();
            assert_eq!(c.handle(Input::AutoStop(reason), 9_000), vec![BeginFinalize(reason)]);
            assert_eq!(c.handle(Input::FinalizeDone, 9_500), vec![NotifySaved(reason)]);
            assert_eq!(c.state(), State::Idle);
        }
    }

    #[test]
    fn auto_stop_is_ignored_when_not_recording() {
        let mut c = Controller::new();
        assert_eq!(c.handle(Input::AutoStop(EndReason::DiskLow), 0), vec![]);
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        assert_eq!(c.handle(Input::AutoStop(EndReason::DiskLow), 5_100), vec![], "已经在收尾");
    }

    #[test]
    fn auto_stop_during_starting_applies_once_recording_begins() {
        let mut c = Controller::new();
        c.handle(cmd(Start), 0);
        assert_eq!(c.handle(Input::AutoStop(EndReason::Sleep), 50), vec![]);
        assert_eq!(c.handle(Input::StartSucceeded, 100), vec![BeginFinalize(EndReason::Sleep)]);
    }

    #[test]
    fn shutdown_saves_then_quits() {
        let mut c = recording();
        assert_eq!(c.handle(Input::EndSession(EndReason::Shutdown), 9_000), vec![BeginFinalize(EndReason::Shutdown)]);
        assert_eq!(c.handle(Input::FinalizeDone, 9_800), vec![NotifySaved(EndReason::Shutdown), QuitApp]);

        let mut idle = Controller::new();
        assert_eq!(idle.handle(Input::EndSession(EndReason::Logoff), 0), vec![QuitApp]);
    }

    #[test]
    fn shutdown_while_finalizing_quits_after_it_completes() {
        let mut c = recording();
        c.handle(cmd(Stop), 5_000);
        assert_eq!(c.handle(Input::EndSession(EndReason::Shutdown), 5_100), vec![]);
        assert_eq!(c.handle(Input::FinalizeDone, 5_900), vec![NotifySaved(EndReason::User), QuitApp]);
    }

    #[test]
    fn stray_completion_events_are_ignored() {
        let mut c = Controller::new();
        assert_eq!(c.handle(Input::StartSucceeded, 0), vec![]);
        assert_eq!(c.handle(Input::FinalizeDone, 0), vec![]);
        assert_eq!(c.handle(Input::StartFailed("x".into()), 0), vec![]);
        assert_eq!(c.state(), State::Idle);
    }
}
