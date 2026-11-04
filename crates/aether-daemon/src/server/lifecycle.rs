use crate::server::*;

pub(crate) fn event_loop(
    state: &mut State,
    command_rx: mpsc::Receiver<Command>,
    accept_thread: thread::JoinHandle<()>,
) -> Result<(), DaemonError> {
    let mut shutdown = false;
    let mut last_idle_cleanup = std::time::Instant::now();
    let mut last_schedule_tick = std::time::Instant::now();
    let mut last_health = Instant::now();
    let mut last_stall = Instant::now();
    while !shutdown {
        // Finalize runs off-thread; applying its result is nearly free.
        while let Ok(outcome) = state.finalize_rx.try_recv() {
            state.finalize_pending = state.finalize_pending.saturating_sub(1);
            apply_finalize(state, outcome);
        }
        // Idle: block on the command channel instead of polling plugins.
        if state.active.is_empty() {
            // A pending finalize must not wait out the full idle timeout.
            let wait = if state.finalize_pending > 0 {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(1)
            };
            match command_rx.recv_timeout(wait) {
                Ok(command) => {
                    shutdown |= dispatch(state, command);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if last_idle_cleanup.elapsed() >= Duration::from_secs(300) {
                        state.manager.stop_all();
                        state.live.clear();
                        last_idle_cleanup = std::time::Instant::now();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            // Retry bookkeeping must run even when commands keep arriving.
            check_retries(state);
            if last_health.elapsed() >= Duration::from_secs(5) {
                last_health = Instant::now();
                recover_dead_plugins(state);
                check_stalled_tasks(state);
                flush_unpersisted(state);
            }
            if last_schedule_tick.elapsed() >= Duration::from_secs(15) {
                last_schedule_tick = std::time::Instant::now();
                tick_schedule(state);
            }
            continue;
        }

        // Active downloads: drain commands, then poll plugin events.
        while let Ok(command) = command_rx.try_recv() {
            shutdown |= dispatch(state, command);
        }
        match state.manager.recv_event(Duration::from_millis(100)) {
            Ok(Some(envelope)) => handle_event(state, envelope),
            Ok(None) => {}
            Err(error) => {
                eprintln!("aether-daemon: plugin event error: {error}");
                recover_dead_plugins(state);
            }
        }
        check_retries(state);
        if last_health.elapsed() >= Duration::from_secs(5) {
            last_health = Instant::now();
            recover_dead_plugins(state);
            flush_unpersisted(state);
        }
        if last_stall.elapsed() >= Duration::from_secs(30) {
            last_stall = Instant::now();
            check_stalled_tasks(state);
        }
        if last_schedule_tick.elapsed() >= Duration::from_secs(15) {
            last_schedule_tick = std::time::Instant::now();
            tick_schedule(state);
        }
    }

    // Give an in-flight finalize a bounded chance to record the completion
    // before the process exits, so a restart does not re-download the task.
    while state.finalize_pending > 0 {
        match state.finalize_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(outcome) => {
                state.finalize_pending = state.finalize_pending.saturating_sub(1);
                apply_finalize(state, outcome);
            }
            Err(_) => break,
        }
    }
    state.manager.shutdown();
    state.services.stop_all();
    drop(accept_thread);
    Ok(())
}
