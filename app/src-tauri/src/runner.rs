//! 控制器的执行者（设计文档 6.1）：热键、菜单、自动保护、系统事件都变成 [`Msg`] 排进同一个队列，
//! 由一个线程串行交给 [`Controller`]，再按它返回的动作去启动、结束录制。
//!
//! 真正的录制通过 [`Backend`] 完成，这样状态流转可以不碰设备来测试。

use crate::notice::Notice;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use record_core::controller::{Action, Command, Controller, Input, State};
use record_core::session::EndReason;
use record_engine::events::{Alert, EngineEvent};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 录制中的进度，托盘提示用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub elapsed: Duration,
    pub bytes: u64,
}

/// 一场录制收尾后的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub dir: PathBuf,
    pub meeting: Option<PathBuf>,
    pub duration_secs: f64,
    pub warnings: Vec<String>,
}

pub trait Backend: Send + 'static {
    /// 创建会话并开始采集，引擎事件发到 `events`；返回会话目录。
    fn start(&mut self, events: Sender<EngineEvent>) -> Result<PathBuf, String>;
    fn progress(&mut self) -> Option<Progress>;
    /// 停止并生成成品。总会返回：出错也要尽量把已有内容留下。
    fn stop(&mut self, reason: EndReason) -> Saved;
}

pub enum Msg {
    Command(Command, u64),
    /// 自动保护、睡眠等需要结束本场的原因；`done` 在本场保存完（或本来就没在录）后收到一个信号。
    Auto(EndReason, Option<Sender<()>>),
    /// 关机、注销：保存本场并退出程序。
    EndSession(EndReason, Option<Sender<()>>),
    Engine(EngineEvent),
    Tick,
}

/// 发给界面（托盘、气泡）的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Ui {
    State(State),
    Progress(Progress),
    /// 本场当前的全部告警，新一场开始时清空。
    Warnings(Vec<String>),
    Notice(Notice),
    Quit,
}

pub struct Runner<B: Backend> {
    backend: B,
    controller: Controller,
    ui: Box<dyn FnMut(Ui) + Send>,
    msgs: Sender<Msg>,
    warnings: Vec<String>,
    waiting: Vec<Sender<()>>,
    last_saved: Option<Saved>,
    published: State,
}

impl<B: Backend> Runner<B> {
    pub fn new(backend: B, msgs: Sender<Msg>, ui: impl FnMut(Ui) + Send + 'static) -> Self {
        Self { backend, controller: Controller::new(), ui: Box::new(ui), msgs, warnings: Vec::new(), waiting: Vec::new(), last_saved: None, published: State::Idle }
    }

    pub fn state(&self) -> State {
        self.controller.state()
    }

    pub fn handle(&mut self, msg: Msg) {
        let actions = match msg {
            Msg::Command(command, at_ms) => self.controller.handle(Input::Command(command), at_ms),
            Msg::Auto(reason, done) => {
                self.waiting.extend(done);
                self.controller.handle(Input::AutoStop(reason), 0)
            }
            Msg::EndSession(reason, done) => {
                self.waiting.extend(done);
                self.controller.handle(Input::EndSession(reason), 0)
            }
            Msg::Engine(event) => self.engine_event(event),
            Msg::Tick => {
                self.tick();
                Vec::new()
            }
        };
        self.run(actions);
        self.publish_state();
        if self.controller.state() == State::Idle {
            for done in self.waiting.drain(..) {
                let _ = done.send(());
            }
        }
    }

    fn run(&mut self, actions: Vec<Action>) {
        let mut queue: VecDeque<Action> = actions.into();
        while let Some(action) = queue.pop_front() {
            let more = match action {
                Action::BeginStart => self.begin_start(),
                Action::BeginFinalize(reason) => self.begin_finalize(reason),
                Action::NotifyStartFailed(why) => {
                    self.ui(Ui::Notice(Notice::StartFailed(why)));
                    Vec::new()
                }
                Action::NotifySaved(reason) => {
                    if let Some(saved) = self.last_saved.take() {
                        let notice = Notice::Saved { reason, meeting: saved.meeting.map(|p| p.display().to_string()), duration_secs: saved.duration_secs, warnings: saved.warnings.len() };
                        self.ui(Ui::Notice(notice));
                    }
                    Vec::new()
                }
                Action::QuitApp => {
                    // 退出之前，托盘先回到待命：用户看到的最后一个状态应当是“已保存完”
                    self.publish_state();
                    self.ui(Ui::Quit);
                    Vec::new()
                }
            };
            queue.extend(more);
        }
    }

    fn begin_start(&mut self) -> Vec<Action> {
        self.warnings.clear();
        self.ui(Ui::Warnings(Vec::new()));
        let (events, incoming) = unbounded();
        match self.backend.start(events) {
            Ok(dir) => {
                // 引擎结束后会丢掉发送端，转发线程随之退出
                let msgs = self.msgs.clone();
                std::thread::spawn(move || {
                    for event in incoming {
                        if msgs.send(Msg::Engine(event)).is_err() {
                            break;
                        }
                    }
                });
                self.ui(Ui::Notice(Notice::Started { dir: dir.display().to_string() }));
                self.controller.handle(Input::StartSucceeded, 0)
            }
            Err(why) => self.controller.handle(Input::StartFailed(why), 0),
        }
    }

    fn begin_finalize(&mut self, reason: EndReason) -> Vec<Action> {
        // 先让托盘显示“正在保存”，停止要花零点几秒到几秒
        self.publish_state();
        self.last_saved = Some(self.backend.stop(reason));
        self.controller.handle(Input::FinalizeDone, 0)
    }

    fn engine_event(&mut self, event: EngineEvent) -> Vec<Action> {
        match event {
            EngineEvent::Info(_) => Vec::new(),
            EngineEvent::Alert(alert) => {
                self.alert(alert);
                Vec::new()
            }
            EngineEvent::RequestStop(reason) => self.controller.handle(Input::AutoStop(reason), 0),
        }
    }

    fn alert(&mut self, alert: Alert) {
        if alert.is_warning() {
            self.warnings.push(alert.describe());
            self.ui(Ui::Warnings(self.warnings.clone()));
        }
        self.ui(Ui::Notice(Notice::Alert(alert)));
    }

    fn tick(&mut self) {
        if self.controller.state() == State::Recording
            && let Some(progress) = self.backend.progress()
        {
            self.ui(Ui::Progress(progress));
        }
    }

    fn publish_state(&mut self) {
        let now = self.controller.state();
        if now != self.published {
            self.published = now;
            self.ui(Ui::State(now));
        }
    }

    fn ui(&mut self, event: Ui) {
        (self.ui)(event);
    }

    /// 在当前线程里一直循环处理，空闲时每 500 ms 补一个 [`Msg::Tick`]。
    /// 执行者自己留着一个发送端（转发引擎事件要用），所以队列不会因为“都关闭了”而结束：线程随进程一起退出。
    pub fn run_loop(mut self, incoming: Receiver<Msg>) {
        loop {
            match incoming.recv_timeout(Duration::from_millis(500)) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => self.handle(Msg::Tick),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

/// 外壳里各处（热键、菜单、系统事件）向执行者投递消息的入口，可以克隆。
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Msg>,
    epoch: Instant,
}

impl Handle {
    pub fn command(&self, command: Command) {
        let at_ms = self.epoch.elapsed().as_millis() as u64;
        let _ = self.tx.send(Msg::Command(command, at_ms));
    }

    /// 结束本场；返回的接收端在保存完成（或本来就没在录）时收到一个信号。
    pub fn auto(&self, reason: EndReason) -> Receiver<()> {
        let (done, wait) = unbounded();
        let _ = self.tx.send(Msg::Auto(reason, Some(done)));
        wait
    }

    pub fn end_session(&self, reason: EndReason) -> Receiver<()> {
        let (done, wait) = unbounded();
        let _ = self.tx.send(Msg::EndSession(reason, Some(done)));
        wait
    }
}

/// 启动执行者线程（常驻，随进程退出）。
pub fn spawn<B: Backend>(backend: B, ui: impl FnMut(Ui) + Send + 'static) -> Handle {
    let (tx, incoming) = unbounded();
    let runner = Runner::new(backend, tx.clone(), ui);
    std::thread::Builder::new().name("runner".into()).spawn(move || runner.run_loop(incoming)).expect("启动执行者线程");
    Handle { tx, epoch: Instant::now() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use record_core::session::TrackKind;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
        fail_start: Option<String>,
        ticks: u64,
    }

    impl Backend for Fake {
        fn start(&mut self, _events: Sender<EngineEvent>) -> Result<PathBuf, String> {
            if let Some(why) = self.fail_start.take() {
                return Err(why);
            }
            self.calls.lock().unwrap().push("start".into());
            Ok(PathBuf::from(r"D:\rec\one"))
        }

        fn progress(&mut self) -> Option<Progress> {
            self.ticks += 1;
            Some(Progress { elapsed: Duration::from_secs(self.ticks), bytes: self.ticks * 1000 })
        }

        fn stop(&mut self, reason: EndReason) -> Saved {
            self.calls.lock().unwrap().push(format!("stop {reason:?}"));
            Saved { dir: PathBuf::from(r"D:\rec\one"), meeting: Some(PathBuf::from(r"D:\rec\one\meeting.mp4")), duration_secs: 3.0, warnings: Vec::new() }
        }
    }

    struct Rig {
        runner: Runner<Fake>,
        ui: Arc<Mutex<Vec<Ui>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Rig {
        fn new() -> Self {
            Self::with(Fake::default())
        }

        fn with(backend: Fake) -> Self {
            let calls = backend.calls.clone();
            let ui = Arc::new(Mutex::new(Vec::new()));
            let sink = ui.clone();
            let (tx, _rx) = unbounded();
            Self { runner: Runner::new(backend, tx, move |e| sink.lock().unwrap().push(e)), ui, calls }
        }

        fn send(&mut self, msg: Msg) {
            self.runner.handle(msg);
        }

        fn events(&self) -> Vec<Ui> {
            self.ui.lock().unwrap().clone()
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn states(&self) -> Vec<State> {
            self.events().into_iter().filter_map(|e| if let Ui::State(s) = e { Some(s) } else { None }).collect()
        }

        fn notices(&self) -> Vec<Notice> {
            self.events().into_iter().filter_map(|e| if let Ui::Notice(n) = e { Some(n) } else { None }).collect()
        }
    }

    #[test]
    fn start_then_stop_walks_through_the_states_and_tells_the_user() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        assert_eq!(rig.runner.state(), State::Recording);
        rig.send(Msg::Command(Command::Stop, 5_000));
        assert_eq!(rig.runner.state(), State::Idle);
        assert_eq!(rig.states(), vec![State::Recording, State::Finalizing, State::Idle]);
        assert_eq!(rig.calls(), vec!["start".to_string(), "stop User".to_string()]);
        assert_eq!(
            rig.notices(),
            vec![Notice::Started { dir: r"D:\rec\one".into() }, Notice::Saved { reason: EndReason::User, meeting: Some(r"D:\rec\one\meeting.mp4".into()), duration_secs: 3.0, warnings: 0 },]
        );
    }

    #[test]
    fn a_start_that_fails_leaves_nothing_running_and_says_why() {
        let mut rig = Rig::with(Fake { fail_start: Some("磁盘剩余空间不足".into()), ..Fake::default() });
        rig.send(Msg::Command(Command::Start, 1_000));
        assert_eq!(rig.runner.state(), State::Idle);
        assert_eq!(rig.notices(), vec![Notice::StartFailed("磁盘剩余空间不足".into())]);
        assert!(rig.calls().is_empty(), "没开始就不该有 stop");
        // 失败后还能再来一次
        rig.send(Msg::Command(Command::Start, 3_000));
        assert_eq!(rig.runner.state(), State::Recording);
    }

    #[test]
    fn the_engine_can_ask_for_a_stop_and_the_reason_reaches_the_notice() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Engine(EngineEvent::RequestStop(EndReason::DiskLow)));
        assert_eq!(rig.runner.state(), State::Idle);
        assert_eq!(rig.calls().last().unwrap(), "stop DiskLow");
        assert!(matches!(rig.notices().last(), Some(Notice::Saved { reason: EndReason::DiskLow, .. })));
    }

    #[test]
    fn warnings_pile_up_during_a_session_and_clear_for_the_next_one() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Engine(EngineEvent::Alert(Alert::Hung { track: TrackKind::Mic, secs: 10 })));
        rig.send(Msg::Engine(EngineEvent::Alert(Alert::HdrActive)));
        let warnings: Vec<Vec<String>> = rig.events().into_iter().filter_map(|e| if let Ui::Warnings(w) = e { Some(w) } else { None }).collect();
        assert_eq!(warnings.len(), 2, "开始时清空一次，Hung 一次；HDR 只是提示，不算告警");
        assert!(warnings[0].is_empty() && warnings[1].len() == 1);
        assert_eq!(rig.notices().iter().filter(|n| matches!(n, Notice::Alert(_))).count(), 2, "两条都要走通知，是否弹出由通知模式决定");
        rig.send(Msg::Command(Command::Stop, 5_000));
        rig.send(Msg::Command(Command::Start, 9_000));
        assert!(matches!(rig.events().iter().rev().find(|e| matches!(e, Ui::Warnings(_))), Some(Ui::Warnings(w)) if w.is_empty()));
    }

    #[test]
    fn quit_while_recording_saves_first_then_quits() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Command(Command::Quit, 5_000));
        assert_eq!(rig.calls().last().unwrap(), "stop User");
        let events = rig.events();
        assert_eq!(events.last(), Some(&Ui::Quit));
        let saved_at = events.iter().position(|e| matches!(e, Ui::Notice(Notice::Saved { .. }))).unwrap();
        assert!(saved_at < events.len() - 1, "先保存再退出");
    }

    #[test]
    fn sleep_saves_the_session_signals_the_waiter_and_keeps_the_app_alive() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        let (done, wait) = unbounded();
        rig.send(Msg::Auto(EndReason::Sleep, Some(done)));
        assert_eq!(rig.calls().last().unwrap(), "stop Sleep");
        assert!(wait.try_recv().is_ok(), "保存完要通知等着的系统事件线程");
        assert!(!rig.events().contains(&Ui::Quit));
        assert_eq!(rig.runner.state(), State::Idle);
        // 睡眠时本来就没在录：立刻放行
        let (done, wait) = unbounded();
        rig.send(Msg::Auto(EndReason::Sleep, Some(done)));
        assert!(wait.try_recv().is_ok());
    }

    #[test]
    fn ending_the_session_saves_then_quits_and_signals() {
        let mut rig = Rig::new();
        rig.send(Msg::Command(Command::Start, 1_000));
        let (done, wait) = unbounded();
        rig.send(Msg::EndSession(EndReason::Shutdown, Some(done)));
        assert_eq!(rig.calls().last().unwrap(), "stop Shutdown");
        assert_eq!(rig.events().last(), Some(&Ui::Quit));
        assert!(wait.try_recv().is_ok());
        // 没在录时也要退出并放行
        let mut idle = Rig::new();
        let (done, wait) = unbounded();
        idle.send(Msg::EndSession(EndReason::Logoff, Some(done)));
        assert_eq!(idle.events(), vec![Ui::Quit]);
        assert!(wait.try_recv().is_ok());
    }

    #[test]
    fn progress_is_reported_only_while_recording() {
        let mut rig = Rig::new();
        rig.send(Msg::Tick);
        assert!(rig.events().is_empty());
        rig.send(Msg::Command(Command::Start, 1_000));
        rig.send(Msg::Tick);
        rig.send(Msg::Tick);
        let progress: Vec<Progress> = rig.events().into_iter().filter_map(|e| if let Ui::Progress(p) = e { Some(p) } else { None }).collect();
        assert_eq!(progress.len(), 2);
        assert!(progress[1].elapsed > progress[0].elapsed);
        rig.send(Msg::Command(Command::Stop, 5_000));
        let before = rig.events().len();
        rig.send(Msg::Tick);
        assert_eq!(rig.events().len(), before);
    }

    #[test]
    fn the_spawned_runner_serves_commands_from_other_threads() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let ui = Arc::new(Mutex::new(Vec::new()));
        let sink = ui.clone();
        let handle = spawn(Fake { calls: calls.clone(), ..Fake::default() }, move |e| sink.lock().unwrap().push(e));
        handle.command(Command::Start);
        let wait = handle.auto(EndReason::Sleep);
        wait.recv_timeout(Duration::from_secs(5)).expect("保存完成的信号");
        assert_eq!(*calls.lock().unwrap(), vec!["start".to_string(), "stop Sleep".to_string()]);
        assert!(ui.lock().unwrap().iter().any(|e| matches!(e, Ui::Notice(Notice::Saved { reason: EndReason::Sleep, .. }))));
    }
}
